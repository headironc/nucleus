import { afterEach, expect, test } from "vitest";
import {
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  EvidenceLog,
  createRun,
  ensureRun,
  readJsonl,
  summarizeReporter,
  verifyReporter,
  type EvidenceRecord,
} from "./evidence";

const directories: string[] = [];
afterEach(() => {
  for (const directory of directories.splice(0))
    rmSync(directory, { recursive: true, force: true });
});

function root(): string {
  const directory = mkdtempSync(join(tmpdir(), "halo evidence "));
  directories.push(directory);
  return directory;
}

function record(
  runId: string,
  event: string,
  data: Record<string, unknown> = {},
): EvidenceRecord {
  return {
    version: 1,
    runId,
    at: new Date().toISOString(),
    event,
    ...data,
  };
}

function inventory(
  runId: string,
  tests: Array<{ id: string; expectedStatus?: string }>,
  failOnFlakyTests = false,
): EvidenceRecord {
  return record(runId, "inventory", {
    config: { failOnFlakyTests },
    tests: tests.map((test) => ({
      id: test.id,
      title: test.id,
      titlePath: ["site.spec.ts", test.id],
      file: "/repo/halo/tests/site.spec.ts",
      line: 10,
      expectedStatus: test.expectedStatus ?? "passed",
      project: null,
    })),
  });
}

test("a unique run binding is visible and every appended event is readable at once", () => {
  const directory = root();
  const run = ensureRun({
    HALO_TEST_EVIDENCE: directory,
    HALO_TEST_RUN_ID: "run-1",
  } as NodeJS.ProcessEnv);
  expect(run.created).toBe(true);
  expect(JSON.parse(readFileSync(join(directory, "current.json"), "utf8"))).toMatchObject({
    runId: "run-1",
  });
  const log = new EvidenceLog(run, "reporter.jsonl");
  log.append("inventory", { tests: [] });
  // Read before the writer is closed or the process exits.
  expect(readFileSync(join(run.dir, "reporter.jsonl"), "utf8")).toContain(
    '"event":"inventory"',
  );
  log.append("test_begin", { id: "a", retry: 0 });
  expect(readJsonl(join(run.dir, "reporter.jsonl"), "run-1").records).toHaveLength(2);
});

test("a fresh run never reuses an existing run directory", () => {
  const directory = root();
  createRun({ root: directory, runId: "run-once" });
  expect(() => createRun({ root: directory, runId: "run-once" })).toThrow(
    "already exists",
  );
  // The failed attempt must not replace the original current binding.
  expect(
    JSON.parse(readFileSync(join(directory, "current.json"), "utf8")),
  ).toMatchObject({ runId: "run-once" });
});

test.each([".", "..", "a/b", ""])("unsafe run identifiers are rejected: %j", (id) => {
  const directory = root();
  expect(() => createRun({ root: directory, runId: id })).toThrow(
    "not a safe run identifier",
  );
});

test("nested consumers reuse the explicit binding without rewriting it", () => {
  const directory = root();
  const run = createRun({ root: directory, runId: "parent" });
  const binding = JSON.parse(readFileSync(join(directory, "current.json"), "utf8"));
  binding.pid = 1;
  writeFileSync(join(directory, "current.json"), JSON.stringify(binding));
  const nested = ensureRun({
    HALO_TEST_EVIDENCE: directory,
    HALO_TEST_RUN_ID: "parent",
    HALO_TEST_EVIDENCE_DIR: run.dir,
  } as NodeJS.ProcessEnv);
  expect(nested.created).toBe(false);
  expect(JSON.parse(readFileSync(join(directory, "current.json"), "utf8")).pid).toBe(1);
});

test("nested consumers reject a foreign binding, missing directory or partial configuration", () => {
  const directory = root();
  const run = createRun({ root: directory, runId: "parent" });
  createRun({ root: directory, runId: "other" });
  expect(() =>
    ensureRun({
      HALO_TEST_EVIDENCE: directory,
      HALO_TEST_RUN_ID: "parent",
      HALO_TEST_EVIDENCE_DIR: run.dir,
    } as NodeJS.ProcessEnv),
  ).toThrow("different current run");
  expect(() =>
    ensureRun({
      HALO_TEST_EVIDENCE: directory,
      HALO_TEST_RUN_ID: "parent",
    } as NodeJS.ProcessEnv),
  ).toThrow("already exists");
  expect(() =>
    ensureRun({
      HALO_TEST_EVIDENCE_DIR: join(directory, "runs", "parent"),
    } as NodeJS.ProcessEnv),
  ).toThrow("current run root");
  expect(() =>
    ensureRun({
      HALO_TEST_EVIDENCE: directory,
      HALO_TEST_RUN_ID: "parent",
      HALO_TEST_EVIDENCE_DIR: join(directory, "runs", "missing"),
    } as NodeJS.ProcessEnv),
  ).toThrow("not the current run directory");
});

test("malformed and foreign records are counted rather than accepted", () => {
  const directory = root();
  const run = ensureRun({
    HALO_TEST_EVIDENCE: directory,
    HALO_TEST_RUN_ID: "run-2",
  } as NodeJS.ProcessEnv);
  const path = join(run.dir, "reporter.jsonl");
  writeFileSync(
    path,
    `${JSON.stringify(inventory("run-2", [{ id: "a" }]))}\nnot-json\n${JSON.stringify(
      inventory("other-run", [{ id: "b" }]),
    )}\n`,
  );
  const read = readJsonl(path, "run-2");
  expect(read.malformed).toBe(1);
  expect(read.foreign).toBe(1);
  const summary = summarizeReporter(read.records, "run-2", read);
  expect(summary.inventoryCount).toBe(1);
  expect(summary.finalized).toBe(false);
  expect(summary.malformed).toBe(1);
  expect(summary.foreign).toBe(1);
});

test("a completed failure keeps its status, duration and original assertion", () => {
  const runId = "run-3";
  const records = [
    inventory(runId, [{ id: "fails" }, { id: "later" }]),
    record(runId, "test_begin", { id: "fails", retry: 0, workerIndex: 0 }),
    record(runId, "test_end", {
      id: "fails",
      retry: 0,
      status: "failed",
      duration: 1234,
      expectedStatus: "passed",
      outcome: "unexpected",
      ok: false,
      workerIndex: 0,
      errors: [
        {
          message: "expect(received).toBe(expected)\n\nExpected: 2\nReceived: 1",
          stack: "at site.spec.ts:1390:5",
          location: null,
          truncated: false,
        },
      ],
      attachments: [{ name: "screenshot", contentType: "image/png", path: "/tmp/shot.png", hasBody: false, bytes: null }],
    }),
    record(runId, "test_begin", { id: "later", retry: 0, workerIndex: 0 }),
    record(runId, "test_end", {
      id: "later",
      retry: 0,
      status: "passed",
      duration: 25,
      expectedStatus: "passed",
      outcome: "expected",
      ok: true,
      workerIndex: 0,
      errors: [],
      attachments: [],
    }),
    record(runId, "end", { status: "failed", duration: 1300 }),
  ];
  const summary = summarizeReporter(records, runId);
  expect(summary.finalized).toBe(true);
  expect(summary.counts.completed).toBe(2);
  expect(summary.counts.neverStarted).toBe(0);
  expect(summary.counts.startUnknown).toBe(0);
  expect(summary.tests[0]?.lastStatus).toBe("failed");
  expect(summary.tests[0]?.lastDuration).toBe(1234);
  expect(summary.tests[0]?.attempts[0]?.errors[0]?.message).toContain(
    "Expected: 2",
  );
  expect(summary.tests[0]?.attempts[0]?.attachments[0]?.path).toBe(
    "/tmp/shot.png",
  );
  const verification = verifyReporter(summary, { childExitCleanly: false });
  expect(verification.ok).toBe(false);
  expect(verification.reasons.join("\n")).toContain("fails ended unexpectedly");
});

test("in-flight and never-started tests have no fabricated status or duration", () => {
  const runId = "run-4";
  const records = [
    inventory(runId, [{ id: "blocked" }, { id: "never" }]),
    record(runId, "test_begin", { id: "blocked", retry: 0, workerIndex: 0 }),
    record(runId, "end", { status: "interrupted", duration: 10 }),
  ];
  const summary = summarizeReporter(records, runId);
  expect(summary.finalized).toBe(true);
  expect(summary.counts.interruptedInFlight).toBe(1);
  expect(summary.counts.neverStarted).toBe(1);
  expect(summary.counts.passed).toBe(0);
  expect(summary.tests[0]?.state).toBe("interrupted_in_flight");
  expect(summary.tests[0]?.lastStatus).toBeNull();
  expect(summary.tests[0]?.lastDuration).toBeNull();
  expect(summary.tests[1]?.state).toBe("never_started");
  expect(JSON.stringify(summary.tests[0])).not.toContain('"duration":0');
  const verification = verifyReporter(summary, { childExitCleanly: false });
  expect(verification.reasons).toEqual(
    expect.arrayContaining([
      expect.stringContaining("still in flight"),
      expect.stringContaining("never started"),
    ]),
  );
});

test("missing start evidence stays unknown until the report is complete", () => {
  const runId = "unknown-start";
  const summary = summarizeReporter([
    inventory(runId, [{ id: "possibly-running" }]),
  ], runId);
  expect(summary.finalized).toBe(false);
  expect(summary.tests[0]?.state).toBe("start_unknown");
  expect(summary.tests[0]?.lastStatus).toBeNull();
  expect(summary.tests[0]?.lastDuration).toBeNull();
  expect(summary.counts.neverStarted).toBe(0);
  expect(summary.counts.startUnknown).toBe(1);
  expect(summary.counts.passed).toBe(0);
  expect(verifyReporter(summary, { childExitCleanly: false }).ok).toBe(false);
});

test("Playwright's workerIndex -1 placeholder is not a skipped or passed completion", () => {
  const runId = "run-5";
  const records = [
    inventory(runId, [{ id: "unrun" }]),
    record(runId, "test_begin", { id: "unrun", retry: 0, workerIndex: -1 }),
    record(runId, "test_end", {
      id: "unrun",
      retry: 0,
      status: "skipped",
      duration: 0,
      expectedStatus: "passed",
      outcome: "skipped",
      workerIndex: -1,
      errors: [],
      attachments: [],
    }),
    record(runId, "end", { status: "interrupted", duration: 5 }),
  ];
  const summary = summarizeReporter(records, runId);
  expect(summary.tests[0]?.state).toBe("never_started");
  expect(summary.counts.skipped).toBe(0);
  expect(summary.tests[0]?.lastDuration).toBeNull();
});

test("retries keep every attempt and verify the final outcome", () => {
  const runId = "run-6";
  const records = [
    inventory(runId, [{ id: "flaky" }]),
    record(runId, "test_begin", { id: "flaky", retry: 0 }),
    record(runId, "test_end", {
      id: "flaky",
      retry: 0,
      status: "failed",
      duration: 5,
      expectedStatus: "passed",
      outcome: "unexpected",
      workerIndex: 0,
      errors: [{ message: "first attempt", stack: null, value: null, location: null, truncated: false }],
      attachments: [],
    }),
    record(runId, "test_begin", { id: "flaky", retry: 1 }),
    record(runId, "test_end", {
      id: "flaky",
      retry: 1,
      status: "passed",
      duration: 7,
      expectedStatus: "passed",
      outcome: "flaky",
      workerIndex: 0,
      errors: [],
      attachments: [],
    }),
    record(runId, "end", { status: "passed", duration: 12 }),
  ];
  const summary = summarizeReporter(records, runId);
  expect(summary.tests[0]?.attempts).toHaveLength(2);
  expect(summary.tests[0]?.attempts[0]?.status).toBe("failed");
  expect(summary.tests[0]?.attempts[1]?.status).toBe("passed");
  expect(summary.tests[0]?.lastOutcome).toBe("flaky");
  expect(verifyReporter(summary, { childExitCleanly: true }).ok).toBe(true);
});

test("an expected failure is a passing run without hiding the failure", () => {
  const runId = "run-7";
  const records = [
    inventory(runId, [{ id: "known", expectedStatus: "failed" }]),
    record(runId, "test_begin", { id: "known", retry: 0 }),
    record(runId, "test_end", {
      id: "known",
      retry: 0,
      status: "failed",
      duration: 9,
      expectedStatus: "failed",
      outcome: "expected",
      workerIndex: 0,
      errors: [{ message: "known failure", stack: null, value: null, location: null, truncated: false }],
      attachments: [],
    }),
    record(runId, "end", { status: "passed", duration: 10 }),
  ];
  const summary = summarizeReporter(records, runId);
  expect(summary.tests[0]?.lastStatus).toBe("failed");
  expect(summary.tests[0]?.lastOutcome).toBe("expected");
  expect(verifyReporter(summary, { childExitCleanly: true }).ok).toBe(true);
});

test("a reporter write error makes the report unfinished", () => {
  const runId = "run-8";
  const records = [
    inventory(runId, [{ id: "a" }]),
    record(runId, "test_begin", { id: "a", retry: 0 }),
    record(runId, "test_end", {
      id: "a",
      retry: 0,
      status: "passed",
      duration: 1,
      expectedStatus: "passed",
      outcome: "expected",
      workerIndex: 0,
      errors: [],
      attachments: [],
    }),
    record(runId, "write_error", { message: "disk full" }),
    record(runId, "end", { status: "passed", duration: 2 }),
  ];
  const summary = summarizeReporter(records, runId);
  expect(summary.finalized).toBe(false);
  const verification = verifyReporter(summary, { childExitCleanly: true });
  expect(verification.ok).toBe(false);
  expect(verification.reasons.join("\n")).toContain("missing a final record");
});
