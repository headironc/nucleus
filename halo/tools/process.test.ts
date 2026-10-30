import { afterEach, expect, test } from "vitest";
import { spawn, type ChildProcess } from "node:child_process";
import { EventEmitter } from "node:events";
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
import { terminateTree, waitForChildExit } from "./process";

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

function directory(): string {
  const path = mkdtempSync(join(tmpdir(), "halo process "));
  directories.push(path);
  return path;
}

async function waitForFile(path: string, timeoutMs = 5_000): Promise<string> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    if (existsSync(path)) return readFileSync(path, "utf8");
    if (Date.now() >= deadline) throw new Error(`Timed out waiting for ${path}`);
    await delay(25);
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

async function waitForDeath(pid: number, timeoutMs = 3_000): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (alive(pid)) {
    if (Date.now() >= deadline) return false;
    await delay(25);
  }
  return true;
}

function tree(
  path: string,
  options: { stubbornParent: boolean; stubbornGrandchild: boolean },
): ChildProcess {
  const ready = join(path, "ready");
  writeFileSync(
    join(path, "parent.mjs"),
    `import { spawn } from "node:child_process";
import { writeFileSync } from "node:fs";
${options.stubbornParent ? "process.on('SIGTERM', () => {});" : ""}
const child = spawn(process.execPath, ["-e", process.env.GRANDCHILD], { stdio: "ignore" });
writeFileSync(process.env.READY, \`\${process.pid} \${child.pid}\`);
setInterval(() => {}, 1000);
`,
  );
  const child = spawn(process.execPath, [join(path, "parent.mjs")], {
    detached: true,
    stdio: "ignore",
    env: {
      ...process.env,
      READY: ready,
      GRANDCHILD: options.stubbornGrandchild
        ? "process.on('SIGTERM', () => {}); setInterval(() => {}, 1000);"
        : "setInterval(() => {}, 1000);",
    },
  });
  children.push(child);
  return child;
}

test("a graceful stop reaps the leader and its descendant", async () => {
  const path = directory();
  const parent = tree(path, {
    stubbornParent: false,
    stubbornGrandchild: false,
  });
  const [parentPid, grandchildPid] = (await waitForFile(join(path, "ready")))
    .trim()
    .split(/\s+/)
    .map(Number);
  const result = await terminateTree(parent, {
    termMs: 2_000,
    killMs: 2_000,
    group: true,
  });
  expect(result.observed).toBe(true);
  expect(result.outcome).toBe("terminated");
  expect(result.forced).toBe(false);
  expect(await waitForDeath(parentPid!)).toBe(true);
  expect(await waitForDeath(grandchildPid!)).toBe(true);
}, 15_000);

test("a forced stop escalates to SIGKILL and still reaps descendants", async () => {
  const path = directory();
  const parent = tree(path, {
    stubbornParent: true,
    stubbornGrandchild: true,
  });
  const [parentPid, grandchildPid] = (await waitForFile(join(path, "ready")))
    .trim()
    .split(/\s+/)
    .map(Number);
  const result = await terminateTree(parent, {
    termMs: 300,
    killMs: 3_000,
    group: true,
  });
  expect(result.observed).toBe(true);
  expect(result.outcome).toBe("killed");
  expect(result.forced).toBe(true);
  expect(await waitForDeath(parentPid!)).toBe(true);
  expect(await waitForDeath(grandchildPid!)).toBe(true);
}, 15_000);

test("an unobserved exit is reported as unknown, not cleaned up", async () => {
  const fake = Object.assign(new EventEmitter(), {
    pid: 999_999,
    exitCode: null,
    signalCode: null,
    kill: () => true,
  }) as unknown as ChildProcess;
  const result = await terminateTree(fake, {
    termMs: 25,
    killMs: 25,
    group: false,
  });
  expect(result.observed).toBe(false);
  expect(result.outcome).toBe("unknown");
  expect(result.forced).toBe(true);
});

test("an already exited child is observed without another signal", async () => {
  const child = spawn(process.execPath, ["-e", ""], { stdio: "ignore" });
  children.push(child);
  const observation = await waitForChildExit(child, 5_000);
  expect(observation.observed).toBe(true);
  expect(observation.code).toBe(0);
  const result = await terminateTree(child, {
    termMs: 100,
    killMs: 100,
    group: false,
  });
  expect(result.outcome).toBe("already_exited");
});
