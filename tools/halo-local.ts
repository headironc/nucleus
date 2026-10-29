// Repository-local Halo acceptance runtime. It prepares or reuses the fixed
// Halo CI image as a non-root container around this working copy, verifies the
// image input fingerprint and the two real browser runtimes, then runs the
// existing Halo checks unchanged.

import { spawn, spawnSync, type ChildProcess } from "node:child_process";
import { randomBytes } from "node:crypto";
import { chmodSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { bunVersionProblems, pinnedBun } from "./check-bun";
import { imageInputs } from "./halo-ci-image";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");

export type Mode = "full" | "prepare" | "native";

export type Options = { mode: Mode; image?: string };

export type Identity = { uid: number; gid: number };

export type Mount = { source: string; target: string; readonly?: boolean };

export type ContainerSpec = {
  name?: string;
  image: string;
  user: string;
  workdir: string;
  command: string[];
  environment: Record<string, string>;
  mounts: Mount[];
};

export type RunResult = {
  status: number | null;
  signal: NodeJS.Signals | null;
  error: string | null;
};

export type Step = {
  name: string;
  workdir: string;
  command: string[];
  environment?: Record<string, string>;
};

export type Workspace = {
  run: string;
  target: string;
  evidence: string;
  mounts: Mount[];
  environment: Record<string, string>;
};

export type Docker = {
  inspect(reference: string): string | null;
  build(tag: string): void;
  imageInput(image: string): string;
  accounts(image: string): { passwd: string; group: string };
};

export type ResolvedImage = { reference: string; id: string };

export type Preparation = {
  image: ResolvedImage;
  workspace: Workspace;
  steps: Step[];
};

const IMAGE_ID = /^sha256:[0-9a-f]{64}$/;
const MOUNT_ROOT = "/workspace";
const usage =
  "Usage: bun tools/halo-local.ts [--prepare | --native] [--image IMAGE]";

export function parseOptions(args: string[]): Options {
  let prepare = false;
  let native = false;
  let image: string | undefined;
  for (let index = 0; index < args.length; index += 1) {
    const argument = args[index]!;
    if (argument === "--prepare") {
      if (native)
        throw new Error(`--prepare and --native are exclusive\n${usage}`);
      prepare = true;
    } else if (argument === "--native") {
      if (prepare)
        throw new Error(`--prepare and --native are exclusive\n${usage}`);
      native = true;
    } else if (argument === "--image" || argument.startsWith("--image=")) {
      if (image !== undefined) throw new Error(`Duplicate --image\n${usage}`);
      const value =
        argument === "--image"
          ? args[(index += 1)]
          : argument.slice("--image=".length);
      if (!value || value.startsWith("-"))
        throw new Error(`--image requires an image reference\n${usage}`);
      image = value;
    } else {
      throw new Error(`Unknown argument: ${argument}\n${usage}`);
    }
  }
  return {
    mode: prepare ? "prepare" : native ? "native" : "full",
    ...(image === undefined ? {} : { image }),
  };
}

export function localImageTag(digest: string): string {
  if (!/^[0-9a-f]{64}$/.test(digest))
    throw new Error("Halo CI image input digest is invalid");
  return `nucleus-halo-ci:local-${digest}`;
}

export function resolveImage(
  docker: Docker,
  requested: string | undefined,
  digest: string,
): ResolvedImage {
  const reference = requested ?? localImageTag(digest);
  const existing = docker.inspect(reference);
  if (existing) return { reference, id: existing };
  if (requested)
    throw new Error(
      `Image ${requested} is not installed; local preparation never pulls images. Build it or pass an installed image.`,
    );
  docker.build(reference);
  const built = docker.inspect(reference);
  if (!built)
    throw new Error(`The Halo CI image builder did not produce ${reference}`);
  return { reference, id: built };
}

export function assertImageInputs(
  docker: Docker,
  image: ResolvedImage,
  digest: string,
): void {
  const recorded = docker.imageInput(image.id).trim();
  if (recorded !== digest)
    throw new Error(
      `Image ${image.id} records Halo CI inputs ${JSON.stringify(recorded)}, but this repository requires ${digest}. Build a matching image or pass a matching --image.`,
    );
}

export function currentIdentity(): Identity {
  const uid = process.getuid?.();
  const gid = process.getgid?.();
  if (uid === undefined || gid === undefined)
    throw new Error("Halo local preparation requires a Linux UID and GID");
  if (uid === 0 || gid === 0)
    throw new Error("Halo local preparation must run as a non-root user");
  return { uid, gid };
}

function createRunId(): string {
  const stamp = new Date().toISOString().replace(/[:.]/g, "-");
  return `${stamp}-${process.pid}-${randomBytes(4).toString("hex")}`;
}

export function prepareWorkspace(
  repository: string,
  runId: string = createRunId(),
): Workspace {
  if (!/^[A-Za-z0-9._-]{1,160}$/.test(runId))
    throw new Error("Halo local run identifier is unsafe");
  const local = join(repository, "target", "halo-local");
  const target = join(local, "target");
  const rootModules = join(local, "root-node_modules");
  const haloModules = join(local, "halo-node_modules");
  const run = join(target, "runs", runId);
  const directories = [
    target,
    rootModules,
    haloModules,
    join(target, "cargo"),
    join(target, "cargo-home"),
    join(target, "bun-cache"),
    join(run, "home"),
    join(run, "xdg-config"),
    join(run, "xdg-cache"),
    join(run, "test-results"),
    join(repository, "node_modules"),
    join(repository, "halo", "node_modules"),
    join(repository, "halo", "test-results"),
  ];
  for (const directory of directories)
    mkdirSync(directory, { recursive: true });
  const containerRun = `${MOUNT_ROOT}/target/runs/${runId}`;
  return {
    run,
    target,
    evidence: join(target, "halo-browser-evidence"),
    mounts: [
      { source: repository, target: MOUNT_ROOT },
      { source: target, target: `${MOUNT_ROOT}/target` },
      { source: rootModules, target: `${MOUNT_ROOT}/node_modules` },
      { source: haloModules, target: `${MOUNT_ROOT}/halo/node_modules` },
      {
        source: join(run, "test-results"),
        target: `${MOUNT_ROOT}/halo/test-results`,
      },
    ],
    environment: {
      CARGO_TARGET_DIR: `${MOUNT_ROOT}/target/cargo`,
      CARGO_BUILD_JOBS: "2",
      CARGO_HOME: `${MOUNT_ROOT}/target/cargo-home`,
      BUN_INSTALL_CACHE_DIR: `${MOUNT_ROOT}/target/bun-cache`,
      HOME: `${containerRun}/home`,
      XDG_CONFIG_HOME: `${containerRun}/xdg-config`,
      XDG_CACHE_HOME: `${containerRun}/xdg-cache`,
    },
  };
}

export function planSteps(mode: Mode): Step[] {
  const common: Step[] = [
    {
      name: "Verify image tools",
      workdir: MOUNT_ROOT,
      command: ["bun", "tools/halo-ci-image.ts", "verify"],
    },
    {
      name: "Install Halo dependencies",
      workdir: `${MOUNT_ROOT}/halo`,
      command: ["bun", "install", "--frozen-lockfile", "--ignore-scripts"],
    },
    {
      name: "Smoke browser runtimes",
      workdir: `${MOUNT_ROOT}/halo`,
      command: ["bun", "tools/browser-smoke.ts"],
    },
  ];
  if (mode === "prepare") return common;
  if (mode === "native")
    return [
      ...common,
      {
        name: "Native history and reload consumer",
        workdir: `${MOUNT_ROOT}/halo`,
        command: [
          "bun",
          "run",
          "test:browser",
          "--",
          "tests/identity.spec.ts",
          "--grep",
          "native history and reload",
        ],
      },
    ];
  return [
    ...common,
    {
      name: "Typecheck Halo",
      workdir: `${MOUNT_ROOT}/halo`,
      command: ["bun", "run", "typecheck"],
    },
    {
      name: "Build Halo",
      workdir: `${MOUNT_ROOT}/halo`,
      command: ["bun", "run", "build"],
    },
    {
      name: "Run Halo browser acceptance",
      workdir: MOUNT_ROOT,
      command: ["bun", "tools/check-halo-test.ts", "browser"],
      environment: { HALO_NGINX: "/usr/sbin/nginx" },
    },
  ];
}

export function containerArguments(spec: ContainerSpec): string[] {
  if (!IMAGE_ID.test(spec.image))
    throw new Error("Containers must run from an immutable image ID");
  return [
    "run",
    "--rm",
    "--init",
    "--ipc=host",
    "--cpus=2",
    "--pull=never",
    ...(spec.name ? ["--name", spec.name] : []),
    "--user",
    spec.user,
    "--workdir",
    spec.workdir,
    ...spec.mounts.flatMap((mount) => [
      "-v",
      `${mount.source}:${mount.target}${mount.readonly ? ":ro" : ""}`,
    ]),
    ...Object.entries(spec.environment).flatMap(([name, value]) => [
      "-e",
      `${name}=${value}`,
    ]),
    spec.image,
    ...spec.command,
  ];
}

export function imageFileArguments(
  image: string,
  identity: Identity,
  path: string,
): string[] {
  if (!IMAGE_ID.test(image))
    throw new Error("The image input check requires an immutable image ID");
  return [
    "run",
    "--rm",
    "--pull=never",
    "--network=none",
    "--read-only",
    "--user",
    `${identity.uid}:${identity.gid}`,
    "--entrypoint",
    "cat",
    image,
    path,
  ];
}

export function createPreparation(input: {
  root: string;
  options: Options;
  digest: string;
  docker: Docker;
  identity: Identity;
  runId?: string;
}): Preparation {
  if (input.identity.uid === 0 || input.identity.gid === 0)
    throw new Error("Halo local preparation must run as a non-root user");
  const image = resolveImage(input.docker, input.options.image, input.digest);
  assertImageInputs(input.docker, image, input.digest);
  const accounts = input.docker.accounts(image.id);
  const workspace = prepareWorkspace(input.root, input.runId);
  workspace.mounts.push(
    ...prepareAccounts(
      accounts,
      input.identity,
      workspace.environment.HOME!,
      workspace.run,
    ),
  );
  return {
    image,
    workspace,
    steps: planSteps(input.options.mode),
  };
}

export function prepareAccounts(
  accounts: { passwd: string; group: string },
  identity: Identity,
  home: string,
  directory: string,
): Mount[] {
  const mounts: Mount[] = [];
  const user = `nucleus-halo-${identity.uid}`;
  const group = `nucleus-halo-${identity.gid}`;
  for (const [file, original, id, name, entry] of [
    [
      "passwd",
      accounts.passwd,
      identity.uid,
      user,
      `${user}:x:${identity.uid}:${identity.gid}::${home}:/bin/sh`,
    ],
    [
      "group",
      accounts.group,
      identity.gid,
      group,
      `${group}:x:${identity.gid}:`,
    ],
  ] as const) {
    const lines = original.split("\n").filter(Boolean);
    const present = lines.some((line) => line.split(":")[2] === String(id));
    if (!present && lines.some((line) => line.split(":")[0] === name))
      throw new Error(
        `Image /etc/${file} conflicts with local identity ${name}`,
      );
    const contents = present
      ? original
      : `${original}${original.endsWith("\n") ? "" : "\n"}${entry}\n`;
    const source = join(directory, file);
    writeFileSync(source, contents, { mode: 0o644, flag: "wx" });
    // These new files contain only public image account records. NSS readers
    // need their usual read mode even when the caller uses a private umask.
    chmodSync(source, 0o644);
    mounts.push({ source, target: `/etc/${file}`, readonly: true });
  }
  return mounts;
}

export async function runPreparation(
  steps: Step[],
  execute: (spec: ContainerSpec) => Promise<RunResult>,
  preparation: Preparation,
  identity: Identity,
  report: (line: string) => void = (line) => console.log(line),
): Promise<number> {
  for (const step of steps) {
    report(`[halo-local] ${step.name}`);
    const result = await execute({
      image: preparation.image.id,
      user: `${identity.uid}:${identity.gid}`,
      workdir: step.workdir,
      command: step.command,
      environment: {
        ...preparation.workspace.environment,
        ...step.environment,
      },
      mounts: preparation.workspace.mounts,
    });
    if (result.error) {
      report(`[halo-local] ${step.name} failed: ${result.error}`);
      return 1;
    }
    if (result.status !== 0 || result.signal !== null) {
      report(
        `[halo-local] ${step.name} ended with ${result.status ?? result.signal}`,
      );
      return result.status ?? 1;
    }
  }
  return 0;
}

export async function runLocalPreparation(input: {
  root: string;
  options: Options;
  digest: string;
  docker: Docker;
  identity: Identity;
  execute: (spec: ContainerSpec) => Promise<RunResult>;
  report?: (line: string) => void;
  runId?: string;
}): Promise<{ code: number; preparation: Preparation }> {
  const report = input.report ?? ((line: string) => console.log(line));
  const preparation = createPreparation(input);
  report(
    `[halo-local] Image ${preparation.image.reference} (${preparation.image.id}) matches inputs ${input.digest}`,
  );
  report(`[halo-local] Writable outputs: ${preparation.workspace.target}`);
  const code = await runPreparation(
    preparation.steps,
    input.execute,
    preparation,
    input.identity,
    report,
  );
  return { code, preparation };
}

export type ChildProcessLike = Pick<ChildProcess, "once" | "kill">;

export type SignalRegistrar = (
  handler: (signal: NodeJS.Signals) => void,
) => () => void;

export function processSignals(): SignalRegistrar {
  return (handler) => {
    const onInterrupt = () => handler("SIGINT");
    const onTerminate = () => handler("SIGTERM");
    process.on("SIGINT", onInterrupt);
    process.on("SIGTERM", onTerminate);
    return () => {
      process.off("SIGINT", onInterrupt);
      process.off("SIGTERM", onTerminate);
    };
  };
}

export async function runContainer(
  spec: ContainerSpec,
  options: {
    cwd: string;
    spawnChild: (
      command: string,
      args: string[],
      options: { cwd: string; stdio: "inherit" },
    ) => ChildProcessLike;
    registerSignals: SignalRegistrar;
    settleContainer?: (name: string, result: RunResult) => Promise<RunResult>;
  },
): Promise<RunResult> {
  const name = `nucleus-halo-local-${randomBytes(12).toString("hex")}`;
  const child = options.spawnChild(
    "docker",
    containerArguments({ ...spec, name }),
    {
      cwd: options.cwd,
      stdio: "inherit",
    },
  );
  const exit = new Promise<RunResult>((resolveExit) => {
    child.once("exit", (status: number | null, signal: NodeJS.Signals | null) =>
      resolveExit({ status, signal, error: null }),
    );
    child.once("error", (error: Error) =>
      resolveExit({ status: null, signal: null, error: error.message }),
    );
  });
  let requested: NodeJS.Signals | null = null;
  const unregister = options.registerSignals((signal) => {
    if (requested) return;
    requested = signal;
    try {
      child.kill(signal);
    } catch {
      // The container may already have exited; its observed result still wins.
    }
  });
  try {
    const result = await exit;
    const settled = result.error
      ? result
      : await (
          options.settleContainer ??
          ((name, result) =>
            settleContainer(name, options.cwd, result, requested))
        )(name, result);
    if (requested && settled.status === 0)
      return {
        status: requested === "SIGINT" ? 130 : 143,
        signal: null,
        error: null,
      };
    return settled;
  } finally {
    unregister();
  }
}

async function settleContainer(
  name: string,
  cwd: string,
  result: RunResult,
  requested: NodeJS.Signals | null,
): Promise<RunResult> {
  const state = dockerCapture(
    ["container", "inspect", "--format", "{{json .State}}", name],
    cwd,
  );
  if (
    state.status !== 0 &&
    /no such container|no such object/i.test(state.stderr)
  )
    return result;
  if (state.error || state.status !== 0)
    return {
      ...result,
      error: `Container ${name} termination is unknown: ${state.error?.message ?? state.stderr.trim()}`,
    };
  let running: unknown;
  try {
    running = JSON.parse(state.stdout).Running;
  } catch {
    /* Report malformed readback below. */
  }
  if (running === false) return result;
  if (running !== true)
    return {
      ...result,
      error: `Container ${name} termination readback is invalid`,
    };

  // A client transport failure can leave the container alive. Cancel the
  // original operation once, then wait for its existing cleanup to finish.
  const stop = requested
    ? null
    : dockerCapture(["kill", "--signal=SIGINT", name], cwd);
  const waited = dockerCapture(["wait", name], cwd);
  const final = dockerCapture(
    ["container", "inspect", "--format", "{{json .State}}", name],
    cwd,
  );
  const removed =
    final.status !== 0 &&
    /no such container|no such object/i.test(final.stderr);
  let stopped = false;
  try {
    stopped = final.status === 0 && JSON.parse(final.stdout).Running === false;
  } catch {
    /* Keep the outcome unknown. */
  }
  if (!removed && !stopped)
    return {
      ...result,
      error: `Container ${name} remains active or unknown after cancellation; stop=${stop?.status ?? "already requested"}, wait=${waited.status}`,
    };
  return {
    ...result,
    error: `Docker client exited before container ${name}; its original operation has now stopped`,
  };
}

function dockerCapture(
  args: string[],
  cwd: string,
): {
  status: number | null;
  stdout: string;
  stderr: string;
  error: Error | null;
} {
  const result = spawnSync("docker", args, {
    cwd,
    encoding: "utf8",
    maxBuffer: 4 * 1024 * 1024,
  });
  return {
    status: result.status,
    stdout: result.stdout ?? "",
    stderr: result.stderr ?? "",
    error: result.error ?? null,
  };
}

export function createDocker(repository: string, identity: Identity): Docker {
  const readImageFile = (image: string, path: string): string => {
    const result = dockerCapture(
      imageFileArguments(image, identity, path),
      repository,
    );
    if (result.error)
      throw new Error(`Cannot run Docker: ${result.error.message}`);
    if (result.status !== 0)
      throw new Error(
        `Docker ${path} read failed for ${image}: ${result.stderr.trim() || result.status}`,
      );
    return result.stdout;
  };
  return {
    inspect(reference) {
      const result = dockerCapture(
        ["image", "inspect", "--format", "{{.Id}}", reference],
        repository,
      );
      if (result.error)
        throw new Error(`Cannot run Docker: ${result.error.message}`);
      if (result.status === null)
        throw new Error(`Docker image inspect did not finish for ${reference}`);
      if (result.status !== 0) {
        if (/no such image|no such object/i.test(result.stderr)) return null;
        throw new Error(
          `Docker image inspect failed for ${reference}: ${result.stderr.trim() || result.status}`,
        );
      }
      const id = result.stdout.trim();
      if (!IMAGE_ID.test(id))
        throw new Error(
          `Docker did not return an immutable image ID for ${reference}`,
        );
      return id;
    },
    build(tag) {
      const result = spawnSync(
        process.execPath,
        [join(repository, "tools/halo-ci-image.ts"), "build", tag],
        { cwd: repository, stdio: "inherit" },
      );
      if (result.error) throw result.error;
      if (result.status !== 0)
        throw new Error(
          `Halo CI image build failed (${result.signal ?? result.status})`,
        );
    },
    imageInput(image) {
      return readImageFile(image, "/etc/nucleus-halo-ci-inputs");
    },
    accounts: (image) => ({
      passwd: readImageFile(image, "/etc/passwd"),
      group: readImageFile(image, "/etc/group"),
    }),
  };
}

function assertHostBun(repository: string): void {
  if (
    pinnedBun(readFileSync(join(repository, ".tool-versions"), "utf8")) ===
    undefined
  )
    throw new Error('.tool-versions must contain exactly one "bun x.y.z" line');
  if (typeof Bun === "undefined")
    throw new Error("Halo local preparation must run with Bun");
  const manifest = JSON.parse(
    readFileSync(join(repository, "package.json"), "utf8"),
  ) as Parameters<typeof bunVersionProblems>[1];
  const problems = bunVersionProblems(
    readFileSync(join(repository, ".tool-versions"), "utf8"),
    manifest,
    Bun.version,
  );
  if (problems.length) throw new Error(problems.join("\n"));
}

async function main(): Promise<void> {
  const options = parseOptions(process.argv.slice(2));
  assertHostBun(root);
  const identity = currentIdentity();
  const inputs = imageInputs();
  const docker = createDocker(root, identity);
  const { code } = await runLocalPreparation({
    root,
    options,
    digest: inputs.digest,
    docker,
    identity,
    execute: (spec) =>
      runContainer(spec, {
        cwd: root,
        spawnChild: (command, args, spawnOptions) =>
          spawn(command, args, spawnOptions),
        registerSignals: processSignals(),
      }),
  });
  process.exitCode = code;
}

if (import.meta.main) {
  main().catch((error: unknown) => {
    console.error(error instanceof Error ? error.message : error);
    process.exitCode = 1;
  });
}
