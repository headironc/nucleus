import { expect, test } from "bun:test";
import { spawn, spawnSync } from "node:child_process";
import {
  existsSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { alive, processHandle, requireRunning } from "./cancel";

const root = resolve(import.meta.dir, "../..");
const helper = join(import.meta.dir, "cancel.ts");
const packageHelper = join(root, "tools/package-postgresql.ts");
const psql = join(root, "target/postgresql/18.4/bin/psql");
const pause = (ms: number) => new Promise((done) => setTimeout(done, ms));
function execute(file: string, command: string, env: NodeJS.ProcessEnv) {
  return spawnSync(process.execPath, [file, command], {
    cwd: root,
    env,
    encoding: "utf8",
    timeout: 55000,
  });
}
function fixture(long = false, mode: "cancel" | "timeout" = "cancel") {
  const parent = mkdtempSync(
    join(tmpdir(), long ? "runner-cancel-" + "x".repeat(80) : "cancel-"),
  );
  const env: NodeJS.ProcessEnv = {
    ...process.env,
    RUNNER_TEMP: parent,
    GITHUB_ENV: join(parent, "env"),
    GITHUB_RUN_ID: "987654321",
    GITHUB_RUN_ATTEMPT: "1",
    GITHUB_SHA: "a".repeat(40),
    CANCEL_AFTER_CHECKS: String(mode === "cancel"),
    TIMEOUT_AFTER_CHECKS: String(mode === "timeout"),
  };
  const prepared = execute(packageHelper, "temp", env);
  expect(prepared.status).toBe(0);
  for (const line of readFileSync(env.GITHUB_ENV!, "utf8").trim().split("\n")) {
    const [name, value] = line.split("=");
    env[name!] = value;
  }
  const directory = join(env.TMPDIR!, "cancel-probe");
  const stateFile = join(directory, "state.json");
  const state = () => JSON.parse(readFileSync(stateFile, "utf8"));
  const sql = (query: string) =>
    spawnSync(
      psql,
      [
        "-X",
        "-w",
        "-h",
        join(directory, "socket"),
        "-U",
        "probe",
        "-d",
        "postgres",
        "-At",
        "-v",
        "ON_ERROR_STOP=1",
        "-c",
        query,
      ],
      {
        env: { ...env, PGCONNECT_TIMEOUT: "2" },
        encoding: "utf8",
        timeout: 5000,
      },
    );
  const clean = () => {
    if (existsSync(stateFile)) {
      const value = state();
      if (alive(value.owner)) execute(helper, "cleanup", env);
      for (const handle of [
        value.owner,
        value.postgres,
        value.query,
        value.backend,
      ])
        expect(!handle || !alive(handle)).toBe(true);
    }
    if (existsSync(env.TMPDIR!)) {
      const result = execute(packageHelper, "cleanup", env);
      expect(result.status, result.stdout + result.stderr).toBe(0);
    }
    expect(existsSync(env.TMPDIR!)).toBe(false);
    rmSync(parent, { recursive: true });
  };
  return { env, directory, stateFile, state, sql, clean };
}
async function until(predicate: () => boolean, timeout = 6000) {
  const deadline = Date.now() + timeout;
  while (!predicate() && Date.now() < deadline) await pause(25);
  expect(predicate()).toBe(true);
}

for (const mode of ["cancel", "timeout"] as const) {
  for (const signal of ["SIGINT", "SIGTERM"] as const) {
    test(`${mode}: real active PostgreSQL survives prepare/upload boundary and ${signal} stops original resources`, async () => {
      const f = fixture(false, mode);
      try {
        const start = execute(helper, "start", f.env);
        expect(start.status, start.stdout + start.stderr).toBe(0);
        expect(start.stdout).toContain("RUNNER_PROBE_PG_READY");
        const original = f.state();
        expect(original.phase).toBe("ready");
        expect(original.mode).toBe(mode);
        expect(f.sql("SHOW listen_addresses").stdout.trim()).toBe("");
        const activity = f.sql(
          `SELECT state || ':' || wait_event FROM pg_stat_activity WHERE pid = ${original.backend.pid}`,
        );
        expect(activity.status).toBe(0);
        expect(activity.stdout.trim()).toBe("active:PgSleep");
        expect(
          f
            .sql(
              `SELECT application_name || ':' || query FROM pg_stat_activity WHERE pid = ${original.backend.pid}`,
            )
            .stdout.trim(),
        ).toBe(`runner_${mode}_probe:SELECT pg_sleep(3600)`);
        const waiting = spawn(process.execPath, [helper, "wait"], {
          cwd: root,
          env: f.env,
          stdio: ["ignore", "pipe", "pipe"],
        });
        let output = "";
        waiting.stdout.on("data", (data) => {
          output += data;
        });
        waiting.stderr.on("data", (data) => {
          output += data;
        });
        const ended = new Promise<number | null>((done) =>
          waiting.once("exit", done),
        );
        await until(() =>
          output.includes(`RUNNER_PROBE_${mode.toUpperCase()}_WAITING`),
        );
        expect(output).not.toContain(
          `RUNNER_PROBE_${mode === "cancel" ? "TIMEOUT" : "CANCEL"}_WAITING`,
        );
        expect(f.state().backend).toEqual(original.backend);
        waiting.kill(signal);
        expect(await ended, output).toBe(signal === "SIGINT" ? 130 : 143);
        expect(output).toContain("RUNNER_PROBE_PG_STOPPED");
        for (let i = 0; i < 2; i++) {
          const cleanup = execute(helper, "cleanup", f.env);
          expect(cleanup.status, cleanup.stdout + cleanup.stderr).toBe(0);
        }
        expect(f.state().phase).toBe("stopped");
        expect(execute(helper, "wait", f.env).status).not.toBe(0);
        for (const handle of [
          original.owner,
          original.postgres,
          original.query,
          original.backend,
        ])
          expect(alive(handle)).toBe(false);
      } finally {
        f.clean();
      }
    }, 60000);
  }
}

for (const mode of ["cancel", "timeout"] as const) {
  test(`${mode}: ownership, mode, path and reused process identity mismatches refuse cleanup without stopping PG`, () => {
    const f = fixture(false, mode);
    try {
      expect(execute(helper, "start", f.env).status).toBe(0);
      const original = readFileSync(f.stateFile, "utf8");
      const other = {
        ...f.env,
        CANCEL_AFTER_CHECKS: String(mode !== "cancel"),
        TIMEOUT_AFTER_CHECKS: String(mode !== "timeout"),
      };
      for (const command of ["start", "own", "wait", "cleanup"]) {
        expect(execute(helper, command, other).status).not.toBe(0);
        expect(f.sql("SELECT 1").status).toBe(0);
        expect(existsSync(join(f.directory, "stop"))).toBe(false);
      }
      for (const alter of [
        (s: any) => {
          s.mode = mode === "cancel" ? "timeout" : "cancel";
        },
        (s: any) => {
          s.mode = "unknown";
        },
        (s: any) => {
          s.source = "b".repeat(40);
        },
        (s: any) => {
          s.data = "/tmp";
        },
        (s: any) => {
          s.postgres.start = "0";
        },
        (s: any) => {
          s.query.pid = process.pid;
        },
      ]) {
        const changed = JSON.parse(original);
        alter(changed);
        writeFileSync(f.stateFile, JSON.stringify(changed));
        try {
          expect(execute(helper, "cleanup", f.env).status).not.toBe(0);
          expect(f.sql("SELECT 1").status).toBe(0);
          expect(existsSync(join(f.directory, "stop"))).toBe(false);
        } finally {
          writeFileSync(f.stateFile, original);
        }
      }
      expect(execute(helper, "cleanup", f.env).status).toBe(0);
    } finally {
      f.clean();
    }
  }, 60000);

  test(`${mode}: query completion without interruption remains a failure and cleans the cluster`, async () => {
    const f = fixture(false, mode);
    try {
      expect(execute(helper, "start", f.env).status).toBe(0);
      const original = f.state();
      expect(
        f
          .sql(`SELECT pg_cancel_backend(${original.backend.pid})`)
          .stdout.trim(),
      ).toBe("t");
      await until(() => !alive(original.owner));
      expect(f.state().failed).toBe(true);
      expect(f.state().phase).toBe("stopped");
      expect(execute(helper, "wait", f.env).status).not.toBe(0);
      expect(execute(helper, "cleanup", f.env).status).not.toBe(0);
    } finally {
      f.clean();
    }
  }, 60000);

  test(`${mode}: partial real cluster startup failure is retained while private resources are reclaimed`, () => {
    // PostgreSQL rejects a Unix socket path exceeding its platform limit after initdb.
    const f = fixture(true, mode);
    try {
      const result = execute(helper, "start", f.env);
      expect(result.status).not.toBe(0);
      expect(existsSync(join(f.directory, "data", "PG_VERSION"))).toBe(true);
      expect(f.state().failed).toBe(true);
      expect(f.state().phase).toBe("stopped");
      expect(execute(helper, "cleanup", f.env).status).not.toBe(0);
    } finally {
      f.clean();
    }
  }, 60000);

  test(`${mode}: failed signal cleanup stays failed, preserves mismatched PID file, and permits bounded original recovery`, async () => {
    const f = fixture(false, mode);
    try {
      expect(execute(helper, "start", f.env).status).toBe(0);
      const original = f.state();
      const pidFile = join(original.data, "postmaster.pid");
      const pidText = readFileSync(pidFile, "utf8");
      writeFileSync(pidFile, pidText.replace(/^\d+/, "1"));
      try {
        process.kill(original.owner.pid, "SIGTERM");
        await until(() => !alive(original.owner));
        expect(f.state().failed).toBe(true);
        expect(alive(original.postgres)).toBe(true);
        expect(execute(helper, "cleanup", f.env).status).not.toBe(0);
        expect(readFileSync(pidFile, "utf8").startsWith("1\n")).toBe(true);
      } finally {
        writeFileSync(pidFile, pidText);
      }
      expect(execute(helper, "cleanup", f.env).status).not.toBe(0);
      await until(() => !alive(original.postgres) && !alive(original.query));
    } finally {
      f.clean();
    }
  }, 60000);
}

test("even an ended receipt does not authorize a reused PID", () => {
  const current = processHandle(process.pid)!;
  expect(() => alive({ ...current, start: "0" })).toThrow("identity changed");
});

for (const mode of ["cancel", "timeout"] as const) {
  test(`${mode}: independent package cleanup reports a leaked cluster even after successful bounded remediation`, async () => {
    const f = fixture(false, mode);
    try {
      expect(execute(helper, "start", f.env).status).toBe(0);
      const original = f.state();
      process.kill(original.owner.pid, "SIGKILL");
      await until(() => !alive(original.owner));
      const cleanup = execute(packageHelper, "cleanup", f.env);
      expect(cleanup.status, cleanup.stdout + cleanup.stderr).toBe(1);
      expect(cleanup.stderr).toContain(
        "Package fixture leaked PostgreSQL; bounded cleanup completed",
      );
      expect(existsSync(f.env.TMPDIR!)).toBe(false);
      await until(
        () =>
          !alive(original.postgres) &&
          !alive(original.query) &&
          !alive(original.backend),
      );
    } finally {
      f.clean();
    }
  }, 60000);

  test(`${mode}: a naturally successful SQL process is not an interruption outcome`, async () => {
    const f = fixture(false, mode);
    try {
      expect(execute(helper, "start", f.env).status).toBe(0);
      const original = f.state();
      expect(execute(helper, "own", f.env).status).not.toBe(0);
      expect(f.state().owner).toEqual(original.owner);
      const finite = spawn(
        psql,
        [
          "-X",
          "-w",
          "-h",
          original.socket,
          "-U",
          "probe",
          "-d",
          "postgres",
          "-At",
          "-c",
          "SELECT 1",
        ],
        { env: f.env, stdio: "ignore" },
      );
      await new Promise((done) => finite.once("exit", done));
      expect(finite.exitCode).toBe(0);
      expect(() => requireRunning([finite])).toThrow(
        "ended without interruption",
      );
      expect(execute(helper, "cleanup", f.env).status).toBe(0);
    } finally {
      f.clean();
    }
  }, 60000);
}

test("invalid or ambiguous modes refuse resource creation", () => {
  const f = fixture();
  try {
    for (const cancel of [undefined, "", "false", "true", "TRUE", "$(exit 0)"])
      for (const timeout of [
        undefined,
        "",
        "false",
        "true",
        "TRUE",
        "$(exit 0)",
      ]) {
        if (
          (cancel === "true" && timeout === "false") ||
          (cancel === "false" && timeout === "true")
        )
          continue;
        const env = {
          ...f.env,
          CANCEL_AFTER_CHECKS: cancel,
          TIMEOUT_AFTER_CHECKS: timeout,
        };
        for (const command of ["start", "own", "wait", "cleanup"])
          expect(execute(helper, command, env).status).not.toBe(0);
        expect(existsSync(f.directory)).toBe(false);
      }
  } finally {
    f.clean();
  }
}, 60000);

test("early cleanup without a published private scope creates no resources", () => {
  for (const mode of ["cancel", "timeout"] as const) {
    const env: NodeJS.ProcessEnv = {
      ...process.env,
      CANCEL_AFTER_CHECKS: String(mode === "cancel"),
      TIMEOUT_AFTER_CHECKS: String(mode === "timeout"),
    };
    delete env.NUCLEUS_PACKAGE_TMPDIR;
    expect(execute(helper, "cleanup", env).status).toBe(0);
  }
});
