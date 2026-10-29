import { afterEach, expect, test } from "bun:test";
import { spawn, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  chmodSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import {
  bindSource,
  buildIdentity,
  assertRestoredArchive,
  inspectDockerArchive,
  fileDigest,
  shouldBuild,
  validationInput,
  verifyArtifact,
} from "./halo-image-artifact";

const root = resolve(import.meta.dir, "..");
const directories: string[] = [];
afterEach(() => {
  for (const directory of directories.splice(0))
    rmSync(directory, { recursive: true, force: true });
});
function temporary() {
  const path = mkdtempSync(join(tmpdir(), "nucleus-oci-test-"));
  directories.push(path);
  return path;
}
const read = (path: string) => readFileSync(join(root, path), "utf8");

test("semantic selection skips unrelated dependency changes and covers every environment input", () => {
  for (const path of ["bun.lock", "halo/bun.lock", "Cargo.lock", "README.md"])
    expect(shouldBuild(read, read, [path]).build).toBe(false);
  const unrelated = (path: string) =>
    path === "halo/package.json"
      ? JSON.stringify({
          ...JSON.parse(read(path)),
          dependencies: { react: "different" },
          devDependencies: {
            ...JSON.parse(read(path)).devDependencies,
            vite: "different",
          },
        })
      : read(path);
  expect(shouldBuild(unrelated, read, ["halo/package.json"]).build).toBe(false);
  for (const path of [
    ".github/halo-ci/Dockerfile",
    ".github/halo-ci/Dockerfile.dockerignore",
    ".tool-versions",
    "rust-toolchain.toml",
    "fixture/postgresql.ts",
    "fixture/postgresql.version",
    "tools/halo-ci-image.ts",
    "tools/check-bun.ts",
  ]) {
    expect(
      shouldBuild((name) => read(name) + (name === path ? "\n" : ""), read, [
        path,
      ]).build,
      path,
    ).toBe(true);
  }
  for (const change of [
    (m: any) => {
      m.devDependencies["@playwright/test"] = "1.64.0";
    },
    (m: any) => {
      m.engines.node = ">=26.0.0";
    },
    (m: any) => {
      m.packageManager = "bun@0.0.1";
    },
  ]) {
    expect(
      shouldBuild(
        (path) => {
          if (path !== "halo/package.json") return read(path);
          const m = JSON.parse(read(path));
          change(m);
          return JSON.stringify(m);
        },
        read,
        ["halo/package.json"],
      ).build,
    ).toBe(true);
  }
  expect(shouldBuild(undefined, read, []).build).toBe(true);
  expect(
    shouldBuild(
      () => {
        throw new Error("Missing baseline");
      },
      read,
      [],
    ).build,
  ).toBe(true);
  expect(() => shouldBuild(read, () => "invalid", [])).toThrow();
});

test("new executable consumers, scan configuration and tool pins force acceptance", () => {
  for (const path of [
    "tools/new-consumer.test.ts",
    "tools/halo-image-oci.py",
    "halo/tools/browser-smoke.ts",
    ".github/halo-ci/gitleaks.toml",
    ".github/halo-ci/artifact-tools.json",
    ".github/workflows/halo-image.yml",
    "package.json",
  ]) {
    expect(validationInput(path)).toBe(true);
    expect(shouldBuild(read, read, [path])).toEqual({
      build: true,
      reason: "validation-inputs-changed",
    });
  }
});

test("Buildx backend and client pins affect environment identity; scan pins force validation", () => {
  for (const field of ["backend", "buildx"] as const) {
    const before = (path: string) => {
      if (path !== ".github/halo-ci/artifact-tools.json") return read(path);
      const pins = JSON.parse(read(path));
      if (field === "backend") pins.backend = "previous-backend";
      else pins.buildx = { ...pins.buildx, version: "0.38.0" };
      return JSON.stringify(pins);
    };
    expect(
      shouldBuild(before, read, [".github/halo-ci/artifact-tools.json"]).reason,
    ).toBe("environment-inputs-changed");
  }
  const scannerOnly = (path: string) =>
    path === ".github/halo-ci/artifact-tools.json"
      ? JSON.stringify({
          ...JSON.parse(read(path)),
          skopeo: "previous-scanner-pin",
        })
      : read(path);
  expect(
    shouldBuild(scannerOnly, read, [".github/halo-ci/artifact-tools.json"])
      .reason,
  ).toBe("validation-inputs-changed");
});

test("builder cancellation reaches the original Docker child and awaits its terminal cleanup", async () => {
  const directory = temporary();
  const ready = join(directory, "ready"),
    signalled = join(directory, "signalled"),
    release = join(directory, "release");
  writeFileSync(
    join(directory, "docker"),
    `#!/usr/bin/env python3
import pathlib,signal,sys,time,os
directory=pathlib.Path(os.environ['BUILD_LIFECYCLE'])
def stop(*args):(directory/'signalled').write_text('SIGTERM')
signal.signal(signal.SIGTERM,stop)
(directory/'ready').write_text(sys.argv[-1])
while not (directory/'release').exists():time.sleep(.01)
`,
    { mode: 0o755 },
  );
  const child = spawn(
    process.execPath,
    [
      join(root, "tools/halo-ci-image.ts"),
      "build",
      "nucleus-test:cancellation",
    ],
    {
      cwd: root,
      env: {
        ...process.env,
        PATH: `${directory}:${process.env.PATH}`,
        BUILD_LIFECYCLE: directory,
      },
      stdio: "ignore",
    },
  );
  const exit = new Promise<number | null>((done, reject) => {
    child.once("exit", done);
    child.once("error", reject);
  });
  let ended = false;
  void exit.then(() => {
    ended = true;
  });
  async function until(path: string) {
    const deadline = Date.now() + 5000;
    while (!existsSync(path) && !ended && Date.now() < deadline)
      await delay(10);
    expect(existsSync(path)).toBe(true);
  }
  try {
    await until(ready);
    const context = readFileSync(ready, "utf8");
    child.kill("SIGTERM");
    await until(signalled);
    expect(ended).toBe(false);
    expect(existsSync(context)).toBe(true);
    writeFileSync(release, "finish");
    expect(await exit).not.toBe(0);
    expect(existsSync(context)).toBe(false);
  } finally {
    writeFileSync(release, "finish");
    await exit;
  }
}, 10000);

const base = "a".repeat(40),
  head = "b".repeat(40),
  merge = "c".repeat(40);
const environment = {
  GITHUB_REPOSITORY: "headironc/nucleus",
  GITHUB_REPOSITORY_ID: "1432619931",
  GITHUB_RUN_ID: "123",
  GITHUB_RUN_ATTEMPT: "2",
  GITHUB_SHA: merge,
  GITHUB_REF: "refs/heads/dev",
  GITHUB_WORKFLOW_REF:
    "headironc/nucleus/.github/workflows/halo-image.yml@refs/heads/dev",
};
function executor(...args: string[]) {
  if (args[0] === "rev-parse" && args[1] === "HEAD") return merge;
  if (args[0] === "rev-parse") return args[2]!.replace(/\^\{commit\}$/, "");
  if (args[0] === "show") return `${base} ${head}`;
  if (args[0] === "merge-base") {
    if (args[2] !== merge) throw new Error("not ancestor");
    return "";
  }
  throw new Error("Unexpected Git operation");
}

test("PR binds exact head/base/actual two-parent merge, push binds before/after", () => {
  const pr = {
    pull_request: {
      base: { sha: base, ref: "dev", repo: { id: 1432619931 } },
      head: { sha: head },
    },
  };
  expect(bindSource("pull_request", pr, environment, executor)).toEqual({
    event: "pull_request",
    source: merge,
    before: base,
    base,
    head,
    merge,
  });
  expect(() =>
    bindSource(
      "pull_request",
      pr,
      { ...environment, GITHUB_SHA: head },
      executor,
    ),
  ).toThrow();
  expect(() =>
    bindSource("pull_request", pr, environment, (...args) =>
      args[0] === "show" ? `${head} ${base}` : executor(...args),
    ),
  ).toThrow();
  expect(
    bindSource(
      "push",
      { before: base, after: merge, ref: "refs/heads/dev" },
      environment,
      executor,
    ),
  ).toEqual({ event: "push", source: merge, before: base });
  expect(() =>
    bindSource(
      "push",
      { before: base, after: head, ref: "refs/heads/dev" },
      environment,
      executor,
    ),
  ).toThrow();
  expect(() =>
    bindSource(
      "push",
      { after: merge, ref: "refs/heads/main" },
      environment,
      executor,
    ),
  ).toThrow();
  expect(() =>
    bindSource(
      "pull_request",
      pr,
      { ...environment, GITHUB_REPOSITORY_ID: "1" },
      executor,
    ),
  ).toThrow();
});

test("manual source requires full dev-ancestor SHA, trusted dev definition and reason", () => {
  expect(
    bindSource(
      "workflow_dispatch",
      { inputs: { revision: merge, reason: "Security rebuild" } },
      environment,
      executor,
    ).source,
  ).toBe(merge);
  for (const revision of [
    "HEAD",
    merge.slice(0, 12),
    "0".repeat(40),
    "--help",
    base,
  ])
    expect(() =>
      bindSource(
        "workflow_dispatch",
        { inputs: { revision, reason: "Rebuild" } },
        environment,
        executor,
      ),
    ).toThrow();
  expect(() =>
    bindSource(
      "workflow_dispatch",
      { inputs: { revision: merge, reason: "" } },
      environment,
      executor,
    ),
  ).toThrow();
  expect(() =>
    bindSource(
      "workflow_dispatch",
      { inputs: { revision: merge, reason: "Rebuild" } },
      { ...environment, GITHUB_REF: "refs/heads/main" },
      executor,
    ),
  ).toThrow();
  expect(() => bindSource("schedule", {}, environment, executor)).toThrow();
});

// Python's mature tar writer creates independently known payloads. Includes a
// deleted secret fixture, overwritten file, traversal and links in old layers.
const fixtureScript = `
import io,json,tarfile,hashlib,sys,pathlib
out=pathlib.Path(sys.argv[1]); mode=sys.argv[2]
def sha(b): return 'sha256:'+hashlib.sha256(b).hexdigest()
def tar(entries):
 b=io.BytesIO()
 with tarfile.open(fileobj=b,mode='w') as t:
  for name,data,kind in entries:
   m=tarfile.TarInfo(name);m.mode=420
   if kind=='link':m.type=tarfile.SYMTYPE;m.linkname=data.decode()
   else:m.size=len(data)
   t.addfile(m,io.BytesIO(data) if kind!='link' else None)
 return b.getvalue()
token=bytes([10]).join([b'-----BEGIN RSA PRIVATE KEY-----',b'NucleusInvalidKeyMaterial'*6,b'-----END RSA PRIVATE KEY-----',b''])
layers=[tar([('old/credential',token,'file'),('../../outside',b'benign','file'),('link',b'/etc/shadow','link'),('overwritten',b'old material','file')]),tar([('old/.wh.credential',b'','file'),('overwritten',b'new material','file')])]
config={'os':'linux','architecture':'arm64' if mode=='platform' else 'amd64','config':{'User':'root' if mode=='root' else 'pwuser'},'rootfs':{'type':'layers','diff_ids':[sha(b) for b in layers]},'history':[{'created_by':'fixture'}]}
cb=json.dumps(config).encode();blobs={sha(cb):cb}
def desc(b,media):return {'mediaType':media,'digest':sha(b),'size':len(b)}
for b in layers:blobs[sha(b)]=b
manifest={'schemaVersion':2,'mediaType':'application/vnd.oci.image.manifest.v1+json','config':desc(cb,'application/vnd.oci.image.config.v1+json'),'layers':[desc(b,'application/vnd.oci.image.layer.v1.tar') for b in layers]}
if mode.startswith('layout-docker'):
 manifest['mediaType']='application/vnd.docker.distribution.manifest.v2+json'
 manifest['config']['mediaType']='application/vnd.docker.container.image.v1+json'
 for layer in manifest['layers']:layer['mediaType']='application/vnd.docker.image.rootfs.diff.tar'
if mode=='layout-mixed-config':manifest['config']['mediaType']='application/vnd.docker.container.image.v1+json'
if mode=='layout-mixed-layer':manifest['layers'][0]['mediaType']='application/vnd.docker.image.rootfs.diff.tar'
if mode=='layout-foreign':manifest['layers'][0]['mediaType']='application/vnd.docker.image.rootfs.foreign.diff.tar.gzip'
if mode=='layout-external':manifest['layers'][0]['urls']=['https://example.invalid/layer']
if mode=='layout-unknown':manifest['mediaType']='application/unknown'
mb=json.dumps(manifest).encode();blobs[sha(mb)]=mb
md=desc(mb,manifest['mediaType'])
if mode=='layout-mismatched-manifest':md['mediaType']='application/vnd.docker.distribution.manifest.v2+json'
index={'schemaVersion':2,'manifests':[md,md] if mode=='multi' else [md]}
entries=[('oci-layout',b'{"imageLayoutVersion":"1.0.0"}','file'),('index.json',json.dumps(index).encode(),'file')]+[('blobs/sha256/'+k[7:],v,'file') for k,v in blobs.items()]
if mode.startswith('docker'):
 dm=[{'Config':'blobs/sha256/'+sha(cb)[7:],'RepoTags':['nucleus-fixture:test'],'Layers':['missing' if mode=='docker-missing' else 'blobs/sha256/'+sha(b)[7:] for b in layers]}]
 if mode=='docker-multi':dm.append(dm[0])
 entries.append(('manifest.json',json.dumps(dm).encode(),'file'))
 (out/'build-metadata.json').write_text(json.dumps({'containerimage.digest':md['digest'],'containerimage.descriptor':md}))
 (out/'original-manifest.json').write_bytes(mb)
 if mode=='docker-manifest-missing':entries=[(n,b,k) for n,b,k in entries if n!='blobs/sha256/'+sha(mb)[7:]]
 if mode=='docker-index':entries=[(n,json.dumps({'schemaVersion':2,'manifests':[md,md]}).encode() if n=='index.json' else b,k) for n,b,k in entries]
 if mode=='docker-duplicate':entries.append(entries[-1])
 if mode=='docker-diff':
  name='blobs/sha256/'+sha(layers[0])[7:]
  entries=[(n,b+b'corrupt' if n==name else b,k) for n,b,k in entries]
if mode=='corrupt':entries[-1]=(entries[-1][0],entries[-1][1]+b'x','file')
(out/'image.oci.tar').write_bytes(tar(entries))
(out/'expected-config.json').write_bytes(cb)
`;
function fixture(mode = "valid") {
  const directory = temporary();
  const result = spawnSync("python3", ["-c", fixtureScript, directory, mode], {
    encoding: "utf8",
  });
  expect(result.status, result.stderr).toBe(0);
  return directory;
}
function inspect(directory: string) {
  return spawnSync(
    "python3",
    [
      join(root, "tools/halo-image-oci.py"),
      "inspect",
      join(directory, "image.oci.tar"),
    ],
    { encoding: "utf8" },
  );
}

test("real tar parser refuses corrupt bytes, multiple manifests, root and wrong platform", () => {
  const valid = inspect(fixture());
  expect(valid.status, valid.stderr).toBe(0);
  expect(JSON.parse(valid.stdout).layers).toHaveLength(2);
  for (const mode of ["corrupt", "multi", "root", "platform"])
    expect(inspect(fixture(mode)).status, mode).not.toBe(0);
});

test("OCI layout validates both complete media type families and rejects mixed or external layers", () => {
  expect(inspect(fixture("layout-docker")).status).toBe(0);
  for (const mode of [
    "layout-mixed-config",
    "layout-mixed-layer",
    "layout-foreign",
    "layout-external",
    "layout-unknown",
    "layout-mismatched-manifest",
  ])
    expect(inspect(fixture(mode)).status, mode).not.toBe(0);
});

test("Docker archive parsing rejects missing bytes and checks uncompressed layer identities", () => {
  const directory = fixture("docker");
  const actual = inspectDockerArchive(join(directory, "image.oci.tar"));
  expect(actual.platform).toEqual({ os: "linux", architecture: "amd64" });
  expect(actual.config.config.User).toBe("pwuser");
  expect(actual.layers).toHaveLength(2);
  expect(actual.layers.map((layer) => layer.diffId)).toEqual(
    actual.config.rootfs.diff_ids,
  );
  for (const mode of ["docker-missing", "docker-diff"])
    expect(() =>
      inspectDockerArchive(join(fixture(mode), "image.oci.tar")),
    ).toThrow();
});

test("Buildx metadata and restored archives preserve separate config and runtime identities", () => {
  const original = inspectDockerArchive(
    join(fixture("docker"), "image.oci.tar"),
  );
  const descriptor = {
    mediaType: "application/vnd.oci.image.manifest.v1+json",
    digest: `sha256:${"e".repeat(64)}`,
    size: 500,
  };
  const metadata = {
    "containerimage.config.digest": original.configDigest,
    "containerimage.digest": descriptor.digest,
    "containerimage.descriptor": descriptor,
  };
  expect(buildIdentity(metadata).configDigest).toBe(original.configDigest);
  for (const change of [
    { "containerimage.config.digest": null },
    { "containerimage.config.digest": "" },
    { "containerimage.config.digest": descriptor.digest + "bad" },
    {
      "containerimage.descriptor": {
        ...descriptor,
        mediaType: "application/unknown",
      },
    },
    { "containerimage.digest": original.configDigest },
  ])
    expect(() => buildIdentity({ ...metadata, ...change })).toThrow();
  const oci = {
    manifest: descriptor,
    config: { digest: original.configDigest, size: 100 },
    layers: [],
    platform: original.platform,
  };
  assertRestoredArchive(original, original, oci);
  expect(() =>
    assertRestoredArchive(
      original,
      { ...original, configDigest: descriptor.digest },
      oci,
    ),
  ).toThrow();
  expect(() =>
    assertRestoredArchive(
      original,
      { ...original, platform: { os: "linux", architecture: "arm64" } },
      oci,
    ),
  ).toThrow();
  expect(() =>
    assertRestoredArchive(
      original,
      { ...original, config: { ...original.config, config: { User: "root" } } },
      oci,
    ),
  ).toThrow();
});

test("missing BuildKit config binds original manifest bytes and rejects conflicting or incomplete proof", () => {
  const directory = fixture("docker");
  const metadataPath = join(directory, "build-metadata.json");
  const manifestPath = join(directory, "build-manifest.json");
  const archive = inspectDockerArchive(
    join(directory, "image.oci.tar"),
    metadataPath,
    manifestPath,
  );
  const metadata = JSON.parse(readFileSync(metadataPath, "utf8"));
  const raw = readFileSync(manifestPath);
  expect(raw).toEqual(readFileSync(join(directory, "original-manifest.json")));
  expect(buildIdentity(metadata, archive, raw)).toMatchObject({
    configDigest: archive.configDigest,
    binding: "manifest-config",
  });
  expect(() => buildIdentity(metadata, archive)).toThrow();
  for (const direct of [null, "", 42, `sha256:${"f".repeat(64)}`])
    expect(() =>
      buildIdentity(
        { ...metadata, "containerimage.config.digest": direct },
        archive,
        raw,
      ),
    ).toThrow();
  expect(
    buildIdentity(
      { ...metadata, "containerimage.config.digest": archive.configDigest },
      archive,
      raw,
    ).binding,
  ).toBe("config-digest");
  for (const bytes of [
    Buffer.concat([raw, Buffer.from(" ")]),
    Buffer.from("{}"),
  ])
    expect(() => buildIdentity(metadata, archive, bytes)).toThrow();
  for (const change of [
    { configSize: archive.configSize + 1 },
    { configDigest: `sha256:${"f".repeat(64)}` },
    { platform: { os: "linux", architecture: "arm64" } },
    { layers: archive.layers.slice(1) },
    {
      layers: archive.layers.map((layer) => ({
        ...layer,
        size: layer.size + 1,
      })),
    },
  ])
    expect(() =>
      buildIdentity(metadata, { ...archive, ...change }, raw),
    ).toThrow();
  for (const mode of [
    "docker-manifest-missing",
    "docker-index",
    "docker-multi",
    "docker-duplicate",
    "docker-missing",
    "docker-diff",
  ]) {
    const invalid = fixture(mode);
    expect(
      () =>
        inspectDockerArchive(
          join(invalid, "image.oci.tar"),
          join(invalid, "build-metadata.json"),
          join(invalid, "build-manifest.json"),
        ),
      mode,
    ).toThrow();
  }
  const altered = {
    ...metadata,
    "containerimage.descriptor": {
      ...metadata["containerimage.descriptor"],
      size: raw.length + 1,
    },
  };
  writeFileSync(metadataPath, JSON.stringify(altered));
  expect(() =>
    inspectDockerArchive(
      join(directory, "image.oci.tar"),
      metadataPath,
      manifestPath,
    ),
  ).toThrow();
  expect(() => buildIdentity(altered, archive, raw)).toThrow();
});

test.skipIf(!process.env.NUCLEUS_IMAGE_BUILDX_BINARY)(
  "private default BuildKit builds, loads and completely re-exports an offline scratch image",
  async () => {
    const directory = temporary();
    const evidence = process.env.NUCLEUS_IMAGE_BUILDX_EVIDENCE;
    if (!evidence || existsSync(evidence))
      throw new Error("New retained Buildx probe evidence directory required");
    mkdirSync(evidence, { recursive: true, mode: 0o700 });
    const binary = join(directory, "buildx");
    copyFileSync(process.env.NUCLEUS_IMAGE_BUILDX_BINARY!, binary);
    const pin = JSON.parse(read(".github/halo-ci/artifact-tools.json")).buildx;
    expect(await fileDigest(binary)).toBe(`sha256:${pin.sha256}`);
    chmodSync(binary, 0o700);
    const config = join(directory, "config");
    const context = join(directory, "context");
    mkdirSync(config, { mode: 0o700 });
    mkdirSync(context);
    writeFileSync(
      join(context, "Dockerfile"),
      "FROM scratch\nCOPY payload /payload\nUSER 1000:1000\n",
    );
    writeFileSync(
      join(context, "payload"),
      "Nucleus offline BuildKit export fixture\n",
    );
    const tag = `nucleus-artifact-buildx-probe:${process.pid}-${createHash("sha256").update(directory).digest("hex").slice(0, 12)}`;
    const restoredTag = `${tag}-restored`;
    function command(executable: string, args: string[]) {
      const result = spawnSync(executable, args, {
        env: { ...process.env, BUILDX_CONFIG: config },
        encoding: "utf8",
        timeout: 120_000,
      });
      writeFileSync(
        join(evidence!, "commands.log"),
        JSON.stringify({
          executable,
          args,
          status: result.status,
          stdout: result.stdout,
          stderr: result.stderr,
        }) + "\n",
        { flag: "a" },
      );
      if (result.error || result.status !== 0)
        throw new Error(
          `Probe command failed: ${result.error ?? result.stderr}`,
        );
      return result.stdout.trim();
    }
    try {
      expect(command(binary, ["version"])).toContain(`v${pin.version} `);
      expect(command(binary, ["inspect", "default"])).toMatch(
        /^Driver:\s+docker\s*$/m,
      );
      command(binary, [
        "build",
        "--builder",
        "default",
        "--network=none",
        "--platform",
        "linux/amd64",
        "--load",
        "--provenance=false",
        "--sbom=false",
        "--metadata-file",
        join(evidence, "metadata.json"),
        "--tag",
        tag,
        context,
      ]);
      const metadata = JSON.parse(
        readFileSync(join(evidence, "metadata.json"), "utf8"),
      );
      const reference = command("docker", [
        "image",
        "inspect",
        "--format",
        "{{.Id}}",
        tag,
      ]);
      command("docker", [
        "save",
        "--output",
        join(evidence, "first.tar"),
        reference,
      ]);
      const first = inspectDockerArchive(join(evidence, "first.tar"));
      if (metadata["containerimage.config.digest"])
        expect(first.configDigest).toBe(
          metadata["containerimage.config.digest"],
        );
      expect(first.layers).toHaveLength(1);
      command("docker", ["image", "rm", tag]);
      const loaded = command("docker", [
        "load",
        "--input",
        join(evidence, "first.tar"),
      ]);
      const loadedReference = loaded.match(
        /^Loaded image ID: (sha256:[0-9a-f]{64})$/m,
      )?.[1];
      if (!loadedReference)
        throw new Error("Probe load did not identify one immutable image");
      command("docker", ["image", "tag", loadedReference, restoredTag]);
      const restoredReference = command("docker", [
        "image",
        "inspect",
        "--format",
        "{{.Id}}",
        restoredTag,
      ]);
      command("docker", [
        "save",
        "--output",
        join(evidence, "second.tar"),
        restoredReference,
      ]);
      const second = inspectDockerArchive(join(evidence, "second.tar"));
      expect(second.configDigest).toBe(first.configDigest);
      expect(second.config).toEqual(first.config);
      expect(second.layers.map((layer) => layer.diffId)).toEqual(
        first.config.rootfs.diff_ids,
      );
      writeFileSync(
        join(evidence, "identity.json"),
        JSON.stringify(
          { metadata, reference, restoredReference, first, second },
          null,
          2,
        ),
      );
      // Preserve complete load/save evidence before enforcing the metadata contract.
      const verified = inspectDockerArchive(
        join(evidence, "first.tar"),
        join(evidence, "metadata.json"),
        join(evidence, "build-manifest.json"),
      );
      expect(
        buildIdentity(
          metadata,
          verified,
          readFileSync(join(evidence, "build-manifest.json")),
        ).configDigest,
      ).toBe(first.configDigest);
    } finally {
      for (const taskTag of [tag, restoredTag]) {
        const absent = spawnSync("docker", ["image", "inspect", taskTag], {
          encoding: "utf8",
        });
        if (absent.status === 0) command("docker", ["image", "rm", taskTag]);
        else expect(absent.stderr).toMatch(/no such (image|object)/i);
        const final = spawnSync("docker", ["image", "inspect", taskTag], {
          encoding: "utf8",
        });
        expect(final.status).not.toBe(0);
        expect(final.stderr).toMatch(/no such (image|object)/i);
      }
      writeFileSync(
        join(evidence, "cleanup.json"),
        JSON.stringify({
          tagAbsent: true,
          privateConfigOwner: "foreground test afterEach",
        }),
      );
    }
  },
  180000,
);

test("accepted findings require the exact source, detector, rule and position", () => {
  const directory = fixture();
  const scanner = join(directory, "scanner");
  writeFileSync(
    scanner,
    `#!/usr/bin/env python3
import pathlib,sys,json
directory=pathlib.Path(sys.argv[2]); hits=[]
for file in directory.iterdir():
 if file.name!='metadata.jsonl' and b'NucleusInvalidKeyMaterial' in file.read_bytes():
  hits.append({'File':str(file),'RuleID':'private-key','StartLine':1})
pathlib.Path(sys.argv[sys.argv.index('--report-path')+1]).write_text(json.dumps(hits))
sys.exit(1 if hits else 0)
`,
    { mode: 0o755 },
  );
  const hash = (value: Uint8Array | string) =>
    createHash("sha256").update(value).digest("hex");
  const config = join(root, ".github/halo-ci/gitleaks.toml");
  const accepted = {
    scannerSha256: hash(readFileSync(scanner)),
    configurationSha256: hash(readFileSync(config)),
    findings: [
      {
        path: "old/credential",
        sourceSha256: hash(
          "-----BEGIN RSA PRIVATE KEY-----\n" +
            "NucleusInvalidKeyMaterial".repeat(6) +
            "\n-----END RSA PRIVATE KEY-----\n",
        ),
        rule: "private-key",
        line: 1,
        valueSha256: "a".repeat(64),
        reason: "Invalid test fixture only",
        count: 1,
      },
    ],
  };
  const policy = join(directory, "accepted.json");
  const scan = (value: unknown) => {
    writeFileSync(policy, JSON.stringify(value));
    return spawnSync(
      "python3",
      [
        join(root, "tools/halo-image-oci.py"),
        "scan",
        join(directory, "image.oci.tar"),
        directory,
        scanner,
        config,
        policy,
      ],
      { encoding: "utf8" },
    );
  };
  const result = scan(accepted);
  expect(result.status, result.stderr).toBe(0);
  expect(
    JSON.parse(readFileSync(join(directory, "scan.json"), "utf8")).accepted,
  ).toHaveLength(1);
  for (const change of [
    { path: "other" },
    { sourceSha256: "b".repeat(64) },
    { rule: "other" },
    { line: 2 },
    { count: 2 },
  ])
    expect(
      scan({ ...accepted, findings: [{ ...accepted.findings[0], ...change }] })
        .status,
    ).not.toBe(0);
  expect(scan({ ...accepted, scannerSha256: "b".repeat(64) }).status).not.toBe(
    0,
  );
  expect(
    scan({ ...accepted, configurationSha256: "b".repeat(64) }).status,
  ).not.toBe(0);
});

test("historical layers read old bytes without following links or traversal paths", () => {
  const directory = fixture();
  const scanner = join(directory, "scanner");
  writeFileSync(
    scanner,
    `#!/usr/bin/env python3
import sys,pathlib,json
p=pathlib.Path(sys.argv[sys.argv.index('--report-path')+1]);p.write_text('[]')
assert all(x.is_file() and not x.is_symlink() for x in pathlib.Path(sys.argv[2]).iterdir())
`,
    { mode: 0o755 },
  );
  const result = spawnSync(
    "python3",
    [
      join(root, "tools/halo-image-oci.py"),
      "scan",
      join(directory, "image.oci.tar"),
      directory,
      scanner,
      join(root, ".github/halo-ci/gitleaks.toml"),
    ],
    { encoding: "utf8" },
  );
  expect(result.status, result.stderr).toBe(0);
  const entries = readFileSync(join(directory, "layers.jsonl"), "utf8")
    .trim()
    .split("\n")
    .map((line) => JSON.parse(line));
  const old = entries.find((entry) => entry.path === "old/credential");
  expect(old.sha256).toBe(
    `sha256:${createHash("sha256")
      .update(
        "-----BEGIN RSA PRIVATE KEY-----\n" +
          "NucleusInvalidKeyMaterial".repeat(6) +
          "\n-----END RSA PRIVATE KEY-----\n",
      )
      .digest("hex")}`,
  );
  expect(old.scan).toBe("gitleaks-dir-all-bytes");
  expect(entries.find((entry) => entry.path === "../../outside").safePath).toBe(
    false,
  );
  expect(entries.find((entry) => entry.path === "link").scan).toBe(
    "metadata-only",
  );
  expect(
    entries
      .filter((entry) => entry.path === "overwritten")
      .map((entry) => entry.sha256),
  ).toEqual(
    ["old material", "new material"].map(
      (value) => `sha256:${createHash("sha256").update(value).digest("hex")}`,
    ),
  );
  expect(existsSync(join(dirname(directory), "outside"))).toBe(false);
});

function dirname(path: string) {
  return resolve(path, "..");
}

test.skipIf(!process.env.NUCLEUS_IMAGE_GITLEAKS_BINARY)(
  "real pinned scanner detects invalid private-key material in a deleted historical file",
  () => {
    const directory = fixture();
    const result = spawnSync(
      "python3",
      [
        join(root, "tools/halo-image-oci.py"),
        "scan",
        join(directory, "image.oci.tar"),
        directory,
        process.env.NUCLEUS_IMAGE_GITLEAKS_BINARY!,
        join(root, ".github/halo-ci/gitleaks.toml"),
      ],
      { encoding: "utf8" },
    );
    expect(result.status).not.toBe(0);
    const report = readFileSync(join(directory, "scan.json"), "utf8");
    const findings = JSON.parse(report).findings;
    expect(
      findings.some(
        (item: any) =>
          item.layer === 0 &&
          item.path === "old/credential" &&
          item.rule === "private-key",
      ),
    ).toBe(true);
    expect(report).not.toContain("NucleusInvalidKeyMaterial");
    expect(findings.every((item: any) => item.secret === "REDACTED")).toBe(
      true,
    );
  },
);

test("download verification cross-checks external identities, archive bytes and required materials", async () => {
  const directory = fixture();
  const identity = JSON.parse(inspect(directory).stdout);
  const expectedConfig = JSON.parse(
    readFileSync(join(directory, "expected-config.json"), "utf8"),
  );
  rmSync(join(directory, "expected-config.json"));
  const runtime = {
    success: true,
    pgDirectoryRemoved: true,
    postmasterGone: true,
  };
  const validation = {
    success: true,
    runtime,
    cleanup: { success: true },
    scanFindings: 0,
  };
  const materials: Record<string, string> = {};
  const archived = {
    configDigest: identity.config.digest,
    configSize: identity.config.size,
    platform: identity.platform,
    config: expectedConfig,
    layers: identity.layers.map((layer: any) => ({
      path: layer.digest,
      digest: layer.digest,
      size: layer.size,
      diffId: layer.digest,
    })),
  };
  const rawManifest = spawnSync("python3", [
    "-c",
    "import sys,tarfile; t=tarfile.open(sys.argv[1]); sys.stdout.buffer.write(t.extractfile('blobs/sha256/'+sys.argv[2][7:]).read())",
    join(directory, "image.oci.tar"),
    identity.manifest.digest,
  ]).stdout;
  const metadata = {
    "containerimage.digest": identity.manifest.digest,
    "containerimage.descriptor": identity.manifest,
  };
  const runtimeBinding = {
    descriptor: null,
    reference: `sha256:${"d".repeat(64)}`,
    containerImage: `sha256:${"d".repeat(64)}`,
    requestedImage: `sha256:${"d".repeat(64)}`,
  };
  for (const file of [
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
    "build-manifest.json",
    "build-archive.json",
    "restored-archive.json",
    "runtime-reference.json",
    "default-user.json",
  ]) {
    writeFileSync(
      join(directory, file),
      file === "scan-policy.json"
        ? readFileSync(join(root, ".github/halo-ci/scan-exceptions.json"))
        : file === "build-manifest.json"
          ? rawManifest
          : JSON.stringify(
              file === "scan.json"
                ? {
                    policySha256: await fileDigest(
                      join(root, ".github/halo-ci/scan-exceptions.json"),
                    ),
                    findings: [],
                    accepted: [],
                    detected: 0,
                    unresolved: 0,
                  }
                : file === "default-user.json"
                  ? { uid: 1000, reference: runtimeBinding.reference }
                  : file === "validation.json"
                    ? validation
                    : file === "build-metadata.json"
                      ? metadata
                      : file === "build-archive.json" ||
                          file === "restored-archive.json"
                        ? archived
                        : file === "runtime-reference.json"
                          ? runtimeBinding
                          : "fixture material",
            ),
    );
    materials[file] = await fileDigest(join(directory, file));
  }
  const provenance = {
    protocol: 2,
    repository: { id: 1432619931, name: "headironc/nucleus" },
    source: { source: merge },
    workflow: {
      path: ".github/workflows/halo-image.yml",
      run: "123",
      attempt: "2",
    },
    archive: {
      format: "oci-image-layout-1.0.0",
      sha256: await fileDigest(join(directory, "image.oci.tar")),
    },
    oci: identity,
    build: {
      ...buildIdentity(metadata, archived, rawManifest),
      runtimeReference: `sha256:${"f".repeat(64)}`,
      daemonDescriptor: null,
    },
    restored: { configDigest: identity.config.digest },
    runtime: runtimeBinding,
    materials,
  };
  writeFileSync(join(directory, "provenance.json"), JSON.stringify(provenance));
  const expected = { source: merge, run: "123", attempt: "2" };
  expect(
    (await verifyArtifact(directory, expected)).identity.config.digest,
  ).toBe(identity.config.digest);
  for (const change of [
    { source: head },
    { run: "124" },
    { attempt: "1" },
    { tar: `sha256:${"d".repeat(64)}` },
    { manifest: identity.config.digest },
    { config: identity.manifest.digest },
  ])
    await expect(
      verifyArtifact(directory, { ...expected, ...change }),
    ).rejects.toThrow();
  const originalRecord = readFileSync(
    join(directory, "build-archive.json"),
    "utf8",
  );
  const originalRecordSha = materials["build-archive.json"]!;
  writeFileSync(
    join(directory, "build-archive.json"),
    JSON.stringify({
      ...archived,
      config: {
        ...archived.config,
        config: { ...archived.config.config, User: "root" },
      },
    }),
  );
  materials["build-archive.json"] = await fileDigest(
    join(directory, "build-archive.json"),
  );
  writeFileSync(join(directory, "provenance.json"), JSON.stringify(provenance));
  await expect(verifyArtifact(directory, expected)).rejects.toThrow(
    "actual OCI config bytes",
  );
  writeFileSync(join(directory, "build-archive.json"), originalRecord);
  materials["build-archive.json"] = originalRecordSha;
  writeFileSync(join(directory, "provenance.json"), JSON.stringify(provenance));
  writeFileSync(
    join(directory, "build-manifest.json"),
    Buffer.concat([rawManifest, Buffer.from(" ")]),
  );
  materials["build-manifest.json"] = await fileDigest(
    join(directory, "build-manifest.json"),
  );
  writeFileSync(join(directory, "provenance.json"), JSON.stringify(provenance));
  await expect(verifyArtifact(directory, expected)).rejects.toThrow(
    "manifest bytes mismatch",
  );
  writeFileSync(join(directory, "build-manifest.json"), rawManifest);
  materials["build-manifest.json"] = await fileDigest(
    join(directory, "build-manifest.json"),
  );
  writeFileSync(join(directory, "provenance.json"), JSON.stringify(provenance));
  writeFileSync(
    join(directory, "default-user.json"),
    JSON.stringify({ uid: 0, reference: runtimeBinding.reference }),
  );
  materials["default-user.json"] = await fileDigest(
    join(directory, "default-user.json"),
  );
  writeFileSync(join(directory, "provenance.json"), JSON.stringify(provenance));
  await expect(verifyArtifact(directory, expected)).rejects.toThrow(
    "default user",
  );
  writeFileSync(
    join(directory, "default-user.json"),
    JSON.stringify({ uid: 1000, reference: runtimeBinding.reference }),
  );
  materials["default-user.json"] = await fileDigest(
    join(directory, "default-user.json"),
  );
  for (const file of ["../outside", "tools.json"]) {
    materials[file] = `sha256:${"f".repeat(64)}`;
    if (file === "tools.json")
      writeFileSync(join(directory, file), "tampered optional material");
    writeFileSync(
      join(directory, "provenance.json"),
      JSON.stringify(provenance),
    );
    await expect(verifyArtifact(directory, expected)).rejects.toThrow(
      "material mismatch",
    );
    delete materials[file];
    if (file === "tools.json") rmSync(join(directory, file));
  }
  writeFileSync(join(directory, "provenance.json"), JSON.stringify(provenance));
  const approvedPolicy = readFileSync(join(directory, "scan-policy.json"));
  writeFileSync(join(directory, "scan-policy.json"), "{}");
  materials["scan-policy.json"] = await fileDigest(
    join(directory, "scan-policy.json"),
  );
  writeFileSync(join(directory, "provenance.json"), JSON.stringify(provenance));
  await expect(verifyArtifact(directory, expected)).rejects.toThrow(
    "candidate policy mismatch",
  );
  writeFileSync(join(directory, "scan-policy.json"), approvedPolicy);
  materials["scan-policy.json"] = await fileDigest(
    join(directory, "scan-policy.json"),
  );
  writeFileSync(join(directory, "provenance.json"), JSON.stringify(provenance));
  writeFileSync(join(directory, "history.json"), "tampered");
  await expect(verifyArtifact(directory, expected)).rejects.toThrow(
    "material mismatch",
  );
});

test("workflow permissions, source binding, immutable upload and hard download digest failure", () => {
  const workflow = Bun.YAML.parse(
    read(".github/workflows/halo-image.yml"),
  ) as any;
  expect(workflow.permissions).toEqual({ contents: "read" });
  expect(workflow.on.pull_request.types).toEqual([
    "opened",
    "reopened",
    "synchronize",
    "edited",
  ]);
  expect(workflow.on.push.branches).toEqual(["dev"]);
  expect(workflow.on.schedule).toBeUndefined();
  expect(workflow.on.workflow_run).toBeUndefined();
  for (const job of Object.values(workflow.jobs) as any[]) {
    expect(job["runs-on"]).toBe("ubuntu-24.04");
    for (const step of job.steps)
      if (step.uses?.startsWith("actions/checkout@"))
        expect(step.with["persist-credentials"]).toBe(false);
  }
  const upload = workflow.jobs.build.steps.find(
    (step: any) => step.id === "upload",
  );
  expect(upload.with.name).toBe(
    "nucleus-ci-${{ github.run_id }}-${{ github.run_attempt }}",
  );
  expect(upload.with["retention-days"]).toBe(7);
  expect(upload.if).toBe("steps.plan.outputs.build == 'true'");
  const download = workflow.jobs.download.steps.find((step: any) =>
    step.uses?.startsWith("actions/download-artifact@"),
  );
  expect(download.with["artifact-ids"]).toBe(
    "${{ needs.build.outputs.artifact-id }}",
  );
  expect(download.with["digest-mismatch"]).toBe("error");
  expect(download.with["github-token"]).toBeUndefined();
});
