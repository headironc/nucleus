import { afterAll, afterEach, beforeAll, expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  statSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const source = join(import.meta.dir, "bootstrap.tmpl");
const inputs = {
  UseJITConfig: true,
  EnableBootDebug: false,
  MetadataURL: "https://10.203.64.1:9444/api/v1/metadata",
  CallbackURL: "https://10.203.64.1:9444/api/v1/callbacks",
  RunnerUsername: "runner",
  CallbackToken: "fixture.secret.token",
};
let compiler: string;
let renderer: string;
const fixtures: string[] = [];
beforeAll(() => {
  compiler = mkdtempSync(join(tmpdir(), "runner-template-renderer-"));
  renderer = join(compiler, "render");
  expect(spawnSync("go", ["version"], { encoding: "utf8" }).stdout).toStartWith(
    "go version go1.27.3 ",
  );
  const build = spawnSync(
    "go",
    ["build", "-o", renderer, join(import.meta.dir, "render-bootstrap.go")],
    {
      encoding: "utf8",
      timeout: 60_000,
      env: {
        ...process.env,
        GOCACHE: join(compiler, "cache"),
        GOPATH: join(compiler, "gopath"),
        GOPROXY: "off",
        GOTOOLCHAIN: "local",
      },
    },
  );
  expect(build.stderr).toBe("");
  expect(build.status).toBe(0);
}, 65_000);
afterEach(() => {
  for (const path of fixtures.splice(0))
    rmSync(path, { recursive: true, force: true });
});
afterAll(() => {
  if (compiler) rmSync(compiler, { recursive: true, force: true });
});

function render(context: Partial<typeof inputs> = {}) {
  const result = spawnSync(renderer, [source], {
    input: JSON.stringify({ ...inputs, ...context }),
    encoding: "utf8",
    timeout: 5_000,
  });
  expect(result.stderr).toBe("");
  expect(result.status).toBe(0);
  const syntax = spawnSync("bash", ["-n"], {
    input: result.stdout,
    encoding: "utf8",
    timeout: 5_000,
  });
  expect(syntax.status).toBe(0);
  return result.stdout;
}

type Call = {
  command: string;
  path?: string;
  args?: string[];
  body?: { status?: string; message?: string; agent_id?: number | null };
};
function fixture() {
  const root = mkdtempSync(join(tmpdir(), "runner-bootstrap-"));
  fixtures.push(root);
  const runner = join(root, "home/runner/actions-runner");
  const etc = join(root, "etc");
  const bin = join(root, "bin");
  for (const path of [
    join(runner, "bin"),
    join(root, "home/runner/.docker"),
    join(etc, "systemd/system/docker.service.d"),
    bin,
  ])
    mkdirSync(path, { recursive: true });
  writeFileSync(join(runner, "bin/Runner.Listener"), "#!/bin/bash\nexit 0\n", {
    mode: 0o755,
  });
  writeFileSync(join(runner, "bin/runsvc.sh"), "#!/bin/bash\nexit 0\n", {
    mode: 0o755,
  });
  writeFileSync(join(runner, "env.sh"), "exit 0\n");
  writeFileSync(join(root, "untouched"), "preserve existing identity\n");
  writeFileSync(
    join(etc, "nucleiron-proxy.env"),
    "http_proxy=http://10.203.64.1:3128\n" +
      "https_proxy=http://10.203.64.1:3128\n" +
      "HTTP_PROXY=http://10.203.64.1:3128\n" +
      "HTTPS_PROXY=http://10.203.64.1:3128\n" +
      "no_proxy=localhost,127.0.0.1,10.203.64.1\n" +
      "NO_PROXY=localhost,127.0.0.1,10.203.64.1\n",
  );
  writeFileSync(
    join(etc, "systemd/system/docker.service.d/proxy.conf"),
    `[Service]\nEnvironmentFile=${etc}/nucleiron-proxy.env\n`,
  );
  writeFileSync(
    join(root, "home/runner/.docker/config.json"),
    JSON.stringify({
      proxies: {
        default: {
          httpProxy: "http://10.203.64.1:3128",
          httpsProxy: "http://10.203.64.1:3128",
          noProxy: "localhost,127.0.0.1,10.203.64.1",
        },
      },
    }),
  );
  writeFileSync(join(etc, "os-release"), 'NAME="Fixture OS"\nVERSION_ID=1\n');
  const quote = (value: string) => "'" + value.replaceAll("'", "'\\''") + "'";
  for (const name of ["curl", "docker", "sudo"])
    writeFileSync(
      join(bin, name),
      `#!/bin/bash\nexec ${quote(process.execPath)} ${quote(join(import.meta.dir, "bootstrap-command.ts"))} ${quote(name)} "$@"\n`,
      { mode: 0o755 },
    );
  return {
    root,
    runner,
    etc,
    run(
      mode = "success",
      context: Partial<typeof inputs> = {},
      extra: NodeJS.ProcessEnv = {},
    ) {
      // Remap filesystem roots only; execute all rendered guards and control flow.
      const script = render(context)
        .replaceAll("/home/runner", join(root, "home/runner"))
        .replaceAll("/etc/", etc + "/");
      const path = join(root, "bootstrap.sh");
      writeFileSync(path, script, { mode: 0o600 });
      const result = spawnSync("bash", ["--noprofile", "--norc", "-x", path], {
        cwd: root,
        encoding: "utf8",
        timeout: 20_000,
        env: {
          PATH: `${bin}:/usr/bin:/bin`,
          HOME: join(root, "home/runner"),
          TMPDIR: root,
          PROBE_FIXTURE: root,
          PROBE_CASE: mode,
          ...extra,
        },
      });
      const calls: Call[] = existsSync(join(root, "calls.jsonl"))
        ? readFileSync(join(root, "calls.jsonl"), "utf8")
            .trim()
            .split("\n")
            .map((line) => JSON.parse(line) as Call)
        : [];
      expect(result.error).toBeUndefined();
      expect(result.stdout + result.stderr).not.toContain(inputs.CallbackToken);
      expect(result.stdout + result.stderr).not.toContain(
        "synthetic-jit-credential",
      );
      return { ...result, calls };
    },
  };
}

test("renders and starts JIT service with null Agent ID callbacks", () => {
  const f = fixture();
  const result = f.run();
  expect(result.status).toBe(0);
  expect(result.stdout).toContain("Runner bootstrap complete");
  const callbacks = result.calls.filter((c) => c.path?.includes("/callbacks/"));
  expect(callbacks.map((c) => c.body?.agent_id)).toEqual([
    null,
    null,
    null,
    null,
  ]);
  expect(callbacks.at(-1)?.body?.status).toBe("idle");
  expect(callbacks.at(-2)?.path).toBe("/api/v1/callbacks/system-info");
  expect(readFileSync(join(f.runner, ".runner"), "utf8")).toContain("123456");
  for (const name of [
    ".runner",
    ".credentials",
    ".credentials_rsaparams",
    ".service",
  ])
    expect(statSync(join(f.runner, name)).mode & 0o777).toBe(0o600);
  const cold = result.calls.findIndex(
    (c) => c.args?.join(" ") === "container ls --all --quiet",
  );
  const identity = result.calls.findIndex((c) =>
    c.path?.includes("/metadata/credentials/"),
  );
  expect(cold).toBeGreaterThanOrEqual(0);
  expect(identity).toBeGreaterThan(cold);
});

test.each([
  { UseJITConfig: false },
  { EnableBootDebug: true },
  { MetadataURL: "https://example.invalid/metadata" },
  { CallbackURL: "https://example.invalid/callbacks" },
  { RunnerUsername: "root" },
])(
  "rejects incompatible provider context before any external command: %j",
  (context) => {
    const result = fixture().run("success", context);
    expect(result.status).toBe(1);
    expect(result.calls).toEqual([]);
  },
);

test.each([".runner", ".credentials", ".credentials_rsaparams", ".service"])(
  "preserves an existing identity file or symlink: %s",
  (name) => {
    for (const link of [false, true]) {
      const f = fixture();
      const path = join(f.runner, name);
      if (link) symlinkSync(join(f.root, "untouched"), path);
      else writeFileSync(path, "existing identity\n");
      const result = f.run();
      expect(result.status).toBe(1);
      expect(
        result.calls.every((c) => c.path === "/api/v1/callbacks/status"),
      ).toBe(true);
      expect(readFileSync(path, "utf8")).toBe(
        link ? "preserve existing identity\n" : "existing identity\n",
      );
    }
  },
);

test.each([
  "wrong-context",
  "remote-docker",
  "warm-images",
  "existing-container",
])("rejects a nonempty or unintended Docker store before JIT: %s", (mode) => {
  const result = fixture().run(mode);
  expect(result.status).toBe(1);
  expect(result.calls.some((c) => c.path?.includes("/metadata/"))).toBe(false);
  expect(result.calls.at(-1)?.body?.message).toBe(
    "Bootstrap failed at docker-cold-store",
  );
});

test.each(["DOCKER_HOST", "DOCKER_CONTEXT"])(
  "rejects inherited Docker selection: %s",
  (key) => {
    const result = fixture().run("success", {}, { [key]: "unexpected" });
    expect(result.status).toBe(1);
    expect(result.calls.some((c) => c.path?.includes("/metadata/"))).toBe(
      false,
    );
  },
);

test("exclusive identity writes reject a file introduced after preflight", () => {
  const f = fixture();
  const result = f.run("identity-race");
  expect(result.status).toBe(1);
  expect(readFileSync(join(f.root, "untouched"), "utf8")).toBe(
    "preserve existing identity\n",
  );
  expect(
    result.calls.some(
      (c) => c.path === "/api/v1/metadata/credentials/credentials",
    ),
  ).toBe(false);
  expect(result.calls.at(-1)?.body?.message).toBe(
    "Bootstrap failed at jit-files",
  );
});

test.each([
  ["metadata-failure", "jit-files"],
  ["service-failure", "service-start"],
  ["wrong-version", "preflight"],
  ["collector-active", "preflight"],
])("reports one redacted failure at its handling stage: %s", (mode, stage) => {
  const result = fixture().run(mode);
  expect(result.status).toBe(1);
  const failures = result.calls.filter((c) => c.body?.status === "failed");
  expect(failures).toHaveLength(1);
  expect(failures[0]?.body).toEqual({
    status: "failed",
    message: `Bootstrap failed at ${stage}`,
    agent_id: null,
  });
  expect(result.stdout).not.toContain("Runner bootstrap complete");
});

test("proxy drift stops before JIT and callback failure stays failed", () => {
  const f = fixture();
  writeFileSync(join(f.etc, "nucleiron-proxy.env"), "unexpected proxy\n");
  const drift = f.run();
  expect(drift.status).toBe(1);
  expect(drift.calls.some((c) => c.path?.includes("/metadata/"))).toBe(false);
  const callback = fixture().run("callback-failure");
  expect(callback.status).toBe(1);
  expect(callback.calls.some((c) => c.path?.includes("/metadata/"))).toBe(
    false,
  );
});
