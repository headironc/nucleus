import { spawn, spawnSync, type ChildProcess } from "node:child_process";
import {
  closeSync,
  existsSync,
  lstatSync,
  mkdirSync,
  openSync,
  readFileSync,
  realpathSync,
  renameSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { join, resolve } from "node:path";
import { privateDirectory } from "../../tools/package-postgresql";

const root = resolve(import.meta.dir, "../..");
const version = readFileSync(
  join(root, "fixture/postgresql.version"),
  "utf8",
).trim();
const installation = join(root, "target/postgresql", version);
const bin = join(installation, "bin");
const sql = "SELECT pg_sleep(3600)";
type Handle = { pid: number; start: string; executable: string; cwd: string };
type State = {
  run: string;
  attempt: string;
  source: string;
  mode: "cancel" | "timeout";
  directory: string;
  data: string;
  socket: string;
  owner: Handle;
  postgres?: Handle;
  query?: Handle;
  backend?: Handle;
  phase: "starting" | "ready" | "stopped";
  failed: boolean;
};
function check(value: unknown, message: string): asserts value {
  if (!value) throw new Error(message);
}
const delay = (ms: number) => new Promise((done) => setTimeout(done, ms));
function identity() {
  const {
    GITHUB_RUN_ID: run,
    GITHUB_RUN_ATTEMPT: attempt,
    GITHUB_SHA: source,
  } = process.env;
  check(
    run &&
      /^\d+$/.test(run) &&
      attempt &&
      /^\d+$/.test(attempt) &&
      source &&
      /^[0-9a-f]{40}$/.test(source),
    "Missing cancellation identity",
  );
  const { CANCEL_AFTER_CHECKS: cancel, TIMEOUT_AFTER_CHECKS: timeout } =
    process.env;
  check(
    ["true", "false"].includes(cancel ?? "") &&
      ["true", "false"].includes(timeout ?? "") &&
      cancel !== timeout,
    "Expected exactly one Boolean cancellation or timeout mode",
  );
  const mode: State["mode"] = cancel === "true" ? "cancel" : "timeout";
  return { run, attempt, source, mode };
}
function scope() {
  check(
    process.platform === "linux" && process.getuid?.() !== 0,
    "Cancellation requires non-root Linux",
  );
  const path = process.env.NUCLEUS_PACKAGE_TMPDIR;
  check(
    path && process.env.RUNNER_TEMP && process.env.TMPDIR === path,
    "Missing private package scope",
  );
  privateDirectory(path, realpathSync(process.env.RUNNER_TEMP));
  const id = identity();
  check(
    readFileSync(join(path, ".owner.json"), "utf8") ===
      JSON.stringify({
        candidate: id.source,
        run: id.run,
        attempt: id.attempt,
        prefix: installation,
        version,
        arch: process.arch,
        os: readFileSync("/etc/os-release", "utf8"),
      }),
    "Cancellation package owner mismatch",
  );
  return join(path, "cancel-probe");
}
function plain(path: string) {
  const info = lstatSync(path);
  check(
    info.isFile() &&
      info.uid === process.getuid!() &&
      (info.mode & 0o777) === 0o600,
    "Cancellation file ownership mismatch",
  );
}
function directory(path: string) {
  const info = lstatSync(path);
  check(
    info.isDirectory() &&
      realpathSync(path) === path &&
      info.uid === process.getuid!() &&
      (info.mode & 0o777) === 0o700,
    "Cancellation directory ownership mismatch",
  );
}
export function processHandle(pid: number): Handle | undefined {
  check(Number.isSafeInteger(pid) && pid > 1, "Invalid process identity");
  const proc = `/proc/${pid}`;
  try {
    const fields = readFileSync(join(proc, "stat"), "utf8")
      .split(") ")[1]!
      .split(" ");
    if (fields[0] === "Z") return undefined;
    check(statSync(proc).uid === process.getuid!(), "Process UID mismatch");
    return {
      pid,
      start: fields[19]!,
      executable: realpathSync(join(proc, "exe")),
      cwd: realpathSync(join(proc, "cwd")),
    };
  } catch (error) {
    if (
      ["ENOENT", "ESRCH"].includes(
        (error as NodeJS.ErrnoException).code ?? "",
      ) &&
      !existsSync(proc)
    )
      return undefined;
    throw error;
  }
}
export function alive(expected: Handle) {
  try {
    const fields = readFileSync(`/proc/${expected.pid}/stat`, "utf8")
      .split(") ")[1]!
      .split(" ");
    check(
      fields[19] === expected.start,
      "Process identity changed; preserving resources",
    );
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
  }
  const actual = processHandle(expected.pid);
  if (!actual) return false;
  check(
    JSON.stringify(actual) === JSON.stringify(expected),
    "Process identity changed; preserving resources",
  );
  return true;
}
export function requireRunning(
  children: Pick<ChildProcess, "exitCode" | "signalCode">[],
) {
  check(
    children.every(
      (child) => child.exitCode === null && child.signalCode === null,
    ),
    "Cancellation workload ended without interruption",
  );
}
function save(state: State) {
  directory(state.directory);
  const path = join(state.directory, "state.json");
  if (existsSync(path)) plain(path);
  const temporary = join(state.directory, "state.next");
  writeFileSync(temporary, JSON.stringify(state), { flag: "wx", mode: 0o600 });
  renameSync(temporary, path);
}
function load(path: string): State {
  directory(path);
  plain(join(path, "state.json"));
  const value = JSON.parse(
    readFileSync(join(path, "state.json"), "utf8"),
  ) as State;
  for (const [key, expected] of Object.entries(identity()))
    check(
      value[key as keyof State] === expected,
      "Cancellation state owner mismatch",
    );
  check(
    value.directory === path &&
      value.data === join(path, "data") &&
      value.socket === join(path, "socket"),
    "Cancellation state path mismatch",
  );
  check(
    ["starting", "ready", "stopped"].includes(value.phase) &&
      typeof value.failed === "boolean",
    "Invalid cancellation state",
  );
  for (const handle of [
    value.owner,
    value.postgres,
    value.query,
    value.backend,
  ]) {
    if (!handle) continue;
    check(
      Number.isSafeInteger(handle.pid) &&
        handle.pid > 1 &&
        /^\d+$/.test(handle.start),
      "Invalid stored process identity",
    );
  }
  check(
    value.owner?.executable === realpathSync(process.execPath) &&
      value.owner.cwd === root,
    "Invalid owner executable or directory",
  );
  for (const handle of [value.postgres, value.backend])
    if (handle)
      check(
        handle.executable === realpathSync(join(bin, "postgres")) &&
          handle.cwd === value.data,
        "Invalid PG executable or directory",
      );
  if (value.query)
    check(
      value.query.executable === realpathSync(join(bin, "psql")) &&
        value.query.cwd === root,
      "Invalid query identity",
    );
  if (existsSync(value.data)) directory(value.data);
  if (existsSync(value.socket)) directory(value.socket);
  return value;
}
function pgEnvironment(): NodeJS.ProcessEnv {
  const env = { ...process.env };
  for (const key of Object.keys(env)) if (key.startsWith("PG")) delete env[key];
  return { ...env, PGCONNECT_TIMEOUT: "2" };
}
function run(program: string, args: string[], timeout = 5000) {
  const result = spawnSync(join(bin, program), args, {
    env: pgEnvironment(),
    encoding: "utf8",
    timeout,
  });
  check(
    !result.error && result.status === 0,
    `Cancellation ${program} failed (${result.signal ?? result.status})`,
  );
  return result.stdout.trim();
}
function queryArgs(state: State, command: string) {
  return [
    "-X",
    "-w",
    "-h",
    state.socket,
    "-U",
    "probe",
    "-d",
    "postgres",
    "-At",
    "-v",
    "ON_ERROR_STOP=1",
    "-c",
    command,
  ];
}
function active(state: State) {
  check(
    state.postgres &&
      alive(state.postgres) &&
      state.query &&
      alive(state.query),
    "Cancellation PG/query ended before interruption",
  );
  const pid = run(
    "psql",
    queryArgs(
      state,
      `SELECT pid FROM pg_stat_activity WHERE application_name = 'runner_${state.mode}_probe' AND state = 'active' AND query = '${sql}' AND wait_event = 'PgSleep'`,
    ),
  );
  check(/^\d+$/.test(pid), "Cancellation SQL is not active");
  const backend = processHandle(Number(pid));
  check(
    backend &&
      backend.executable === realpathSync(join(bin, "postgres")) &&
      backend.cwd === state.data,
    "Cancellation backend identity mismatch",
  );
  if (state.backend)
    check(
      JSON.stringify(backend) === JSON.stringify(state.backend),
      "Cancellation backend changed",
    );
  return backend;
}
function stopPostgres(state: State) {
  // Validate every original handle before any stop. Missing handles are terminal;
  // a reused PID is a mismatch, including after a prior successful cleanup.
  for (const handle of [state.postgres, state.query, state.backend])
    if (handle) alive(handle);
  if (state.postgres && alive(state.postgres)) {
    const pid = readFileSync(join(state.data, "postmaster.pid"), "utf8").split(
      "\n",
    )[0];
    check(pid === String(state.postgres.pid), "Postmaster PID file changed");
    run(
      "pg_ctl",
      ["-D", state.data, "stop", "-m", "fast", "-w", "-t", "3"],
      4000,
    );
  }
}
function requestStop(path: string) {
  const file = join(path, "stop");
  if (existsSync(file)) plain(file);
  else writeFileSync(file, "stop\n", { flag: "wx", mode: 0o600 });
}
function requested(path: string) {
  const file = join(path, "stop");
  if (!existsSync(file)) return false;
  plain(file);
  check(readFileSync(file, "utf8") === "stop\n", "Invalid stop request");
  return true;
}
async function stopped(path: string) {
  const deadline = Date.now() + 6000;
  while (Date.now() < deadline) {
    const state = load(path);
    if (state.phase === "stopped" && !alive(state.owner)) {
      for (const handle of [state.postgres, state.query, state.backend])
        check(
          !handle || !alive(handle),
          "Cancellation process survived cleanup",
        );
      check(!state.failed, "Cancellation owner reported failure");
      console.log("RUNNER_PROBE_PG_STOPPED " + JSON.stringify(state));
      return;
    }
    check(
      alive(state.owner) || state.phase === "stopped",
      "Cancellation owner ended without cleanup receipt",
    );
    await delay(50);
  }
  throw new Error("Cancellation cleanup deadline exceeded");
}
async function own(path: string) {
  directory(path);
  // The cross-step owner is single-use, including direct accidental re-entry.
  closeSync(openSync(join(path, "owner.lock"), "wx", 0o600));
  const state: State = {
    ...identity(),
    directory: path,
    data: join(path, "data"),
    socket: join(path, "socket"),
    owner: processHandle(process.pid)!,
    phase: "starting",
    failed: false,
  };
  let interrupt = false;
  let completed = false;
  const children: ChildProcess[] = [];
  const signal = () => {
    interrupt = true;
  };
  process.on("SIGINT", signal);
  process.on("SIGTERM", signal);
  const exit = () => {
    if (!completed) {
      try {
        stopPostgres(state);
      } catch {
        process.exitCode = 1;
      }
    }
  };
  process.on("exit", exit);
  const stopping = () => interrupt || requested(path);
  const launch = (program: string, args: string[], env = pgEnvironment()) => {
    const child = spawn(join(bin, program), args, {
      cwd: root,
      env,
      stdio: ["ignore", "ignore", "inherit"],
    });
    children.push(child);
    child.on("error", () => {
      state.failed = true;
      interrupt = true;
    });
    return child;
  };
  try {
    save(state);
    check(version === "18.4", "Unreviewed cancellation PostgreSQL version");
    for (const program of ["postgres", "initdb", "pg_ctl", "psql"])
      check(
        run(program, ["--version"]) === `${program} (PostgreSQL) ${version}`,
        "PostgreSQL version mismatch",
      );
    run(
      "initdb",
      [
        "-D",
        state.data,
        "-U",
        "probe",
        "--auth=trust",
        "--no-locale",
        "--encoding=UTF8",
      ],
      30000,
    );
    mkdirSync(state.socket, { mode: 0o700 });
    check(!stopping(), "Cancellation interrupted during initialization");
    const postgres = launch("postgres", [
      "-D",
      state.data,
      "-c",
      "listen_addresses=",
      "-c",
      `unix_socket_directories=${state.socket}`,
      "-c",
      "unix_socket_permissions=0700",
      "-c",
      "logging_collector=off",
    ]);
    // Capture the child only after exec and PostgreSQL's chdir to its data directory.
    const deadline = Date.now() + 10000;
    while (Date.now() < deadline && !stopping()) {
      const handle = postgres.pid && processHandle(postgres.pid);
      if (
        handle &&
        handle.executable === realpathSync(join(bin, "postgres")) &&
        handle.cwd === state.data
      ) {
        state.postgres = handle;
        save(state);
        break;
      }
      check(
        postgres.exitCode === null && postgres.signalCode === null,
        "PostgreSQL startup failed",
      );
      await delay(25);
    }
    check(state.postgres, "PostgreSQL start identity unavailable");
    while (Date.now() < deadline && !stopping()) {
      try {
        run("psql", queryArgs(state, "SELECT 1"));
        break;
      } catch {
        await delay(50);
      }
    }
    check(!stopping(), "Cancellation interrupted during PostgreSQL startup");
    run("psql", queryArgs(state, "SELECT 1"));
    const query = launch("psql", queryArgs(state, sql), {
      ...pgEnvironment(),
      PGAPPNAME: `runner_${state.mode}_probe`,
    });
    while (Date.now() < deadline && !stopping()) {
      const handle = query.pid && processHandle(query.pid);
      if (handle && handle.executable === realpathSync(join(bin, "psql"))) {
        state.query = handle;
        save(state);
        try {
          state.backend = active(state);
          break;
        } catch {
          /* Wait for the actual SQL activity. */
        }
      }
      check(
        query.exitCode === null && query.signalCode === null,
        "Cancellation query startup failed",
      );
      await delay(50);
    }
    check(state.backend && !stopping(), "Cancellation SQL never became ready");
    state.phase = "ready";
    save(state);
    console.log("RUNNER_PROBE_PG_READY " + JSON.stringify(state));
    while (!stopping()) {
      requireRunning([query, postgres]);
      await delay(50);
    }
  } catch (error) {
    state.failed = true;
    console.error(
      error instanceof Error ? error.message : "Cancellation owner failed",
    );
  } finally {
    try {
      stopPostgres(state);
      // Partially initialized children remain owned even before a handle was saved.
      for (const child of children)
        if (child.exitCode === null && child.signalCode === null)
          child.kill("SIGTERM");
      const deadline = Date.now() + 1000;
      while (
        children.some(
          (child) => child.exitCode === null && child.signalCode === null,
        ) &&
        Date.now() < deadline
      )
        await delay(25);
      check(
        children.every(
          (child) => child.exitCode !== null || child.signalCode !== null,
        ),
        "Owned child did not exit",
      );
      for (const handle of [state.postgres, state.query, state.backend])
        check(!handle || !alive(handle), "PostgreSQL child survived stop");
      state.phase = "stopped";
    } catch (error) {
      state.failed = true;
      console.error(
        error instanceof Error ? error.message : "Cancellation cleanup failed",
      );
    }
    save(state);
    completed = true;
    process.exitCode = state.failed ? 1 : 0;
    for (const child of children) child.unref();
    process.removeListener("exit", exit);
  }
}
async function main() {
  check(process.argv.length === 3, "Expected one cancellation command");
  const command = process.argv[2];
  check(
    ["start", "own", "wait", "cleanup"].includes(command!),
    "Invalid cancellation command",
  );
  if (command === "cleanup" && !process.env.NUCLEUS_PACKAGE_TMPDIR) return;
  const path = scope();
  if (command === "own") {
    await own(path);
    return;
  }
  if (command === "cleanup" && !existsSync(path)) {
    console.log("No cancellation resources created");
    return;
  }
  let signalCode = 0;
  let owned = false;
  const signal = (name: NodeJS.Signals) => {
    signalCode = name === "SIGINT" ? 130 : 143;
    if (owned) requestStop(path);
  };
  process.on("SIGINT", signal);
  process.on("SIGTERM", signal);
  try {
    if (command === "start") {
      mkdirSync(path, { mode: 0o700 });
      owned = true;
      const log = openSync(join(path, "owner.log"), "wx", 0o600);
      const child = spawn(process.execPath, [import.meta.path, "own"], {
        cwd: root,
        env: process.env,
        detached: true,
        stdio: ["ignore", log, log],
      });
      closeSync(log);
      child.on("error", () => {
        signalCode = 1;
      });
      child.unref();
      const deadline = Date.now() + 45000;
      while (Date.now() < deadline && !signalCode) {
        if (existsSync(join(path, "state.json"))) {
          const state = load(path);
          check(
            alive(state.owner) && state.phase !== "stopped",
            "Cancellation initialization failed",
          );
          if (state.phase === "ready") {
            active(state);
            console.log("RUNNER_PROBE_PG_READY " + JSON.stringify(state));
            return;
          }
        }
        await delay(50);
      }
      throw new Error("Cancellation startup interrupted or timed out");
    }
    const state = load(path);
    for (const handle of [
      state.owner,
      state.postgres,
      state.query,
      state.backend,
    ])
      if (handle) alive(handle);
    owned = true;
    if (command === "cleanup") {
      if (alive(state.owner)) requestStop(path);
      else if (state.phase !== "stopped") {
        stopPostgres(state);
        throw new Error("Cancellation owner lost; bounded PG stop attempted");
      }
      await stopped(path);
      return;
    }
    check(
      state.phase === "ready" && alive(state.owner),
      "Cancellation wait requires live owner",
    );
    active(state);
    const marker =
      state.mode === "cancel"
        ? "RUNNER_PROBE_CANCEL_WAITING"
        : "RUNNER_PROBE_TIMEOUT_WAITING";
    console.log(marker + " " + JSON.stringify(state));
    while (!signalCode) {
      const current = load(path);
      check(
        current.phase === "ready" && alive(current.owner),
        "Cancellation workload ended before signal",
      );
      await delay(100);
    }
    requestStop(path);
    await stopped(path);
    process.exitCode = signalCode;
  } catch (error) {
    // Preparation/upload/wait failures retain their nonzero result after cleanup.
    if (owned && existsSync(path)) {
      try {
        directory(path);
        requestStop(path);
        await stopped(path);
      } catch (cleanup) {
        console.error(
          cleanup instanceof Error
            ? cleanup.message
            : "Cancellation cleanup failed",
        );
      }
    }
    throw error;
  } finally {
    process.removeListener("SIGINT", signal);
    process.removeListener("SIGTERM", signal);
    if (owned && command !== "wait") {
      const log = join(path, "owner.log");
      if (existsSync(log)) {
        plain(log);
        console.log(readFileSync(log).subarray(-65536).toString("utf8"));
      }
    }
  }
}
if (import.meta.main) {
  try {
    await main();
  } catch (error) {
    console.error(
      error instanceof Error ? error.message : "Cancellation probe failed",
    );
    process.exitCode = 1;
  }
}
