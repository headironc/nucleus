import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  appendFileSync,
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  realpathSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { dirname, isAbsolute, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = realpathSync(fileURLToPath(new URL("../", import.meta.url)));
const version = readFileSync(
  join(root, "fixture/postgresql.version"),
  "utf8",
).trim();
const installation = join(root, "target/postgresql", version);
const artifact = join(root, "target/package-postgresql");

function check(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

function run(command: string, args: string[], timeout = 30_000) {
  const result = spawnSync(command, args, { stdio: "inherit", timeout });
  check(
    !result.error && result.status === 0,
    `${command} failed (${result.signal ?? result.status})`,
  );
}

function identity() {
  const {
    GITHUB_SHA: candidate,
    GITHUB_RUN_ID: run,
    GITHUB_RUN_ATTEMPT: attempt,
  } = process.env;
  check(
    candidate &&
      /^[0-9a-f]{40}$/.test(candidate) &&
      run &&
      /^\d+$/.test(run) &&
      attempt &&
      /^\d+$/.test(attempt),
    "Missing package candidate/run identity",
  );
  check(
    process.platform === "linux" && process.arch === "x64",
    "Package PostgreSQL requires Linux x64",
  );
  return {
    candidate,
    run,
    attempt,
    prefix: installation,
    version,
    arch: process.arch,
    os: readFileSync("/etc/os-release", "utf8"),
  };
}

function sha(path: string) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

export function artifactBinding(
  manifest: unknown,
  expected: ReturnType<typeof identity>,
  digest: string,
) {
  check(
    manifest && typeof manifest === "object" && !Array.isArray(manifest),
    "Missing PostgreSQL artifact manifest",
  );
  const fields = manifest as Record<string, unknown>;
  for (const [name, value] of Object.entries(expected))
    check(fields[name] === value, `PostgreSQL artifact ${name} mismatch`);
  check(
    /^[0-9a-f]{64}$/.test(digest) && fields.sha256 === digest,
    "PostgreSQL artifact checksum mismatch",
  );
}

export function privateDirectory(path: string, parent: string) {
  check(
    isAbsolute(path) &&
      resolve(path) === path &&
      !path.split("/").includes(".."),
    "Noncanonical package TMPDIR",
  );
  check(
    realpathSync(path) === path &&
      realpathSync(parent) === parent &&
      dirname(path) === parent,
    "Package TMPDIR escaped runner temp",
  );
  check(
    relative(root, path).startsWith("../"),
    "Package TMPDIR is inside Cargo workspace",
  );
  const info = statSync(path);
  check(
    info.uid === process.getuid!() && (info.mode & 0o777) === 0o700,
    "Package TMPDIR ownership or mode mismatch",
  );
}

function temporary() {
  check(process.getuid?.() !== 0, "Package fixtures must run non-root");
  check(process.env.RUNNER_TEMP, "Missing RUNNER_TEMP");
  const parent = realpathSync(process.env.RUNNER_TEMP);
  const path = mkdtempSync(join(parent, "nucleus-packages-"));
  chmodSync(path, 0o700);
  privateDirectory(path, parent);
  writeFileSync(join(path, ".owner.json"), JSON.stringify(identity()), {
    mode: 0o600,
  });
  const probe = join(path, ".writable");
  writeFileSync(probe, "");
  rmSync(probe);
  check(process.env.GITHUB_ENV, "Missing GITHUB_ENV");
  appendFileSync(
    process.env.GITHUB_ENV,
    `TMPDIR=${path}\nNUCLEUS_PACKAGE_TMPDIR=${path}\n`,
  );
  console.log(`Private package TMPDIR: ${path}; uid=${process.getuid!()}`);
}

type PrivatePostgres = { pid: number; start: string; data: string | null };
function privatePostgres(path: string): PrivatePostgres[] {
  const executable = realpathSync(join(installation, "bin/postgres"));
  const found: PrivatePostgres[] = [];
  for (const name of readdirSync("/proc")) {
    if (!/^\d+$/.test(name)) continue;
    const proc = join("/proc", name);
    try {
      if (statSync(proc).uid !== process.getuid!()) continue;
      const fields = readFileSync(join(proc, "stat"), "utf8")
        .split(") ")[1]!
        .split(" ");
      if (fields[0] === "Z") continue;
      if (readFileSync(join(proc, "comm"), "utf8").trim() !== "postgres")
        continue;
      const cwd = realpathSync(join(proc, "cwd"));
      if (!cwd.startsWith(path + "/")) continue;
      check(
        realpathSync(join(proc, "exe")) === executable,
        "Private cluster process executable mismatch",
      );
      const args = readFileSync(join(proc, "cmdline"), "utf8").split("\0");
      const index = args.indexOf("-D");
      const data = index < 0 ? null : args[index + 1];
      if (data !== null)
        check(
          data &&
            isAbsolute(data) &&
            resolve(data) === data &&
            realpathSync(data) === cwd,
          "Unverifiable PostgreSQL data path",
        );
      // Include backend children even if their original postmaster has already exited.
      found.push({ pid: Number(name), start: fields[19]!, data });
    } catch (error) {
      // A process disappearing during /proc inspection has a known terminal outcome.
      const code = (error as NodeJS.ErrnoException).code;
      if (!(["ENOENT", "ESRCH"].includes(code ?? "") && !existsSync(proc)))
        throw error;
    }
  }
  return found;
}

function cleanup() {
  const path = process.env.NUCLEUS_PACKAGE_TMPDIR;
  // A failed preparation before TMPDIR publication has no fixture scope to clean.
  if (!path) {
    console.log("No package fixture TMPDIR was created");
    return;
  }
  check(process.env.RUNNER_TEMP, "Missing runner temp during cleanup");
  privateDirectory(path, realpathSync(process.env.RUNNER_TEMP));
  check(
    readFileSync(join(path, ".owner.json"), "utf8") ===
      JSON.stringify(identity()),
    "Package cleanup owner mismatch",
  );
  const leaked = privatePostgres(path);
  const masters = leaked.filter(
    (process): process is PrivatePostgres & { data: string } =>
      process.data !== null,
  );
  check(
    !leaked.length || masters.length > 0,
    "Orphan private PostgreSQL children; preserving TMPDIR",
  );
  for (const master of masters) {
    console.error(
      `Leaked private PostgreSQL postmaster pid=${master.pid}; requesting fast stop`,
    );
    check(
      privatePostgres(path).some(
        (current) =>
          current.pid === master.pid &&
          current.start === master.start &&
          current.data === master.data,
      ),
      "PostgreSQL handle changed; preserving TMPDIR",
    );
    const pid = readFileSync(join(master.data, "postmaster.pid"), "utf8").split(
      "\n",
    )[0];
    check(
      pid === String(master.pid),
      "PostgreSQL PID file mismatch; preserving TMPDIR",
    );
    run(
      join(installation, "bin/pg_ctl"),
      ["-D", master.data, "stop", "-m", "fast", "-w", "-t", "10"],
      15_000,
    );
  }
  check(
    privatePostgres(path).length === 0,
    "Private PostgreSQL still running; preserving TMPDIR",
  );
  rmSync(path, { recursive: true });
  check(!existsSync(path), "Package TMPDIR removal failed");
  console.log(`Package PG scan empty; private TMPDIR removed: ${path}`);
  check(
    leaked.length === 0,
    "Package fixture leaked PostgreSQL; bounded cleanup completed",
  );
}

if (import.meta.main) {
  try {
    check(process.getuid?.() !== 0, "Package PostgreSQL must run non-root");
    const command = process.argv[2];
    check(process.argv.length === 3, "Expected one package PostgreSQL command");
    if (command === "pack") {
      check(
        existsSync(join(installation, "bin/postgres")),
        "Missing built PostgreSQL installation",
      );
      mkdirSync(artifact, { recursive: true });
      const archive = join(artifact, "postgresql.tar");
      run("tar", ["-cf", archive, "-C", dirname(installation), version]);
      const digest = sha(archive);
      writeFileSync(
        join(artifact, "postgresql.json"),
        JSON.stringify({ ...identity(), sha256: digest }) + "\n",
      );
      check(process.env.GITHUB_OUTPUT, "Missing producer output");
      appendFileSync(process.env.GITHUB_OUTPUT, `sha256=${digest}\n`);
    } else if (command === "unpack") {
      check(
        process.env.POSTGRESQL_SHA256 &&
          /^[0-9a-f]{64}$/.test(process.env.POSTGRESQL_SHA256),
        "Missing producer PostgreSQL checksum",
      );
      const archive = join(artifact, "postgresql.tar");
      const digest = sha(archive);
      check(
        digest === process.env.POSTGRESQL_SHA256,
        "Producer PostgreSQL checksum mismatch",
      );
      artifactBinding(
        JSON.parse(readFileSync(join(artifact, "postgresql.json"), "utf8")),
        identity(),
        digest,
      );
      check(
        !existsSync(installation),
        "PostgreSQL consumer installation already exists",
      );
      mkdirSync(dirname(installation), { recursive: true });
      run("tar", [
        "-xf",
        archive,
        "-C",
        dirname(installation),
        "--no-same-owner",
      ]);
      for (const program of ["postgres", "initdb", "pg_ctl", "psql"])
        check(
          existsSync(join(installation, "bin", program)),
          `Missing artifact program ${program}`,
        );
      // Installation must exist before this verifier: never silently rebuild a missing artifact.
      run(process.execPath, ["fixture/postgresql.ts"]);
    } else if (command === "temp") temporary();
    else if (command === "cleanup") cleanup();
    else
      throw new Error(
        "Usage: bun tools/package-postgresql.ts pack|unpack|temp|cleanup",
      );
  } catch (error) {
    console.error(
      error instanceof Error ? error.message : "Package PostgreSQL failed",
    );
    process.exitCode = 1;
  }
}
