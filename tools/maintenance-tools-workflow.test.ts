import { afterEach, expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import {
  copyFileSync,
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

const root = fileURLToPath(new URL("../", import.meta.url));
type Step = {
  run?: string;
  uses?: string;
  with?: Record<string, unknown>;
  if?: unknown;
  "continue-on-error"?: unknown;
};
const workflow = Bun.YAML.parse(
  readFileSync(join(root, ".github/workflows/maintenance-tools.yml"), "utf8"),
) as {
  on: unknown;
  permissions: unknown;
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
const job = workflow.jobs.tools!;
const commands = job.steps.slice(-3).map((step) => step.run!);
const directories: string[] = [];

afterEach(() => {
  for (const directory of directories.splice(0))
    rmSync(directory, { recursive: true, force: true });
});

function fixture() {
  const directory = mkdtempSync(join(tmpdir(), "nucleus maintenance "));
  directories.push(directory);
  mkdirSync(join(directory, "tools"));
  for (const path of [
    "package.json",
    ".tool-versions",
    "tsconfig.json",
    "tools/check-bun.ts",
    "tools/check-dependencies.ts",
    "rust-toolchain.toml",
  ])
    copyFileSync(join(root, path), join(directory, path));
  symlinkSync(
    join(root, "node_modules"),
    join(directory, "node_modules"),
    "dir",
  );
  return directory;
}

function run(command: string, cwd: string) {
  return spawnSync(
    "bash",
    [
      "--noprofile",
      "--norc",
      "-eo",
      "pipefail",
      "-c",
      command + "\nprintf 'UNEXPECTED_SUCCESS_AFTER_CHECK\\n'",
    ],
    {
      cwd,
      env: { ...process.env, CARGO_NET_OFFLINE: "true" },
      encoding: "utf8",
      timeout: 30_000,
    },
  );
}

function rejected(result: ReturnType<typeof run>, diagnostic: RegExp) {
  expect(result.error).toBeUndefined();
  expect(result.status, result.stdout + result.stderr).toBe(1);
  expect(result.stdout + result.stderr).toMatch(diagnostic);
  expect(result.stdout).not.toContain("UNEXPECTED_SUCCESS_AFTER_CHECK");
}

test("all dev/main PR changes and retarget edits execute one read-only job without success skips", () => {
  expect(workflow.on).toEqual({
    pull_request: {
      branches: ["dev", "main"],
      types: ["opened", "reopened", "synchronize", "edited"],
    },
  });
  expect(Object.keys(workflow.jobs)).toEqual(["tools"]);
  expect(job.name).toBe("Nucleus Maintenance tools");
  expect(workflow.permissions).toEqual({ contents: "read" });
  expect(job.permissions).toBeUndefined();
  expect(job.if).toBeUndefined();
  expect(job["continue-on-error"]).toBeUndefined();
  for (const step of job.steps) {
    expect(step.if).toBeUndefined();
    expect(step["continue-on-error"]).toBeUndefined();
  }
  expect(job.steps[0]!.with).toEqual({ "persist-credentials": false });
  expect(
    job.steps.find((step) => step.uses?.startsWith("actions/setup-node@"))
      ?.with,
  ).toEqual({
    "node-version-file": ".tool-versions",
    "package-manager-cache": false,
  });
  for (const step of job.steps.filter((step) => step.uses))
    expect(step.uses).toMatch(/^[\w-]+\/[\w-]+@[0-9a-f]{40}$/);
  expect(commands).toEqual([
    "bun run check:tools",
    "bun test tools/*.test.ts",
    "bun tools/check-dependencies.ts",
  ]);
});

test("the actual type check rejects a TypeScript error and stops the shell", () => {
  const directory = fixture();
  writeFileSync(
    join(directory, "tools/invalid.ts"),
    'const value: number = "invalid";\n',
  );
  const result = run(commands[0]!, directory);
  expect(result.error).toBeUndefined();
  expect(result.status).toBeGreaterThan(0);
  expect(result.stdout + result.stderr).toMatch(/TS2322/);
  expect(result.stdout).not.toContain("UNEXPECTED_SUCCESS_AFTER_CHECK");
}, 30_000);

test("the test glob consumes a failing additional tool test and stops the shell", () => {
  const directory = fixture();
  writeFileSync(
    join(directory, "tools/first.test.ts"),
    'import { test, expect } from "bun:test"; test("first", () => expect(1).toBe(1));\n',
  );
  writeFileSync(
    join(directory, "tools/additional.test.ts"),
    'import { test, expect } from "bun:test"; test("additional", () => expect("actual").toBe("expected"));\n',
  );
  rejected(run(commands[1]!, directory), /1 pass[\s\S]*1 fail/);
}, 30_000);

test("the actual default dependency command rejects a production fixture dependency without changing the lock", () => {
  const directory = fixture();
  writeFileSync(
    join(directory, "Cargo.toml"),
    '[workspace]\nresolver = "3"\nmembers = ["nucleus", "fixture"]\n',
  );
  for (const name of ["nucleus", "fixture"]) {
    mkdirSync(join(directory, name, "src"), { recursive: true });
    writeFileSync(join(directory, name, "src/lib.rs"), "");
    writeFileSync(
      join(directory, name, "Cargo.toml"),
      '[package]\nname = "' +
        name +
        '"\nversion = "0.1.0"\nedition = "2024"\n' +
        (name === "nucleus"
          ? '[dependencies]\nfixture = { path = "../fixture" }\n'
          : ""),
    );
  }
  const locked = spawnSync("cargo", ["generate-lockfile", "--offline"], {
    cwd: directory,
    encoding: "utf8",
    timeout: 30_000,
  });
  expect(locked.status, locked.stderr).toBe(0);
  const before = readFileSync(join(directory, "Cargo.lock"));
  rejected(run(commands[2]!, directory), /fixture/);
  expect(readFileSync(join(directory, "Cargo.lock"))).toEqual(before);
}, 30_000);
