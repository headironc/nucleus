import { afterEach, expect, test } from "vitest";
import { spawn, spawnSync, type ChildProcess } from "node:child_process";
import { createHash } from "node:crypto";
import {
  chmodSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { readJsonl, summarizeReporter } from "./evidence.ts";

const halo = fileURLToPath(new URL("..", import.meta.url));
const production = fileURLToPath(new URL("./browser.ts", import.meta.url));
const reporter = fileURLToPath(new URL("./reporter.ts", import.meta.url));
const directories: string[] = [];
const runners: ChildProcess[] = [];

afterEach(() => {
  for (const child of runners.splice(0)) {
    try {
      if (child.pid) child.kill("SIGKILL");
    } catch {
      // Already gone.
    }
  }
  for (const directory of directories.splice(0)) {
    killOwned(directory);
    rmSync(directory, { recursive: true, force: true });
  }
});

/** A failed test still releases the process tree this fixture started. */
function killOwned(root: string): void {
  try {
    const evidence = join(root, "evidence");
    const id = (
      JSON.parse(readFileSync(join(evidence, "current.json"), "utf8")) as {
        runId: string;
      }
    ).runId;
    for (const line of readFileSync(
      join(evidence, "runs", id, "runner.jsonl"),
      "utf8",
    ).split("\n")) {
      if (!line.trim()) continue;
      const record = JSON.parse(line) as {
        event?: string;
        pid?: number;
        group?: boolean;
      };
      if (record.event !== "child_spawn" || typeof record.pid !== "number")
        continue;
      if (record.group) {
        try {
          process.kill(-record.pid, "SIGKILL");
        } catch {
          // Already gone.
        }
      }
      try {
        process.kill(record.pid, "SIGKILL");
      } catch {
        // Already gone.
      }
    }
  } catch {
    // No evidence or already removed.
  }
}

function directory(): string {
  const path = mkdtempSync(join(tmpdir(), "halo-browser-run-"));
  directories.push(path);
  return path;
}

async function freePort(): Promise<number> {
  const server = createServer();
  server.listen(0, "127.0.0.1");
  await new Promise<void>((resolve) => server.once("listening", () => resolve()));
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("Missing port");
  const port = address.port;
  await new Promise<void>((resolve) => server.close(() => resolve()));
  return port;
}

function copy(root: string, module: string): void {
  copyFileSync(join(halo, "tools", module), join(root, "tools", module));
}

/**
 * Build a disposable copy of the production runner. The only source change is
 * the checked deadline constant; all services are local controlled fixtures and
 * the Playwright suite uses `expect` only, so no Chromium is required.
 */
function fixture(deadlineMs: number): {
  root: string;
  evidence: string;
  material: string;
  run: (spec: string) => Promise<ChildProcess>;
} {
  const root = directory();
  mkdirSync(join(root, "tools"), { recursive: true });
  mkdirSync(join(root, "deploy"), { recursive: true });
  mkdirSync(join(root, "specs"), { recursive: true });
  mkdirSync(join(root, "build/client"), { recursive: true });
  mkdirSync(join(root, "build/server"), { recursive: true });
  for (const module of [
    "browser-ready.ts",
    "evidence.ts",
    "nginx.ts",
    "process.ts",
    "redact.ts",
    "release.mjs",
    "runner-lifecycle.ts",
  ])
    copy(root, module);
  const source = readFileSync(production, "utf8");
  const marker = "const EXECUTION_DEADLINE_MS = 900_000;";
  if (source.split(marker).length !== 2)
    throw new Error("The production execution deadline constant changed");
  writeFileSync(
    join(root, "tools/browser.ts"),
    source.replace(marker, `const EXECUTION_DEADLINE_MS = ${deadlineMs};`),
  );
  copyFileSync(join(halo, "deploy/halo.nginx.conf"), join(root, "deploy/halo.nginx.conf"));
  symlinkSync(join(halo, "node_modules"), join(root, "node_modules"), "dir");

  writeFileSync(
    join(root, "playwright.config.mjs"),
    `import { defineConfig } from "@playwright/test";
export default defineConfig({
  testDir: "./specs",
  workers: 1,
  retries: 0,
  timeout: 60000,
  expect: { timeout: 5000 },
  reporter: [["line"], [process.env.HALO_REPORTER]],
});
`,
  );

  const cert = join(root, "fake-cert.pem");
  const key = join(root, "fake-key.pem");
  const generated = spawnSync(
    "openssl",
    ["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", key, "-out", cert, "-days", "1", "-subj", "/CN=127.0.0.1"],
    { stdio: "ignore", timeout: 10_000 },
  );
  if (generated.status !== 0) throw new Error("Fixture certificate generation failed");
  // The runner generates its own certificate; these standalone files only prove
  // the local toolchain is available for the same fixture step.

  writeFileSync(
    join(root, "fake-nginx.mjs"),
    `#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { createServer } from "node:https";
const args = process.argv.slice(2);
const config = readFileSync(args[args.indexOf("-c") + 1], "utf8");
if (args.includes("-t")) process.exit(0);
const port = Number(/listen\\s+127\\.0\\.0\\.1:(\\d+)\\s+ssl;/.exec(config)[1]);
const cert = readFileSync(/ssl_certificate\\s+([^;]+);/.exec(config)[1]);
const key = readFileSync(/ssl_certificate_key\\s+([^;]+);/.exec(config)[1]);
createServer({ cert, key }, (_request, response) => {
  response.writeHead(200, { "Content-Type": "application/json" });
  response.end("{}");
}).listen(port, "127.0.0.1");
`,
  );
  chmodSync(join(root, "fake-nginx.mjs"), 0o755);

  writeFileSync(
    join(root, "server.mjs"),
    `import { createServer } from "node:http";
const server = createServer((_request, response) => {
  response.writeHead(404);
  response.end();
});
server.listen(Number(process.env.HALO_PORT), "127.0.0.1", () =>
  process.stdout.write("halo_ready\\n"),
);
`,
  );
  writeFileSync(join(root, "package.json"), '{"name":"halo-browser-fixture","private":true}\n');
  writeFileSync(join(root, "bun.lock"), "\n");
  const buildId = "0123456789abcdef0123";
  writeFileSync(join(root, "build/client/asset.css"), "body{}\n");
  const digest = (file: string) =>
    createHash("sha256").update(readFileSync(join(root, file))).digest("hex");
  writeFileSync(
    join(root, "build/release.json"),
    `${JSON.stringify(
      {
        version: 1,
        buildId,
        resources: [
          {
            file: "asset.css",
            path: `/assets/${buildId}/asset.css`,
            mediaType: "text/css",
            sha256: digest("build/client/asset.css"),
          },
        ],
        server: [],
        runtime: ["bun.lock", "package.json", "server.mjs", "tools/release.mjs"].map(
          (file) => ({ file, sha256: digest(file) }),
        ),
      },
      null,
      2,
    )}\n`,
  );

  const evidence = join(root, "evidence");
  const material = join(root, "material");
  mkdirSync(material, { recursive: true });
  return {
    root,
    evidence,
    material,
    async run(spec: string) {
      writeFileSync(join(root, "specs/tiny.spec.mjs"), spec);
      return start({ root, evidence, material });
    },
  };
}

async function start(options: {
  root: string;
  evidence: string;
  material: string;
}): Promise<ChildProcess> {
  const port = await freePort();
  const mediaPort = await freePort();
  const child = spawn(process.execPath, ["tools/browser.ts"], {
    cwd: options.root,
    stdio: ["ignore", "pipe", "pipe"],
    env: {
      ...process.env,
      HALO_NGINX: join(options.root, "fake-nginx.mjs"),
      HALO_TEST_BACKEND: "http://127.0.0.1:9",
      HALO_TEST_PORT: String(port),
      HALO_TEST_MEDIA_PORT: String(mediaPort),
      HALO_AVATAR_ORIGIN: `https://127.0.0.1:${mediaPort}`,
      HALO_TEST_MATERIAL: options.material,
      HALO_TEST_EVIDENCE: options.evidence,
      HALO_REPORTER: reporter,
      HALO_MARKER: join(options.root, "marker"),
    },
  });
  runners.push(child);
  return child;
}

function exit(child: ChildProcess, timeoutMs: number): Promise<number | null> {
  return Promise.race([
    new Promise<number | null>((resolve) =>
      child.exitCode !== null || child.signalCode !== null
        ? resolve(child.exitCode)
        : child.once("exit", (code) => resolve(code)),
    ),
    delay(timeoutMs).then(() => {
      throw new Error("browser runner did not exit within the bound");
    }),
  ]);
}

async function waitFor<T>(
  operation: () => T | undefined | false,
  timeoutMs: number,
  label: string,
): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const value = operation();
    if (value) return value;
    if (Date.now() >= deadline) throw new Error(`Timed out waiting for ${label}`);
    await delay(25);
  }
}

function run(current: string) {
  const root = join(current, "evidence");
  const id = (JSON.parse(readFileSync(join(root, "current.json"), "utf8")) as { runId: string })
    .runId;
  const directory = join(root, "runs", id);
  return {
    directory,
    summary: () =>
      JSON.parse(readFileSync(join(directory, "summary.json"), "utf8")) as {
        ok: boolean;
        reasons: string[];
        cancellation: string | null;
        cleanup: { ok: boolean };
      },
    records: (file: string) =>
      readFileSync(join(directory, file), "utf8")
        .split("\n")
        .filter(Boolean)
        .map((line) => JSON.parse(line) as Record<string, unknown>),
    reporter: () => {
      const parsed = readJsonl(join(directory, "reporter.jsonl"), id);
      return summarizeReporter(parsed.records, id, parsed);
    },
  };
}

function dead(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return false;
  } catch (error) {
    return (error as NodeJS.ErrnoException).code === "ESRCH";
  }
}

function groupDead(pid: number): boolean {
  try {
    process.kill(-pid, 0);
    return false;
  } catch (error) {
    return (error as NodeJS.ErrnoException).code === "ESRCH";
  }
}

test("a successful tiny suite completes with clean runner and process evidence", async () => {
  const sample = fixture(660_000);
  const child = await sample.run(
    `import { test, expect } from "@playwright/test";
test("tiny", async () => { expect(2).toBe(2); });
`,
  );
  expect(await exit(child, 30_000)).toBe(0);
  const evidence = run(sample.root);
  const summary = evidence.summary();
  expect(summary.ok).toBe(true);
  expect(summary.cleanup.ok).toBe(true);
  const spawned = evidence
    .records("runner.jsonl")
    .filter((record) => record.event === "child_spawn");
  expect(spawned.length).toBeGreaterThanOrEqual(3);
  for (const record of spawned) expect(dead(record.pid as number)).toBe(true);
  for (const record of spawned.filter((entry) => entry.group === true))
    expect(groupDead(record.pid as number)).toBe(true);
  expect(evidence.reporter().counts.passed).toBe(1);
}, 40_000);

test("an original assertion failure survives a later hung test and termination", async () => {
  const sample = fixture(660_000);
  const child = await sample.run(
    `import { test, expect } from "@playwright/test";
import { writeFileSync } from "node:fs";
test("original assertion", async () => { expect(1).toBe(2); });
test("blocked later", async () => {
  writeFileSync(process.env.HALO_MARKER, "ready");
  await new Promise((resolve) => setTimeout(resolve, 120000));
});
test("never started", async () => {});
`,
  );
  await waitFor(
    () => existsSync(join(sample.root, "marker")),
    20_000,
    "the later test to start",
  );
  const before = run(sample.root).reporter();
  const failed = before.tests.find((entry) => entry.title === "original assertion");
  expect(failed?.lastStatus).toBe("failed");
  expect(failed?.attempts[0]?.errors[0]?.message).toContain("Expected: 2");
  expect(before.tests.find((entry) => entry.title === "blocked later")?.state).toBe(
    "interrupted_in_flight",
  );
  expect(before.tests.find((entry) => entry.title === "never started")?.state).toBe(
    "start_unknown",
  );
  child.kill("SIGTERM");
  const code = await exit(child, 20_000);
  expect(code).not.toBe(0);
  const after = run(sample.root);
  const summary = after.summary();
  expect(summary.ok).toBe(false);
  expect(summary.cancellation).toBe("sigterm");
  expect(summary.cleanup.ok).toBe(true);
  const report = after.reporter();
  expect(report.tests.find((entry) => entry.title === "original assertion")?.lastStatus).toBe(
    "failed",
  );
  expect(report.tests.find((entry) => entry.title === "blocked later")?.lastStatus).not.toBe(
    "passed",
  );
  expect(report.tests.find((entry) => entry.title === "never started")?.lastDuration).toBeNull();
  const spawned = after
    .records("runner.jsonl")
    .filter((entry) => entry.event === "child_spawn");
  for (const record of spawned)
    expect(dead(record.pid as number)).toBe(true);
  for (const record of spawned.filter((entry) => entry.group === true))
    expect(groupDead(record.pid as number)).toBe(true);
}, 40_000);

test.each([0, 2_000])("the short deadline cleans the runner with %i ms discovery delay", async (discoveryDelayMs) => {
  const sample = fixture(1_500);
  const child = await sample.run(
    `import { test } from "@playwright/test";
import { writeFileSync } from "node:fs";
await new Promise((resolve) => setTimeout(resolve, ${discoveryDelayMs}));
test("hangs beyond the budget", async () => {
  writeFileSync(process.env.HALO_MARKER, "ready");
  await new Promise((resolve) => setTimeout(resolve, 120000));
});
`,
  );
  const code = await exit(child, 30_000);
  expect(code).not.toBe(0);
  const evidence = run(sample.root);
  const summary = evidence.summary();
  expect(summary.cancellation).toBe("execution_deadline_1500ms");
  expect(summary.ok).toBe(false);
  expect(summary.cleanup.ok).toBe(true);
  const report = evidence.reporter();
  // The budget includes discovery; cancellation may precede the inventory or
  // arrive after Playwright has reported an interrupted test's end.
  expect(report.counts.passed).toBe(0);
  expect(report.counts.failed).toBe(0);
  expect(report.counts.timedOut).toBe(0);
  expect(report.globalErrors).toEqual([]);
  const started = existsSync(join(sample.root, "marker"));
  if (discoveryDelayMs > 0) {
    expect(started).toBe(false);
  }
  if (started) {
    expect(report.tests).toHaveLength(1);
    expect(report.tests[0]?.state).not.toBe("never_started");
    expect(report.counts.skipped).toBe(0);
  }
  for (const entry of report.tests) {
    if (entry.state === "completed")
      expect(["interrupted", "skipped"]).toContain(entry.lastStatus);
    else {
      expect(entry.lastStatus).toBeNull();
      expect(entry.lastDuration).toBeNull();
    }
  }
  const spawned = evidence
    .records("runner.jsonl")
    .filter((entry) => entry.event === "child_spawn");
  for (const record of spawned)
    expect(dead(record.pid as number)).toBe(true);
  for (const record of spawned.filter((entry) => entry.group === true))
    expect(groupDead(record.pid as number)).toBe(true);
}, 40_000);

test.each(["SIGINT", "SIGTERM"] as const)(
  "a parent %s reaches the real runner cleanup and leaves no owned process",
  async (signal) => {
    const sample = fixture(60_000);
    const child = await sample.run(
      `import { test } from "@playwright/test";
import { writeFileSync } from "node:fs";
test("blocks", async () => {
  writeFileSync(process.env.HALO_MARKER, "ready");
  await new Promise((resolve) => setTimeout(resolve, 120000));
});
`,
    );
    await waitFor(
      () => existsSync(join(sample.root, "marker")),
      20_000,
      "the test to start",
    );
    child.kill(signal);
    const code = await exit(child, 20_000);
    expect(code).not.toBe(0);
    const evidence = run(sample.root);
    const summary = evidence.summary();
    expect(summary.cancellation).toBe(signal === "SIGINT" ? "sigint" : "sigterm");
    expect(summary.cleanup.ok).toBe(true);
    const records = evidence.records("runner.jsonl");
    const cleanup = records.find((record) => record.event === "cleanup_end");
    expect(cleanup?.ok).toBe(true);
    const spawned = records.filter((entry) => entry.event === "child_spawn");
    for (const record of spawned)
      expect(dead(record.pid as number)).toBe(true);
    for (const record of spawned.filter((entry) => entry.group === true))
      expect(groupDead(record.pid as number)).toBe(true);
  },
  40_000,
);
