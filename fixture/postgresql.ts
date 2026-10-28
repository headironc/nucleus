import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  renameSync,
  rmSync,
} from "node:fs";
import { availableParallelism } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const fixture = dirname(fileURLToPath(import.meta.url));
const version = readFileSync(
  join(fixture, "postgresql.version"),
  "utf8",
).trim();
if (!/^\d+\.\d+$/.test(version)) {
  throw new Error("Invalid fixture PostgreSQL version");
}

// https://ftp.postgresql.org/pub/source/v18.4/postgresql-18.4.tar.bz2.sha256
const digest =
  "01a81ec695fb0c7901407defaa1d2f7973617154cf27ba74e3a7ab8e64436094";
const parent = resolve(fixture, "../target/postgresql");
const installation = join(parent, version);

function run(command: string, args: string[], cwd: string) {
  const result = spawnSync(command, args, { cwd, stdio: "inherit" });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`${command} failed (${result.signal ?? result.status})`);
  }
}

function verify(directory: string) {
  for (const program of ["postgres", "initdb", "pg_ctl", "psql"]) {
    const result = spawnSync(join(directory, "bin", program), ["--version"], {
      encoding: "utf8",
      timeout: 10_000,
      // Staged executables are linked for the final prefix, which does not exist yet.
      env: { ...process.env, LD_LIBRARY_PATH: join(directory, "lib") },
    });
    if (result.error) throw result.error;
    if (
      result.status !== 0 ||
      result.stdout.trim() !== `${program} (PostgreSQL) ${version}`
    ) {
      throw new Error(`Fixture ${program} must run as PostgreSQL ${version}`);
    }
  }
}

if (existsSync(installation)) {
  verify(installation);
  console.log(`PostgreSQL ${version} is ready at ${installation}`);
} else {
  mkdirSync(parent, { recursive: true });
  const temporary = mkdtempSync(join(parent, ".build-"));
  try {
    const name = `postgresql-${version}`;
    const archive = join(temporary, `${name}.tar.bz2`);
    run(
      "curl",
      [
        "--fail",
        "--location",
        "--silent",
        "--show-error",
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "--connect-timeout",
        "10",
        "--max-time",
        "120",
        "--output",
        archive,
        `https://ftp.postgresql.org/pub/source/v${version}/${name}.tar.bz2`,
      ],
      temporary,
    );
    const bytes = readFileSync(archive);
    if (createHash("sha256").update(bytes).digest("hex") !== digest) {
      throw new Error("PostgreSQL source checksum mismatch");
    }
    run("tar", ["-xjf", archive, "--no-same-owner"], temporary);
    const source = join(temporary, name);
    const staging = join(temporary, "staging");
    run(
      "./configure",
      [`--prefix=${installation}`, "--with-ssl=openssl", "--with-libxml"],
      source,
    );
    run("make", [`-j${Math.min(availableParallelism(), 8)}`], source);
    run("make", ["install", `DESTDIR=${staging}`], source);
    const staged = join(staging, installation);
    verify(staged);
    // Publish a complete installation; a failed build never becomes the test runtime.
    renameSync(staged, installation);
    console.log(`PostgreSQL ${version} is ready at ${installation}`);
  } finally {
    rmSync(temporary, { recursive: true, force: true });
  }
}
