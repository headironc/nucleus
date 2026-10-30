// Persistent, run-bound browser evidence. Every write is synchronous so a
// failure or a later hang cannot hide an earlier event behind an end-of-run
// buffer. Read paths treat missing, malformed or foreign records as unknown
// rather than as success.

import { randomBytes } from "node:crypto";
import {
  closeSync,
  existsSync,
  mkdirSync,
  openSync,
  readFileSync,
  renameSync,
  rmSync,
  statSync,
  writeFileSync,
  writeSync,
} from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export const EVIDENCE_VERSION = 1;

export type EvidenceRecord = {
  version: number;
  runId: string;
  at: string;
  event: string;
  [key: string]: unknown;
};

export type EvidenceRun = {
  root: string;
  runId: string;
  dir: string;
  created: boolean;
};

export type EvidenceRead = {
  records: EvidenceRecord[];
  malformed: number;
  foreign: number;
  missing: boolean;
};

const RUN_ID = /^[A-Za-z0-9._-]{1,160}$/;

/** Reject identifiers that would alias the run root or escape its directory. */
export function validateRunId(id: string): string {
  if (!RUN_ID.test(id) || id === "." || id === "..")
    throw new Error("HALO_TEST_RUN_ID is not a safe run identifier");
  return id;
}

export function defaultEvidenceRoot(): string {
  const repository = fileURLToPath(new URL("../..", import.meta.url));
  return join(repository, "target", "halo-browser-evidence");
}

function createRunId(): string {
  const stamp = new Date().toISOString().replace(/[:.]/g, "-");
  return `${stamp}-${process.pid}-${randomBytes(4).toString("hex")}`;
}

function writeCurrent(root: string, runId: string): void {
  mkdirSync(root, { recursive: true });
  const temporary = join(
    root,
    `.current-${process.pid}-${randomBytes(4).toString("hex")}`,
  );
  writeFileSync(
    temporary,
    `${JSON.stringify({ runId, startedAt: new Date().toISOString(), pid: process.pid })}\n`,
  );
  renameSync(temporary, join(root, "current.json"));
}

function readCurrent(root: string): string | null {
  try {
    const parsed = JSON.parse(readFileSync(join(root, "current.json"), "utf8")) as {
      runId?: unknown;
    };
    return typeof parsed.runId === "string" ? parsed.runId : null;
  } catch {
    return null;
  }
}

/**
 * Initialize a brand-new parent run. The run directory is created without
 * recursion so two attempts can never share one binding; a preexisting
 * directory is rejected instead of being reused or replaced.
 */
export function createRun(
  options: {
    root?: string;
    runId?: string;
    environment?: NodeJS.ProcessEnv;
  } = {},
): EvidenceRun {
  const environment = options.environment ?? process.env;
  const root = options.root
    ? resolve(options.root)
    : environment.HALO_TEST_EVIDENCE
      ? resolve(environment.HALO_TEST_EVIDENCE)
      : defaultEvidenceRoot();
  const runId = validateRunId(
    options.runId ?? environment.HALO_TEST_RUN_ID ?? createRunId(),
  );
  mkdirSync(join(root, "runs"), { recursive: true });
  const dir = join(root, "runs", runId);
  try {
    mkdirSync(dir);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "EEXIST")
      throw new Error(`Browser evidence run ${runId} already exists`);
    throw error;
  }
  try {
    writeCurrent(root, runId);
  } catch (error) {
    rmSync(dir, { recursive: true, force: true });
    throw error;
  }
  return { root, runId, dir, created: true };
}

export function ensureRun(
  environment: NodeJS.ProcessEnv = process.env,
): EvidenceRun {
  if (environment.HALO_TEST_EVIDENCE_DIR !== undefined) {
    const suppliedRoot = environment.HALO_TEST_EVIDENCE;
    const suppliedRun = environment.HALO_TEST_RUN_ID;
    if (suppliedRoot === undefined || suppliedRun === undefined)
      throw new Error(
        "Nested browser evidence requires the current run root, identifier and directory",
      );
    const root = resolve(suppliedRoot);
    const runId = validateRunId(suppliedRun);
    const dir = resolve(environment.HALO_TEST_EVIDENCE_DIR);
    if (dir !== join(root, "runs", runId))
      throw new Error("Browser evidence directory is not the current run directory");
    if (!existsSync(dir) || !statSync(dir).isDirectory())
      throw new Error("Browser evidence run directory is missing");
    const current = readCurrent(root);
    if (current === null)
      throw new Error("Browser evidence current run binding is missing");
    if (current !== runId)
      throw new Error("Browser evidence is bound to a different current run");
    return { root, runId, dir, created: false };
  }
  return createRun({ environment });
}

export class EvidenceLog {
  private closed = false;
  private readonly run: EvidenceRun;
  readonly name: string;

  constructor(run: EvidenceRun, name: string) {
    this.run = run;
    this.name = name;
  }

  append(
    event: string,
    data: Record<string, unknown> = {},
  ): EvidenceRecord {
    if (this.closed) throw new Error(`Evidence log ${this.name} is closed`);
    const record: EvidenceRecord = {
      version: EVIDENCE_VERSION,
      runId: this.run.runId,
      at: new Date().toISOString(),
      event,
      ...data,
    };
    // One synchronous append per event. The line is visible to another reader
    // as soon as this returns, which is what makes a later hang diagnosable.
    // fsync is deliberately omitted: process loss does not discard written
    // pages, and per-event disk flushes would consume the execution budget.
    const fd = openSync(join(this.run.dir, this.name), "a");
    try {
      writeSync(fd, `${JSON.stringify(record)}\n`);
    } finally {
      closeSync(fd);
    }
    return record;
  }

  close(): void {
    this.closed = true;
  }
}

export function writeJsonAtomic(path: string, value: unknown): void {
  mkdirSync(dirname(path), { recursive: true });
  const temporary = `${path}.tmp-${process.pid}-${randomBytes(4).toString("hex")}`;
  writeFileSync(temporary, `${JSON.stringify(value, null, 2)}\n`);
  renameSync(temporary, path);
}

export function readJsonl(
  path: string,
  expectedRunId?: string,
): EvidenceRead {
  let content: string;
  try {
    content = readFileSync(path, "utf8");
  } catch {
    return { records: [], malformed: 0, foreign: 0, missing: true };
  }
  const records: EvidenceRecord[] = [];
  let malformed = 0;
  let foreign = 0;
  for (const line of content.split("\n")) {
    if (!line.trim()) continue;
    try {
      const parsed = JSON.parse(line) as EvidenceRecord;
      if (
        !parsed ||
        typeof parsed !== "object" ||
        typeof parsed.event !== "string" ||
        typeof parsed.runId !== "string" ||
        typeof parsed.at !== "string"
      ) {
        malformed += 1;
        continue;
      }
      if (expectedRunId !== undefined && parsed.runId !== expectedRunId) {
        foreign += 1;
        continue;
      }
      records.push(parsed);
    } catch {
      malformed += 1;
    }
  }
  return { records, malformed, foreign, missing: false };
}

export type SanitizedError = {
  message: string | null;
  stack: string | null;
  value: string | null;
  location: unknown;
  truncated: boolean;
};

export type InventoryTest = {
  id: string;
  title: string;
  titlePath: string[];
  file: string;
  line: number;
  expectedStatus: string;
  project: string | null;
};

export type AttemptSummary = {
  retry: number;
  state: "completed" | "interrupted_in_flight" | "never_started" | "start_unknown";
  status: string | null;
  duration: number | null;
  outcome: string | null;
  workerIndex: number | null;
  beganAt: string | null;
  endedAt: string | null;
  errors: SanitizedError[];
  attachments: Array<Record<string, unknown>>;
};

export type TestSummary = {
  id: string;
  title: string;
  titlePath: string[];
  file: string;
  line: number;
  expectedStatus: string;
  project: string | null;
  state: AttemptSummary["state"];
  attempts: AttemptSummary[];
  lastStatus: string | null;
  lastDuration: number | null;
  lastOutcome: string | null;
};

export type ReporterSummary = {
  runId: string | null;
  inventoryCount: number;
  config: Record<string, unknown> | null;
  tests: TestSummary[];
  counts: {
    completed: number;
    interruptedInFlight: number;
    neverStarted: number;
    startUnknown: number;
    passed: number;
    failed: number;
    timedOut: number;
    skipped: number;
    interrupted: number;
    expected: number;
    unexpected: number;
    flaky: number;
  };
  finalized: boolean;
  reportStatus: string | null;
  writeErrors: string[];
  globalErrors: SanitizedError[];
  malformed: number;
  foreign: number;
  undeclared: number;
  startedAt: string | null;
};

function inventoryFrom(record: EvidenceRecord): InventoryTest[] {
  const tests = record.tests;
  if (!Array.isArray(tests)) return [];
  return tests.filter((test): test is InventoryTest => {
    if (!test || typeof test !== "object") return false;
    const item = test as Record<string, unknown>;
    return (
      typeof item.id === "string" &&
      typeof item.title === "string" &&
      Array.isArray(item.titlePath) &&
      typeof item.file === "string" &&
      typeof item.line === "number" &&
      typeof item.expectedStatus === "string"
    );
  });
}

function errorsFrom(value: unknown): SanitizedError[] {
  if (!Array.isArray(value)) return [];
  return value.flatMap((error) => {
    if (!error || typeof error !== "object") return [];
    const item = error as Record<string, unknown>;
    return [
      {
        message: typeof item.message === "string" ? item.message : null,
        stack: typeof item.stack === "string" ? item.stack : null,
        value: typeof item.value === "string" ? item.value : null,
        location: item.location ?? null,
        truncated: item.truncated === true,
      },
    ];
  });
}

function attachmentsFrom(value: unknown): Array<Record<string, unknown>> {
  if (!Array.isArray(value)) return [];
  return value.flatMap((attachment) =>
    attachment && typeof attachment === "object"
      ? [attachment as Record<string, unknown>]
      : [],
  );
}

type AttemptEvents = {
  retry: number;
  beganAt: string | null;
  end: EvidenceRecord | null;
};

export function summarizeReporter(
  records: EvidenceRecord[],
  expectedRunId?: string,
  read: { malformed?: number; foreign?: number } = {},
): ReporterSummary {
  let inventoryRecord: EvidenceRecord | null = null;
  let endRecord: EvidenceRecord | null = null;
  const inventory: InventoryTest[] = [];
  const declared = new Set<string>();
  const attempts = new Map<string, Map<number, AttemptEvents>>();
  const globalErrors: SanitizedError[] = [];
  const writeErrors: string[] = [];
  let foreign = read.foreign ?? 0;
  let undeclared = 0;
  for (const record of records) {
    if (expectedRunId !== undefined && record.runId !== expectedRunId) {
      foreign += 1;
      continue;
    }
    if (record.event === "inventory") {
      if (!inventoryRecord) {
        inventoryRecord = record;
        for (const test of inventoryFrom(record)) {
          inventory.push(test);
          declared.add(test.id);
        }
      }
      continue;
    }
    if (record.event === "end") {
      endRecord = record;
      continue;
    }
    if (record.event === "write_error") {
      writeErrors.push(
        typeof record.message === "string" ? record.message : "unknown write error",
      );
      continue;
    }
    if (record.event === "error") {
      globalErrors.push(...errorsFrom([record]));
      continue;
    }
    if (record.event !== "test_begin" && record.event !== "test_end") continue;
    if (typeof record.id !== "string") continue;
    if (!declared.has(record.id)) {
      undeclared += 1;
      continue;
    }
    const retry = typeof record.retry === "number" ? record.retry : 0;
    let byRetry = attempts.get(record.id);
    if (!byRetry) {
      byRetry = new Map();
      attempts.set(record.id, byRetry);
    }
    let attempt = byRetry.get(retry);
    if (!attempt) {
      attempt = { retry, beganAt: null, end: null };
      byRetry.set(retry, attempt);
    }
    if (record.event === "test_begin") {
      // Playwright can emit a synthetic begin for an unassigned test.
      if (record.workerIndex !== -1) attempt.beganAt = record.at;
    } else {
      attempt.end = record;
    }
  }

  const finalized = Boolean(endRecord) && writeErrors.length === 0 &&
    foreign === 0 && undeclared === 0 && (read.malformed ?? 0) === 0;
  // Missing IPC is not proof that a worker never started a test.
  const unobserved: AttemptSummary["state"] = finalized ? "never_started" : "start_unknown";
  const tests: TestSummary[] = inventory.map((test) => {
    const byRetry = attempts.get(test.id) ?? new Map<number, AttemptEvents>();
    const summaries: AttemptSummary[] = [...byRetry.values()]
      .sort((left, right) => left.retry - right.retry)
      .map((attempt) => {
        const end = attempt.end;
        const workerIndex =
          end && typeof end.workerIndex === "number" ? end.workerIndex : null;
        const placeholder =
          Boolean(end) && !attempt.beganAt && workerIndex === -1;
        let state: AttemptSummary["state"];
        if (placeholder) state = unobserved;
        else if (end) state = "completed";
        else if (attempt.beganAt) state = "interrupted_in_flight";
        else state = unobserved;
        return {
          retry: attempt.retry,
          state,
          status:
            end && !placeholder && typeof end.status === "string"
              ? end.status
              : null,
          duration:
            end && !placeholder && typeof end.duration === "number"
              ? end.duration
              : null,
          outcome:
            end && !placeholder && typeof end.outcome === "string"
              ? end.outcome
              : null,
          workerIndex,
          beganAt: attempt.beganAt,
          endedAt: end && !placeholder ? end.at : null,
          errors: end ? errorsFrom(end.errors) : [],
          attachments: end ? attachmentsFrom(end.attachments) : [],
        };
      });
    const completed = summaries.filter((attempt) => attempt.state === "completed");
    const inFlight = summaries.some(
      (attempt) => attempt.state === "interrupted_in_flight",
    );
    let state: TestSummary["state"];
    if (inFlight) state = "interrupted_in_flight";
    else if (summaries.some((attempt) => attempt.state === "start_unknown"))
      state = "start_unknown";
    else if (completed.length > 0) state = "completed";
    else state = unobserved;
    // Only a completed test exposes a final status/duration. An in-flight
    // attempt keeps its earlier actual attempts visible without presenting an
    // earlier duration as the current one.
    const last = state === "completed" ? (completed.at(-1) ?? null) : null;
    return {
      ...test,
      state,
      attempts: summaries,
      lastStatus: last?.status ?? null,
      lastDuration: last?.duration ?? null,
      lastOutcome: last?.outcome ?? null,
    };
  });

  const counts = {
    completed: 0,
    interruptedInFlight: 0,
    neverStarted: 0,
    startUnknown: 0,
    passed: 0,
    failed: 0,
    timedOut: 0,
    skipped: 0,
    interrupted: 0,
    expected: 0,
    unexpected: 0,
    flaky: 0,
  };
  for (const test of tests) {
    if (test.state === "completed") counts.completed += 1;
    else if (test.state === "interrupted_in_flight")
      counts.interruptedInFlight += 1;
    else if (test.state === "never_started") counts.neverStarted += 1;
    else counts.startUnknown += 1;
    if (test.lastStatus === "passed") counts.passed += 1;
    else if (test.lastStatus === "failed") counts.failed += 1;
    else if (test.lastStatus === "timedOut") counts.timedOut += 1;
    else if (test.lastStatus === "skipped") counts.skipped += 1;
    else if (test.lastStatus === "interrupted") counts.interrupted += 1;
    if (test.lastOutcome === "expected") counts.expected += 1;
    else if (test.lastOutcome === "unexpected") counts.unexpected += 1;
    else if (test.lastOutcome === "flaky") counts.flaky += 1;
  }

  return {
    runId: inventoryRecord?.runId ?? endRecord?.runId ?? null,
    inventoryCount: inventory.length,
    config:
      inventoryRecord && typeof inventoryRecord.config === "object"
        ? (inventoryRecord.config as Record<string, unknown>)
        : null,
    tests,
    counts,
    finalized,
    reportStatus:
      endRecord && typeof endRecord.status === "string"
        ? endRecord.status
        : null,
    writeErrors,
    globalErrors,
    malformed: read.malformed ?? 0,
    foreign,
    undeclared,
    startedAt:
      inventoryRecord && typeof inventoryRecord.at === "string"
        ? inventoryRecord.at
        : null,
  };
}

export type ReportVerification = { ok: boolean; reasons: string[] };

export function verifyReporter(
  summary: ReporterSummary,
  options: { childExitCleanly: boolean },
): ReportVerification {
  const reasons: string[] = [];
  if (!summary.finalized)
    reasons.push("Playwright report is missing a final record or has write/read errors");
  if (summary.inventoryCount === 0)
    reasons.push("Playwright recorded no selected test inventory");
  for (const test of summary.tests) {
    if (test.state === "interrupted_in_flight")
      reasons.push(`${test.titlePath.join(" › ")} was still in flight when the report ended`);
    else if (test.state === "never_started")
      reasons.push(`${test.titlePath.join(" › ")} never started`);
    else if (test.state === "start_unknown")
      reasons.push(`${test.titlePath.join(" › ")} has unknown start state because the report is incomplete`);
    else if (test.lastOutcome === "unexpected")
      reasons.push(`${test.titlePath.join(" › ")} ended unexpectedly (${test.lastStatus ?? "unknown"})`);
    else if (test.lastOutcome === "flaky" && summary.config?.failOnFlakyTests === true)
      reasons.push(`${test.titlePath.join(" › ")} is flaky and the run rejects flaky tests`);
  }
  if (summary.reportStatus !== "passed")
    reasons.push(`Playwright report status is ${summary.reportStatus ?? "missing"}`);
  if (!options.childExitCleanly)
    reasons.push("Playwright process did not exit cleanly");
  return { ok: reasons.length === 0, reasons };
}
