import { afterEach, expect, test } from "bun:test";
import { EventEmitter } from "node:events";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  assertImageInputs,
  containerArguments,
  createPreparation,
  imageFileArguments,
  localImageTag,
  parseOptions,
  planSteps,
  prepareWorkspace,
  resolveImage,
  runContainer,
  runLocalPreparation,
  runPreparation,
  type ChildProcessLike,
  type ContainerSpec,
  type Docker,
  type Identity,
  type Preparation,
  type RunResult,
} from "./halo-local";

const digest = "a".repeat(64);
const imageId = `sha256:${"b".repeat(64)}`;
const identity: Identity = { uid: 1000, gid: 1000 };
const directories: string[] = [];

afterEach(() => {
  for (const directory of directories.splice(0))
    rmSync(directory, { recursive: true, force: true });
});

function temporary(prefix = "nucleus halo local "): string {
  const directory = mkdtempSync(join(tmpdir(), prefix));
  directories.push(directory);
  return directory;
}

function docker(
  options: {
    id?: string | null;
    input?: string;
    build?: (tag: string) => void;
  } = {},
): Docker & { builds: string[] } {
  const builds: string[] = [];
  return {
    builds,
    inspect: () => (options.id === undefined ? imageId : options.id),
    build:
      options.build ??
      ((tag) => {
        builds.push(tag);
      }),
    imageInput: () => options.input ?? digest,
    accounts: () => ({
      passwd:
        "root:x:0:0::/root:/bin/sh\nubuntu:x:1000:1000::/home/ubuntu:/bin/sh\n",
      group: "root:x:0:\nubuntu:x:1000:\n",
    }),
  };
}

test("options select exactly one preparation mode and an optional installed image", () => {
  expect(parseOptions([])).toEqual({ mode: "full" });
  expect(parseOptions(["--prepare"])).toEqual({ mode: "prepare" });
  expect(parseOptions(["--native", "--image", "example:test"])).toEqual({
    mode: "native",
    image: "example:test",
  });
  expect(parseOptions(["--image=example:test"])).toEqual({
    mode: "full",
    image: "example:test",
  });
  expect(() => parseOptions(["--prepare", "--native"])).toThrow("exclusive");
  expect(() => parseOptions(["--image"])).toThrow("requires an image");
  expect(() => parseOptions(["--image", "--native"])).toThrow(
    "requires an image",
  );
  expect(() => parseOptions(["unexpected"])).toThrow("Unknown argument");
});

test("the default image is built once, then reused from its input-bound tag", () => {
  const tag = localImageTag(digest);
  let installed = false;
  const builds: string[] = [];
  const dockerWithState: Docker = {
    ...docker(),
    inspect: () => (installed ? imageId : null),
    build: (requested) => {
      builds.push(requested);
      installed = true;
    },
    imageInput: () => digest,
  };

  expect(resolveImage(dockerWithState, undefined, digest)).toEqual({
    reference: tag,
    id: imageId,
  });
  expect(builds).toEqual([tag]);
  expect(resolveImage(dockerWithState, undefined, digest)).toEqual({
    reference: tag,
    id: imageId,
  });
  expect(builds).toHaveLength(1);
});

test("a missing explicit image is rejected without pulling or building", () => {
  let built = false;
  const missing: Docker = {
    ...docker(),
    inspect: () => null,
    build: () => {
      built = true;
    },
    imageInput: () => digest,
  };
  expect(() => resolveImage(missing, "example:missing", digest)).toThrow(
    "never pulls images",
  );
  expect(built).toBe(false);
  expect(() =>
    resolveImage(
      {
        ...docker(),
        inspect: () => null,
        build: () => {},
        imageInput: () => digest,
      },
      undefined,
      digest,
    ),
  ).toThrow("did not produce");
});

test("an image from different inputs is rejected before any container step runs", async () => {
  assertImageInputs(
    docker(),
    { reference: "example:test", id: imageId },
    digest,
  );
  expect(() =>
    assertImageInputs(
      docker({ input: "c".repeat(64) }),
      { reference: "example:test", id: imageId },
      digest,
    ),
  ).toThrow("records Halo CI inputs");

  const executed: ContainerSpec[] = [];
  await expect(
    runLocalPreparation({
      root: temporary(),
      options: { mode: "full" },
      digest,
      docker: docker({ input: "c".repeat(64) }),
      identity,
      execute: async (spec) => {
        executed.push(spec);
        return { status: 0, signal: null, error: null };
      },
      report: () => {},
      runId: "mismatch",
    }),
  ).rejects.toThrow("Build a matching image");
  expect(executed).toHaveLength(0);
});

test("a failed image build cannot proceed to dependency or browser work", async () => {
  const executed: ContainerSpec[] = [];
  await expect(
    runLocalPreparation({
      root: temporary(),
      options: { mode: "full" },
      digest,
      docker: docker({
        id: null,
        build: () => {
          throw new Error("builder failed");
        },
      }),
      identity,
      execute: async (spec) => {
        executed.push(spec);
        return { status: 0, signal: null, error: null };
      },
      report: () => {},
      runId: "build-failure",
    }),
  ).rejects.toThrow("builder failed");
  expect(executed).toHaveLength(0);
});

test("container arguments keep spaced paths in argv and isolate the runtime", () => {
  const repository = `${temporary()}with spaces`;
  mkdirSync(repository, { recursive: true });
  const workspace = prepareWorkspace(repository, "run-1");
  const args = containerArguments({
    image: imageId,
    user: "1000:1000",
    workdir: "/workspace/halo",
    command: ["bun", "tools/browser-smoke.ts"],
    environment: workspace.environment,
    mounts: workspace.mounts,
  });

  expect(args).toContain(`${repository}:/workspace`);
  expect(args).toContain(
    `${repository}/target/halo-local/target:/workspace/target`,
  );
  expect(args).toContain("--init");
  expect(args).toContain("--ipc=host");
  expect(args).toContain("--cpus=2");
  expect(args).toContain("--pull=never");
  expect(args).toContain("1000:1000");
  expect(args).not.toContain("--privileged");
  expect(args).not.toContain("--network");
  expect(args).not.toContain("-p");
  expect(args.join(" ")).not.toContain("docker.sock");
  expect(args).toContain("CARGO_TARGET_DIR=/workspace/target/cargo");
  expect(args).toContain("CARGO_BUILD_JOBS=2");
  expect(args).toContain("CARGO_HOME=/workspace/target/cargo-home");
  expect(args).toContain("BUN_INSTALL_CACHE_DIR=/workspace/target/bun-cache");
  expect(args).toContain(
    "XDG_CONFIG_HOME=/workspace/target/runs/run-1/xdg-config",
  );
  expect(args).toContain(
    "XDG_CACHE_HOME=/workspace/target/runs/run-1/xdg-cache",
  );
  expect(args).toContain("HOME=/workspace/target/runs/run-1/home");
  expect(Object.keys(workspace.environment).sort()).toEqual([
    "BUN_INSTALL_CACHE_DIR",
    "CARGO_BUILD_JOBS",
    "CARGO_HOME",
    "CARGO_TARGET_DIR",
    "HOME",
    "XDG_CACHE_HOME",
    "XDG_CONFIG_HOME",
  ]);
});

test("each run gets unique writable XDG paths while caches persist", () => {
  const repository = temporary();
  const first = prepareWorkspace(repository, "run-a");
  const second = prepareWorkspace(repository, "run-b");
  expect(first.environment.XDG_CONFIG_HOME).not.toBe(
    second.environment.XDG_CONFIG_HOME,
  );
  expect(first.environment.XDG_CACHE_HOME).not.toBe(
    second.environment.XDG_CACHE_HOME,
  );
  const cache = join(repository, "target", "halo-local", "target", "cargo");
  writeFileSync(join(cache, "marker"), "kept");
  prepareWorkspace(repository, "run-c");
  expect(existsSync(join(cache, "marker"))).toBe(true);
  expect(first.evidence).toBe(
    join(repository, "target", "halo-local", "target", "halo-browser-evidence"),
  );
});

test("the image input preflight uses the immutable ID without host binds or network", () => {
  const args = imageFileArguments(
    imageId,
    identity,
    "/etc/nucleus-halo-ci-inputs",
  );
  expect(args).toContain("--pull=never");
  expect(args).toContain("--network=none");
  expect(args).toContain("1000:1000");
  expect(args).not.toContain("-v");
  expect(args).not.toContain("--privileged");
  expect(args.at(-1)).toBe("/etc/nucleus-halo-ci-inputs");
  expect(() =>
    imageFileArguments(
      "nucleus-halo-ci:local-tag",
      identity,
      "/etc/nucleus-halo-ci-inputs",
    ),
  ).toThrow("immutable image ID");
});

test("smoke runs before type, build and browser business work in full mode", () => {
  const full = planSteps("full");
  expect(full.map((step) => step.name)).toEqual([
    "Verify image tools",
    "Install Halo dependencies",
    "Smoke browser runtimes",
    "Typecheck Halo",
    "Build Halo",
    "Run Halo browser acceptance",
  ]);
  expect(full.at(-1)?.environment).toEqual({ HALO_NGINX: "/usr/sbin/nginx" });
  expect(full[2]?.environment).toBeUndefined();

  expect(planSteps("prepare").map((step) => step.name)).toEqual([
    "Verify image tools",
    "Install Halo dependencies",
    "Smoke browser runtimes",
  ]);
  const native = planSteps("native");
  expect(native.at(-1)?.command).toEqual([
    "bun",
    "run",
    "test:browser",
    "--",
    "tests/identity.spec.ts",
    "--grep",
    "native history and reload",
  ]);
});

test("the full plan runs immutable-image containers in order", async () => {
  const executed: ContainerSpec[] = [];
  const repository = temporary();
  const { code } = await runLocalPreparation({
    root: repository,
    options: { mode: "full" },
    digest,
    docker: docker(),
    identity,
    execute: async (spec) => {
      executed.push(spec);
      return { status: 0, signal: null, error: null };
    },
    report: () => {},
    runId: "order",
  });

  expect(code).toBe(0);
  expect(executed.map((spec) => spec.command.join(" "))).toEqual([
    "bun tools/halo-ci-image.ts verify",
    "bun install --frozen-lockfile --ignore-scripts",
    "bun tools/browser-smoke.ts",
    "bun run typecheck",
    "bun run build",
    "bun tools/check-halo-test.ts browser",
  ]);
  for (const spec of executed) {
    expect(spec.image).toBe(imageId);
    expect(spec.user).toBe("1000:1000");
    expect(spec.mounts).toHaveLength(7);
  }
  expect(executed.at(-1)?.environment.HALO_NGINX).toBe("/usr/sbin/nginx");
});

test("a child nonzero status is returned unchanged", async () => {
  const preparation: Preparation = createPreparation({
    root: temporary(),
    options: { mode: "prepare" },
    digest,
    docker: docker(),
    identity,
    runId: "nonzero",
  });
  const code = await runPreparation(
    preparation.steps,
    async (spec) =>
      spec.command.includes("tools/browser-smoke.ts")
        ? { status: 7, signal: null, error: null }
        : { status: 0, signal: null, error: null },
    preparation,
    identity,
    () => {},
  );
  expect(code).toBe(7);
});

test("cancellation is forwarded while the container's own final result wins", async () => {
  let exit:
    ((status: number | null, signal: NodeJS.Signals | null) => void) | null =
    null;
  let unregistered = false;
  const kills: NodeJS.Signals[] = [];
  const child = {
    once(event: string, listener: (...args: unknown[]) => void) {
      if (event === "exit")
        exit = listener as (
          status: number | null,
          signal: NodeJS.Signals | null,
        ) => void;
      return child;
    },
    kill(signal: NodeJS.Signals) {
      kills.push(signal);
      return true;
    },
  } as ChildProcessLike;
  let handler: ((signal: NodeJS.Signals) => void) | null = null;
  const settled = runContainer(
    {
      image: imageId,
      user: "1000:1000",
      workdir: "/workspace",
      command: ["bun", "tool.ts"],
      environment: {},
      mounts: [],
    },
    {
      cwd: "/repository",
      spawnChild: () => child,
      settleContainer: async (_name, result) => result,
      registerSignals: (callback) => {
        handler = callback;
        return () => {
          unregistered = true;
        };
      },
    },
  );

  expect(handler).not.toBeNull();
  handler!("SIGTERM");
  expect(kills).toEqual(["SIGTERM"]);
  expect(unregistered).toBe(false);
  exit!(7, null);
  expect(await settled).toEqual({ status: 7, signal: null, error: null });
  expect(unregistered).toBe(true);
});

test("a container spawn error is reported without waiting for an exit", async () => {
  let error: ((error: Error) => void) | null = null;
  let unregistered = false;
  const child = {
    once(event: string, listener: (...args: unknown[]) => void) {
      if (event === "error") error = listener as (error: Error) => void;
      return child;
    },
    kill: () => false,
  } as ChildProcessLike;
  const settled: Promise<RunResult> = runContainer(
    {
      image: imageId,
      user: "1000:1000",
      workdir: "/workspace",
      command: ["bun", "tool.ts"],
      environment: {},
      mounts: [],
    },
    {
      cwd: "/repository",
      spawnChild: () => child,
      registerSignals: () => () => {
        unregistered = true;
      },
    },
  );

  error!(new Error("docker is unavailable"));
  expect(await settled).toEqual({
    status: null,
    signal: null,
    error: "docker is unavailable",
  });
  expect(unregistered).toBe(true);
});

test("client exit cannot finish acceptance before container termination readback", async () => {
  const child = Object.assign(new EventEmitter(), { kill: () => true });
  let entered!: () => void;
  const reading = new Promise<void>((resolve) => {
    entered = resolve;
  });
  let release!: (result: RunResult) => void;
  const readback = new Promise<RunResult>((resolve) => {
    release = resolve;
  });
  let completed = false;
  const pending = runContainer(
    {
      image: imageId,
      user: "1000:1000",
      workdir: "/workspace",
      command: ["true"],
      environment: {},
      mounts: [],
    },
    {
      cwd: "/repository",
      spawnChild: () => child as unknown as ChildProcessLike,
      registerSignals: () => () => {},
      settleContainer: async (name, result) => {
        expect(name).toStartWith("nucleus-halo-local-");
        expect(result.status).toBe(0);
        entered();
        return readback;
      },
    },
  ).then((result) => {
    completed = true;
    return result;
  });
  child.emit("exit", 0, null);
  await reading;
  expect(completed).toBe(false);
  release({
    status: 0,
    signal: null,
    error: "container termination is unknown",
  });
  expect((await pending).error).toBe("container termination is unknown");
});

test("repeated cancellation forwards once and cannot advance after a zero client exit", async () => {
  const signals: NodeJS.Signals[] = [];
  const child = Object.assign(new EventEmitter(), {
    kill: (signal: NodeJS.Signals) => {
      signals.push(signal);
      return true;
    },
  });
  let cancel!: (signal: NodeJS.Signals) => void;
  const pending = runContainer(
    {
      image: imageId,
      user: "1000:1000",
      workdir: "/workspace",
      command: ["true"],
      environment: {},
      mounts: [],
    },
    {
      cwd: "/repository",
      spawnChild: () => child as unknown as ChildProcessLike,
      registerSignals: (handler) => {
        cancel = handler;
        return () => {};
      },
      settleContainer: async (_name, result) => result,
    },
  );
  cancel("SIGINT");
  cancel("SIGTERM");
  child.emit("exit", 0, null);
  expect(signals).toEqual(["SIGINT"]);
  expect((await pending).status).toBe(130);
});

test("missing host UID and GID receive private readonly account mappings", () => {
  const preparation = createPreparation({
    root: temporary(),
    options: { mode: "prepare" },
    digest,
    docker: docker(),
    identity: { uid: 1002, gid: 1003 },
    runId: "new-uid",
  });
  const passwd = preparation.workspace.mounts.find(
    (mount) => mount.target === "/etc/passwd",
  )!;
  const group = preparation.workspace.mounts.find(
    (mount) => mount.target === "/etc/group",
  )!;
  expect(passwd.readonly).toBe(true);
  expect(group.readonly).toBe(true);
  expect(readFileSync(passwd.source, "utf8")).toBe(
    `root:x:0:0::/root:/bin/sh\nubuntu:x:1000:1000::/home/ubuntu:/bin/sh\nnucleus-halo-1002:x:1002:1003::${preparation.workspace.environment.HOME}:/bin/sh\n`,
  );
  expect(readFileSync(group.source, "utf8")).toBe(
    "root:x:0:\nubuntu:x:1000:\nnucleus-halo-1003:x:1003:\n",
  );
  expect(
    containerArguments({
      image: imageId,
      user: "1002:1003",
      workdir: "/workspace",
      command: ["true"],
      mounts: [passwd, group],
      environment: {},
    }),
  ).toContain(`${passwd.source}:/etc/passwd:ro`);
});

test("existing image account records are preserved without duplicate identities", () => {
  const original = docker().accounts(imageId);
  const preparation = createPreparation({
    root: temporary(),
    options: { mode: "prepare" },
    digest,
    docker: docker(),
    identity,
    runId: "known-uid",
  });
  for (const [file, contents] of Object.entries(original)) {
    const mount = preparation.workspace.mounts.find(
      (entry) => entry.target === `/etc/${file}`,
    )!;
    expect(readFileSync(mount.source, "utf8")).toBe(contents);
  }
});
