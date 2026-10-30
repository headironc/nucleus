import { afterEach, expect, test } from "bun:test";
import { data, Evaluator, Lexer, Parser } from "@actions/expressions";
import { truthy } from "@actions/expressions/result";
import { spawnSync } from "node:child_process";
import {
  copyFileSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../", import.meta.url));
type Step = {
  id?: string;
  env?: Record<string, string>;
  name?: string;
  run?: string;
  uses?: string;
  with?: Record<string, unknown>;
  if?: string;
  "continue-on-error"?: unknown;
};
type Workflow = {
  on: unknown;
  concurrency: unknown;
  permissions: unknown;
  defaults: { run: { shell: string } };
  jobs: Record<
    string,
    {
      name: string;
      "runs-on": string;
      "timeout-minutes": number;
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
const record = job.steps.find(
  (step) => step.name === "Record candidate and Bun",
)!;
const classify = job.steps.find((step) => step.id === "inputs")!;
const selection = job.steps.find(
  (step) => step.name === "Validate Rust selection",
)!;
const rustSteps = job.steps.slice(job.steps.indexOf(selection) + 1);
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

test("every dev/main PR classifies the verified candidate in one read-only job", () => {
  expect(workflow.on).toEqual({
    pull_request: {
      branches: ["dev", "main"],
      types: ["opened", "reopened", "synchronize", "edited"],
    },
  });
  expect(Object.keys(workflow.jobs)).toEqual(["checks"]);
  expect(job.name).toBe("Nucleus Rust checks");
  expect(job["runs-on"]).toBe("ubuntu-24.04");
  expect(job["timeout-minutes"]).toBe(30);
  expect(workflow.concurrency).toEqual({
    group: "${{ github.workflow }}-pr-${{ github.event.pull_request.number }}",
    "cancel-in-progress": true,
  });
  expect(workflow.permissions).toEqual({ contents: "read" });
  expect(job.permissions).toBeUndefined();
  expect(job.if).toBeUndefined();
  expect(job["continue-on-error"]).toBeUndefined();
  expect(workflow.defaults.run.shell).toBe("bash");
  expect(job.steps[0]!.with).toEqual({
    "persist-credentials": false,
    "fetch-depth": 0,
  });
  expect(job.steps.indexOf(record)).toBeLessThan(job.steps.indexOf(classify));
  expect(job.steps.indexOf(classify)).toBeLessThan(
    job.steps.indexOf(selection),
  );
  expect(classify.env).toEqual({
    EVENT_NAME: "${{ github.event_name }}",
    BASE_SHA: "${{ github.event.pull_request.base.sha }}",
    HEAD_SHA: "${{ github.event.pull_request.head.sha }}",
    MERGE_SHA: "${{ github.sha }}",
  });
  expect(selection.env).toEqual({ TIER: "${{ steps.inputs.outputs.tier }}" });
  for (const step of job.steps) {
    if (!rustSteps.includes(step)) expect(step.if).toBeUndefined();
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
  const command = record.run!;
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

function enabled(
  step: Step,
  tier: string,
  status: "success" | "failure" | "cancelled",
) {
  const success = {
    name: "success",
    minArgs: 0,
    maxArgs: 0,
    call: () => new data.BooleanData(status === "success"),
  };
  const parsed = new Parser(
    new Lexer(step.if!.slice(3, -2)).lex().tokens,
    ["steps"],
    [success],
  ).parse();
  const context = new data.Dictionary({
    key: "steps",
    value: new data.Dictionary({
      key: "inputs",
      value: new data.Dictionary({
        key: "outputs",
        value: new data.Dictionary({
          key: "tier",
          value: new data.StringData(tier),
        }),
      }),
    }),
  });
  return truthy(
    new Evaluator(parsed, context, new Map([["success", success]])).evaluate(),
  );
}

function candidate(
  changes: (write: (path: string) => void, directory: string) => void,
) {
  const directory = mkdtempSync(join(tmpdir(), "nucleus rust selection "));
  directories.push(directory);
  const env = {
    ...Object.fromEntries(
      Object.entries(process.env).filter(([key]) => !key.startsWith("GIT_")),
    ),
    GIT_CONFIG_NOSYSTEM: "1",
    GIT_CONFIG_GLOBAL: "/dev/null",
    GIT_AUTHOR_NAME: "Nucleus fixture",
    GIT_AUTHOR_EMAIL: "fixture@example.invalid",
    GIT_COMMITTER_NAME: "Nucleus fixture",
    GIT_COMMITTER_EMAIL: "fixture@example.invalid",
  };
  function git(...args: string[]) {
    const result = spawnSync(
      "git",
      ["-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null", ...args],
      {
        cwd: directory,
        env,
        encoding: "utf8",
        timeout: 5000,
      },
    );
    expect(result.error).toBeUndefined();
    expect(result.status, result.stderr).toBe(0);
    return result.stdout.trim();
  }
  function write(path: string) {
    mkdirSync(dirname(join(directory, path)), { recursive: true });
    writeFileSync(join(directory, path), "candidate\n");
  }
  git("init", "--initial-branch=base", "--object-format=sha1", "--template=");
  write("src/removed.rs");
  write("docs/renamed.md");
  mkdirSync(join(directory, "tools"));
  for (const file of [
    "tools/check-inputs.ts",
    "tools/check-bun.ts",
    ".tool-versions",
    "package.json",
  ])
    copyFileSync(join(root, file), join(directory, file));
  git("add", ".");
  git("commit", "-m", "Create fixture");
  const base = git("rev-parse", "HEAD");
  git("switch", "-c", "feature");
  changes(write, directory);
  git("add", ".");
  git("commit", "--allow-empty", "-m", "Change fixture");
  const head = git("rev-parse", "HEAD");
  git("switch", "base");
  git("merge", "--no-ff", "--no-edit", "feature");
  const merge = git("rev-parse", "HEAD");
  return {
    directory,
    env: {
      ...env,
      EVENT_NAME: "pull_request",
      BASE_SHA: base,
      HEAD_SHA: head,
      MERGE_SHA: merge,
      GITHUB_SHA: merge,
      GITHUB_OUTPUT: join(directory, "output"),
    },
  };
}

test.each([
  ["documentation", ["docs/guide.md"], "skip"],
  ["design", ["design/page.html"], "skip"],
  ["tools", ["tools/example.ts"], "tools"],
  ["format settings", ["rustfmt.toml"], "checks"],
  ["Rust", ["src/lib.rs"], "full"],
  ["shared classifier", ["tools/check-packages.ts"], "full"],
  ["unknown", ["unknown-input"], "full"],
  ["mixed", ["docs/guide.md", "tools/example.ts", "src/lib.rs"], "full"],
] as const)(
  "actual %s candidate selects only the required Rust steps",
  (_, paths, expected) => {
    const { directory, env } = candidate((write) => paths.forEach(write));
    const bound = run(record.run!, directory, env);
    expect(bound.status, bound.stderr).toBe(0);
    const result = run(classify.run!, directory, env);
    expect(result.status, result.stderr).toBe(0);
    const tier = readFileSync(env.GITHUB_OUTPUT, "utf8").match(
      /^tier=(.*)$/m,
    )![1]!;
    expect(tier).toBe(expected);
    const validated = run(selection.run!, directory, { ...env, TIER: tier });
    expect(validated.status, validated.stderr).toBe(0);
    const selected = rustSteps.filter((step) => enabled(step, tier, "success"));
    expect(selected.map((step) => step.name)).toEqual(
      expected === "skip" || expected === "tools"
        ? []
        : [
            "Install native build dependencies",
            "Install Rust toolchain",
            "Install nightly rustfmt",
            "Record Rust tools",
            "Check Rust formatting",
            "Check workspace Clippy",
          ],
    );
    if (selected.length === 0)
      expect(validated.stdout).toContain("Rust checks skipped:");
  },
);

test.each(["delete", "rename from Rust", "rename to Rust"])(
  "%s preserves Rust inputs",
  (operation) => {
    const { directory, env } = candidate((_, dir) => {
      if (operation === "delete") rmSync(join(dir, "src/removed.rs"));
      else if (operation === "rename from Rust")
        renameSync(join(dir, "src/removed.rs"), join(dir, "docs/moved.md"));
      else renameSync(join(dir, "docs/renamed.md"), join(dir, "src/moved.rs"));
    });
    const result = run(classify.run!, directory, env);
    expect(result.status, result.stderr).toBe(0);
    expect(readFileSync(env.GITHUB_OUTPUT, "utf8")).toContain("tier=full\n");
  },
);

test("unverified comparison runs Rust while output failure fails classification", () => {
  const { directory, env } = candidate((write) => write("docs/guide.md"));
  const result = run(classify.run!, directory, {
    ...env,
    HEAD_SHA: "0".repeat(40),
  });
  expect(result.status, result.stderr).toBe(0);
  expect(result.stderr).toContain("Unverified comparison");
  expect(readFileSync(env.GITHUB_OUTPUT, "utf8")).toContain("tier=full\n");
  for (const GITHUB_OUTPUT of ["", directory]) {
    const failed = run(classify.run!, directory, { ...env, GITHUB_OUTPUT });
    expect(failed.status).toBe(1);
  }
});

test.each(["", "unknown", "FULL"])(
  "selection rejects invalid tier %j",
  (TIER) => {
    expect(run(selection.run!, root, { ...process.env, TIER }).status).toBe(1);
  },
);

test.each(["skip", "tools", "checks", "full", "", "unknown"])(
  "tier %j cannot bypass failure or cancellation",
  (tier) => {
    for (const status of ["failure", "cancelled"] as const)
      for (const step of rustSteps)
        expect(enabled(step, tier, status)).toBe(false);
  },
);

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
