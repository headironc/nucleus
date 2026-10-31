import { spawn, type ChildProcess } from "node:child_process";
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import {
  createRun,
  readJsonl,
  summarizeReporter,
  verifyReporter,
  writeJsonAtomic,
  type EvidenceRecord,
  type EvidenceRun,
} from "../halo/tools/evidence";
import {
  browserScope,
  verifyBrowserScope,
  type BrowserScope,
} from "../halo/tools/browser-scope.ts";
import { groupAlive } from "../halo/tools/process";
import { OutputCapture } from "./check-halo-output";

const SIGNAL_GRACE_MS = 60_000;
const KILL_GRACE_MS = 10_000;
const STREAM_CLOSE_MS = 2_000;
const SETTLE_POLL_MS = 50;

type Observation = {
  observed: boolean;
  code: number | null;
  signal: NodeJS.Signals | null;
  error: string | null;
};

type Failure = { kind: string; message: string };

function usage(): never {
  throw new Error("Usage: bun tools/check-halo-test.ts snapshot | browser");
}

function safeWrite(target: NodeJS.WriteStream, chunk: Buffer | string): void {
  try {
    target.write(chunk);
  } catch {
    // A closed consumer must not change the child's outcome.
  }
}

function observeExit(child: ChildProcess): Promise<Observation> {
  if (child.exitCode !== null || child.signalCode !== null)
    return Promise.resolve({
      observed: true,
      code: child.exitCode,
      signal: child.signalCode,
      error: null,
    });
  return new Promise((resolve) => {
    let settled = false;
    const finish = (observation: Observation) => {
      if (settled) return;
      settled = true;
      child.off("exit", onExit);
      child.off("error", onError);
      resolve(observation);
    };
    const onExit = (code: number | null, signal: NodeJS.Signals | null) =>
      finish({ observed: true, code, signal, error: null });
    const onError = (error: Error) =>
      finish({ observed: false, code: null, signal: null, error: error.message });
    child.once("exit", onExit);
    child.once("error", onError);
  });
}

function last(records: EvidenceRecord[], event: string): EvidenceRecord | null {
  for (let index = records.length - 1; index >= 0; index -= 1)
    if (records[index]!.event === event) return records[index]!;
  return null;
}

function runnerPid(run: EvidenceRun): number | null {
  const record = last(
    readJsonl(join(run.dir, "runner.jsonl"), run.runId).records,
    "run_start",
  );
  return record && typeof record.pid === "number" ? record.pid : null;
}

function commandLine(pid: number): string | null {
  try {
    return readFileSync(`/proc/${pid}/cmdline`, "utf8").replaceAll("\0", " ");
  } catch {
    return null;
  }
}

function alive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return (error as NodeJS.ErrnoException).code !== "ESRCH";
  }
}

function runnerAlive(run: EvidenceRun): number | null {
  const pid = runnerPid(run);
  if (pid === null || !alive(pid)) return null;
  if (process.platform === "linux" && !stillTagged(pid, run)) return null;
  const command = commandLine(pid);
  // The real runner is `node .../tools/browser.ts`; the bounded test fixture
  // uses a file with `browser` in its name. An unidentifiable process is not
  // signalled, which keeps a reused PID from receiving this attempt's signal.
  if (command !== null && !/browser/i.test(command)) return null;
  return pid;
}

function runnerProcessGone(pid: number): boolean {
  if (!alive(pid)) return true;
  const command = commandLine(pid);
  // A reused PID is not this run's runner. Without /proc, liveness alone is
  // the available observation on this platform.
  return command !== null && !/browser/i.test(command);
}

function taggedProcesses(run: EvidenceRun): number[] {
  if (process.platform !== "linux") return [];
  let entries: string[];
  try {
    entries = readdirSync("/proc");
  } catch {
    return [];
  }
  const pids: number[] = [];
  for (const entry of entries) {
    if (!/^\d+$/.test(entry)) continue;
    const pid = Number(entry);
    if (stillTagged(pid, run)) pids.push(pid);
  }
  return pids;
}

function stillTagged(pid: number, run: EvidenceRun): boolean {
  try {
    return readFileSync(`/proc/${pid}/environ`).includes(
      Buffer.from(`HALO_TEST_RUN_ID=${run.runId}\0`),
    );
  } catch {
    return false;
  }
}

function processGroup(pid: number): number | null {
  try {
    const stat = readFileSync(`/proc/${pid}/stat`, "utf8");
    const fields = stat.slice(stat.lastIndexOf(")") + 2).split(" ");
    const group = Number(fields[2]);
    return Number.isInteger(group) ? group : null;
  } catch {
    return null;
  }
}

function signal(pid: number, name: NodeJS.Signals, group = false): boolean {
  try {
    process.kill(group ? -pid : pid, name);
    return true;
  } catch {
    return false;
  }
}

function ownedGroups(run: EvidenceRun): number[] {
  const groups = new Set<number>();
  for (const record of readJsonl(join(run.dir, "runner.jsonl"), run.runId).records)
    if (
      record.event === "child_spawn" &&
      record.group === true &&
      typeof record.pid === "number"
    )
      groups.add(record.pid);
  return [...groups];
}

type Settlement = {
  gone: boolean;
  unknown: boolean;
  escalated: boolean;
  observations: Array<Record<string, unknown>>;
};

async function settleOwned(
  run: EvidenceRun | null,
  cargo: ChildProcess,
  options: {
    requestRunner: boolean;
    parentSignal: () => NodeJS.Signals | null;
    graceMs: number;
  },
): Promise<Settlement> {
  const observations: Array<Record<string, unknown>> = [];
  const cargoPid = cargo.pid;
  let runner: number | null = null;
  const signalled = new Set<number>();
  let cargoSignalled = false;
  const deadline = Date.now() + options.graceMs;
  let groups: number[] = [];
  let tagged: number[] = [];
  const gone = async (): Promise<boolean> => {
    // Registration and child creation may race Cargo's exit or cancellation.
    // Refresh ownership until settlement, rather than freezing an early list.
    runner = run ? runnerAlive(run) : null;
    groups = run ? ownedGroups(run) : [];
    tagged = run ? taggedProcesses(run) : [];
    const parentSignal = options.parentSignal();
    if (options.requestRunner || parentSignal !== null) {
      // A signalled runner's exit starts backend drain; preserve its grace.
      if (runner !== null && !signalled.has(runner)) {
        signalled.add(runner);
        observations.push({
          label: "runner", pid: runner,
          termSent: signal(runner, parentSignal ?? "SIGTERM"),
        });
      } else if (runner === null && signalled.size === 0 && !cargoSignalled &&
                 cargo.exitCode === null && cargo.signalCode === null &&
                 cargoPid !== undefined) {
        cargoSignalled = true;
        signal(cargoPid, parentSignal ?? "SIGTERM", true);
      }
    }
    if (runner !== null && !runnerProcessGone(runner)) return false;
    if (run && process.platform === "linux") {
      if (tagged.length > 0) return false;
    } else {
      for (const group of groups) if (await groupAlive(group)) return false;
    }
    if (cargoPid !== undefined && (await groupAlive(cargoPid))) return false;
    return true;
  };
  let settled = await gone();
  let escalated = false;
  while (!settled && Date.now() < deadline) {
    await delay(SETTLE_POLL_MS);
    settled = await gone();
  }
  if (!settled) {
    escalated = true;
    if (runner !== null && !runnerProcessGone(runner)) {
      observations.push({
        label: "runner",
        pid: runner,
        killed: signal(runner, "SIGKILL"),
      });
    }
    for (const group of groups) {
      // A historical group number is not sufficient ownership after PID reuse.
      if (run && process.platform === "linux" &&
          !tagged.some((pid) => stillTagged(pid, run) && processGroup(pid) === group))
        continue;
      if (!(await groupAlive(group))) continue;
      observations.push({
        label: "detached_group",
        pid: group,
        killed: signal(group, "SIGKILL", true),
      });
    }
    if (run) {
      tagged = taggedProcesses(run);
      for (const pid of tagged) {
        if (!alive(pid) || !stillTagged(pid, run)) continue;
        const group = processGroup(pid);
        const killed =
          group === pid
            ? signal(pid, "SIGKILL", true)
            : signal(pid, "SIGKILL");
        observations.push({ label: "run_process", pid, killed });
      }
    }
    if (cargoPid !== undefined && (await groupAlive(cargoPid)) &&
        (cargo.exitCode === null && cargo.signalCode === null ||
         run && tagged.some((pid) => stillTagged(pid, run) && processGroup(pid) === cargoPid))) {
      observations.push({
        label: "cargo_group",
        pid: cargoPid,
        killed: signal(cargoPid, "SIGKILL", true),
      });
    }
    const killDeadline = Date.now() + KILL_GRACE_MS;
    settled = await gone();
    while (!settled && Date.now() < killDeadline) {
      await delay(SETTLE_POLL_MS);
      settled = await gone();
    }
  }
  observations.push({
    label: "settled",
    cargoGroup: cargoPid ?? null,
    runner,
    detachedGroups: groups,
    runTagged: tagged,
    gone: settled,
  });
  return { gone: settled, unknown: !settled, escalated, observations };
}

function evidenceFailure(run: EvidenceRun, scope: BrowserScope): {
  reason: string | null;
  runner: Record<string, unknown>;
  backend: Record<string, unknown>;
} {
  const runnerRead = readJsonl(join(run.dir, "runner.jsonl"), run.runId);
  const backendRead = readJsonl(join(run.dir, "backend.jsonl"), run.runId);
  const runEnd = last(runnerRead.records, "run_end");
  const cleanupEnd = last(runnerRead.records, "cleanup_end");
  const drain = last(backendRead.records, "backend_drain");
  const acceptance = last(backendRead.records, "backend_acceptance");
  const facts = {
    runner: {
      present: !runnerRead.missing,
      ok: runEnd?.ok ?? null,
      cleanupOk: cleanupEnd?.ok ?? null,
      malformed: runnerRead.malformed,
      foreign: runnerRead.foreign,
      missing: runnerRead.missing,
    },
    backend: {
      present: !backendRead.missing,
      outcome: drain?.outcome ?? "unknown",
      acceptance: acceptance?.outcome ?? "unknown",
      malformed: backendRead.malformed,
      foreign: backendRead.foreign,
      missing: backendRead.missing,
    },
  };
  let reason: string | null = null;
  if (runnerRead.missing) reason = "runner evidence is missing";
  else if (runnerRead.malformed > 0 || backendRead.malformed > 0)
    reason = "browser evidence contains malformed records";
  else if (runnerRead.foreign > 0 || backendRead.foreign > 0)
    reason = "browser evidence contains records from another run";
  else if (!runEnd) reason = "runner completion record is missing";
  else if (runEnd.ok !== true) reason = "browser runner recorded a failed outcome";
  else if (!cleanupEnd || cleanupEnd.ok !== true)
    reason = "browser runner cleanup did not complete";
  else if (backendRead.missing) reason = "backend evidence is missing";
  else if (!drain || drain.outcome !== "completed")
    reason = "backend drain did not complete";
  else if (!acceptance || acceptance.outcome !== "completed")
    reason = "backend acceptance did not complete";
  if (!reason && scope === "badge-cancellation") {
    const start = last(runnerRead.records, "run_start");
    if (start?.scope !== scope) reason = "browser runner did not execute the requested cancellation scope";
    else {
      const read = readJsonl(join(run.dir, "reporter.jsonl"), run.runId);
      const report = summarizeReporter(read.records, run.runId, read);
      const exit = runEnd?.exit as Partial<Observation> | undefined;
      const verification = verifyReporter(report, {
        childExitCleanly: exit?.observed === true && exit.code === 0 && exit.signal === null,
      });
      const reasons = verification.reasons.concat(verifyBrowserScope(scope, report));
      if (reasons.length) reason = reasons.join("; ");
    }
  }
  return { reason, ...facts };
}

async function check(): Promise<number> {
  const [mode, ...extra] = process.argv.slice(2);
  if (extra.length || (mode !== "snapshot" && mode !== "browser")) usage();
  const typedMode = mode as "snapshot" | "browser";
  const scope = browserScope(typedMode === "browser" ? process.env.HALO_TEST_SCOPE : undefined);
  const name =
    typedMode === "snapshot"
      ? "protocol::http_validates_requests_before_business_work_and_describes_actual_routes"
      : "website::browser_c1_preserves_original_send";
  const root = dirname(fileURLToPath(new URL("../Cargo.toml", import.meta.url)));
  const args = [
    "test",
    "-p",
    "nucleus",
    "--locked",
    "--test",
    "http",
    name,
    "--",
    "--exact",
    "--format=pretty",
    "--color=never",
    "--test-threads=2",
    ...(typedMode === "browser" ? ["--ignored", "--nocapture"] : []),
  ];

  let requested: NodeJS.Signals | null = null;
  let signalResolve: (signal: NodeJS.Signals) => void = () => {};
  const signalRequested = new Promise<NodeJS.Signals>((resolve) => {
    signalResolve = resolve;
  });
  const onSignal = (value: NodeJS.Signals) => {
    if (requested) return;
    requested = value;
    signalResolve(value);
  };
  const onSigterm = () => onSignal("SIGTERM");
  const onSigint = () => onSignal("SIGINT");
  process.on("SIGTERM", onSigterm);
  process.on("SIGINT", onSigint);

  try {
    const evidenceRoot = process.env.HALO_TEST_EVIDENCE
      ? resolve(process.env.HALO_TEST_EVIDENCE)
      : join(root, "target", "halo-browser-evidence");
    const run = typedMode === "browser" ? createRun({ root: evidenceRoot }) : null;
    if (requested) {
      throw new Error(`interrupted by ${requested} before the acceptance started`);
    }
    const environment = run
      ? {
          ...process.env,
          HALO_TEST_EVIDENCE: run.root,
          HALO_TEST_RUN_ID: run.runId,
          HALO_TEST_EVIDENCE_DIR: run.dir,
          HALO_TEST_SCOPE: scope,
        }
      : process.env;
    const startedAt = new Date().toISOString();
    const child = spawn("cargo", args, {
      cwd: root,
      env: environment,
      stdio: ["inherit", "pipe", "pipe"],
      detached: true,
    });
    const capture = new OutputCapture(name);
    child.stdout?.on("data", (chunk: Buffer) => {
      safeWrite(process.stdout, chunk);
      capture.push(chunk, "stdout");
    });
    child.stderr?.on("data", (chunk: Buffer) => {
      safeWrite(process.stderr, chunk);
      capture.push(chunk, "stderr");
    });

    const exitPromise = observeExit(child);
    const first = await Promise.race([
      exitPromise.then((value) => ({ kind: "exit" as const, value })),
      signalRequested.then((value) => ({ kind: "signal" as const, value })),
    ]);

    // Cargo exit, pipe close, the runner's final record and actual process
    // termination are separate observations. Let an already-started runner
    // finish its bounded cleanup, then prove every owned process is gone.
    const settlement = await settleOwned(run, child, {
      requestRunner:
        first.kind === "signal" ||
        !first.value.observed ||
        first.value.code !== 0 ||
        first.value.signal !== null,
      parentSignal: () => requested,
      graceMs: SIGNAL_GRACE_MS,
    });
    const observation: Observation = first.kind === "exit" ? first.value :
      await Promise.race([
        exitPromise,
        delay(STREAM_CLOSE_MS).then(() => ({
          observed: false, code: null, signal: null,
          error: "cargo exit was not observed after process settlement",
        })),
      ]);
    const streamClosed = await Promise.race([
      new Promise<boolean>((resolve) => {
        if (child.stdout?.closed && child.stderr?.closed) resolve(true);
        else child.once("close", () => resolve(true));
      }),
      delay(STREAM_CLOSE_MS).then(() => false),
    ]);

    const completed = capture.completed();
    let failure: Failure | null = null;
    if (observation.error)
      failure = { kind: "spawn_or_exit_error", message: observation.error };
    else if (requested)
      failure = { kind: "parent_signal", message: `interrupted by ${requested}` };
    else if (!observation.observed)
      failure = { kind: "exit_unknown", message: "cargo exit was not observed" };
    else if (observation.code !== 0 || observation.signal !== null)
      failure = {
        kind: "cargo_exit",
        message: `cargo ended with ${observation.code ?? observation.signal}`,
      };
    else if (!completed)
      failure = {
        kind: "missing_proof",
        message: `did not complete exactly one required test: ${name}`,
      };
    else if (!streamClosed)
      failure = {
        kind: "stream_close_unknown",
        message: "cargo output streams did not close",
      };

    const evidence = run ? evidenceFailure(run, scope) : null;
    if (!failure && evidence?.reason)
      failure = { kind: "evidence", message: evidence.reason };
    if (!failure && settlement.unknown)
      failure = {
        kind: "process_termination_unknown",
        message: "owned browser processes did not terminate",
      };
    if (!failure && settlement.escalated)
      failure = {
        kind: "processes_escalated",
        message: "owned browser processes required forced termination",
      };

    const finishedAt = new Date().toISOString();
    const harness = {
      version: 1,
      runId: run?.runId ?? null,
      mode: typedMode,
      scope: typedMode === "browser" ? scope : null,
      selection: { test: name, arguments: args },
      startedAt,
      finishedAt,
      exit: observation,
      parentSignal: requested,
      forcedKill: settlement.escalated,
      streamClosed,
      proof: { matched: completed, expectedTest: name },
      failure,
      evidenceFailure: evidence?.reason ?? null,
      processes: settlement.observations,
      processesGone: settlement.gone,
      processesEscalated: settlement.escalated,
      observedResults: capture.observedResults,
      runner: evidence?.runner ?? { present: false },
      backend: evidence?.backend ?? { present: false, outcome: "unknown" },
    };

    if (run) {
      try {
        writeJsonAtomic(join(run.dir, "harness.json"), harness);
      } catch (error) {
        const detail = error instanceof Error ? error.message : String(error);
        console.error(`Halo evidence write failed: ${detail}`);
        if (!failure) failure = { kind: "evidence_write", message: detail };
      }
    }
    if (failure) throw new Error(failure.message);
    return 0;
  } finally {
    process.off("SIGTERM", onSigterm);
    process.off("SIGINT", onSigint);
  }
}

try {
  const code = await check();
  process.exit(code);
} catch (error) {
  console.error(error instanceof Error ? error.message : error);
  process.exit(1);
}
