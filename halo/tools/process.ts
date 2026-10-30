// Child lifetime helpers shared by the browser runner. They never translate a
// signal API return value into "cleaned up": an outcome is only reported when
// the operating system no longer exposes the process (or its group).

import { setTimeout as delay } from "node:timers/promises";
import type { ChildProcess } from "node:child_process";

export type ExitObservation = {
  observed: boolean;
  code: number | null;
  signal: NodeJS.Signals | null;
};

export function waitForChildExit(
  child: ChildProcess,
  timeoutMs?: number,
): Promise<ExitObservation> {
  if (child.exitCode !== null || child.signalCode !== null)
    return Promise.resolve({
      observed: true,
      code: child.exitCode,
      signal: child.signalCode,
    });
  return new Promise((resolve) => {
    let timer: NodeJS.Timeout | undefined;
    const finish = (observed: boolean) => {
      if (timer) clearTimeout(timer);
      child.off("exit", onExit);
      resolve({
        observed,
        code: child.exitCode,
        signal: child.signalCode,
      });
    };
    const onExit = () => finish(true);
    child.once("exit", onExit);
    if (timeoutMs !== undefined)
      timer = setTimeout(() => finish(false), timeoutMs);
  });
}

export type SignalDelivery = { delivered: boolean; errors: string[] };

export function signalTree(
  child: ChildProcess,
  signal: NodeJS.Signals,
  group: boolean,
): SignalDelivery {
  const errors: string[] = [];
  let delivered = false;
  if (group && process.platform !== "win32" && child.pid) {
    try {
      process.kill(-child.pid, signal);
      delivered = true;
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "ESRCH")
        errors.push(error instanceof Error ? error.message : String(error));
    }
  }
  try {
    if (child.kill(signal)) delivered = true;
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ESRCH")
      errors.push(error instanceof Error ? error.message : String(error));
  }
  return { delivered, errors };
}

export async function groupAlive(pid: number): Promise<boolean> {
  if (process.platform === "win32") return false;
  try {
    process.kill(-pid, 0);
    return true;
  } catch (error) {
    return (error as NodeJS.ErrnoException).code !== "ESRCH";
  }
}

export async function waitForGroupGone(
  pid: number,
  timeoutMs: number,
): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    if (!(await groupAlive(pid))) return true;
    if (Date.now() >= deadline) return false;
    await delay(25);
  }
}

export type Termination = {
  outcome: "already_exited" | "terminated" | "killed" | "unknown";
  forced: boolean;
  observed: boolean;
  code: number | null;
  signal: NodeJS.Signals | null;
  groupGone: boolean | null;
  errors: string[];
};

export async function terminateTree(
  child: ChildProcess,
  options: { termMs: number; killMs: number; group: boolean },
): Promise<Termination> {
  const { termMs, killMs, group } = options;
  if (child.exitCode !== null || child.signalCode !== null) {
    const groupGone =
      group && child.pid ? await waitForGroupGone(child.pid, 1_000) : null;
    if (groupGone === false && child.pid) {
      // The leader ended but its descendants still share the group. Terminate
      // the group rather than reporting a successful cleanup.
      const killed = signalTree(child, "SIGKILL", group);
      const gone = await waitForGroupGone(child.pid, killMs);
      return {
        outcome: gone ? "killed" : "unknown",
        forced: true,
        observed: true,
        code: child.exitCode,
        signal: child.signalCode,
        groupGone: gone,
        errors: killed.errors,
      };
    }
    return {
      outcome: "already_exited",
      forced: false,
      observed: true,
      code: child.exitCode,
      signal: child.signalCode,
      groupGone,
      errors: [],
    };
  }
  const errors: string[] = [];
  const term = signalTree(child, "SIGTERM", group);
  errors.push(...term.errors);
  const exited = await waitForChildExit(child, termMs);
  if (exited.observed) {
    const groupGone =
      group && child.pid
        ? await waitForGroupGone(child.pid, Math.max(250, termMs))
        : null;
    if (groupGone !== false)
      return { ...exited, outcome: "terminated", forced: false, groupGone, errors };
    // The leader exited but descendants remain in its group.
    const killed = signalTree(child, "SIGKILL", group);
    errors.push(...killed.errors);
    const gone = await waitForGroupGone(child.pid ?? -1, killMs);
    return {
      ...exited,
      outcome: gone ? "killed" : "unknown",
      forced: true,
      groupGone: gone,
      errors,
    };
  }
  const killed = signalTree(child, "SIGKILL", group);
  errors.push(...killed.errors);
  const forcedExit = await waitForChildExit(child, killMs);
  const groupGone =
    group && child.pid
      ? await waitForGroupGone(child.pid, Math.max(250, killMs))
      : null;
  return {
    ...forcedExit,
    outcome: forcedExit.observed && groupGone !== false ? "killed" : "unknown",
    forced: true,
    groupGone,
    errors,
  };
}

export type BoundedResult<T> =
  | { ok: true; value: T }
  | { ok: false; reason: "timeout" | "error"; error: string };

export async function bounded<T>(
  operation: Promise<T>,
  timeoutMs: number,
  label: string,
): Promise<BoundedResult<T>> {
  let timer: NodeJS.Timeout | undefined;
  try {
    const value = await Promise.race([
      operation,
      new Promise<never>((_, reject) => {
        timer = setTimeout(
          () => reject(new Error(`${label} exceeded ${timeoutMs}ms`)),
          timeoutMs,
        );
      }),
    ]);
    return { ok: true, value };
  } catch (error) {
    return {
      ok: false,
      reason:
        error instanceof Error && error.message.includes("exceeded")
          ? "timeout"
          : "error",
      error: error instanceof Error ? error.message : String(error),
    };
  } finally {
    if (timer) clearTimeout(timer);
  }
}
