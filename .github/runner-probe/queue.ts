import { setTimeout as delay } from "node:timers/promises";

// This window lets the field owner observe demand; completion proves no queue fact.
export async function queueWindow(
  env: NodeJS.ProcessEnv,
  signal: AbortSignal,
  wait: (milliseconds: number, signal: AbortSignal) => Promise<void> = (
    milliseconds,
    signal,
  ) => delay(milliseconds, undefined, { signal }),
) {
  const {
    GITHUB_RUN_ID: run,
    GITHUB_RUN_ATTEMPT: attempt,
    GITHUB_SHA: source,
    QUEUE_CELL: cell,
  } = env;
  if (
    !run ||
    !/^[1-9][0-9]*$/.test(run) ||
    !attempt ||
    !/^[1-9][0-9]*$/.test(attempt) ||
    !source ||
    !/^[0-9a-f]{40}$/.test(source) ||
    env.EXPECTED_REVISION !== source ||
    env.GITHUB_WORKFLOW_SHA !== source ||
    !["one", "two"].includes(cell ?? "") ||
    env.QUEUE_PROBE !== "true" ||
    [
      env.FAIL_AFTER_CHECKS,
      env.CANCEL_AFTER_CHECKS,
      env.TIMEOUT_AFTER_CHECKS,
    ].some((value) => value !== "false")
  )
    throw new Error("Invalid queue probe identity or mode");
  signal.throwIfAborted();
  const identity = `run=${run} attempt=${attempt} source=${source} cell=${cell} duration_seconds=300`;
  console.log(`RUNNER_PROBE_QUEUE_WINDOW_STARTED ${identity}`);
  await wait(300_000, signal);
  signal.throwIfAborted();
  console.log(`RUNNER_PROBE_QUEUE_WINDOW_COMPLETED ${identity}`);
}

if (import.meta.main) {
  const controller = new AbortController();
  const interrupt = () => {
    process.exitCode = 130;
    controller.abort();
  };
  const terminate = () => {
    process.exitCode = 143;
    controller.abort();
  };
  process.once("SIGINT", interrupt);
  process.once("SIGTERM", terminate);
  try {
    await queueWindow(process.env, controller.signal);
  } catch (error) {
    process.exitCode ||= 1;
    console.error(
      controller.signal.aborted
        ? "Runner queue window interrupted"
        : error instanceof Error
          ? error.message
          : "Runner queue window failed",
    );
  } finally {
    process.removeListener("SIGINT", interrupt);
    process.removeListener("SIGTERM", terminate);
  }
}
