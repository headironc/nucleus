import { test, expect } from "bun:test";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { readFileSync } from "node:fs";
import { bounded, stopChild, type Child } from "./runner.ts";

test("parent deadline reaches termination when the child cannot run timers", async () => {
  const childProcess = spawn(
    process.execPath,
    [
      "--no-env-file",
      "--eval",
      'process.stdout.write("ready\\n"); setInterval(() => {}, 1000);',
    ],
    { detached: true, stdio: ["pipe", "pipe", "pipe"] },
  );
  const exit = new Promise<{
    code: number | null;
    signal: NodeJS.Signals | null;
  }>((resolve) => {
    childProcess.once("exit", (code, signal) => resolve({ code, signal }));
  });
  const closed = once(childProcess, "close");
  const child: Child = {
    process: childProcess,
    ready: bounded(once(childProcess.stdout, "data"), 5_000).then(([data]) => {
      expect(data.toString()).toBe("ready\n");
    }),
    exit,
    log: () => "",
  };
  try {
    await child.ready;
    const pid = childProcess.pid;
    if (!pid) throw new Error("supervision child has no PID");
    process.kill(-pid, "SIGSTOP");
    await bounded(
      (async () => {
        while (
          !/^State:\s+T/m.test(readFileSync(`/proc/${pid}/status`, "utf8"))
        ) {
          await new Promise((resolve) => setTimeout(resolve, 1));
        }
      })(),
      1_000,
    );
    await expect(bounded(exit, 25)).rejects.toThrow(
      "independent supervisor deadline expired",
    );
    // Skip the production drain grace; retain the real TERM and KILL escalation budgets.
    const result = await stopChild(child, 0);
    expect(result.forced).toBe(true);
    expect(result.exit).toEqual({ code: null, signal: "SIGKILL" });
    await bounded(closed, 1_000);
    let remaining: string | undefined;
    try {
      process.kill(-pid, 0);
    } catch (error) {
      remaining = (error as NodeJS.ErrnoException).code;
    }
    expect(remaining).toBe("ESRCH");
  } finally {
    if (
      childProcess.pid &&
      childProcess.exitCode === null &&
      childProcess.signalCode === null
    )
      process.kill(-childProcess.pid, "SIGKILL");
    await bounded(closed, 2_000);
  }
}, 20_000);
