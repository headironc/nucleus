import { spawn, spawnSync } from "node:child_process";
import { createHash, randomBytes } from "node:crypto";
import {
  appendFileSync,
  chmodSync,
  closeSync,
  constants,
  copyFileSync,
  createReadStream,
  createWriteStream,
  existsSync,
  fstatSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  readSync,
  readFileSync,
  readdirSync,
  rmSync,
  statSync,
  statfsSync,
  writeFileSync,
  writeSync,
} from "node:fs";
import { copyFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { pipeline } from "node:stream/promises";
import { fileURLToPath } from "node:url";
import { createGzip } from "node:zlib";
import { imageInputs } from "./halo-ci-image";
import { createDocker, currentIdentity, prepareAccounts } from "./halo-local";
import { prepareHaloDependencies } from "./halo-image-runtime";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const sha = /^[0-9a-f]{40}$/;
const digest = /^sha256:[0-9a-f]{64}$/;
const read = (path: string) => readFileSync(join(root, path), "utf8");
const pins = JSON.parse(read(".github/halo-ci/artifact-tools.json")) as {
  backend: string;
  buildx: { version: string; url: string; size: number; sha256: string };
  skopeo: string;
  gitleaks: { version: string; url: string; sha256: string };
};
const gitEnv = Object.fromEntries(
  Object.entries(process.env).filter(([key]) => !key.startsWith("GIT_")),
);

function capture(command: string, args: string[], cwd = root) {
  const result = spawnSync(command, args, {
    cwd,
    env: command === "git" ? gitEnv : process.env,
    encoding: "utf8",
    maxBuffer: 32 * 1024 * 1024,
  });
  if (result.error || result.status !== 0)
    throw new Error(
      `${command} failed (${result.status}): ${result.error?.message ?? result.stderr.trim()}`,
    );
  return result.stdout.trim();
}
const git = (...args: string[]) => capture("git", ["--no-pager", ...args]);

export type Source = {
  event: "pull_request" | "push" | "workflow_dispatch" | "local";
  source: string;
  before?: string;
  head?: string;
  base?: string;
  merge?: string;
  reason?: string;
};

export function bindSource(
  event: string,
  payload: any,
  env: NodeJS.ProcessEnv,
  execute = git,
): Source {
  const full = (value: unknown): string => {
    if (typeof value !== "string" || !sha.test(value) || /^0+$/.test(value))
      throw new Error("A full nonzero source SHA is required");
    if (execute("rev-parse", "--verify", `${value}^{commit}`) !== value)
      throw new Error("Source commit unavailable");
    return value;
  };
  const actual = full(execute("rev-parse", "HEAD"));
  if (event === "local") return { event, source: actual };
  if (
    env.GITHUB_REPOSITORY !== "headironc/nucleus" ||
    env.GITHUB_REPOSITORY_ID !== "1432619931"
  )
    throw new Error("Wrong artifact repository");
  if (
    !/^[1-9][0-9]*$/.test(env.GITHUB_RUN_ID ?? "") ||
    !/^[1-9][0-9]*$/.test(env.GITHUB_RUN_ATTEMPT ?? "")
  )
    throw new Error("Invalid workflow run identity");
  if (event === "pull_request") {
    const pr = payload.pull_request;
    if (
      !["dev", "main"].includes(pr?.base?.ref) ||
      pr.base.repo.id !== 1432619931
    )
      throw new Error("Wrong PR base");
    const head = full(pr.head.sha),
      base = full(pr.base.sha),
      merge = full(env.GITHUB_SHA);
    if (
      actual !== merge ||
      execute("show", "-s", "--format=%P", merge) !== `${base} ${head}`
    )
      throw new Error("PR merge does not match event parents");
    return { event, source: actual, head, base, merge, before: base };
  }
  if (event === "push") {
    if (
      env.GITHUB_REF !== "refs/heads/dev" ||
      payload.ref !== "refs/heads/dev" ||
      full(payload.after) !== actual ||
      env.GITHUB_SHA !== actual
    )
      throw new Error("Wrong push source");
    // An absent/deleted previous commit makes comparison conservative, not a skip.
    const before =
      typeof payload.before === "string" &&
      sha.test(payload.before) &&
      !/^0+$/.test(payload.before)
        ? payload.before
        : undefined;
    return { event, source: actual, ...(before ? { before } : {}) };
  }
  if (event === "workflow_dispatch") {
    if (
      env.GITHUB_REF !== "refs/heads/dev" ||
      env.GITHUB_WORKFLOW_REF !==
        "headironc/nucleus/.github/workflows/halo-image.yml@refs/heads/dev"
    )
      throw new Error("Manual build must use the dev workflow definition");
    const source = full(payload.inputs?.revision);
    const reason = payload.inputs?.reason;
    if (typeof reason !== "string" || !reason.trim() || reason.length > 500)
      throw new Error("Manual build reason required (1..500 characters)");
    execute("merge-base", "--is-ancestor", source, "refs/remotes/origin/dev");
    if (actual !== source) throw new Error("Manual source checkout mismatch");
    return { event, source, reason };
  }
  throw new Error("Unsupported artifact event");
}

// Every executable consumer is covered, including newly added tools/tests.
export function validationInput(path: string) {
  return (
    path.startsWith("tools/") ||
    path.startsWith("halo/tools/") ||
    path.startsWith(".github/halo-ci/") ||
    path === ".github/workflows/halo-image.yml" ||
    ["package.json", "tsconfig.json", "halo/tsconfig.json"].includes(path)
  );
}

export function shouldBuild(
  beforeRead: ((path: string) => string) | undefined,
  afterRead: (path: string) => string,
  changed: string[],
): { build: boolean; reason: string } {
  const current = imageInputs(afterRead);
  if (!beforeRead) return { build: true, reason: "comparison-unavailable" };
  try {
    if (imageInputs(beforeRead).digest !== current.digest)
      return { build: true, reason: "environment-inputs-changed" };
  } catch {
    return { build: true, reason: "comparison-inputs-invalid" };
  }
  if (changed.some(validationInput))
    return { build: true, reason: "validation-inputs-changed" };
  return { build: false, reason: "environment-and-validation-unchanged" };
}

export function plan(source: Source) {
  if (source.event === "local" || source.event === "workflow_dispatch")
    return { build: true, reason: source.event };
  if (!source.before) return shouldBuild(undefined, read, []);
  let changed: string[];
  try {
    changed = git(
      "diff",
      "--no-ext-diff",
      "--name-only",
      "-z",
      source.before,
      source.source,
    )
      .split("\0")
      .filter(Boolean);
  } catch {
    return shouldBuild(undefined, read, []);
  }
  return shouldBuild(
    (path) => {
      const result = spawnSync("git", ["show", `${source.before}:${path}`], {
        cwd: root,
        env: gitEnv,
        encoding: "utf8",
      });
      if (result.error || result.status !== 0)
        throw new Error("Comparison file unavailable");
      return result.stdout;
    },
    read,
    changed,
  );
}

export async function fileDigest(path: string) {
  const hash = createHash("sha256");
  for await (const bytes of createReadStream(path)) hash.update(bytes);
  return `sha256:${hash.digest("hex")}`;
}
function json(path: string, value: unknown) {
  writeFileSync(path, JSON.stringify(value, null, 2) + "\n", { flag: "wx" });
}
function outputs(values: Record<string, string>) {
  if (process.env.GITHUB_OUTPUT)
    appendFileSync(
      process.env.GITHUB_OUTPUT,
      Object.entries(values)
        .map(([key, value]) => `${key}=${value}\n`)
        .join(""),
    );
}

function validationSources() {
  const paths = new Set([
    ".github/workflows/halo-image.yml",
    ".tool-versions",
    "rust-toolchain.toml",
    "fixture/postgresql.ts",
    "fixture/postgresql.version",
    "halo/package.json",
    "halo/bun.lock",
    "package.json",
    "bun.lock",
    "tsconfig.json",
  ]);
  for (const pattern of ["tools/**/*", "halo/tools/**/*", ".github/halo-ci/*"])
    for (const path of new Bun.Glob(pattern).scanSync({
      cwd: root,
      onlyFiles: true,
      followSymlinks: false,
    }))
      paths.add(path);
  return Object.fromEntries(
    [...paths].sort().map((path) => [
      path,
      createHash("sha256")
        .update(readFileSync(join(root, path)))
        .digest("hex"),
    ]),
  );
}

type OCIIdentity = {
  manifest: { digest: string; size: number };
  config: { digest: string; size: number };
  layers: { digest: string; size: number }[];
  platform: { os: string; architecture: string };
};
const inspect = (archive: string, dockerRecords: string[] = []) =>
  JSON.parse(
    capture("python3", [
      join(root, "tools/halo-image-oci.py"),
      "inspect",
      archive,
      ...dockerRecords,
    ]),
  ) as OCIIdentity;

export type DockerArchive = {
  configDigest: string;
  configSize: number;
  platform: { os: string; architecture: string };
  config: { rootfs: { diff_ids: string[] }; config: Record<string, unknown> };
  layers: { path: string; size: number; digest: string; diffId: string }[];
};
export const inspectDockerArchive = (
  archive: string,
  metadata?: string,
  manifest?: string,
) =>
  JSON.parse(
    capture("python3", [
      join(root, "tools/halo-image-oci.py"),
      "docker",
      archive,
      ...(metadata && manifest ? [metadata, manifest] : []),
    ]),
  ) as DockerArchive;

export function imageDescriptor(value: unknown) {
  const descriptor = value as Record<string, unknown> | undefined;
  if (
    !descriptor ||
    typeof descriptor.digest !== "string" ||
    !digest.test(descriptor.digest) ||
    !Number.isSafeInteger(descriptor.size) ||
    Number(descriptor.size) <= 0 ||
    ![
      "application/vnd.oci.image.manifest.v1+json",
      "application/vnd.docker.distribution.manifest.v2+json",
      "application/vnd.oci.image.index.v1+json",
      "application/vnd.docker.distribution.manifest.list.v2+json",
    ].includes(String(descriptor.mediaType))
  )
    throw new Error("Unsupported image descriptor");
  return descriptor;
}

export function buildIdentity(
  metadata: unknown,
  archive?: DockerArchive,
  rawManifest?: Uint8Array,
): {
  configDigest: string;
  binding: string;
  descriptor?: ReturnType<typeof imageDescriptor>;
} {
  if (!metadata || typeof metadata !== "object" || Array.isArray(metadata))
    throw new Error("Build metadata object required");
  const value = metadata as Record<string, unknown>;
  const direct = value?.["containerimage.config.digest"];
  const supplied = Object.hasOwn(value, "containerimage.config.digest");
  if (!Object.hasOwn(value, "containerimage.descriptor")) {
    if (
      !supplied ||
      typeof direct !== "string" ||
      !digest.test(direct) ||
      !archive ||
      direct !== archive.configDigest ||
      rawManifest
    )
      throw new Error("Direct build config binding unavailable or conflicting");
    return { configDigest: direct, binding: "config-digest" };
  }
  const manifestDigest = value?.["containerimage.digest"];
  const descriptor = imageDescriptor(value?.["containerimage.descriptor"]);
  if (
    (supplied && (typeof direct !== "string" || !digest.test(direct))) ||
    typeof manifestDigest !== "string" ||
    !digest.test(manifestDigest) ||
    descriptor.digest !== manifestDigest ||
    ![
      "application/vnd.oci.image.manifest.v1+json",
      "application/vnd.docker.distribution.manifest.v2+json",
    ].includes(String(descriptor.mediaType))
  )
    throw new Error("Invalid Buildx config/descriptor metadata");
  let configDigest = direct as string | undefined;
  const platform = (value: Record<string, unknown>) => {
    if (Object.hasOwn(value, "platform")) {
      const actual = value.platform as Record<string, unknown> | null;
      if (
        !actual ||
        actual.os !== "linux" ||
        actual.architecture !== "amd64" ||
        Object.keys(actual).length !== 2
      )
        throw new Error("Build manifest platform mismatch");
    }
  };
  platform(descriptor);
  if (rawManifest) {
    if (
      !archive ||
      rawManifest.length !== descriptor.size ||
      `sha256:${createHash("sha256").update(rawManifest).digest("hex")}` !==
        manifestDigest
    )
      throw new Error("Build manifest bytes mismatch");
    const manifest = JSON.parse(Buffer.from(rawManifest).toString("utf8"));
    if (
      manifest.schemaVersion !== 2 ||
      manifest.mediaType !== descriptor.mediaType ||
      !Array.isArray(manifest.layers)
    )
      throw new Error("Unsupported build manifest");
    platform(manifest);
    const docker =
      descriptor.mediaType ===
      "application/vnd.docker.distribution.manifest.v2+json";
    const configType = docker
      ? "application/vnd.docker.container.image.v1+json"
      : "application/vnd.oci.image.config.v1+json";
    const layerTypes = docker
      ? [
          "application/vnd.docker.image.rootfs.diff.tar",
          "application/vnd.docker.image.rootfs.diff.tar.gzip",
        ]
      : [
          "application/vnd.oci.image.layer.v1.tar",
          "application/vnd.oci.image.layer.v1.tar+gzip",
        ];
    const config = manifest.config;
    if (
      !config ||
      config.mediaType !== configType ||
      config.digest !== archive.configDigest ||
      config.size !== archive.configSize ||
      (supplied && direct !== config.digest)
    )
      throw new Error("Build manifest config conflict");
    platform(config);
    if (
      archive.platform.os !== "linux" ||
      archive.platform.architecture !== "amd64" ||
      manifest.layers.length !== archive.layers.length ||
      archive.layers.length !== archive.config.rootfs.diff_ids.length
    )
      throw new Error("Build manifest layer/platform conflict");
    manifest.layers.forEach((layer: Record<string, unknown>, index: number) => {
      const actual = archive.layers[index]!;
      if (
        !layer ||
        !layerTypes.includes(String(layer.mediaType)) ||
        Object.hasOwn(layer, "urls") ||
        layer.digest !== actual.digest ||
        layer.size !== actual.size ||
        actual.diffId !== archive.config.rootfs.diff_ids[index]
      )
        throw new Error("Build manifest layer conflict");
      platform(layer);
    });
    configDigest = config.digest;
  }
  if (!configDigest || (archive && configDigest !== archive.configDigest))
    throw new Error("Build config binding unavailable or conflicting");
  return {
    configDigest,
    descriptor,
    binding: supplied ? "config-digest" : "manifest-config",
  };
}

export function assertRestoredArchive(
  original: DockerArchive,
  restored: DockerArchive,
  oci: OCIIdentity,
) {
  if (
    original.configDigest !== oci.config.digest ||
    restored.configDigest !== oci.config.digest ||
    JSON.stringify(restored.config) !== JSON.stringify(original.config) ||
    JSON.stringify(restored.platform) !== JSON.stringify(oci.platform) ||
    JSON.stringify(restored.layers.map((layer) => layer.diffId)) !==
      JSON.stringify(original.config.rootfs.diff_ids)
  )
    throw new Error("Restored config/platform/RootFS mismatch");
}

export async function verifyArtifact(
  directory: string,
  expected: {
    source: string;
    run: string | null;
    attempt: string | null;
    tar?: string;
    manifest?: string;
    config?: string;
    binding?: Source;
    workflowRevision?: string;
  },
) {
  const provenance = JSON.parse(
    readFileSync(join(directory, "provenance.json"), "utf8"),
  );
  const validation = JSON.parse(
    readFileSync(join(directory, "validation.json"), "utf8"),
  );
  if (
    provenance.protocol !== 2 ||
    provenance.archive.format !== "oci-image-layout-1.0.0" ||
    provenance.repository.id !== 1432619931 ||
    provenance.repository.name !== "headironc/nucleus" ||
    provenance.source.source !== expected.source ||
    provenance.workflow.run !== expected.run ||
    provenance.workflow.attempt !== expected.attempt ||
    provenance.workflow.path !== ".github/workflows/halo-image.yml"
  )
    throw new Error("Artifact provenance identity mismatch");
  if (
    (expected.binding &&
      JSON.stringify(provenance.source) !== JSON.stringify(expected.binding)) ||
    (expected.workflowRevision &&
      (provenance.workflow.revision !== expected.workflowRevision ||
        provenance.workflow.job !== "build"))
  )
    throw new Error("Artifact event/workflow binding mismatch");
  if (
    validation.success !== true ||
    validation.cleanup.success !== true ||
    !validation.runtime.success ||
    !validation.runtime.pgDirectoryRemoved ||
    !validation.runtime.postmasterGone ||
    validation.scanFindings !== 0
  )
    throw new Error("Artifact acceptance incomplete");
  const archive = join(directory, "image.oci.tar");
  const tar = await fileDigest(archive);
  if (
    tar !== provenance.archive.sha256 ||
    (expected.tar && tar !== expected.tar)
  )
    throw new Error("OCI archive SHA mismatch");
  const identity = inspect(archive, [
    join(directory, "build-archive.json"),
    join(directory, "restored-archive.json"),
  ]);
  if (
    JSON.stringify(identity) !== JSON.stringify(provenance.oci) ||
    identity.config.digest !== provenance.build.configDigest ||
    identity.config.digest !== provenance.restored.configDigest ||
    (expected.manifest && identity.manifest.digest !== expected.manifest) ||
    (expected.config && identity.config.digest !== expected.config)
  )
    throw new Error("OCI manifest/config identity mismatch");
  const entries = provenance.materials as Record<string, string>;
  for (const required of [
    "source-files.json",
    "config.json",
    "history.json",
    "oci.json",
    "layers.jsonl",
    "scan.json",
    "scan-policy.json",
    "runtime.json",
    "packages.tsv",
    "licenses.tar.gz",
    "licenses.json",
    "validation.json",
    "build-metadata.json",
    "build-archive.json",
    "restored-archive.json",
    "runtime-reference.json",
    "default-user.json",
    ...(entries?.["build-manifest.json"] ? ["build-manifest.json"] : []),
  ])
    if (!entries?.[required])
      throw new Error(`Artifact material mismatch: ${required}`);
  for (const [file, hash] of Object.entries(entries))
    if (
      !/^[a-z][a-z0-9.-]*$/.test(file) ||
      !digest.test(hash) ||
      (await fileDigest(join(directory, file))) !== hash
    )
      throw new Error(`Artifact material mismatch: ${file}`);
  const policySha = await fileDigest(
    join(root, ".github/halo-ci/scan-exceptions.json"),
  );
  const scan = JSON.parse(readFileSync(join(directory, "scan.json"), "utf8"));
  const policy = JSON.parse(
    readFileSync(join(directory, "scan-policy.json"), "utf8"),
  );
  if (
    entries["scan-policy.json"] !== policySha ||
    scan.policySha256 !== policySha ||
    scan.findings.length !== 0 ||
    scan.unresolved !== 0 ||
    scan.detected !== scan.accepted.length ||
    scan.accepted.some(
      (item: any) =>
        !policy.findings.some(
          (approved: any) =>
            JSON.stringify(approved) === JSON.stringify(item.evidence),
        ),
    )
  )
    throw new Error("Artifact scan disposition or candidate policy mismatch");
  const built = JSON.parse(
    readFileSync(join(directory, "build-archive.json"), "utf8"),
  ) as DockerArchive;
  const defaultUser = JSON.parse(
    readFileSync(join(directory, "default-user.json"), "utf8"),
  );
  if (
    !Number.isSafeInteger(defaultUser.uid) ||
    defaultUser.uid <= 0 ||
    defaultUser.reference !== provenance.runtime.reference
  )
    throw new Error("Image default user acceptance missing");
  const restored = JSON.parse(
    readFileSync(join(directory, "restored-archive.json"), "utf8"),
  ) as DockerArchive;
  const metadata = buildIdentity(
    JSON.parse(readFileSync(join(directory, "build-metadata.json"), "utf8")),
    built,
    existsSync(join(directory, "build-manifest.json"))
      ? readFileSync(join(directory, "build-manifest.json"))
      : undefined,
  );
  if (
    metadata.configDigest !== provenance.build.configDigest ||
    built.configSize !== identity.config.size ||
    metadata.binding !== provenance.build.binding ||
    JSON.stringify(metadata.descriptor) !==
      JSON.stringify(provenance.build.descriptor) ||
    !digest.test(provenance.build.runtimeReference)
  )
    throw new Error("Build config binding mismatch");
  if (provenance.build.daemonDescriptor !== null)
    imageDescriptor(provenance.build.daemonDescriptor);
  assertRestoredArchive(built, restored, identity);
  const runtime = JSON.parse(
    readFileSync(join(directory, "runtime-reference.json"), "utf8"),
  );
  if (runtime.descriptor !== null) imageDescriptor(runtime.descriptor);
  if (
    !digest.test(runtime.reference) ||
    runtime.reference !== runtime.containerImage ||
    runtime.reference !== runtime.requestedImage ||
    JSON.stringify(runtime) !== JSON.stringify(provenance.runtime)
  )
    throw new Error("Runtime reference binding mismatch");
  if (
    expected.binding &&
    readFileSync(join(directory, "source-files.json"), "utf8").trim() !==
      JSON.stringify(validationSources(), null, 2)
  )
    throw new Error("Artifact validation source bytes mismatch");
  const actual = readdirSync(directory).sort();
  if (
    JSON.stringify(actual) !==
    JSON.stringify(
      ["image.oci.tar", "provenance.json", ...Object.keys(entries)].sort(),
    )
  )
    throw new Error("Unexpected artifact files");
  return { tar, identity };
}

export async function retainArchive(
  source: string,
  destination: string,
  expected: string,
) {
  await copyFile(
    source,
    destination,
    constants.COPYFILE_EXCL | constants.COPYFILE_FICLONE,
  );
  chmodSync(destination, 0o444);
  if ((await fileDigest(destination)) !== expected)
    throw new Error("Retained OCI bytes differ from the frozen archive");
  // Keep the original on any failed or unverified copy.
  rmSync(source);
}

export async function saveRestoredArchive(
  reference: string,
  output: string,
  signal: AbortSignal,
) {
  signal.throwIfAborted();
  const child = spawn("docker", ["save", reference], {
    cwd: root,
    stdio: ["ignore", "pipe", "inherit"],
  });
  let closed = false;
  let spawnError: Error | undefined;
  let killTimer: ReturnType<typeof setTimeout> | undefined;
  const terminal = new Promise<number | null>((done) => {
    child.once("error", (error) => {
      spawnError = error;
    });
    child.once("close", (status) => {
      closed = true;
      done(status);
    });
  });
  const stop = () => {
    if (closed || killTimer) return;
    child.kill("SIGTERM");
    // Bound termination of this CLI; retain the archive until it has closed.
    killTimer = setTimeout(() => {
      child.kill("SIGKILL");
    }, 10000);
  };
  signal.addEventListener("abort", stop, { once: true });
  try {
    await pipeline(
      child.stdout,
      createGzip(),
      createWriteStream(output, { flags: "wx", mode: 0o600 }),
      { signal },
    );
    const status = await terminal;
    if (spawnError) throw spawnError;
    if (status !== 0 || signal.aborted)
      throw new Error(
        `Docker archive export failed (${status}, cancelled=${signal.aborted})`,
      );
  } catch (error) {
    stop();
    await terminal;
    throw error;
  } finally {
    signal.removeEventListener("abort", stop);
    clearTimeout(killTimer);
  }
}

export function importRuntimeEvidence(directory: string, output: string) {
  const files = [
    "licenses.json",
    "licenses.tar.gz",
    "packages.tsv",
    "pg-ready.json",
    "runtime.json",
  ];
  const descriptors: number[] = [];
  try {
    const directoryFlags =
      constants.O_RDONLY | constants.O_DIRECTORY | constants.O_NOFOLLOW;
    const source = openSync(directory, directoryFlags);
    descriptors.push(source);
    const destination = openSync(output, directoryFlags);
    descriptors.push(destination);
    // The pipeline runs on Linux. Anchor every child to the opened directory,
    // rather than resolving a container-controlled path again.
    const sourcePath = `/proc/self/fd/${source}`;
    const destinationPath = `/proc/self/fd/${destination}`;
    if (
      JSON.stringify(readdirSync(sourcePath).sort()) !== JSON.stringify(files)
    )
      throw new Error("Unexpected runtime evidence files");
    const inputs = files.map((file) => {
      const descriptor = openSync(
        `${sourcePath}/${file}`,
        constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK,
      );
      descriptors.push(descriptor);
      const stat = fstatSync(descriptor);
      if (!stat.isFile() || stat.nlink !== 1)
        throw new Error(
          `Runtime evidence must be a regular file with one link: ${file}`,
        );
      return { file, descriptor };
    });
    const buffer = Buffer.alloc(64 * 1024);
    for (const { file, descriptor } of inputs) {
      const target = openSync(
        `${destinationPath}/${file}`,
        constants.O_WRONLY |
          constants.O_CREAT |
          constants.O_EXCL |
          constants.O_NOFOLLOW,
        0o600,
      );
      try {
        let bytes: number;
        while (
          (bytes = readSync(descriptor, buffer, 0, buffer.length, null)) > 0
        ) {
          let offset = 0;
          while (offset < bytes) {
            const written = writeSync(target, buffer, offset, bytes - offset);
            if (!written) throw new Error("Runtime evidence copy stalled");
            offset += written;
          }
        }
      } finally {
        closeSync(target);
      }
    }
  } finally {
    for (const descriptor of descriptors.reverse()) closeSync(descriptor);
  }
}

export async function buildArtifact(output: string, source: Source) {
  const hostIdentity = currentIdentity();
  if (existsSync(output))
    throw new Error("Artifact output must be new; preserve earlier attempts");
  mkdirSync(output, { recursive: true, mode: 0o700 });
  const sourceFiles = validationSources();
  json(join(output, "source-files.json"), sourceFiles);
  const scratch = mkdtempSync(join(tmpdir(), "nucleus-artifact-"));
  const work = join(scratch, "work");
  mkdirSync(work);
  mkdirSync(join(work, "tmp"));
  const nonce = randomBytes(12).toString("hex");
  const tag = `nucleus-image-artifact:${nonce}`;
  const restoredTag = `nucleus-image-restored:${nonce}`;
  const containers: string[] = [];
  const completed: { name: string; state: unknown }[] = [];
  const cleanupErrors: string[] = [];
  let cancelled = false;
  const archiveCancellation = new AbortController();
  let current: ReturnType<typeof spawn> | null = null;
  const cancel = () => {
    cancelled = true;
    archiveCancellation.abort();
    current?.kill("SIGTERM");
  };
  process.on("SIGINT", cancel);
  process.on("SIGTERM", cancel);
  const run = async (
    command: string,
    args: string[],
    container?: string,
    environment?: NodeJS.ProcessEnv,
  ) => {
    if (cancelled) throw new Error("Artifact build cancelled");
    if (container) containers.push(container);
    const child = spawn(command, args, {
      cwd: root,
      stdio: "inherit",
      ...(environment ? { env: environment } : {}),
    });
    current = child;
    const status = await new Promise<number | null>((done, reject) => {
      child.once("error", reject);
      child.once("exit", done);
    });
    current = null;
    if (container) {
      const state = JSON.parse(
        capture("docker", [
          "container",
          "inspect",
          "--format",
          "{{json .State}}",
          container,
        ]),
      );
      if (state.Running) {
        capture("docker", ["stop", "--time", "60", container]);
        capture("docker", ["wait", container]);
      }
      const final = JSON.parse(
        capture("docker", [
          "container",
          "inspect",
          "--format",
          "{{json .State}}",
          container,
        ]),
      );
      completed.push({ name: container, state: final });
      if (final.Running || final.ExitCode !== 0 || cancelled)
        throw new Error(`Container ${container} failed or was cancelled`);
    }
    if (status !== 0 || cancelled)
      throw new Error(`${command} failed (${status}, cancelled=${cancelled})`);
  };
  const identity = `${hostIdentity.uid}:${hostIdentity.gid}`;
  const skopeo = async (args: string[], serial: string, input?: string) => {
    const name = `nucleus-artifact-${nonce}-${serial}`;
    await run(
      "docker",
      [
        "run",
        "--name",
        name,
        "--pull=never",
        "--network=none",
        "--read-only",
        "--user",
        identity,
        "--tmpfs",
        "/tmp:rw,nosuid,nodev,mode=1777",
        "-v",
        `${work}:/data`,
        ...(input ? ["-v", `${input}:/frozen-image.oci.tar:ro`] : []),
        "--entrypoint",
        "skopeo",
        pins.skopeo,
        "--tmpdir",
        "/data/tmp",
        ...args,
      ],
      name,
    );
  };
  let runtimeReference: string | undefined;
  let builtIdentity: ReturnType<typeof buildIdentity> | undefined;
  let builtDaemon:
    | {
        runtimeReference: string;
        daemonDescriptor: Record<string, unknown> | null;
      }
    | undefined;
  let restoredArchive: DockerArchive | undefined;
  let runtimeBinding: Record<string, unknown> | undefined;
  let oci: OCIIdentity | undefined;
  let failure: unknown;
  let versions: Record<string, string> = {};
  let archiveSha: string | undefined;
  const disk = (stage: string) => {
    const space = statfsSync(work);
    console.log(
      "[image-disk]",
      JSON.stringify({
        stage,
        availableBytes: space.bavail * space.bsize,
        temporaryArchives: Object.fromEntries(
          ["built.tar", "image.oci.tar", "restored.tar", "roundtrip.tar.gz"]
            .filter((file) => existsSync(join(work, file)))
            .map((file) => [file, statSync(join(work, file)).size]),
        ),
        retainedBytes: existsSync(join(output, "image.oci.tar"))
          ? statSync(join(output, "image.oci.tar")).size
          : 0,
        archiveSha256: archiveSha,
        manifest: oci?.manifest.digest,
        config: oci?.config.digest,
      }),
    );
  };
  try {
    const inputs = imageInputs();
    if (
      Bun.version !== inputs.bun ||
      capture("node", ["--version"]) !== `v${inputs.node}`
    )
      throw new Error("Use the repository's exact Bun/Node pins");
    versions = {
      bun: Bun.version,
      node: capture("node", ["--version"]),
      docker: capture("docker", ["version", "--format", "{{.Server.Version}}"]),
      python: capture("python3", ["--version"]),
    };
    const installedSkopeo = spawnSync(
      "docker",
      ["image", "inspect", pins.skopeo],
      { encoding: "utf8" },
    );
    if (installedSkopeo.status !== 0) {
      if (
        installedSkopeo.error ||
        !/no such (image|object)/i.test(installedSkopeo.stderr)
      )
        throw new Error("Skopeo image availability unknown");
      await run("docker", ["pull", pins.skopeo]);
    }
    const skopeoPlatform = JSON.parse(
      capture("docker", [
        "image",
        "inspect",
        "--format",
        "{{json .}}",
        pins.skopeo,
      ]),
    );
    if (
      skopeoPlatform.Os !== "linux" ||
      skopeoPlatform.Architecture !== "amd64"
    )
      throw new Error("Wrong Skopeo platform");
    versions.skopeo = capture("docker", [
      "run",
      "--rm",
      "--network=none",
      "--read-only",
      "--user",
      identity,
      "--entrypoint",
      "skopeo",
      pins.skopeo,
      "--version",
    ]);
    capture("docker", [
      "run",
      "--rm",
      "--network=none",
      "--read-only",
      "--user",
      identity,
      "--entrypoint",
      "skopeo",
      pins.skopeo,
      "copy",
      "--help",
    ]);
    const scannerArchive = process.env.NUCLEUS_IMAGE_GITLEAKS_ARCHIVE
      ? resolve(process.env.NUCLEUS_IMAGE_GITLEAKS_ARCHIVE)
      : join(scratch, "gitleaks.tar.gz");
    if (!process.env.NUCLEUS_IMAGE_GITLEAKS_ARCHIVE)
      await run("curl", [
        "--fail",
        "--location",
        "--silent",
        "--show-error",
        "--connect-timeout",
        "10",
        "--max-time",
        "120",
        "--retry",
        "1",
        "--retry-all-errors",
        "--retry-delay",
        "2",
        "--output",
        scannerArchive,
        pins.gitleaks.url,
      ]);
    if ((await fileDigest(scannerArchive)) !== `sha256:${pins.gitleaks.sha256}`)
      throw new Error("Gitleaks release checksum mismatch");
    await run("tar", ["-xzf", scannerArchive, "-C", scratch, "gitleaks"]);
    const scanner = join(scratch, "gitleaks");
    versions.gitleaks = capture(scanner, ["version"]);
    if (versions.gitleaks !== pins.gitleaks.version)
      throw new Error("Gitleaks version mismatch");
    capture(scanner, ["dir", "--help"]);
    const suppliedBuildx = process.env.NUCLEUS_IMAGE_BUILDX_BINARY;
    const buildx = join(scratch, "buildx");
    if (suppliedBuildx) await run("cp", [resolve(suppliedBuildx), buildx]);
    else
      await run("curl", [
        "--fail",
        "--location",
        "--silent",
        "--show-error",
        "--connect-timeout",
        "10",
        "--max-time",
        "120",
        "--output",
        buildx,
        pins.buildx.url,
      ]);
    if (
      statSync(buildx).size !== pins.buildx.size ||
      (await fileDigest(buildx)) !== `sha256:${pins.buildx.sha256}`
    )
      throw new Error("Buildx checksum/size mismatch");
    chmodSync(buildx, 0o700);
    versions.buildx = capture(buildx, ["version"]);
    if (!versions.buildx.includes(`v${pins.buildx.version} `))
      throw new Error("Buildx version mismatch");
    const buildxConfig = join(scratch, "buildx-config");
    mkdirSync(buildxConfig, { mode: 0o700 });
    const buildEnvironment = { ...process.env, BUILDX_CONFIG: buildxConfig };
    const driver = spawnSync(buildx, ["inspect", "default"], {
      env: buildEnvironment,
      encoding: "utf8",
    });
    if (
      driver.error ||
      driver.status !== 0 ||
      !/^Driver:\s+docker\s*$/m.test(driver.stdout)
    )
      throw new Error("Only the existing default Docker driver is supported");
    json(join(output, "tools.json"), versions);
    await run(
      process.execPath,
      [
        "tools/halo-ci-image.ts",
        "build",
        tag,
        "--buildx",
        buildx,
        "--metadata",
        join(output, "build-metadata.json"),
      ],
      undefined,
      buildEnvironment,
    );
    const builtInspect = JSON.parse(
      capture("docker", ["image", "inspect", "--format", "{{json .}}", tag]),
    );
    const builtReference = builtInspect.Id as string;
    if (!digest.test(builtReference))
      throw new Error("Builder image identity unavailable");
    builtDaemon = {
      runtimeReference: builtReference,
      daemonDescriptor: builtInspect.Descriptor
        ? imageDescriptor(builtInspect.Descriptor)
        : null,
    };
    await run("docker", [
      "save",
      "--output",
      join(work, "built.tar"),
      builtReference,
    ]);
    const builtArchive = inspectDockerArchive(
      join(work, "built.tar"),
      join(output, "build-metadata.json"),
      join(output, "build-manifest.json"),
    );
    builtIdentity = buildIdentity(
      JSON.parse(readFileSync(join(output, "build-metadata.json"), "utf8")),
      builtArchive,
      existsSync(join(output, "build-manifest.json"))
        ? readFileSync(join(output, "build-manifest.json"))
        : undefined,
    );
    if (builtArchive.configDigest !== builtIdentity.configDigest)
      throw new Error("Buildx/export config mismatch");
    json(join(output, "build-archive.json"), builtArchive);
    disk("before-export");
    await skopeo(
      [
        "copy",
        "--format",
        "v2s2",
        "--dest-compress-format",
        "gzip",
        "--dest-force-compress-format",
        "docker-archive:/data/built.tar",
        "oci-archive:/data/image.oci.tar:nucleus-ci",
      ],
      "export",
    );
    const exported = join(work, "image.oci.tar");
    oci = inspect(exported);
    if (oci.config.digest !== builtIdentity.configDigest)
      throw new Error("OCI export changed image config");
    chmodSync(exported, 0o444);
    archiveSha = await fileDigest(exported);
    rmSync(join(work, "built.tar"));
    const archive = join(output, "image.oci.tar");
    await retainArchive(exported, archive, archiveSha);
    disk("before-restore");
    await skopeo(
      [
        "copy",
        "--preserve-digests",
        "oci-archive:/frozen-image.oci.tar",
        `docker-archive:/data/restored.tar:${restoredTag}`,
      ],
      "restore",
      archive,
    );
    await run("docker", ["load", "--input", join(work, "restored.tar")]);
    const restoredInspect = JSON.parse(
      capture("docker", [
        "image",
        "inspect",
        "--format",
        "{{json .}}",
        restoredTag,
      ]),
    );
    runtimeReference = restoredInspect.Id;
    if (!runtimeReference || !digest.test(runtimeReference))
      throw new Error("Invalid restored runtime reference");
    rmSync(join(work, "restored.tar"));
    disk("before-roundtrip");
    await saveRestoredArchive(
      runtimeReference,
      join(work, "roundtrip.tar.gz"),
      archiveCancellation.signal,
    );
    restoredArchive = inspectDockerArchive(join(work, "roundtrip.tar.gz"));
    assertRestoredArchive(builtArchive, restoredArchive, oci);
    json(join(output, "restored-archive.json"), restoredArchive);
    rmSync(join(work, "roundtrip.tar.gz"));
    const defaultName = `nucleus-artifact-${nonce}-default-user`;
    await run(
      "docker",
      [
        "run",
        "--name",
        defaultName,
        "--pull=never",
        "--network=none",
        "--read-only",
        "--entrypoint",
        "bun",
        runtimeReference,
        "-e",
        'if (!process.getuid() || !Number.isSafeInteger(process.getuid())) throw Error("Image default user is root or unknown"); console.log(JSON.stringify({uid:process.getuid(),gid:process.getgid()}));',
      ],
      defaultName,
    );
    const defaultUser = JSON.parse(capture("docker", ["logs", defaultName]));
    json(join(output, "default-user.json"), {
      ...defaultUser,
      reference: runtimeReference,
    });
    for (const dir of ["target", "root-modules", "home", "runtime-evidence"])
      mkdirSync(join(scratch, dir));
    const modules = prepareHaloDependencies(
      root,
      join(scratch, "dependencies"),
    );
    const accounts = prepareAccounts(
      createDocker(root, hostIdentity).accounts(runtimeReference),
      hostIdentity,
      "/home/acceptance",
      scratch,
    );
    // Nested bind destinations must already exist in the readonly source bind.
    for (const path of ["target", "node_modules", "halo/node_modules"])
      mkdirSync(join(root, path), { recursive: true });
    // Docker can mount private host paths without making their parents public.
    const name = `nucleus-artifact-${nonce}-runtime`;
    await run(
      "docker",
      [
        "run",
        "--name",
        name,
        "--init",
        "--network=none",
        "--ipc=private",
        "--shm-size=1g",
        "--cpus=2",
        "--pull=never",
        "--read-only",
        "--user",
        identity,
        "--workdir",
        "/workspace",
        "--tmpfs",
        "/tmp:rw,nosuid,nodev,mode=1777",
        "-e",
        "HOME=/home/acceptance",
        "-e",
        "BUN_INSTALL_CACHE_DIR=/home/acceptance/bun-cache",
        "-v",
        `${root}:/workspace:ro`,
        "-v",
        `${join(scratch, "target")}:/workspace/target`,
        "-v",
        `${modules}:/workspace/halo/node_modules:ro`,
        "-v",
        `${join(scratch, "root-modules")}:/workspace/node_modules`,
        "-v",
        `${join(scratch, "home")}:/home/acceptance`,
        ...accounts.flatMap((mount) => [
          "-v",
          `${mount.source}:${mount.target}:ro`,
        ]),
        "-v",
        `${join(scratch, "runtime-evidence")}:/evidence`,
        runtimeReference,
        "bun",
        "tools/halo-image-runtime.ts",
        "/evidence",
      ],
      name,
    );
    const containerImage = capture("docker", [
      "container",
      "inspect",
      "--format",
      "{{.Image}}",
      name,
    ]);
    const requestedImage = capture("docker", [
      "container",
      "inspect",
      "--format",
      "{{.Config.Image}}",
      name,
    ]);
    if (
      containerImage !== runtimeReference ||
      requestedImage !== runtimeReference
    )
      throw new Error("Container used a different runtime reference");
    importRuntimeEvidence(join(scratch, "runtime-evidence"), output);
    runtimeBinding = {
      reference: runtimeReference,
      descriptor: restoredInspect.Descriptor
        ? imageDescriptor(restoredInspect.Descriptor)
        : null,
      containerImage,
      requestedImage,
    };
    json(join(output, "runtime-reference.json"), runtimeBinding);
    copyFileSync(
      join(root, ".github/halo-ci/scan-exceptions.json"),
      join(output, "scan-policy.json"),
    );
    await run("python3", [
      "tools/halo-image-oci.py",
      "scan",
      archive,
      output,
      scanner,
      join(root, ".github/halo-ci/gitleaks.toml"),
      join(root, ".github/halo-ci/scan-exceptions.json"),
    ]);
    if ((await fileDigest(archive)) !== archiveSha)
      throw new Error("Frozen OCI bytes changed during acceptance");
    if (JSON.stringify(validationSources()) !== JSON.stringify(sourceFiles))
      throw new Error("Validation source changed during acceptance");
  } catch (error) {
    failure = error;
  } finally {
    for (const name of containers) {
      try {
        const state = JSON.parse(
          capture("docker", [
            "container",
            "inspect",
            "--format",
            "{{json .State}}",
            name,
          ]),
        );
        if (state.Running) {
          capture("docker", ["stop", "--time", "60", name]);
          capture("docker", ["wait", name]);
        }
        const final = JSON.parse(
          capture("docker", [
            "container",
            "inspect",
            "--format",
            "{{json .State}}",
            name,
          ]),
        );
        if (final.Running) throw new Error("container still running");
        capture("docker", ["rm", name]);
        const absent = spawnSync("docker", ["container", "inspect", name], {
          encoding: "utf8",
        });
        if (
          absent.status === 0 ||
          !/no such (container|object)/i.test(absent.stderr)
        )
          throw new Error("container absence unknown");
      } catch (error) {
        cleanupErrors.push(`${name}: ${String(error)}`);
      }
    }
    if (failure) {
      for (const name of [
        "built.tar",
        "restored.tar",
        "roundtrip.tar.gz",
        "image.oci.tar",
      ])
        if (existsSync(join(work, name))) {
          try {
            capture("mv", [
              "--",
              join(work, name),
              join(output, `failed-${name}`),
            ]);
            if (
              existsSync(join(work, name)) ||
              !existsSync(join(output, `failed-${name}`))
            )
              throw new Error("Failed archive transfer outcome unknown");
          } catch (error) {
            cleanupErrors.push(`Failed archive preservation: ${String(error)}`);
          }
        }
    }
    for (const taskTag of [tag, restoredTag])
      try {
        // Untag only this task's unique tag; never remove a shared image ID/cache.
        const present = spawnSync("docker", ["image", "inspect", taskTag], {
          encoding: "utf8",
        });
        if (present.status === 0) capture("docker", ["image", "rm", taskTag]);
        else if (!/no such (image|object)/i.test(present.stderr))
          throw new Error("tag state unknown");
      } catch (error) {
        cleanupErrors.push(String(error));
      }
    if (!cleanupErrors.length) {
      try {
        rmSync(scratch, { recursive: true });
      } catch (error) {
        cleanupErrors.push(String(error));
      }
    }
    if (existsSync(scratch))
      cleanupErrors.push(
        `Scratch retained under current task owner: ${scratch}`,
      );
    process.off("SIGINT", cancel);
    process.off("SIGTERM", cancel);
    json(join(output, "cleanup.json"), {
      success: cleanupErrors.length === 0,
      containers: completed,
      errors: cleanupErrors,
      scratchRemoved: !existsSync(scratch),
      cancelled,
    });
  }
  if (
    failure ||
    cleanupErrors.length ||
    cancelled ||
    !oci ||
    !runtimeReference ||
    !builtIdentity ||
    !builtDaemon ||
    !restoredArchive ||
    !runtimeBinding ||
    !archiveSha
  )
    throw new Error(
      `Artifact failed: ${failure instanceof Error ? failure.message : String(failure ?? "cleanup/cancellation")}; ${cleanupErrors.join("; ")}`,
    );
  const runtime = JSON.parse(
    readFileSync(join(output, "runtime.json"), "utf8"),
  );
  const scan = JSON.parse(readFileSync(join(output, "scan.json"), "utf8"));
  json(join(output, "validation.json"), {
    success: true,
    runtime,
    cleanup: { success: true },
    scanFindings: scan.findings.length,
  });
  const materials: Record<string, string> = {};
  for (const file of readdirSync(output).sort())
    if (file !== "image.oci.tar")
      materials[file] = await fileDigest(join(output, file));
  json(join(output, "provenance.json"), {
    protocol: 2,
    repository: { id: 1432619931, name: "headironc/nucleus" },
    source,
    workflow: {
      path: ".github/workflows/halo-image.yml",
      revision: process.env.GITHUB_WORKFLOW_SHA ?? null,
      ref: process.env.GITHUB_WORKFLOW_REF ?? null,
      run: process.env.GITHUB_RUN_ID ?? null,
      attempt: process.env.GITHUB_RUN_ATTEMPT ?? null,
      job: process.env.GITHUB_JOB ?? null,
    },
    inputs: imageInputs(),
    baseImage: read(".github/halo-ci/Dockerfile").match(/^FROM (\S+)/m)?.[1],
    versions,
    tools: pins,
    build: { ...builtIdentity, ...builtDaemon },
    restored: { configDigest: restoredArchive.configDigest },
    runtime: runtimeBinding,
    oci,
    archive: {
      format: "oci-image-layout-1.0.0",
      file: "image.oci.tar",
      sha256: archiveSha,
      size: statSync(join(output, "image.oci.tar")).size,
    },
    materials,
  });
  await verifyArtifact(output, {
    source: source.source,
    run: process.env.GITHUB_RUN_ID ?? null,
    attempt: process.env.GITHUB_RUN_ATTEMPT ?? null,
  });
  outputs({
    tar: archiveSha,
    manifest: oci.manifest.digest,
    config: oci.config.digest,
    source: source.source,
  });
}

if (import.meta.main) {
  const [command, directory, ...extra] = process.argv.slice(2);
  if (
    !directory ||
    extra.length ||
    !["plan", "build", "verify"].includes(command ?? "")
  )
    throw new Error(
      "Usage: bun tools/halo-image-artifact.ts plan SOURCE_JSON | build OUTPUT | verify ARTIFACT",
    );
  if (command === "verify") {
    const binding = process.env.GITHUB_EVENT_NAME
      ? bindSource(
          process.env.GITHUB_EVENT_NAME,
          JSON.parse(readFileSync(process.env.GITHUB_EVENT_PATH!, "utf8")),
          process.env,
        )
      : undefined;
    const result = await verifyArtifact(resolve(directory), {
      source: process.env.EXPECT_SOURCE!,
      run: process.env.GITHUB_RUN_ID ?? null,
      attempt: process.env.GITHUB_RUN_ATTEMPT ?? null,
      tar: process.env.EXPECT_TAR,
      manifest: process.env.EXPECT_MANIFEST,
      config: process.env.EXPECT_CONFIG,
      binding,
      workflowRevision: process.env.GITHUB_WORKFLOW_SHA,
    });
    console.log(JSON.stringify(result));
  } else {
    const event = process.env.GITHUB_EVENT_NAME ?? "local";
    const payload = process.env.GITHUB_EVENT_PATH
      ? JSON.parse(readFileSync(process.env.GITHUB_EVENT_PATH, "utf8"))
      : {};
    const source = bindSource(event, payload, process.env);
    if (command === "plan") {
      json(resolve(directory), source);
      const decision = plan(source);
      outputs({
        build: String(decision.build),
        source: source.source,
        reason: decision.reason,
      });
      console.log(JSON.stringify({ source, ...decision }));
    } else await buildArtifact(resolve(directory), source);
  }
}
