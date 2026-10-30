import { afterEach, expect, test } from "vitest";
import { spawn, type ChildProcess } from "node:child_process";
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { readJsonl, summarizeReporter } from "./evidence";

const halo = fileURLToPath(new URL("..", import.meta.url));
const cli = fileURLToPath(
  new URL("../node_modules/@playwright/test/cli.js", import.meta.url),
);
const reporter = fileURLToPath(new URL("./reporter.ts", import.meta.url));
const directories: string[] = [];
const children: ChildProcess[] = [];

afterEach(() => {
  for (const child of children.splice(0)) {
    try {
      if (child.pid) process.kill(-child.pid, "SIGKILL");
    } catch {
      // Already gone.
    }
  }
  for (const directory of directories.splice(0))
    rmSync(directory, { recursive: true, force: true });
});

function fixture(
  spec: string,
  extra: Record<string, string> = {},
  options: {
    runId?: string;
    prepare?: (evidence: string, runId: string) => Record<string, string> | void;
  } = {},
) {
  const directory = mkdtempSync(join(halo, ".reporter-test-"));
  directories.push(directory);
  writeFileSync(
    join(directory, "playwright.config.mjs"),
    `import { defineConfig } from "@playwright/test";
export default defineConfig({
  testDir: ".",
  testMatch: "*.spec.mjs",
  workers: 1,
  retries: 0,
  timeout: 60000,
  reporter: [["list"], [process.env.REPORTER_PATH]],
});
`,
  );
  writeFileSync(join(directory, "case.spec.mjs"), spec);
  const evidence = join(directory, "evidence");
  const runId = options.runId ?? `reporter-${process.pid}-${Date.now()}`;
  const prepared = options.prepare?.(evidence, runId) ?? {};
  const child = spawn(process.execPath, [cli, "test", "-c", "playwright.config.mjs"], {
    cwd: directory,
    detached: true,
    stdio: ["ignore", "pipe", "pipe"],
    env: {
      ...process.env,
      REPORTER_PATH: reporter,
      HALO_TEST_EVIDENCE: evidence,
      HALO_TEST_RUN_ID: runId,
      HALO_READY: join(directory, "ready"),
      ...prepared,
      ...extra,
    },
  });
  children.push(child);
  let output = "";
  child.stdout?.on("data", (chunk: Buffer) => {
    output += String(chunk);
  });
  child.stderr?.on("data", (chunk: Buffer) => {
    output += String(chunk);
  });
  return {
    directory,
    evidence,
    runId,
    child,
    output: () => output,
    reporterPath: join(evidence, "runs", runId, "reporter.jsonl"),
  };
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

function exit(child: ChildProcess): Promise<{
  code: number | null;
  signal: NodeJS.Signals | null;
}> {
  if (child.exitCode !== null || child.signalCode !== null)
    return Promise.resolve({ code: child.exitCode, signal: child.signalCode });
  return new Promise((resolve) =>
    child.once("exit", (code, signal) => resolve({ code, signal })),
  );
}

function read(runId: string, path: string) {
  const result = readJsonl(path, runId);
  return { ...result, summary: summarizeReporter(result.records, runId, result) };
}

test("an original assertion failure is on disk while a later test is still blocked", async () => {
  const sample = fixture(`import { test, expect } from "@playwright/test";
import { writeFileSync } from "node:fs";
test("original assertion", async () => {
  expect(1).toBe(2);
});
test("blocked later", async () => {
  writeFileSync(process.env.HALO_READY, "ready");
  await new Promise((resolve) => setTimeout(resolve, 120000));
});
test("never started", async () => {});
`);
  try {
    await waitFor(
      () => existsSync(join(sample.directory, "ready")),
      30_000,
      "the blocked test to start",
    );
    const before = read(sample.runId, sample.reporterPath);
    expect(before.summary.inventoryCount).toBe(3);
    expect(before.summary.finalized).toBe(false);
    expect(before.summary.reportStatus).toBeNull();
    const failed = before.summary.tests.find(
      (test) => test.title === "original assertion",
    );
    expect(failed?.state).toBe("completed");
    expect(failed?.lastStatus).toBe("failed");
    expect(failed?.lastDuration).toBeTypeOf("number");
    const message = failed?.attempts[0]?.errors[0]?.message ?? "";
    expect(message).toContain("Expected: 2");
    expect(message).toContain("Received: 1");
    expect(failed?.attempts[0]?.errors[0]?.stack).toContain("case.spec.mjs");
    const blocked = before.summary.tests.find(
      (test) => test.title === "blocked later",
    );
    expect(blocked?.state).toBe("interrupted_in_flight");
    expect(blocked?.lastDuration).toBeNull();
    const never = before.summary.tests.find(
      (test) => test.title === "never started",
    );
    expect(never?.state).toBe("start_unknown");
    expect(never?.lastDuration).toBeNull();

    // Simulate a lost process: the evidence must still describe the failure.
    process.kill(-sample.child.pid!, "SIGKILL");
    const ended = await exit(sample.child);
    expect(ended.code === 0 && ended.signal === null).toBe(false);
    const after = read(sample.runId, sample.reporterPath);
    expect(after.summary.finalized).toBe(false);
    expect(after.summary.tests.find((test) => test.title === "never started"))
      .toMatchObject({ state: "start_unknown", lastStatus: null, lastDuration: null });
    expect(
      after.summary.tests.find((test) => test.title === "original assertion")
        ?.lastStatus,
    ).toBe("failed");
  } finally {
    // The child is stopped by afterEach; nothing else to release here.
  }
}, 60_000);

test.skipIf(
  typeof process.getuid === "function" && process.getuid() === 0,
)("a report write failure cannot make the acceptance green", async () => {
  const runId = `readonly-${process.pid}-${Date.now()}`;
  let runDirectory = "";
  const sample = fixture(
    `import { test } from "@playwright/test";
test("would pass", async () => {});
`,
    {},
    {
      runId,
      prepare(evidence, id) {
        runDirectory = join(evidence, "runs", id);
        mkdirSync(runDirectory, { recursive: true });
        writeFileSync(
          join(evidence, "current.json"),
          JSON.stringify({ runId: id, startedAt: new Date().toISOString(), pid: process.pid }),
        );
        chmodSync(runDirectory, 0o500);
        return { HALO_TEST_EVIDENCE_DIR: runDirectory };
      },
    },
  );
  try {
    const ended = await Promise.race([
      exit(sample.child),
      delay(60_000).then(() => {
        throw new Error("Playwright did not finish");
      }),
    ]);
    // Playwright catches reporter exceptions but marks the run failed; the
    // missing report also prevents any parent from accepting it.
    expect(ended.code === 0 && ended.signal === null).toBe(false);
    expect(existsSync(sample.reporterPath)).toBe(false);
  } finally {
    if (runDirectory) chmodSync(runDirectory, 0o700);
  }
}, 60_000);

test("a graceful termination keeps the failure and never invents a pass", async () => {
  const sample = fixture(`import { test, expect } from "@playwright/test";
import { writeFileSync } from "node:fs";
test("original assertion", async () => {
  expect(1).toBe(2);
});
test("blocked later", async () => {
  writeFileSync(process.env.HALO_READY, "ready");
  await new Promise((resolve) => setTimeout(resolve, 120000));
});
test("never started", async () => {});
`);
  await waitFor(
    () => existsSync(join(sample.directory, "ready")),
    30_000,
    "the blocked test to start",
  );
  sample.child.kill("SIGINT");
  let ended = await Promise.race([
    exit(sample.child),
    delay(15_000).then(() => null),
  ]);
  if (!ended) {
    process.kill(-sample.child.pid!, "SIGKILL");
    ended = await exit(sample.child);
  }
  expect(ended.code === 0 && ended.signal === null).toBe(false);
  const after = read(sample.runId, sample.reporterPath);
  expect(after.summary.inventoryCount).toBe(3);
  const failed = after.summary.tests.find(
    (test) => test.title === "original assertion",
  );
  expect(failed?.lastStatus).toBe("failed");
  const blocked = after.summary.tests.find(
    (test) => test.title === "blocked later",
  );
  expect(blocked?.lastStatus).not.toBe("passed");
  if (blocked?.state === "completed")
    expect(blocked.lastStatus).toBe("interrupted");
  else expect(blocked?.state).toBe("interrupted_in_flight");
  const never = after.summary.tests.find(
    (test) => test.title === "never started",
  );
  expect(never?.state).toBe("never_started");
  expect(never?.lastDuration).toBeNull();
  expect(after.summary.finalized).toBe(true);
  expect(after.summary.reportStatus).toBe("interrupted");
}, 60_000);

test("a scalar Set-Cookie assertion is sanitized before reporter persistence", async () => {
  const sample = fixture(`import { test, expect } from "@playwright/test";
test("unexpected response cookie", () => {
  const cookie = "__Host-nucleus-session-example=" + ["opaque", "scalar", "credential"].join("-") + "; Path=/; Secure; HttpOnly";
  expect(cookie).toBeUndefined();
});
`);
  const ended = await exit(sample.child);
  expect(ended.code).not.toBe(0);
  const raw = readFileSync(sample.reporterPath, "utf8");
  expect(raw).not.toContain("opaque-scalar-credential");
  const after = read(sample.runId, sample.reporterPath);
  expect(after.summary.finalized).toBe(true);
  expect(after.summary.tests[0]?.lastStatus).toBe("failed");
  expect(after.summary.tests[0]?.attempts[0]?.errors[0]?.message).toContain("toBeUndefined");
  expect(after.summary.tests[0]?.attempts[0]?.errors[0]?.message).toContain("<redacted>");
}, 30_000);

test("a completed run keeps every status and the earlier failure after finalization", async () => {
  const sample = fixture(`import { test, expect } from "@playwright/test";
test("original assertion", async () => {
  expect("actual").toBe("expected");
});
test("later passes", async () => {
  expect(2).toBe(2);
});
`);
  const ended = await Promise.race([
    exit(sample.child),
    delay(60_000).then(() => {
      throw new Error("Playwright did not finish");
    }),
  ]);
  expect(ended.code).toBe(1);
  expect(ended.signal).toBeNull();
  const after = read(sample.runId, sample.reporterPath);
  expect(after.summary.finalized).toBe(true);
  expect(after.summary.reportStatus).toBe("failed");
  expect(after.summary.counts.completed).toBe(2);
  const failed = after.summary.tests.find(
    (test) => test.title === "original assertion",
  );
  expect(failed?.lastStatus).toBe("failed");
  expect(failed?.attempts[0]?.errors[0]?.message).toContain(
    'Expected: "expected"',
  );
  expect(
    after.summary.tests.find((test) => test.title === "later passes")
      ?.lastStatus,
  ).toBe("passed");
  // The report file is the actual Playwright integration, not fabricated data.
  expect(readFileSync(sample.reporterPath, "utf8")).toContain('"event":"test_end"');
}, 60_000);
