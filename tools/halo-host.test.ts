import { afterEach, expect, test } from "bun:test";
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";

const source = new URL("../", import.meta.url);
const directories: string[] = [];
const children: ReturnType<typeof Bun.spawn>[] = [];

afterEach(async () => {
  for (const child of children.splice(0)) {
    child.kill();
    await child.exited;
  }
  for (const directory of directories.splice(0))
    rmSync(directory, { recursive: true, force: true });
});

function fixture() {
  const root = mkdtempSync(join(tmpdir(), "halo-host-"));
  directories.push(root);
  const put = (path: string, content: string) => {
    const target = join(root, path);
    mkdirSync(dirname(target), { recursive: true });
    writeFileSync(target, content);
  };
  const executable = (path: string, body: string) => {
    put(path, `#!/bin/sh\n${body}\n`);
    chmodSync(join(root, path), 0o755);
  };
  for (const path of [
    "tools/halo-host.ts",
    "tools/check-bun.ts",
    "halo/tools/process.ts",
    ".tool-versions",
    "rust-toolchain.toml",
    "package.json",
    "fixture/postgresql.version",
  ])
    put(path, readFileSync(new URL(path, source), "utf8"));
  const versions = readFileSync(new URL(".tool-versions", source), "utf8");
  const node = versions.match(/^nodejs (.+)$/m)![1];
  const rust = Bun.TOML.parse(
    readFileSync(new URL("rust-toolchain.toml", source), "utf8"),
  ) as { toolchain: { channel: string } };
  const postgres = readFileSync(
    new URL("fixture/postgresql.version", source),
    "utf8",
  ).trim();
  executable("bin/node", `echo v${node}`);
  executable("bin/bun", `echo ${Bun.version}`);
  executable(
    "bin/rustup",
    `case "$*" in
    'toolchain list') echo '${rust.toolchain.channel}-x86_64-unknown-linux-gnu' ;;
    'run ${rust.toolchain.channel} rustc --version') echo 'rustc ${rust.toolchain.channel} fixture' ;;
    'run ${rust.toolchain.channel} cargo --version') echo 'cargo ${rust.toolchain.channel}' ;;
    *) exit 1 ;;
  esac`,
  );
  executable("bin/nginx", "echo nginx/fixture >&2");
  executable("bin/openssl", "echo OpenSSL-fixture");
  for (const program of ["postgres", "initdb", "pg_ctl", "psql"])
    executable(
      `target/postgresql/${postgres}/bin/${program}`,
      `echo '${program} (PostgreSQL) ${postgres}'`,
    );
  const playwright = JSON.parse(
    readFileSync(new URL("halo/package.json", source), "utf8"),
  ).devDependencies["@playwright/test"];
  put(
    "halo/node_modules/@playwright/test/package.json",
    JSON.stringify({ version: playwright }),
  );
  put(
    "halo/package.json",
    JSON.stringify({
      devDependencies: { "@playwright/test": playwright },
      scripts: {
        typecheck: `${process.execPath} step.ts typecheck`,
        build: `${process.execPath} step.ts build`,
      },
    }),
  );
  put(
    "halo/step.ts",
    `import { appendFileSync, existsSync } from 'node:fs';
import { setTimeout as delay } from 'node:timers/promises';
const label = process.argv[2];
const log = (value: string) => appendFileSync(process.env.EVENTS!, value + '\\n');
if (process.env.BLOCK_STEP === label) {
  process.on('SIGTERM', () => log('signal'));
  log(label);
  while (!existsSync(process.env.RELEASE!)) await delay(10);
  log('cleaned');
} else log(label);
if (process.env.FAIL_STEP === label) process.exit(7);
`,
  );
  put("tools/check-halo-test.ts", "import '../halo/step.ts';\n");
  put(
    "halo/tools/browser-smoke.ts",
    "process.argv[2] = 'smoke'; await import('../step.ts');\n",
  );
  const events = join(root, "events");
  const release = join(root, "release");
  function start(
    args: string[] = [],
    environment: Record<string, string> = {},
  ) {
    const child = Bun.spawn(
      [process.execPath, join(root, "tools/halo-host.ts"), ...args],
      {
        cwd: root,
        env: {
          ...process.env,
          PATH: `${join(root, "bin")}:${process.env.PATH}`,
          HALO_NGINX: join(root, "bin/nginx"),
          EVENTS: events,
          RELEASE: release,
          ...environment,
        },
        stdout: "pipe",
        stderr: "pipe",
      },
    );
    children.push(child);
    return child;
  }
  const lines = () =>
    existsSync(events) ? readFileSync(events, "utf8").trim().split("\n") : [];
  return { root, put, executable, start, lines, release };
}

test("prepare checks prerequisites without starting acceptance or writing runtime directories", async () => {
  const host = fixture();
  const child = host.start(["--prepare"]);
  expect(await child.exited).toBe(0);
  expect(await new Response(child.stdout).text()).toContain(
    "no acceptance was run",
  );
  expect(host.lines()).toEqual([]);
  expect(existsSync(join(host.root, "target/halo-host"))).toBe(false);
});

test("default checks stop at snapshot; browsers require explicit selection", async () => {
  const host = fixture();
  expect(await host.start().exited).toBe(0);
  expect(host.lines()).toEqual(["typecheck", "build", "snapshot"]);
  host.put("events", "");
  expect(await host.start(["--browser"]).exited).toBe(0);
  expect(host.lines()).toEqual([
    "typecheck",
    "build",
    "snapshot",
    "smoke",
    "browser",
  ]);
});

test("invalid options and incompatible tools fail before acceptance", async () => {
  const host = fixture();
  expect(await host.start(["--prepare", "--browser"]).exited).toBe(1);
  host.executable("bin/node", "echo v0.0.0");
  const child = host.start();
  expect(await child.exited).toBe(1);
  expect(await new Response(child.stderr).text()).toContain(
    "must be available on PATH",
  );
  expect(host.lines()).toEqual([]);
});

test("missing dependencies fail without installing or running steps", async () => {
  const host = fixture();
  rmSync(join(host.root, "halo/node_modules"), { recursive: true });
  expect(await host.start(["--prepare"]).exited).toBe(1);
  expect(existsSync(join(host.root, "halo/node_modules"))).toBe(false);
  expect(host.lines()).toEqual([]);
});

test("a failed build preserves its exit and prevents snapshot or browser steps", async () => {
  const host = fixture();
  expect(await host.start(["--browser"], { FAIL_STEP: "build" }).exited).toBe(
    7,
  );
  expect(host.lines()).toEqual(["typecheck", "build"]);
});

test("a missing browser import fails without Bun auto-install fallback", async () => {
  const host = fixture();
  host.put(
    "halo/tools/browser-smoke.ts",
    "import 'nucleus-host-missing-fixture-package';\n",
  );
  const child = host.start(["--browser"]);
  expect(await child.exited).toBe(1);
  expect(await new Response(child.stderr).text()).toContain(
    "Cannot find package",
  );
  expect(host.lines()).toEqual(["typecheck", "build", "snapshot"]);
});

test("interrupt reaches descendants once and waits for cleanup before returning", async () => {
  const host = fixture();
  const child = host.start([], { BLOCK_STEP: "typecheck" });
  const until = async (predicate: () => boolean) => {
    const deadline = Date.now() + 5_000;
    while (!predicate()) {
      if (Date.now() >= deadline)
        throw new Error("Timed out waiting for fixture handshake");
      await delay(10);
    }
  };
  try {
    await until(() => host.lines().includes("typecheck"));
    child.kill("SIGTERM");
    await until(() => host.lines().includes("signal"));
    child.kill("SIGTERM");
    expect(child.exitCode).toBeNull();
  } finally {
    writeFileSync(host.release, "release");
  }
  expect(await child.exited).not.toBe(0);
  expect(host.lines()).toEqual(["typecheck", "signal", "cleaned"]);
}, 15_000);
