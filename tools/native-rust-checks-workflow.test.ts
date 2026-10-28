import { afterEach, expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import {
  copyFileSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../", import.meta.url));
type Step = {
  name?: string;
  run?: string;
  uses?: string;
  with?: Record<string, unknown>;
  if?: unknown;
  "continue-on-error"?: unknown;
};
type Workflow = {
  on: unknown;
  permissions: unknown;
  defaults: { run: { shell: string } };
  jobs: Record<
    string,
    {
      name: string;
      if?: unknown;
      permissions?: unknown;
      "continue-on-error"?: unknown;
      steps: Step[];
    }
  >;
};
const workflow = Bun.YAML.parse(
  readFileSync(join(root, ".github/workflows/rust-checks.yml"), "utf8"),
) as Workflow;
const maintenance = Bun.YAML.parse(
  readFileSync(join(root, ".github/workflows/maintenance-tools.yml"), "utf8"),
) as Workflow;
const job = workflow.jobs.checks!;
const [format, clippy] = job.steps.slice(-2).map((step) => step.run!);
const nightly = readFileSync(
  join(root, "docs/engineering/rust.md"),
  "utf8",
).match(/rustup toolchain install (nightly-\d{4}-\d{2}-\d{2})/)![1]!;
const directories: string[] = [];

afterEach(() => {
  for (const directory of directories.splice(0))
    rmSync(directory, { recursive: true, force: true });
});

function run(command: string, cwd: string, env = process.env) {
  return spawnSync(
    "bash",
    ["--noprofile", "--norc", "-eo", "pipefail", "-c", command],
    {
      cwd,
      env,
      encoding: "utf8",
      timeout: 30_000,
    },
  );
}

function fixture(source: string) {
  const directory = mkdtempSync(join(tmpdir(), "nucleus rust checks "));
  directories.push(directory);
  mkdirSync(join(directory, "sample", "src"), { recursive: true });
  writeFileSync(
    join(directory, "Cargo.toml"),
    '[workspace]\nmembers = ["sample"]\nresolver = "3"\n',
  );
  writeFileSync(
    join(directory, "sample/Cargo.toml"),
    '[package]\nname = "sample"\nversion = "0.1.0"\nedition = "2024"\n',
  );
  writeFileSync(join(directory, "sample/src/lib.rs"), source);
  for (const path of ["rust-toolchain.toml", "rustfmt.toml"])
    copyFileSync(join(root, path), join(directory, path));
  const env = {
    ...process.env,
    CARGO_NET_OFFLINE: "true",
    CARGO_TARGET_DIR: join(directory, "target"),
  };
  const locked = run("cargo generate-lockfile --offline", directory, env);
  expect(locked.error).toBeUndefined();
  expect(locked.status, locked.stderr).toBe(0);
  return { directory, env };
}

test("every dev/main PR runs the same read-only Rust checks without success skips", () => {
  expect(workflow.on).toEqual({
    pull_request: {
      branches: ["dev", "main"],
      types: ["opened", "reopened", "synchronize", "edited"],
    },
  });
  expect(Object.keys(workflow.jobs)).toEqual(["checks"]);
  expect(job.name).toBe("Nucleus Rust checks");
  expect(workflow.permissions).toEqual({ contents: "read" });
  expect(job.permissions).toBeUndefined();
  expect(job.if).toBeUndefined();
  expect(job["continue-on-error"]).toBeUndefined();
  expect(workflow.defaults.run.shell).toBe("bash");
  expect(job.steps[0]!.with).toEqual({ "persist-credentials": false });
  for (const step of job.steps) {
    expect(step.if).toBeUndefined();
    expect(step["continue-on-error"]).toBeUndefined();
    if (step.uses) expect(step.uses).toMatch(/^[\w-]+\/[\w-]+@[0-9a-f]{40}$/);
  }
  expect(format).toBe("cargo +" + nightly + " fmt --all -- --check");
  expect(clippy).toBe(
    "cargo clippy --workspace --all-targets --locked -- -D warnings",
  );
});

test("both actual formatter consumers install the nightly owned by the Rust rules", () => {
  const install =
    "rustup toolchain install " +
    nightly +
    " --profile minimal --component rustfmt";
  for (const consumer of [job, maintenance.jobs.tools!])
    expect(
      consumer.steps.find((step) => step.name === "Install nightly rustfmt")
        ?.run,
    ).toBe(install);
});

test("the candidate record accepts only the actual event checkout", () => {
  const command = job.steps.find(
    (step) => step.name === "Record candidate and tools",
  )!.run!;
  const head = spawnSync("git", ["rev-parse", "HEAD"], {
    cwd: root,
    encoding: "utf8",
  });
  expect(head.status).toBe(0);
  const matching = run(command, root, {
    ...process.env,
    GITHUB_SHA: head.stdout.trim(),
  });
  expect(matching.error).toBeUndefined();
  expect(matching.status, matching.stdout + matching.stderr).toBe(0);
  const mismatch = run(command, root, {
    ...process.env,
    GITHUB_SHA: "0".repeat(40),
  });
  expect(mismatch.error).toBeUndefined();
  expect(mismatch.status).toBe(1);
}, 60_000);

test.each([
  [
    "format accepts a valid workspace",
    format,
    "pub fn number() -> u8 {\n    1\n}\n",
    0,
    undefined,
  ],
  [
    "format rejects actual source formatting",
    format,
    "pub fn number()->u8{1}\n",
    1,
    /Diff in/,
  ],
  [
    "Clippy accepts a valid workspace",
    clippy,
    "pub fn number() -> u8 {\n    1\n}\n",
    0,
    undefined,
  ],
  [
    "Clippy rejects an actual lint",
    clippy,
    "pub fn number() -> u8 {\n    Some(1).unwrap()\n}\n",
    101,
    /unnecessary.literal.unwrap|used.*unwrap/,
  ],
] as const)(
  "%s and preserves shell failure propagation",
  (_, command, source, status, diagnostic) => {
    const { directory, env } = fixture(source);
    const lock = readFileSync(join(directory, "Cargo.lock"));
    const result = run(
      command + "\nprintf 'SUCCESS_AFTER_CHECK\\n'",
      directory,
      env,
    );
    expect(result.error).toBeUndefined();
    expect(result.status, result.stdout + result.stderr).toBe(status);
    expect(readFileSync(join(directory, "Cargo.lock"))).toEqual(lock);
    if (diagnostic) {
      expect(result.stdout + result.stderr).toMatch(diagnostic);
      expect(result.stdout).not.toContain("SUCCESS_AFTER_CHECK");
    } else expect(result.stdout).toContain("SUCCESS_AFTER_CHECK");
  },
  60_000,
);
