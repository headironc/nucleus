import { afterEach, expect, test } from "vitest";
import { spawn, type ChildProcess } from "node:child_process";
import {
  existsSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { RunnerLifecycle } from "./runner-lifecycle";

const lifecycleModule = fileURLToPath(new URL("./runner-lifecycle.ts", import.meta.url));
const directories: string[] = [];
const children: Array<{ child: ChildProcess; group: boolean }> = [];

afterEach(() => {
  for (const { child, group } of children.splice(0)) {
    try {
      if (child.pid && group) process.kill(-child.pid, "SIGKILL");
    } catch {
      // Already gone.
    }
    try {
      if (child.pid) child.kill("SIGKILL");
    } catch {
      // Already gone.
    }
  }
  for (const directory of directories.splice(0))
    rmSync(directory, { recursive: true, force: true });
});

function directory(): string {
  const path = mkdtempSync(join(tmpdir(), "halo lifecycle "));
  directories.push(path);
  return path;
}

function alive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return (error as NodeJS.ErrnoException).code !== "ESRCH";
  }
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

function stubbornTree(path: string): { child: ChildProcess; pids: number[] } {
  const ready = join(path, "ready");
  writeFileSync(
    join(path, "parent.mjs"),
    `import { spawn } from "node:child_process";
import { writeFileSync } from "node:fs";
process.on("SIGTERM", () => {});
process.on("SIGINT", () => {});
const child = spawn(process.execPath, ["-e", "process.on('SIGTERM',()=>{});setInterval(()=>{},1000)"], { stdio: "ignore" });
writeFileSync(process.env.READY, \`\${process.pid} \${child.pid}\`);
setInterval(() => {}, 1000);
`,
  );
  const child = spawn(process.execPath, [join(path, "parent.mjs")], {
    detached: true,
    stdio: "ignore",
    env: { ...process.env, READY: ready },
  });
  children.push({ child, group: true });
  return { child, pids: [] };
}

async function treeReady(path: string): Promise<number[]> {
  const value = await waitFor(
    () => (existsSync(join(path, "ready")) ? readFileSync(join(path, "ready"), "utf8") : false),
    5_000,
    "the process tree",
  );
  return value.trim().split(/\s+/).map(Number);
}

test.each(["service", "playwright"])("an injected short deadline cancels and forcibly reaps the %s tree", async (label) => {
  const path = directory();
  const temporary = directory();
  const events: Array<{ event: string; data?: Record<string, unknown> }> = [];
  const lifecycle = new RunnerLifecycle({
    deadlineMs: 250,
    termMs: 300,
    killMs: 2_000,
    serviceMs: 500,
    drainMs: 500,
    removeMs: 2_000,
    record: (event, data) => events.push({ event, data }),
  });
  lifecycle.start();
  const { child } = stubbornTree(path);
  const pids = await treeReady(path);
  lifecycle.add({ label, child, group: true, spawnFailed: false });
  const cancelled = lifecycle.cancellation.signal.aborted
    ? true
    : await Promise.race([
        new Promise<true>((resolve) =>
          lifecycle.cancellation.signal.addEventListener("abort", () => resolve(true), { once: true }),
        ),
        delay(5_000).then(() => {
          throw new Error("the injected deadline did not fire");
        }),
      ]);
  expect(cancelled).toBe(true);
  expect(lifecycle.cancellationReason).toBe("execution_deadline_250ms");
  const cleanup = await lifecycle.cleanupAll(temporary, {
    services: [{ label: "closed", run: async () => ({ label: "closed", outcome: "closed" }) }],
    drains: [{ label: "drained", run: () => Promise.resolve() }],
  });
  lifecycle.stop();
  expect(cleanup.ok).toBe(true);
  const service = cleanup.results.find((result) => result.label === label);
  expect(service?.outcome).toBe("killed");
  expect(service?.forced).toBe(true);
  for (const pid of pids) expect(alive(pid)).toBe(false);
  expect(existsSync(temporary)).toBe(false);
  expect(events.map((entry) => entry.event)).toEqual(
    expect.arrayContaining([
      "cancellation_requested",
      "cleanup_begin",
      "cleanup_child",
      "cleanup_service",
      "cleanup_end",
    ]),
  );
}, 20_000);

test("Playwright gets a leader interrupt to drain its worker before forced cleanup", async () => {
  const path = directory();
  writeFileSync(join(path, "parent.mjs"), `
import { spawn } from "node:child_process";
import { once } from "node:events";
import { writeFileSync } from "node:fs";
const worker = spawn(process.execPath, ["-e", "setInterval(() => {}, 1000)"], { stdio: "ignore" });
process.on("SIGINT", async () => {
  const stopped = once(worker, "exit");
  worker.kill("SIGTERM");
  await stopped;
  writeFileSync(process.env.DRAINED, "complete");
  process.exit(130);
});
writeFileSync(process.env.READY, worker.pid.toString());
setInterval(() => {}, 1000);
`);
  const child = spawn(process.execPath, [join(path, "parent.mjs")], {
    detached: true,
    stdio: "ignore",
    env: { ...process.env, READY: join(path, "ready"), DRAINED: join(path, "drained") },
  });
  children.push({ child, group: true });
  const workerPid = Number(await waitFor(
    () => existsSync(join(path, "ready")) && readFileSync(join(path, "ready"), "utf8"),
    5_000,
    "the worker to start",
  ));
  const lifecycle = new RunnerLifecycle({
    deadlineMs: 60_000, termMs: 2_000, killMs: 2_000,
    serviceMs: 500, drainMs: 500, removeMs: 500, record: () => {},
  });
  lifecycle.add({ label: "playwright", child, group: true, spawnFailed: false });
  const result = await lifecycle.cleanupAll(null);
  expect(result.ok).toBe(true);
  expect(existsSync(join(path, "drained"))).toBe(true);
  expect(result.results[0]).toMatchObject({ observed: true, code: 130, forced: false, groupGone: true });
  expect(alive(child.pid!)).toBe(false);
  expect(alive(workerPid)).toBe(false);
}, 10_000);

test.each(["SIGTERM", "SIGINT"] as const)(
  "a parent %s runs cleanup through the same lifecycle and leaves no owned process",
  async (signal) => {
    const path = directory();
    writeFileSync(
      join(path, "harness.mjs"),
      `import { RunnerLifecycle } from ${JSON.stringify(lifecycleModule)};
import { spawn } from "node:child_process";
import { writeFileSync } from "node:fs";
const events = [];
const lifecycle = new RunnerLifecycle({
  deadlineMs: 60000,
  termMs: 300,
  killMs: 2000,
  serviceMs: 500,
  drainMs: 500,
  removeMs: 1000,
  record: (event, data) => {
    events.push({ event, data });
    if (event === "cleanup_end") {
      writeFileSync(process.env.RESULT, JSON.stringify({ reason: lifecycle.cancellationReason, events }));
      process.exit(0);
    }
  },
});
lifecycle.start();
const child = spawn(process.execPath, ["-e", "process.on('SIGTERM',()=>{});setInterval(()=>{},1000)"], {
  detached: true,
  stdio: "ignore",
});
lifecycle.add({ label: "service", child, group: true, spawnFailed: false });
writeFileSync(process.env.READY, String(child.pid));
lifecycle.cancellation.signal.addEventListener("abort", async () => {
  await lifecycle.cleanupAll(null);
  lifecycle.stop();
  process.exit(2);
}, { once: true });
setTimeout(() => process.exit(3), 15000);
`,
    );
    const child = spawn(process.execPath, [join(path, "harness.mjs")], {
      stdio: "ignore",
      env: {
        ...process.env,
        READY: join(path, "ready"),
        RESULT: join(path, "result.json"),
      },
    });
    children.push({ child, group: false });
    const servicePid = Number(
      await waitFor(
        () => (existsSync(join(path, "ready")) ? readFileSync(join(path, "ready"), "utf8") : false),
        10_000,
        "the harness service",
      ),
    );
    process.kill(child.pid!, signal);
    const code = await Promise.race([
      new Promise<number | null>((resolve) => child.once("exit", (value) => resolve(value))),
      delay(15_000).then(() => {
        throw new Error("harness did not finish bounded cleanup");
      }),
    ]);
    expect(code).toBe(0);
    const result = JSON.parse(readFileSync(join(path, "result.json"), "utf8")) as {
      reason: string | null;
      events: Array<{ event: string }>;
    };
    expect(result.reason).toBe(signal === "SIGTERM" ? "sigterm" : "sigint");
    expect(result.events.map((event) => event.event)).toEqual(
      expect.arrayContaining(["cancellation_requested", "cleanup_end"]),
    );
    // The lifecycle reports termination only after the group is gone.
    await waitFor(() => !alive(servicePid), 5_000, "the owned service to stop");
  },
  25_000,
);

test("a failed service does not skip later services, drains or temporary removal", async () => {
  const temporary = directory();
  const lifecycle = new RunnerLifecycle({
    deadlineMs: 60_000,
    termMs: 200,
    killMs: 500,
    serviceMs: 200,
    drainMs: 200,
    removeMs: 2_000,
    record: () => {},
  });
  lifecycle.start();
  const cleanup = await lifecycle.cleanupAll(temporary, {
    services: [
      {
        label: "first",
        run: async () => {
          throw new Error("service close failed");
        },
      },
      { label: "second", run: async () => ({ label: "second", outcome: "closed" }) },
    ],
    drains: [{ label: "drained", run: () => Promise.reject(new Error("drain failed")) }],
  });
  lifecycle.stop();
  expect(cleanup.ok).toBe(false);
  expect(cleanup.results).toEqual(
    expect.arrayContaining([
      expect.objectContaining({ label: "first", outcome: "unknown" }),
      expect.objectContaining({ label: "second", outcome: "closed" }),
      expect.objectContaining({ label: "drained", outcome: "unknown" }),
      expect.objectContaining({ label: "temporary_directory", outcome: "removed" }),
    ]),
  );
  expect(existsSync(temporary)).toBe(false);
}, 10_000);

test.each([false, true])("drains writes queued while closing producers (failure: %s)", async (fail) => {
  const lifecycle = new RunnerLifecycle({
    deadlineMs: 60_000, termMs: 200, killMs: 500,
    serviceMs: 500, drainMs: 500, removeMs: 500,
    record: () => {},
  });
  let queue = Promise.resolve();
  let written = false;
  let release!: () => void;
  const pending = new Promise<void>((resolve) => { release = resolve; });
  const cleanup = lifecycle.cleanupAll(null, {
    services: [{
      label: "producer",
      run: async () => {
        queue = queue.then(async () => {
          await pending;
          if (fail) throw new Error("late evidence write failed");
          written = true;
        });
        void queue.catch(() => {});
        return { label: "producer", outcome: "closed" };
      },
    }],
    drains: [{ label: "evidence", run: () => queue }],
  });
  release();
  const result = await cleanup;
  expect(result.ok).toBe(!fail);
  expect(written).toBe(!fail);
  expect(result.results).toContainEqual(fail
    ? { label: "evidence", outcome: "unknown", error: "late evidence write failed" }
    : { label: "evidence", outcome: "drained" });
});

test("a cancellation during cleanup is recorded without skipping later work", async () => {
  const temporary = directory();
  const events: Array<{ event: string; data?: Record<string, unknown> }> = [];
  let lifecycle: RunnerLifecycle;
  lifecycle = new RunnerLifecycle({
    deadlineMs: 60_000,
    termMs: 200,
    killMs: 500,
    serviceMs: 200,
    drainMs: 200,
    removeMs: 2_000,
    record: (event, data) => events.push({ event, data }),
  });
  lifecycle.start();
  const cleanup = await lifecycle.cleanupAll(temporary, {
    services: [
      {
        label: "first",
        run: async () => {
          lifecycle.requestCancellation("sigterm-during-cleanup");
          return { label: "first", outcome: "closed" };
        },
      },
      { label: "second", run: async () => ({ label: "second", outcome: "closed" }) },
    ],
  });
  lifecycle.stop();
  expect(lifecycle.cancellationReason).toBe("sigterm-during-cleanup");
  lifecycle.requestCancellation("sigint-later");
  expect(lifecycle.cancellationReason).toBe("sigterm-during-cleanup");
  expect(events).toEqual(
    expect.arrayContaining([
      expect.objectContaining({ event: "cancellation_during_cleanup" }),
    ]),
  );
  expect(cleanup.results).toEqual(
    expect.arrayContaining([
      expect.objectContaining({ label: "first", outcome: "closed" }),
      expect.objectContaining({ label: "second", outcome: "closed" }),
    ]),
  );
}, 10_000);

test("a record failure cannot skip cleanup and is reported as unknown", async () => {
  const temporary = directory();
  const lifecycle = new RunnerLifecycle({
    deadlineMs: 60_000,
    termMs: 200,
    killMs: 500,
    serviceMs: 200,
    drainMs: 200,
    removeMs: 2_000,
    record: () => {
      throw new Error("evidence append failed");
    },
  });
  lifecycle.start();
  const cleanup = await lifecycle.cleanupAll(temporary, {
    services: [{ label: "closed", run: async () => ({ label: "closed", outcome: "closed" }) }],
  });
  lifecycle.stop();
  expect(cleanup.ok).toBe(false);
  expect(cleanup.results).toEqual(
    expect.arrayContaining([
      expect.objectContaining({ label: "closed", outcome: "closed" }),
      expect.objectContaining({ label: "evidence_write", outcome: "unknown" }),
    ]),
  );
  expect(existsSync(temporary)).toBe(false);
}, 10_000);
