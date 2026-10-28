import { afterEach, expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import {
  chmodSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { packageMatrix, packageResult, selection } from "./native-packages.ts";
import { artifactBinding, privateDirectory } from "./package-postgresql.ts";

const root = fileURLToPath(new URL("../", import.meta.url));
type Step = {
  name?: string;
  run?: string;
  uses?: string;
  if?: string;
  with?: Record<string, unknown>;
  "continue-on-error"?: unknown;
};
type Job = {
  if?: string;
  needs?: unknown;
  steps: Step[];
  strategy?: Record<string, unknown>;
};
const workflow = Bun.YAML.parse(
  readFileSync(join(root, ".github/workflows/packages.yml"), "utf8"),
) as {
  on: unknown;
  permissions: unknown;
  jobs: Record<string, Job>;
};
const directories: string[] = [];
afterEach(() => {
  for (const path of directories.splice(0))
    rmSync(path, { recursive: true, force: true });
});

function fixture() {
  const path = mkdtempSync(join(tmpdir(), "nucleus native packages "));
  const runner = mkdtempSync(join(tmpdir(), "nucleus package runner "));
  directories.push(path, runner);
  for (const dir of ["tools", "fixture"]) mkdirSync(join(path, dir));
  for (const name of [
    "native-packages.ts",
    "package-postgresql.ts",
    "check-packages.ts",
  ])
    copyFileSync(join(root, "tools", name), join(path, "tools", name));
  for (const name of ["postgresql.ts", "postgresql.version"])
    copyFileSync(join(root, "fixture", name), join(path, "fixture", name));
  const env = {
    ...process.env,
    GITHUB_SHA: "a".repeat(40),
    GITHUB_RUN_ID: "123",
    GITHUB_RUN_ATTEMPT: "1",
    RUNNER_TEMP: runner,
    GITHUB_OUTPUT: join(path, "output"),
    GITHUB_ENV: join(path, "env"),
    CARGO_NET_OFFLINE: "true",
    CARGO_TARGET_DIR: join(path, "target/cargo"),
  };
  const run = (args: string[], extra: NodeJS.ProcessEnv = {}) =>
    spawnSync(process.execPath, args, {
      cwd: path,
      env: { ...env, ...extra },
      encoding: "utf8",
      timeout: 30_000,
    });
  return { path, env, run };
}

const rows = {
  include: [
    { package: "domain", shard: "1/4" },
    { package: "domain", shard: "2/4" },
    { package: "domain", shard: "3/4" },
    { package: "domain", shard: "4/4" },
    { package: "nucleus", group: "rest" },
    { package: "nucleus", group: "http" },
  ],
};
const success = {
  CANCELLED: "false",
  PLAN_RESULT: "success",
  POSTGRESQL_RESULT: "success",
  PACKAGES_RESULT: "success",
  TIER: "full",
  AFFECTED: "all",
  PACKAGE_NAMES: '["domain","nucleus"]',
  MATRIX: JSON.stringify(rows),
};

test("native package workflow owns a read-only PR candidate and only this run's artifact", () => {
  expect(workflow.on).toEqual({
    pull_request: {
      branches: ["dev", "main"],
      types: ["opened", "reopened", "synchronize", "edited"],
    },
  });
  expect(workflow.permissions).toEqual({ contents: "read" });
  expect(Object.keys(workflow.jobs)).toEqual([
    "plan",
    "postgresql",
    "packages",
    "finalcheck",
  ]);
  expect(workflow.jobs.plan!.if).toBeUndefined();
  expect(workflow.jobs.finalcheck!.if).toBe("always()");
  expect(workflow.jobs.packages!.strategy!["fail-fast"]).toBe(false);
  for (const job of Object.values(workflow.jobs))
    for (const step of job.steps) {
      expect(step["continue-on-error"]).toBeUndefined();
      if (step.uses) expect(step.uses).toMatch(/^[\w-]+\/[\w-]+@[0-9a-f]{40}$/);
      if (step.uses?.startsWith("actions/checkout@"))
        expect(step.with!["persist-credentials"]).toBe(false);
    }
  const download = workflow.jobs.packages!.steps.find((step) =>
    step.uses?.startsWith("actions/download-artifact@"),
  )!;
  expect(download.with!["artifact-ids"]).toBe(
    "${{ needs.postgresql.outputs.artifact-id }}",
  );
  for (const key of ["github-token", "repository", "run-id", "name", "pattern"])
    expect(download.with![key]).toBeUndefined();
  expect(download.with!["digest-mismatch"]).toBe("error");
  expect(workflow.jobs.packages!.steps.at(-1)!.if).toBe("always()");
});

test("complete plans reject a missing whole package, partition, duplicate or malformed selection", () => {
  expect(packageMatrix(["domain", "nucleus"], rows)).toHaveLength(6);
  for (const value of [
    { include: [] },
    { include: rows.include.slice(0, -1) },
    { include: [...rows.include, rows.include[0]] },
    { include: [...rows.include, { package: "other" }] },
    { include: [{ package: "domain", shard: "1/4", extra: true }] },
  ])
    expect(() => packageMatrix(["domain", "nucleus"], value)).toThrow();
  expect(() =>
    packageMatrix(["domain", "nucleus", "platform"], rows),
  ).toThrow();
  for (const affected of ["[]", "{}", '["missing",null]', ""])
    expect(() => selection("full", affected)).toThrow();
  expect(() => selection("unknown", "[]")).toThrow();
  expect(() => selection("tools", "all")).toThrow();
});

test("only all selected successes or an explicit non-full plan can pass the stable result", () => {
  expect(packageResult(success)).toContain("passed");
  for (const key of ["PLAN_RESULT", "POSTGRESQL_RESULT", "PACKAGES_RESULT"])
    for (const value of ["failure", "cancelled", "skipped", "", "timed_out"])
      expect(() => packageResult({ ...success, [key]: value })).toThrow();
  expect(() => packageResult({ ...success, CANCELLED: "true" })).toThrow();
  expect(() =>
    packageResult({ ...success, MATRIX: '{"include":[]}' }),
  ).toThrow();
  const skipped = {
    ...success,
    TIER: "tools",
    AFFECTED: "[]",
    PACKAGE_NAMES: "[]",
    MATRIX: '{"include":[]}',
    POSTGRESQL_RESULT: "skipped",
    PACKAGES_RESULT: "skipped",
  };
  expect(packageResult(skipped)).toContain("No package execution");
  expect(() =>
    packageResult({ ...skipped, POSTGRESQL_RESULT: "success" }),
  ).toThrow();
});

test("actual metadata discovery produces a complete plan and refuses unknown packages", () => {
  const f = fixture();
  mkdirSync(join(f.path, "src"));
  writeFileSync(
    join(f.path, "Cargo.toml"),
    '[package]\nname="sample"\nversion="0.1.0"\nedition="2024"\n',
  );
  writeFileSync(join(f.path, "src/lib.rs"), "pub fn value() -> u8 { 1 }\n");
  const locked = spawnSync("cargo", ["generate-lockfile", "--offline"], {
    cwd: f.path,
    env: f.env,
  });
  expect(locked.status).toBe(0);
  const good = f.run(["tools/native-packages.ts", "plan"], {
    TIER: "full",
    AFFECTED: "all",
  });
  expect(good.error).toBeUndefined();
  expect(good.status, good.stderr).toBe(0);
  expect(readFileSync(f.env.GITHUB_OUTPUT, "utf8")).toBe(
    'matrix={"include":[{"package":"sample"}]}\npackages=["sample"]\n',
  );
  const bad = f.run(["tools/native-packages.ts", "plan"], {
    TIER: "full",
    AFFECTED: '["missing"]',
  });
  expect(bad.status).toBe(1);
  expect(bad.stderr).toContain("Package discovery failed");
}, 40_000);

test("the actual matrix command propagates a Rust test failure", () => {
  const f = fixture();
  mkdirSync(join(f.path, "src"));
  copyFileSync(
    join(root, "rust-toolchain.toml"),
    join(f.path, "rust-toolchain.toml"),
  );
  writeFileSync(
    join(f.path, "Cargo.toml"),
    '[package]\nname="sample"\nversion="0.1.0"\nedition="2024"\n',
  );
  writeFileSync(
    join(f.path, "src/lib.rs"),
    '#[test] fn package_failure() { panic!("native package assertion"); }\n',
  );
  expect(
    spawnSync("cargo", ["generate-lockfile", "--offline"], {
      cwd: f.path,
      env: f.env,
    }).status,
  ).toBe(0);
  const command = workflow.jobs.packages!.steps.find(
    (step) => step.name === "Check selected package",
  )!.run!;
  const result = spawnSync(
    "bash",
    ["--noprofile", "--norc", "-eo", "pipefail", "-c", command],
    {
      cwd: f.path,
      env: { ...f.env, PACKAGE: "sample", SHARD: "", GROUP: "" },
      encoding: "utf8",
      timeout: 30_000,
    },
  );
  expect(result.error).toBeUndefined();
  expect(result.status).toBe(1);
  expect(result.stdout + result.stderr).toContain("native package assertion");
}, 40_000);

function installation(f: ReturnType<typeof fixture>) {
  const path = join(f.path, "target/postgresql/18.4");
  mkdirSync(join(path, "bin"), { recursive: true });
  for (const name of ["postgres", "initdb", "pg_ctl", "psql"]) {
    const program = join(path, "bin", name);
    writeFileSync(
      program,
      `#!/bin/sh\nprintf '%s\\n' '${name} (PostgreSQL) 18.4'\n`,
    );
    chmodSync(program, 0o755);
  }
  symlinkSync("postgres", join(path, "bin/symbolic"));
  return path;
}

test("real tar transport preserves executable modes/symlinks and verifies all four programs", () => {
  const f = fixture();
  const path = installation(f);
  expect(f.run(["tools/package-postgresql.ts", "pack"]).status).toBe(0);
  const digest = readFileSync(f.env.GITHUB_OUTPUT, "utf8")
    .trim()
    .split("=")[1]!;
  rmSync(path, { recursive: true });
  const result = f.run(["tools/package-postgresql.ts", "unpack"], {
    POSTGRESQL_SHA256: digest,
  });
  expect(result.error).toBeUndefined();
  expect(result.status, result.stderr).toBe(0);
  expect(readFileSync(join(path, "bin/symbolic"), "utf8")).toContain(
    "PostgreSQL",
  );
  writeFileSync(
    join(path, "bin/psql"),
    "#!/bin/sh\necho 'psql (PostgreSQL) 18.3'\n",
  );
  const mismatch = f.run(["fixture/postgresql.ts"]);
  expect(mismatch.status).toBe(1);
  expect(mismatch.stderr).toContain("Fixture psql must run as PostgreSQL 18.4");
});

test("missing/corrupt PG artifacts and candidate/run/prefix drift fail before rebuilding", () => {
  const f = fixture();
  const path = installation(f);
  expect(f.run(["tools/package-postgresql.ts", "pack"]).status).toBe(0);
  const manifest = JSON.parse(
    readFileSync(
      join(f.path, "target/package-postgresql/postgresql.json"),
      "utf8",
    ),
  );
  const { sha256, ...binding } = manifest;
  artifactBinding(manifest, binding, sha256);
  for (const key of [
    "candidate",
    "run",
    "attempt",
    "prefix",
    "version",
    "arch",
    "os",
  ])
    expect(() =>
      artifactBinding({ ...manifest, [key]: "wrong" }, binding, sha256),
    ).toThrow();
  rmSync(path, { recursive: true });
  for (const changes of [
    { POSTGRESQL_SHA256: "" },
    { POSTGRESQL_SHA256: "0".repeat(64) },
    { POSTGRESQL_SHA256: sha256, GITHUB_RUN_ATTEMPT: "2" },
  ]) {
    expect(
      f.run(["tools/package-postgresql.ts", "unpack"], changes).status,
    ).toBe(1);
    expect(existsSync(path)).toBe(false);
  }
  writeFileSync(
    join(f.path, "target/package-postgresql/postgresql.tar"),
    "corrupt",
  );
  expect(
    f.run(["tools/package-postgresql.ts", "unpack"], {
      POSTGRESQL_SHA256: sha256,
    }).status,
  ).toBe(1);
  expect(existsSync(path)).toBe(false);
});

test("private fixture scope is writable/canonical and cleanup refuses an unrelated identity", () => {
  const f = fixture();
  installation(f);
  const created = f.run(["tools/package-postgresql.ts", "temp"]);
  expect(created.status, created.stderr).toBe(0);
  const path = readFileSync(f.env.GITHUB_ENV, "utf8")
    .split("\n")[0]!
    .slice("TMPDIR=".length);
  privateDirectory(path, f.env.RUNNER_TEMP);
  expect(() =>
    privateDirectory(path + "/../" + path.split("/").at(-1), f.env.RUNNER_TEMP),
  ).toThrow();
  const foreign = f.run(["tools/package-postgresql.ts", "cleanup"], {
    NUCLEUS_PACKAGE_TMPDIR: path,
    GITHUB_RUN_ATTEMPT: "2",
  });
  expect(foreign.status).toBe(1);
  expect(existsSync(path)).toBe(true);
  const cleaned = f.run(["tools/package-postgresql.ts", "cleanup"], {
    NUCLEUS_PACKAGE_TMPDIR: path,
  });
  expect(cleaned.error).toBeUndefined();
  expect(cleaned.status, cleaned.stderr).toBe(0);
  expect(cleaned.stdout).toContain("PG scan empty");
  expect(existsSync(path)).toBe(false);
});
