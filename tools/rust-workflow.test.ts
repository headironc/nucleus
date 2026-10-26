import { afterEach, expect, test } from "bun:test";
import { data, Evaluator, Lexer, Parser } from "@actions/expressions";
import { truthy } from "@actions/expressions/result";
import { spawnSync } from "node:child_process";
import {
  chmodSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";

const workflow = Bun.YAML.parse(
  readFileSync(
    new URL("../.github/workflows/rust.yml", import.meta.url),
    "utf8",
  ),
) as {
  on: Record<string, unknown>;
  concurrency?: { group: string; "cancel-in-progress": boolean };
  jobs: Record<
    string,
    {
      name?: string;
      if?: string;
      needs?: string | string[];
      "continue-on-error"?: boolean;
      outputs?: Record<string, string>;
      strategy?: {
        "fail-fast": boolean;
        "max-parallel": number;
        matrix: string;
      };
      steps: {
        id?: string;
        if?: string;
        "continue-on-error"?: boolean;
        run?: string;
        env?: Record<string, string>;
        uses?: string;
        with?: Record<string, unknown>;
      }[];
    }
  >;
};

const directories: string[] = [];
const environment = {
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

afterEach(() => {
  for (const directory of directories.splice(0)) {
    rmSync(directory, { recursive: true, force: true });
  }
});

function temporaryDirectory() {
  const directory = mkdtempSync(join(tmpdir(), "nucleus workflow "));
  directories.push(directory);
  return directory;
}

function repository() {
  const directory = temporaryDirectory();
  function git(args: string[]) {
    const result = spawnSync(
      "git",
      ["-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null", ...args],
      { cwd: directory, env: environment, encoding: "utf8", timeout: 5_000 },
    );
    expect(result.error).toBeUndefined();
    expect(result.status, result.stderr).toBe(0);
    return result.stdout.trim();
  }
  function write(path: string, content = "changed\n") {
    const destination = join(directory, path);
    mkdirSync(dirname(destination), { recursive: true });
    writeFileSync(destination, content);
  }
  git(["init", "--initial-branch=base", "--object-format=sha1", "--template="]);
  mkdirSync(join(directory, "tools"));
  copyFileSync(
    new URL("./check-inputs.ts", import.meta.url),
    join(directory, "tools/check-inputs.ts"),
  );
  write("docs/guide.md", "# Guide\n");
  write("src/lib.rs", "pub fn value() {}\n");
  write("design/page.html", "<!doctype html><title>Prototype</title>\n");
  write("assets/avatars/0-cat.webp", "runtime asset fixture\n");
  write("tools/check-bun.ts", "original tool fixture\n");
  write("rustfmt.toml", "original format fixture\n");
  git(["add", "."]);
  git(["commit", "-m", "Create fixture"]);
  git(["switch", "-c", "feature"]);
  function merge(squash = false) {
    git(["add", "."]);
    git(["commit", "--allow-empty", "-m", "Change fixture"]);
    const head = git(["rev-parse", "HEAD"]);
    git(["switch", "base"]);
    const base = git(["rev-parse", "HEAD"]);
    git([
      "merge",
      ...(squash ? ["--squash"] : ["--no-ff", "--no-edit"]),
      "feature",
    ]);
    if (squash) git(["commit", "--allow-empty", "-m", "Create queue candidate"]);
    return {
      EVENT_NAME: "pull_request",
      BASE_SHA: base,
      HEAD_SHA: head,
      MERGE_SHA: git(["rev-parse", "HEAD"]),
    };
  }
  function queue() {
    const revisions = merge(true);
    return {
      EVENT_NAME: "merge_group",
      BASE_SHA: revisions.BASE_SHA,
      QUEUE_SHA: revisions.MERGE_SHA,
      MERGE_SHA: revisions.MERGE_SHA,
    };
  }
  return { directory, git, write, merge, queue };
}

function classify(directory: string, revisions: NodeJS.ProcessEnv) {
  const step = workflow.jobs.changes.steps.find(
    (step) => step.id === "changes",
  );
  expect(step?.run).toBeString();
  const output = join(directory, ".git", "classification");
  writeFileSync(output, "");
  const result = spawnSync(
    "bash",
    ["--noprofile", "--norc", "-c", step!.run!],
    {
      cwd: directory,
      env: { ...environment, ...revisions, GITHUB_OUTPUT: output },
      encoding: "utf8",
      timeout: 5_000,
    },
  );
  expect(result.error).toBeUndefined();
  expect(result.status, result.stderr).toBe(0);
  const lines = readFileSync(output, "utf8").split("\n");
  expect(lines.some((line) => /^halo=(run|skip)$/.test(line))).toBe(true);
  return lines.find((line) => line.startsWith("tier=")) + "\n";
}

test.each([
  "AGENTS.md",
  "README.md",
  ".gitignore",
  ".prettierrc.json",
  ".markdownlint.jsonc",
  "docs/guide.md",
  "docs/nested/space ' and\nnewline.md",
  "design/orbit-pages.html",
  "design/assets/orbit.css",
  "design/assets/halo-home.js",
  "design/README.md",
  "design/assets/lucide-LICENSE.txt",
  "design/assets/illustration.svg",
  "design/assets/image.png",
  "design/assets/image.jpg",
  "design/assets/image.jpeg",
  "design/assets/image.webp",
  "design/assets/image.avif",
  "design/assets/image.gif",
  "design/nested/space ' and\nnewline.html",
  "halo/app/routes/codes.tsx",
  "halo/bun.lock",
  ".github/workflows/halo.yml",
])("an input owned by Docs or Halo can skip Rust checks: %s", (path) => {
  const fixture = repository();
  fixture.write(path);
  const revisions = fixture.merge();
  expect(classify(fixture.directory, revisions)).toBe("tier=skip\n");
  expect(fixture.git(["status", "--porcelain"])).toBe("");
});

test.each([
  "src/lib.rs",
  "Cargo.toml",
  "Cargo.lock",
  "rust-toolchain.toml",
  "tools/check-packages.ts",
  ".github/workflows/rust.yml",
  ".github/workflows/cache.yml",
  ".github/workflows/new-review.yml",
  ".github/actions/rust-setup/action.yml",
  "fixture/postgresql.version",
  "deployment/migrations/V1__keys.sql",
  "assets/avatars/0-cat.webp",
  "docs/input.json",
  "design/build.rs",
  "design/package.json",
  "design/assets/module.mjs",
  "design/assets/client.ts",
  "design-tools/preview.html",
  "orbit/src/app.ts",
  "unknown.txt",
  "src/space ' and\nnewline.md",
])("a mixed merge keeps full validation for %s", (path) => {
  const fixture = repository();
  fixture.write("docs/guide.md");
  fixture.write(path);
  expect(classify(fixture.directory, fixture.merge())).toBe("tier=full\n");
});

const toolsOnly = [
  "tools/check-bun.ts",
  "tools/check-bun.test.ts",
  "tools/rust-workflow.test.ts",
  "tools/check-packages.test.ts",
  "tools/check-dependencies.ts",
  "tools/check-dependencies.test.ts",
  "tools/space ' and\nnewline.ts",
  ".tool-versions",
  "package.json",
  "bun.lock",
  "tsconfig.json",
  ".github/workflows/docs.yml",
  ".github/workflows/review-gate.yml",
  ".github/workflows/review-context.yml",
  ".github/workflows/review-activity.yml",
];

test.each(toolsOnly)(
  "a tool or tool configuration change runs only the tools job: %s",
  (path) => {
    for (const companions of [[], ["docs/guide.md", "design/page.html"]]) {
      const fixture = repository();
      for (const companion of companions) fixture.write(companion);
      fixture.write(path);
      expect(classify(fixture.directory, fixture.merge())).toBe(
        "tier=tools\n",
      );
    }
  },
);

test.each([
  [".github/workflows/review-gate.yml", "tools"],
  [".github/workflows/review-context.yml", "tools"],
  [".github/workflows/review-activity.yml", "tools"],
  [".github/workflows/new-review.yml", "full"],
] as const)("review workflow classification preserves full Rust consumers: %s", (path, tier) => {
  for (const queue of [false, true]) {
    for (const rust of [false, true]) {
      const fixture = repository();
      fixture.write(path);
      if (rust) fixture.write("src/lib.rs", "changed consumer source\n");
      const revisions = queue ? fixture.queue() : fixture.merge();
      expect(classify(fixture.directory, revisions)).toBe(
        `tier=${rust ? "full" : tier}\n`,
      );
    }
  }
});

test("mixed inputs select the highest Rust tier", () => {
  for (const [inputs, tier] of [
    [["docs/guide.md", "tools/check-bun.ts"], "tools"],
    [["rustfmt.toml"], "checks"],
    [["rustfmt.toml", "tools/check-bun.ts", "docs/guide.md"], "checks"],
    [["src/lib.rs", "rustfmt.toml", "tools/check-bun.ts"], "full"],
    [["tools/check-bun.ts", "unknown.txt", "rustfmt.toml"], "full"],
  ] as const) {
    const fixture = repository();
    for (const input of inputs) fixture.write(input);
    expect(classify(fixture.directory, fixture.merge())).toBe(`tier=${tier}\n`);
  }
});

test("any full input outranks tools and skipped inputs", () => {
  for (const full of ["tools/check-packages.ts", "src/lib.rs", "unknown.txt"]) {
    for (const order of [
      ["tools/check-bun.ts", full],
      [full, "tools/check-bun.ts"],
    ]) {
      const fixture = repository();
      fixture.write("docs/guide.md");
      for (const path of order) fixture.write(path);
      expect(classify(fixture.directory, fixture.merge())).toBe("tier=full\n");
    }
  }
  // A deleted package checker is still a full input.
  const fixture = repository();
  fixture.write("tools/check-packages.ts");
  fixture.git(["add", "."]);
  fixture.git(["commit", "-m", "Add checker"]);
  fixture.git(["switch", "base"]);
  fixture.git(["merge", "--ff-only", "feature"]);
  fixture.git(["switch", "feature"]);
  rmSync(join(fixture.directory, "tools/check-packages.ts"));
  expect(classify(fixture.directory, fixture.merge())).toBe("tier=full\n");
});

test("PR and queue deletions and renames preserve every tier boundary", () => {
  for (const queue of [false, true]) {
    for (const [source, destination, tier] of [
      ["tools/check-bun.ts", "", "tools"],
      ["rustfmt.toml", "", "checks"],
      ["tools/check-bun.ts", "docs/tool.md", "tools"],
      ["docs/guide.md", "tools/guide.ts", "tools"],
      ["tools/check-bun.ts", "rustfmt.toml", "checks"],
      ["rustfmt.toml", "tools/format.ts", "checks"],
      ["tools/check-bun.ts", "src/tool.rs", "full"],
      ["src/lib.rs", "tools/source.ts", "full"],
    ] as const) {
      const fixture = repository();
      fixture.write("docs/guide.md", "mixed documentation\n");
      if (destination) {
        renameSync(
          join(fixture.directory, source),
          join(fixture.directory, destination),
        );
      } else rmSync(join(fixture.directory, source));
      const revisions = queue ? fixture.queue() : fixture.merge();
      expect(classify(fixture.directory, revisions)).toBe(`tier=${tier}\n`);
    }
  }
});

test("design, documentation and their metadata can change together without Rust inputs", () => {
  const fixture = repository();
  for (const path of [
    "design/orbit-pages.html",
    "design/assets/orbit.css",
    "design/assets/logo.png",
    "docs/client/pages.md",
    "README.md",
    ".gitignore",
  ]) {
    fixture.write(path);
  }
  const revisions = fixture.merge();
  expect(classify(fixture.directory, revisions)).toBe("tier=skip\n");
  fixture.write("deployment/migrations/V1__keys.sql");
  fixture.git(["add", "."]);
  fixture.git(["commit", "--amend", "--no-edit"]);
  expect(
    classify(fixture.directory, {
      ...revisions,
      MERGE_SHA: fixture.git(["rev-parse", "HEAD"]),
    }),
  ).toBe("tier=full\n");
});

test("deleted sources and both sides of a rename remain in the comparison", () => {
  for (const destination of ["docs/renamed.md", "src/renamed.rs"]) {
    const fixture = repository();
    renameSync(
      join(fixture.directory, "docs/guide.md"),
      join(fixture.directory, destination),
    );
    expect(classify(fixture.directory, fixture.merge())).toBe(
      destination.startsWith("docs/") ? "tier=skip\n" : "tier=full\n",
    );
  }
  for (const source of [
    "docs/guide.md",
    "design/page.html",
    "src/lib.rs",
    "assets/avatars/0-cat.webp",
  ]) {
    const fixture = repository();
    rmSync(join(fixture.directory, source));
    expect(classify(fixture.directory, fixture.merge())).toBe(
      source.startsWith("docs/") || source.startsWith("design/")
        ? "tier=skip\n"
        : "tier=full\n",
    );
  }
  const fixture = repository();
  renameSync(
    join(fixture.directory, "src/lib.rs"),
    join(fixture.directory, "docs/source.md"),
  );
  expect(classify(fixture.directory, fixture.merge())).toBe("tier=full\n");
});

test("renames across runtime and design boundaries keep Rust validation", () => {
  for (const [source, destination] of [
    ["assets/avatars/0-cat.webp", "design/assets/cat.webp"],
    ["design/page.html", "assets/page.html"],
    ["src/lib.rs", "design/source.txt"],
    ["design/page.html", "design/renamed.html"],
  ]) {
    const fixture = repository();
    mkdirSync(dirname(join(fixture.directory, destination)), {
      recursive: true,
    });
    renameSync(
      join(fixture.directory, source),
      join(fixture.directory, destination),
    );
    expect(classify(fixture.directory, fixture.merge())).toBe(
      source.startsWith("design/") && destination.startsWith("design/")
        ? "tier=skip\n"
        : "tier=full\n",
    );
  }
});

test("a successful empty comparison is distinct from an unavailable comparison", () => {
  const fixture = repository();
  const revisions = fixture.merge();
  expect(classify(fixture.directory, revisions)).toBe("tier=skip\n");
  for (const variable of ["HEAD_SHA", "MERGE_SHA"]) {
    for (const value of ["", "missing-commit", "0".repeat(40)]) {
      expect(
        classify(fixture.directory, { ...revisions, [variable]: value }),
      ).toBe("tier=full\n");
    }
  }
  expect(classify(fixture.directory, {})).toBe("tier=full\n");
});

test("mismatched parents, candidate and a failed diff require all checks", () => {
  const fixture = repository();
  fixture.write("docs/guide.md");
  const revisions = fixture.merge();
  for (const mismatch of [
    { HEAD_SHA: revisions.BASE_SHA },
    { MERGE_SHA: revisions.HEAD_SHA },
  ]) {
    expect(classify(fixture.directory, { ...revisions, ...mismatch })).toBe(
      "tier=full\n",
    );
  }
  // Parent IDs still resolve, but the base tree is unavailable to git diff.
  const tree = fixture.git(["rev-parse", `${revisions.BASE_SHA}^{tree}`]);
  rmSync(
    join(fixture.directory, ".git/objects", tree.slice(0, 2), tree.slice(2)),
  );
  expect(classify(fixture.directory, revisions)).toBe("tier=full\n");
});

test("depth two supplies both merge parents; depth one falls back to full checks", () => {
  const fixture = repository();
  fixture.write("docs/guide.md");
  const revisions = fixture.merge();
  for (const depth of [1, 2]) {
    const shallow = temporaryDirectory();
    fixture.git([
      "clone",
      "--no-local",
      `--depth=${depth}`,
      fixture.directory,
      shallow,
    ]);
    expect(classify(shallow, revisions)).toBe(
      depth === 2 ? "tier=skip\n" : "tier=full\n",
    );
  }
});

test("classification compares the tested merge tree to its actual target", () => {
  const fixture = repository();
  const oldBase = fixture.git(["rev-parse", "base"]);
  fixture.git(["switch", "base"]);
  fixture.write("src/lib.rs", "pub fn newer_base() {}\n");
  fixture.git(["add", "."]);
  fixture.git(["commit", "-m", "Advance target"]);
  fixture.git(["switch", "feature"]);
  fixture.write("docs/guide.md");
  const revisions = fixture.merge();
  expect(classify(fixture.directory, revisions)).toBe("tier=skip\n");
  expect(classify(fixture.directory, { ...revisions, BASE_SHA: oldBase })).toBe(
    "tier=skip\n",
  );
  // A change present only in the merge tree must still require Rust validation.
  fixture.write("src/lib.rs", "pub fn merge_change() {}\n");
  fixture.git(["add", "."]);
  fixture.git(["commit", "--amend", "--no-edit"]);
  expect(
    classify(fixture.directory, {
      ...revisions,
      MERGE_SHA: fixture.git(["rev-parse", "HEAD"]),
    }),
  ).toBe("tier=full\n");
});

test("the required rust gate accepts only the exact status table for all four tiers", () => {
  const gate = workflow.jobs.rust;
  expect(gate.if).toBe("${{ always() }}");
  expect(gate.needs).toEqual(["changes", "tools", "checks", "packages"]);
  expect(gate["continue-on-error"]).not.toBe(true);
  expect(gate.steps).toHaveLength(1);
  const step = gate.steps[0];
  expect(step.env).toEqual({
    CHANGES_RESULT: "${{ needs.changes.result }}",
    TIER: "${{ needs.changes.outputs.tier }}",
    TOOLS_RESULT: "${{ needs.tools.result }}",
    CHECKS_RESULT: "${{ needs.checks.result }}",
    PACKAGES_RESULT: "${{ needs.packages.result }}",
  });
  expect(step.run).toBeString();
  // Execute the workflow's actual shell, including its failure propagation.
  const outcomes = [
    "success",
    "failure",
    "cancelled",
    "skipped",
    "neutral",
    "",
  ];
  for (const changes of outcomes) {
    for (const tier of ["full", "checks", "tools", "skip", "", "true", "false"]) {
      for (const tools of outcomes) {
        for (const checks of outcomes) {
          for (const packages of outcomes) {
            const result = spawnSync(
              "bash",
              ["--noprofile", "--norc", "-c", step.run!],
              {
                env: {
                  ...environment,
                  CHANGES_RESULT: changes,
                  TIER: tier,
                  TOOLS_RESULT: tools,
                  CHECKS_RESULT: checks,
                  PACKAGES_RESULT: packages,
                },
                encoding: "utf8",
                timeout: 5_000,
              },
            );
            expect(result.error).toBeUndefined();
            const expected = {
              full: ["success", "success", "success"],
              checks: ["success", "success", "skipped"],
              tools: ["success", "skipped", "skipped"],
              skip: ["skipped", "skipped", "skipped"],
            }[tier];
            const accepted =
              changes === "success" &&
              expected?.[0] === tools &&
              expected[1] === checks &&
              expected[2] === packages;
            expect(
              result.status,
              `changes=${changes}, tier=${tier}, tools=${tools}, checks=${checks}, packages=${packages}: ${result.stderr}`,
            ).toBe(accepted ? 0 : 1);
          }
        }
      }
    }
  }
}, 120_000);

test("PR and queue runs have separate cancellation groups", () => {
  const expected = {
    group: "${{ github.workflow }}-${{ github.event_name }}-${{ github.event.pull_request.number || github.ref }}",
    "cancel-in-progress": true,
  };
  expect(workflow.concurrency).toEqual(expected);
  const docs = Bun.YAML.parse(
    readFileSync(
      new URL("../.github/workflows/docs.yml", import.meta.url),
      "utf8",
    ),
  ) as { on: Record<string, unknown>; concurrency?: unknown };
  expect(docs.on).toEqual({ pull_request: null, merge_group: { types: ["checks_requested"] } });
  expect(docs.concurrency).toEqual(expected);
});

test.each([
  ["docs/guide.md", "skip"],
  ["design/page.html", "skip"],
  ["tools/check-bun.ts", "tools"],
  ["rustfmt.toml", "checks"],
  ["src/lib.rs", "full"],
  ["Cargo.toml", "full"],
  [".github/workflows/rust.yml", "full"],
  ["unknown.txt", "full"],
])("a queue candidate reuses the input tiers for %s", (path, tier) => {
  const fixture = repository();
  fixture.write("docs/guide.md", "# Updated guide\n");
  fixture.write(path);
  expect(classify(fixture.directory, fixture.queue())).toBe(`tier=${tier}\n`);
});

test("queue classification includes earlier changes in the combined candidate", () => {
  const fixture = repository();
  fixture.write("src/lib.rs", "pub fn earlier_change() {}\n");
  fixture.git(["add", "."]);
  fixture.git(["commit", "-m", "Add earlier Rust change"]);
  fixture.write("docs/guide.md", "# Later documentation change\n");
  expect(classify(fixture.directory, fixture.queue())).toBe("tier=full\n");
});

test("queue classification retains deletions and both sides of renames", () => {
  for (const [source, destination, tier] of [
    ["docs/guide.md", "docs/renamed.md", "skip"],
    ["src/lib.rs", "docs/source.md", "full"],
    ["src/lib.rs", "", "full"],
  ] as const) {
    const fixture = repository();
    if (destination) {
      renameSync(
        join(fixture.directory, source),
        join(fixture.directory, destination),
      );
    } else {
      rmSync(join(fixture.directory, source));
    }
    expect(classify(fixture.directory, fixture.queue())).toBe(`tier=${tier}\n`);
  }
});

test("queue classification binds the event head, checkout and single parent", () => {
  const fixture = repository();
  fixture.write("docs/guide.md");
  const revisions = fixture.queue();
  expect(classify(fixture.directory, revisions)).toBe("tier=skip\n");
  for (const variable of ["BASE_SHA", "QUEUE_SHA", "MERGE_SHA"]) {
    for (const value of ["", "missing-commit", "0".repeat(40)]) {
      expect(
        classify(fixture.directory, { ...revisions, [variable]: value }),
      ).toBe("tier=full\n");
    }
  }
  for (const mismatch of [
    { BASE_SHA: revisions.QUEUE_SHA },
    { QUEUE_SHA: revisions.BASE_SHA },
    { MERGE_SHA: revisions.BASE_SHA },
    { EVENT_NAME: "push" },
  ]) {
    expect(classify(fixture.directory, { ...revisions, ...mismatch })).toBe(
      "tier=full\n",
    );
  }
  const mergeFixture = repository();
  mergeFixture.write("docs/guide.md");
  const merge = mergeFixture.merge();
  expect(classify(mergeFixture.directory, {
    ...merge,
    EVENT_NAME: "merge_group",
    QUEUE_SHA: merge.MERGE_SHA,
  })).toBe("tier=full\n");
});

test("queue comparison falls back to full checks for missing history or trees", () => {
  const fixture = repository();
  fixture.write("docs/guide.md");
  const revisions = fixture.queue();
  for (const depth of [1, 2]) {
    const shallow = temporaryDirectory();
    fixture.git([
      "clone", "--no-local", `--depth=${depth}`, fixture.directory, shallow,
    ]);
    expect(classify(shallow, revisions)).toBe(
      depth === 2 ? "tier=skip\n" : "tier=full\n",
    );
  }
  const tree = fixture.git(["rev-parse", `${revisions.BASE_SHA}^{tree}`]);
  rmSync(
    join(fixture.directory, ".git/objects", tree.slice(0, 2), tree.slice(2)),
  );
  expect(classify(fixture.directory, revisions)).toBe("tier=full\n");
});

test("queue classification excludes changes already in its verified baseline", () => {
  const fixture = repository();
  const oldBase = fixture.git(["rev-parse", "base"]);
  fixture.git(["switch", "base"]);
  fixture.write("src/lib.rs", "pub fn accepted_base_change() {}\n");
  fixture.git(["add", "."]);
  fixture.git(["commit", "-m", "Advance target"]);
  fixture.git(["switch", "feature"]);
  fixture.write("docs/guide.md");
  const queue = fixture.queue();
  expect(classify(fixture.directory, queue)).toBe("tier=skip\n");
  expect(classify(fixture.directory, { ...queue, BASE_SHA: oldBase })).toBe(
    "tier=full\n",
  );
});

test("an unknown event cannot use the pull request comparison", () => {
  const fixture = repository();
  fixture.write("docs/guide.md");
  const revisions = fixture.merge();
  expect(classify(fixture.directory, revisions)).toBe("tier=skip\n");
  expect(classify(fixture.directory, { ...revisions, EVENT_NAME: "push" })).toBe("tier=full\n");
});

test("classification always runs and all validation jobs consume its output directly", () => {
  expect(workflow.on).toEqual({ pull_request: null, merge_group: { types: ["checks_requested"] } });
  const changes = workflow.jobs.changes;
  expect(changes.if).toBeUndefined();
  expect(changes.needs).toBeUndefined();
  expect(changes["continue-on-error"]).not.toBe(true);
  expect(changes.outputs).toEqual({
    tier: "${{ steps.changes.outputs.tier }}",
    packages: "${{ steps.changes.outputs.packages }}",
    matrix: "${{ steps.packages.outputs.matrix }}",
  });
  const checkout = changes.steps.find((step) =>
    step.uses?.startsWith("actions/checkout@"),
  );
  expect(checkout?.with?.["fetch-depth"]).toBe(2);
  expect(checkout?.with?.["persist-credentials"]).toBe(false);
  const classify = changes.steps.find((step) => step.id === "changes");
  expect(classify?.env).toEqual({
    BASE_SHA: "${{ github.event.merge_group.base_sha }}",
    HEAD_SHA: "${{ github.event.pull_request.head.sha }}",
    QUEUE_SHA: "${{ github.event.merge_group.head_sha }}",
    MERGE_SHA: "${{ github.sha }}",
    EVENT_NAME: "${{ github.event_name }}",
  });
  expect(workflow.jobs.tools.needs).toBe("changes");
  // Missing or unexpected output runs every job, which the gate then rejects.
  expect(workflow.jobs.tools.if).toBe(
    "${{ needs.changes.outputs.tier != 'skip' }}",
  );
  expect(workflow.jobs.checks.needs).toBe("changes");
  expect(workflow.jobs.checks.if).toBe(
    "${{ needs.changes.outputs.tier != 'skip' && needs.changes.outputs.tier != 'tools' }}",
  );
  expect(workflow.jobs.packages.if).toBe(
    "${{ needs.changes.outputs.tier != 'skip' && needs.changes.outputs.tier != 'tools' && needs.changes.outputs.tier != 'checks' }}",
  );
});

test("dependency regression and boundary commands execute with Bun and propagate failures", () => {
  const steps = workflow.jobs.tools.steps;
  const regression = steps.find(
    (step) => step.run === "bun test tools/check-dependencies.test.ts",
  );
  const boundary = steps.find(
    (step) => step.run === "bun tools/check-dependencies.ts",
  );
  expect(regression).toBeDefined();
  expect(boundary).toBeDefined();
  expect(steps.indexOf(regression!)).toBeLessThan(steps.indexOf(boundary!));
  const directory = temporaryDirectory();
  const stub = join(directory, "bun");
  writeFileSync(
    stub,
    '#!/bin/bash\nprintf "%s\\n" "$@"\nexit "${COMMAND_STATUS:-0}"\n',
  );
  chmodSync(stub, 0o700);
  for (const [step, expected] of [
    [regression!, ["test", "tools/check-dependencies.test.ts"]],
    [boundary!, ["tools/check-dependencies.ts"]],
  ] as const) {
    for (const status of [0, 17]) {
      const result = spawnSync(
        "bash",
        ["--noprofile", "--norc", "-c", step.run!],
        {
          env: {
            ...environment,
            PATH: `${directory}:${process.env.PATH}`,
            COMMAND_STATUS: String(status),
          },
          encoding: "utf8",
          timeout: 5_000,
        },
      );
      expect(result.error).toBeUndefined();
      expect(result.status, result.stderr).toBe(status);
      expect(result.stdout.trim().split("\n")).toEqual([...expected]);
    }
  }
});

test("tool validation prepares pinned runtimes and covers every maintenance regression without backend setup", () => {
  const tools = workflow.jobs.tools;
  expect(tools["continue-on-error"]).toBeUndefined();
  const steps = tools.steps;
  for (const step of steps) {
    expect(step.if).toBeUndefined();
    expect(step["continue-on-error"]).toBeUndefined();
  }
  const setup = steps.find((step) => step.uses?.startsWith("oven-sh/setup-bun@"));
  expect(setup?.uses).toBe("oven-sh/setup-bun@0c5077e51419868618aeaa5fe8019c62421857d6");
  expect(setup?.with).toEqual({ "bun-version-file": ".tool-versions" });
  expect(steps.some((step) => step.uses?.includes("rust-setup") || step.uses?.includes("postgresql"))).toBe(false);
  expect(steps.some((step) => /apt-get|cargo (?:clippy|build|test|doc)|nightly-/.test(step.run ?? ""))).toBe(false);

  const regressions = steps.filter((step) => step.run?.startsWith("bun test "));
  const files = regressions.flatMap((step) => step.run!.trim().split(/\s+/).slice(2));
  expect(files.sort()).toEqual(
    readdirSync(new URL("./", import.meta.url))
      .filter((name) => name.endsWith(".test.ts") && name !== "preview-design.test.ts")
      .map((name) => `tools/${name}`)
      .sort(),
  );
  const install = steps.find((step) => step.run === "bun install --frozen-lockfile --ignore-scripts");
  const types = steps.find((step) => step.run === "bun run check:tools");
  const rust = steps.find((step) => step.run?.includes("rustup show active-toolchain"));
  expect(install).toBeDefined();
  expect(types).toBeDefined();
  expect(rust).toBeDefined();
  expect(steps.indexOf(setup!)).toBeLessThan(steps.indexOf(install!));
  expect(steps.indexOf(install!)).toBeLessThan(steps.indexOf(types!));
  expect(steps.indexOf(rust!)).toBeLessThan(steps.indexOf(regressions[0]));

  const directory = temporaryDirectory();
  for (const name of ["bun", "rustup"]) {
    const stub = join(directory, name);
    writeFileSync(stub, '#!/bin/bash\nprintf "%s\\n" "$*"\nexit "${COMMAND_STATUS:-0}"\n');
    chmodSync(stub, 0o700);
  }
  for (const [step, expected] of [
    [install!, ["install --frozen-lockfile --ignore-scripts"]],
    [types!, ["run check:tools"]],
    [rust!, ["show active-toolchain", "component add clippy"]],
    ...regressions.map((step) => [step, [step.run!.trim().slice("bun ".length)]] as const),
  ] as const) {
    for (const status of [0, 17]) {
      const result = spawnSync("bash", ["--noprofile", "--norc", "-e", "-o", "pipefail", "-c", step.run!], {
        env: { ...environment, PATH: `${directory}:${process.env.PATH}`, COMMAND_STATUS: String(status) },
        encoding: "utf8",
        timeout: 5_000,
      });
      expect(result.error).toBeUndefined();
      expect(result.status, result.stderr).toBe(status);
      expect(result.stdout.trim().split("\n")).toEqual(status === 0 ? [...expected] : [expected[0]]);
    }
  }

  const checks = workflow.jobs.checks.steps;
  expect(checks.some((step) => step.uses === "./.github/actions/rust-setup")).toBe(true);
  expect(checks.some((step) => step.run === "cargo +nightly-2026-09-06 fmt --all -- --check")).toBe(true);
  expect(checks.some((step) => step.run === "cargo clippy --workspace --all-targets --locked -- -D warnings")).toBe(true);
  expect(checks.some((step) => step.run?.startsWith("bun test ") || step.run === "bun run check:tools")).toBe(false);
});

test("the package matrix consumes automatic discovery without allowing failed members", () => {
  const changes = workflow.jobs.changes;
  expect(changes.outputs?.matrix).toBe("${{ steps.packages.outputs.matrix }}");
  const discovery = changes.steps.find((step) => step.id === "packages");
  const prepare = changes.steps.find((step) => step.run === "rustup show active-toolchain");
  const classify = changes.steps.find((step) => step.id === "changes");
  expect(prepare).toBeDefined();
  expect(changes.steps.indexOf(classify!)).toBeLessThan(changes.steps.indexOf(prepare!));
  expect(changes.steps.indexOf(prepare!)).toBeLessThan(changes.steps.indexOf(discovery!));
  for (const step of [prepare!, discovery!]) {
    expect(step.if).toBe("${{ steps.changes.outputs.tier == 'full' }}");
    expect(step["continue-on-error"]).not.toBe(true);
    for (const tier of ["full", "checks", "tools", "skip", "", "unknown"]) {
      const tokens = new Lexer(step.if!.slice(3, -2)).lex().tokens;
      const parsed = new Parser(tokens, ["steps"], []).parse();
      const context = JSON.parse(JSON.stringify({ steps: { changes: { outputs: { tier } } } }), data.reviver) as data.Dictionary;
      expect(truthy(new Evaluator(parsed, context).evaluate())).toBe(tier === "full");
    }
  }
  expect(discovery?.env).toEqual({
    AFFECTED_PACKAGES: "${{ steps.changes.outputs.packages }}",
  });
  expect(discovery?.run).toContain(
    'bun tools/check-packages.ts --matrix --affected "$AFFECTED_PACKAGES"',
  );
  expect(workflow.jobs.checks.outputs).toBeUndefined();
  expect(workflow.jobs.checks.steps.some((step) => step.id === "packages")).toBe(false);
  const packages = workflow.jobs.packages;
  expect(packages.needs).toBe("changes");
  expect(packages["continue-on-error"]).not.toBe(true);
  expect(packages.strategy?.matrix).toBe(
    "${{ fromJSON(needs.changes.outputs.matrix) }}",
  );
  expect(packages.strategy?.["fail-fast"]).toBe(false);
  expect(packages.strategy?.["max-parallel"]).toBe(8);
  expect(packages.name).toBe(
    "package (${{ matrix.package }}${{ matrix.group && format(' {0}', matrix.group) || '' }}${{ matrix.shard && format(' shard {0}', matrix.shard) || '' }})",
  );
  const run = packages.steps.find((step) => step.env?.PACKAGE !== undefined);
  expect(run?.env?.PACKAGE).toBe("${{ matrix.package }}");
  expect(run?.env?.SHARD).toBe("${{ matrix.shard }}");
  expect(run?.env?.GROUP).toBe("${{ matrix.group }}");
});

test("actual discovery selects consumers and runs every selected matrix entry through the package command", () => {
  const job = workflow.jobs.packages;
  const directory = temporaryDirectory();
  mkdirSync(join(directory, "tools"));
  copyFileSync(
    new URL("./check-packages.ts", import.meta.url),
    join(directory, "tools/check-packages.ts"),
  );
  copyFileSync(
    new URL("../rust-toolchain.toml", import.meta.url),
    join(directory, "rust-toolchain.toml"),
  );
  const dependencies = {
    nucleus:
      '[dependencies]\ndomain = { path = "domain" }\nplatform = { path = "platform" }\n[dev-dependencies]\ndeployment = { path = "deployment" }\nfixture = { path = "fixture" }\n',
    domain:
      '[dependencies]\nplatform = { path = "../platform" }\n[dev-dependencies]\nfixture = { path = "../fixture" }\n',
    deployment:
      '[dependencies]\ndomain = { path = "../domain" }\nplatform = { path = "../platform" }\n[dev-dependencies]\nfixture = { path = "../fixture" }\n',
    platform: '[dev-dependencies]\nfixture = { path = "../fixture" }\n',
    fixture: "",
    newcomer: "",
  };
  for (const [name, declarations] of Object.entries(dependencies)) {
    const packageDirectory =
      name === "nucleus" ? directory : join(directory, name);
    mkdirSync(join(packageDirectory, "src"), { recursive: true });
    writeFileSync(
      join(packageDirectory, "Cargo.toml"),
      `[package]\nname = "${name}"\nversion = "0.1.0"\nedition = "2024"\n${declarations}` +
        (name === "nucleus"
          ? '\n[workspace]\nresolver = "3"\nmembers = ["domain", "deployment", "platform", "fixture", "newcomer"]\n'
          : ""),
    );
    writeFileSync(
      join(packageDirectory, "src/lib.rs"),
      'compile_error!("discovery must not build");\n',
    );
  }
  mkdirSync(join(directory, "tests"));
  writeFileSync(join(directory, "tests/http.rs"), 'compile_error!("discovery must not build");\n');
  const locked = spawnSync("cargo", ["generate-lockfile", "--offline"], {
    cwd: directory,
    env: environment,
    encoding: "utf8",
    timeout: 5_000,
  });
  expect(locked.error).toBeUndefined();
  expect(locked.status, locked.stderr).toBe(0);
  const discovery = workflow.jobs.changes.steps.find(
    (step) => step.id === "packages",
  )!;
  function discover(selection: string) {
    const output = join(directory, "matrix-output");
    writeFileSync(output, "");
    const result = spawnSync(
      "bash",
      ["--noprofile", "--norc", "-c", discovery.run!],
      {
        cwd: directory,
        env: {
          ...environment,
          CARGO_NET_OFFLINE: "true",
          AFFECTED_PACKAGES: selection,
          GITHUB_OUTPUT: output,
        },
        encoding: "utf8",
        timeout: 5_000,
      },
    );
    expect(result.error).toBeUndefined();
    return { ...result, output: readFileSync(output, "utf8") };
  }
  const entries: { package: string; shard?: string; group?: string }[] = [];
  for (const [selection, expected] of [
    ['["nucleus"]', ["nucleus"]],
    ['["deployment"]', ["deployment", "nucleus"]],
    ['["domain"]', ["deployment", "domain", "nucleus"]],
    ['["domain","deployment"]', ["deployment", "domain", "nucleus"]],
    ['["fixture"]', ["deployment", "domain", "fixture", "nucleus", "platform"]],
    [
      "all",
      ["deployment", "domain", "fixture", "newcomer", "nucleus", "platform"],
    ],
  ] as const) {
    const result = discover(selection);
    expect(result.status, result.stderr).toBe(0);
    expect(result.output).toStartWith("matrix=");
    const selected: { package: string; shard?: string; group?: string }[] = JSON.parse(
      result.output.slice("matrix=".length),
    ).include;
    expect([...new Set(selected.map((entry) => entry.package))].sort()).toEqual(
      [...expected],
    );
    expect(
      selected
        .filter((entry) => entry.package === "domain")
        .map((entry) => entry.shard),
    ).toEqual(
      expected.some((name) => name === "domain")
        ? ["1/4", "2/4", "3/4", "4/4"]
        : [],
    );
    for (const name of expected.filter((name) => name !== "domain" && name !== "nucleus"))
      expect(selected.filter((entry) => entry.package === name)).toEqual([
        { package: name },
      ]);
    expect(selected.filter((entry) => entry.package === "nucleus")).toEqual(
      expected.some((name) => name === "nucleus")
        ? [{ package: "nucleus", group: "rest" }, { package: "nucleus", group: "http" }]
        : [],
    );
    if (selection === "all") entries.push(...selected);
  }
  for (const selection of ["", "[]", '["missing"]', "all\ninvalid"]) {
    const result = discover(selection);
    expect(result.status).not.toBe(0);
    expect(result.output).toBe("");
  }
  expect(existsSync(join(directory, "target"))).toBe(false);
  const stub = join(directory, "bun");
  writeFileSync(
    stub,
    '#!/bin/bash\nprintf "%s\\n" "$@"\nexit "${COMMAND_STATUS:-0}"\n',
  );
  chmodSync(stub, 0o700);
  const step = job.steps.find((step) => step.env?.PACKAGE !== undefined)!;
  for (const entry of entries) {
    for (const status of [0, 17]) {
      const result = spawnSync(
        "bash",
        ["--noprofile", "--norc", "-c", step.run!],
        {
          env: {
            ...environment,
            PATH: `${directory}:${process.env.PATH}`,
            PACKAGE: entry.package,
            SHARD: entry.shard ?? "",
            GROUP: entry.group ?? "",
            COMMAND_STATUS: String(status),
          },
          encoding: "utf8",
          timeout: 5_000,
        },
      );
      expect(result.error).toBeUndefined();
      expect(result.status, result.stderr).toBe(status);
      expect(result.stdout.trim().split("\n")).toEqual([
        "tools/check-packages.ts",
        "--package",
        entry.package,
        "--test-threads",
        "4",
        ...(entry.shard ? ["--shard", entry.shard] : []),
        ...(entry.group ? ["--group", entry.group] : []),
      ]);
    }
  }
}, 30_000);

test("discovery rejects failed, empty or malformed matrices before publishing job output", () => {
  const directory = temporaryDirectory();
  const stub = join(directory, "bun");
  writeFileSync(stub, '#!/bin/bash\nif test "$1" = tools/check-packages.ts; then\n  printf "%s" "$TEST_MATRIX"\n  exit "$TEST_EXIT"\nfi\nexec "$TEST_BUN" "$@"\n');
  chmodSync(stub, 0o700);
  const discovery = workflow.jobs.changes.steps.find((step) => step.id === "packages")!;
  const valid = '{"include":[{"package":"nucleus","group":"rest"},{"package":"nucleus","group":"http"}]}';
  for (const [matrix, status, accepted] of [
    [valid, 0, true],
    [valid, 17, false],
    ["", 0, false],
    ["{", 0, false],
    ["null", 0, false],
    ["[]", 0, false],
    ["{}", 0, false],
    ['{"include":[]}', 0, false],
    ['{"include":{}}', 0, false],
    ['{"include":[null]}', 0, false],
    ['{"include":[{}]}', 0, false],
    ['{"include":[{"package":""}]}', 0, false],
    ['{"include":[{"package":42}]}', 0, false],
    ['{"include":[{"package":"domain","shard":4}]}', 0, false],
    ['{"include":[{"package":"domain","shard":""}]}', 0, false],
    ['{"include":[{"package":"nucleus"}]}', 0, false],
    ['{"include":[{"package":"nucleus","group":"http"}]}', 0, false],
    ['{"include":[{"package":"nucleus","group":"rest"}]}', 0, false],
    ['{"include":[{"package":"nucleus","group":"other"}]}', 0, false],
    ['{"include":[{"package":"nucleus","group":"http","shard":"1/4"}]}', 0, false],
    ['{"include":[{"package":"other","group":"http"}]}', 0, false],
    ['{"include":[{"package":"other","shard":"1/4"}]}', 0, false],
    ['{"include":[{"package":"other"},{"package":"other"}]}', 0, false],
    ['{"include":[{"package":"other","unexpected":"value"}]}', 0, false],
    ['{"include":[{"package":"domain","shard":"1/4"}]}', 0, false],
    [valid.replace('"http"', '"rest"'), 0, false],
  ] as const) {
    const output = join(directory, "output");
    writeFileSync(output, "");
    const result = spawnSync("bash", ["--noprofile", "--norc", "-c", discovery.run!], {
      cwd: directory,
      env: {
        ...environment,
        PATH: `${directory}:${process.env.PATH}`,
        TEST_BUN: process.execPath,
        TEST_MATRIX: matrix,
        TEST_EXIT: String(status),
        AFFECTED_PACKAGES: "all",
        GITHUB_OUTPUT: output,
      },
      encoding: "utf8",
      timeout: 5_000,
    });
    expect(result.error).toBeUndefined();
    expect(result.status === 0, `matrix=${matrix}, status=${status}: ${result.stderr}`).toBe(accepted);
    expect(readFileSync(output, "utf8")).toBe(accepted ? `matrix=${valid}\n` : "");
  }
});

test("Docs always runs design preview regressions and propagates failures", () => {
  const docs = Bun.YAML.parse(
    readFileSync(
      new URL("../.github/workflows/docs.yml", import.meta.url),
      "utf8",
    ),
  ) as typeof workflow;
  expect(docs.on).toEqual({
    pull_request: null,
    merge_group: { types: ["checks_requested"] },
  });
  const job = docs.jobs.docs;
  expect(job.if).toBeUndefined();
  expect(job.needs).toBeUndefined();
  expect(job["continue-on-error"]).toBeUndefined();
  const step = job.steps.find(
    (step) => step.run === "bun test tools/preview-design.test.ts",
  );
  expect(step).toBeDefined();
  expect(step!.if).toBeUndefined();
  expect(step!["continue-on-error"]).toBeUndefined();
  const setup = job.steps.find((step) =>
    step.uses?.startsWith("oven-sh/setup-bun@"),
  );
  expect(setup?.with?.["bun-version-file"]).toBe(".tool-versions");
  const install = job.steps.find(
    (step) => step.run === "bun install --frozen-lockfile --ignore-scripts",
  );
  expect(install).toBeDefined();
  expect(install!.if).toBeUndefined();
  expect(install!["continue-on-error"]).toBeUndefined();
  expect(job.steps.indexOf(setup!)).toBeLessThan(job.steps.indexOf(install!));
  expect(job.steps.indexOf(install!)).toBeLessThan(job.steps.indexOf(step!));
  expect(
    workflow.jobs.checks.steps.some(
      (step) => step.run === "bun test tools/preview-design.test.ts",
    ),
  ).toBe(false);
  const directory = temporaryDirectory();
  const stub = join(directory, "bun");
  writeFileSync(
    stub,
    '#!/bin/bash\nprintf "%s\\n" "$@"\nexit "${COMMAND_STATUS:-0}"\n',
  );
  chmodSync(stub, 0o700);
  for (const status of [0, 17]) {
    const result = spawnSync(
      "bash",
      ["--noprofile", "--norc", "-c", step!.run!],
      {
        env: {
          ...environment,
          PATH: `${directory}:${process.env.PATH}`,
          COMMAND_STATUS: String(status),
        },
        encoding: "utf8",
        timeout: 5_000,
      },
    );
    expect(result.error).toBeUndefined();
    expect(result.status, result.stderr).toBe(status);
    expect(result.stdout.trim().split("\n")).toEqual([
      "test",
      "tools/preview-design.test.ts",
    ]);
  }
});

test("the protected aggregate job identities remain rust and docs", () => {
  expect(workflow.jobs.rust.name).toBeUndefined();
  const docs = Bun.YAML.parse(
    readFileSync(
      new URL("../.github/workflows/docs.yml", import.meta.url),
      "utf8",
    ),
  ) as typeof workflow;
  expect(docs.jobs.docs).toBeDefined();
  expect(docs.jobs.docs.name).toBeUndefined();
});

const gateScheduling = Bun.YAML.parse(
  readFileSync(
    new URL("../.github/workflows/review-gate.yml", import.meta.url),
    "utf8",
  ),
) as {
  concurrency: { group: string; "cancel-in-progress": boolean; queue: string };
  jobs: {
    gate: {
      if: string;
      concurrency: {
        group: string;
        "cancel-in-progress": boolean;
        queue: string;
      };
    };
  };
};

const gateEvents: [string, string, object, number | string | null, boolean][] = [
  [
    "PR head update",
    "pull_request_target",
    {
      action: "synchronize",
      pull_request: { number: 42, head: { sha: "new" } },
    },
    42,
    true,
  ],
  [
    "another PR",
    "pull_request_target",
    { pull_request: { number: 43 } },
    43,
    true,
  ],
  [
    "PR closed",
    "pull_request_target",
    { action: "closed", pull_request: { number: 42, state: "closed" } },
    42,
    true,
  ],
  [
    "PR reopened",
    "pull_request_target",
    { action: "reopened", pull_request: { number: 42, state: "open" } },
    42,
    true,
  ],
  [
    "missing PR number",
    "pull_request_target",
    { pull_request: {} },
    null,
    true,
  ],
  [
    "Codex PR comment",
    "issue_comment",
    {
      issue: { number: 42, pull_request: {} },
      comment: { user: { login: "chatgpt-codex-connector[bot]" } },
    },
    42,
    true,
  ],
  [
    "deleted Codex PR comment",
    "issue_comment",
    {
      action: "deleted",
      issue: { number: 42, pull_request: {} },
      comment: { user: { login: "chatgpt-codex-connector[bot]" } },
    },
    42,
    true,
  ],
  [
    "ordinary PR comment",
    "issue_comment",
    {
      issue: { number: 42, pull_request: {} },
      comment: { user: { login: "contributor" } },
    },
    null,
    false,
  ],
  [
    "another bot PR comment",
    "issue_comment",
    {
      issue: { number: 42, pull_request: {} },
      comment: { user: { login: "cladiron[bot]" } },
    },
    null,
    false,
  ],
  [
    "Codex issue comment",
    "issue_comment",
    {
      issue: { number: 42 },
      comment: { user: { login: "chatgpt-codex-connector[bot]" } },
    },
    null,
    false,
  ],
  [
    "missing comment author",
    "issue_comment",
    { issue: { number: 42, pull_request: {} }, comment: {} },
    null,
    false,
  ],
  [
    "single PR workflow",
    "workflow_run",
    {
      workflow_run: { event: "pull_request", pull_requests: [{ number: 42 }] },
    },
    42,
    true,
  ],
  [
    "single target PR workflow",
    "workflow_run",
    {
      workflow_run: {
        event: "pull_request_target",
        pull_requests: [{ number: 42 }],
      },
    },
    42,
    true,
  ],
  [
    "another PR workflow",
    "workflow_run",
    {
      workflow_run: { event: "pull_request", pull_requests: [{ number: 43 }] },
    },
    43,
    true,
  ],
  [
    "queue workflow with PR association",
    "workflow_run",
    { workflow_run: { event: "merge_group", pull_requests: [{ number: 42 }] } },
    null,
    true,
  ],
  [
    "queue workflow without association",
    "workflow_run",
    { workflow_run: { event: "merge_group", pull_requests: [] } },
    null,
    true,
  ],
  [
    "multiple associated PRs",
    "workflow_run",
    {
      workflow_run: {
        event: "pull_request",
        pull_requests: [{ number: 42 }, { number: 43 }],
      },
    },
    null,
    true,
  ],
  [
    "second association without number",
    "workflow_run",
    {
      workflow_run: {
        event: "pull_request",
        pull_requests: [{ number: 42 }, {}],
      },
    },
    null,
    true,
  ],
  [
    "association without number",
    "workflow_run",
    { workflow_run: { event: "pull_request", pull_requests: [{}] } },
    null,
    true,
  ],
  [
    "empty association",
    "workflow_run",
    { workflow_run: { event: "pull_request", pull_requests: [] } },
    null,
    true,
  ],
  [
    "missing association",
    "workflow_run",
    { workflow_run: { event: "pull_request" } },
    null,
    true,
  ],
  [
    "unassociated branch matching a PR",
    "workflow_run",
    {
      workflow_run: {
        event: "pull_request",
        pull_requests: [],
        head_branch: "feature",
        head_repository: { full_name: "owner/repository" },
      },
    },
    null,
    true,
  ],
  [
    "unassociated context name",
    "workflow_run",
    {
      workflow_run: {
        event: "pull_request_target",
        pull_requests: [],
        display_title: "context:42:head:base:dev",
      },
    },
    null,
    true,
  ],
  [
    "unassociated activity name",
    "workflow_run",
    {
      workflow_run: {
        event: "pull_request_review",
        pull_requests: [],
        display_title: "activity:42",
      },
    },
    null,
    true,
  ],
  ["dev push", "push", { ref: "refs/heads/dev" }, "sweep", true],
  ["main push", "push", { ref: "refs/heads/main" }, "sweep", true],
  ["scheduled sweep", "schedule", {}, "sweep", true],
  ["manual sweep", "workflow_dispatch", {}, "sweep", true],
  ["unknown event", "repository_dispatch", {}, null, false],
];

// Execute the real YAML with GitHub's expression semantics, including missing
// properties and out-of-range array indexes, rather than a JS translation.
function evaluateGate(
  expression: string,
  eventName: string,
  event: object,
  run: number,
  repository = "silence-force/nucleus",
) {
  const source = expression.trim();
  const body = source.startsWith("${{") ? source.slice(3, -2) : source;
  const tokens = new Lexer(body).lex().tokens;
  const parsed = new Parser(tokens, ["github"], []).parse();
  const context = JSON.parse(
    JSON.stringify({
      github: {
        repository,
        event_name: eventName,
        event,
        run_id: run,
      },
    }),
    data.reviver,
  ) as data.Dictionary;
  return new Evaluator(parsed, context).evaluate();
}

test.each(gateEvents)(
  "review gate coalesces notifications with the same scope: %s",
  (_name, eventName, event, group, runnable) => {
    for (const run of [1001, 1002]) {
      const expected =
        group === null
          ? `run-${run}`
          : typeof group === "number"
            ? `pr-${group}`
            : group;
      expect(
        evaluateGate(
          gateScheduling.concurrency.group,
          eventName,
          event,
          run,
        ).coerceString(),
      ).toBe(`review-gate-events-${expected}`);
      expect(
        truthy(evaluateGate(gateScheduling.jobs.gate.if, eventName, event, run)),
      ).toBe(runnable);
    }
  },
);

test("required main lifecycle runs cannot be coalesced by PR notifications", () => {
  for (const action of ["opened", "synchronize", "reopened"])
    for (const run of [1001, 1002]) {
      const event = {
        action,
        pull_request: { number: 42, base: { ref: "main" } },
      };
      expect(
        evaluateGate(
          gateScheduling.concurrency.group,
          "pull_request_target",
          event,
          run,
        ).coerceString(),
      ).toBe(`review-gate-events-lifecycle-${run}`);
      expect(
        truthy(
          evaluateGate(
            gateScheduling.jobs.gate.if,
            "pull_request_target",
            event,
            run,
          ),
        ),
      ).toBe(true);
    }
});

const queueNotice = {
  id: 10,
  event: "merge_group",
  repository: { full_name: "silence-force/nucleus" },
  path: ".github/workflows/rust.yml",
  head_branch: "gh-readonly-queue/dev/pr-42-abcdef",
  head_sha: "a".repeat(40),
  pull_requests: [],
};
const queueGroup = `queue-gh-readonly-queue/dev/pr-42-abcdef-${"a".repeat(40)}`;
const queueNotifications: [string, object, string | null][] = [
  ["Rust completion", {}, queueGroup],
  ["Docs completion", { path: ".github/workflows/docs.yml" }, queueGroup],
  ["Halo completion", { path: ".github/workflows/halo.yml" }, queueGroup],
  [
    "Halo path with ref",
    { path: ".github/workflows/halo.yml@refs/heads/dev" },
    queueGroup,
  ],
  [
    "Rust path with ref",
    { path: ".github/workflows/rust.yml@refs/heads/dev" },
    queueGroup,
  ],
  [
    "workflow path with ref",
    { path: ".github/workflows/docs.yml@refs/heads/dev" },
    queueGroup,
  ],
  ["rerun completion", { id: 11, run_attempt: 2 }, queueGroup],
  ["failed completion", { conclusion: "failure" }, queueGroup],
  ["cancelled completion", { conclusion: "cancelled" }, queueGroup],
  ["associated PR", { pull_requests: [{ number: 42 }] }, queueGroup],
  [
    "multiple associations still identify the exact queue ref",
    { pull_requests: [{ number: 42 }, { number: 43 }] },
    queueGroup,
  ],
  [
    "same ref with a new candidate",
    { head_sha: "b".repeat(40) },
    `queue-gh-readonly-queue/dev/pr-42-abcdef-${"b".repeat(40)}`,
  ],
  [
    "same SHA with a new queue ref",
    { head_branch: "gh-readonly-queue/dev/pr-42-fedcba" },
    `queue-gh-readonly-queue/dev/pr-42-fedcba-${"a".repeat(40)}`,
  ],
  [
    "another PR queue ref",
    { head_branch: "gh-readonly-queue/dev/pr-43-abcdef" },
    `queue-gh-readonly-queue/dev/pr-43-abcdef-${"a".repeat(40)}`,
  ],
  ["foreign repository", { repository: { full_name: "other/repository" } }, null],
  ["missing repository", { repository: {} }, null],
  ["missing workflow path", { path: null }, null],
  ["unsupported workflow", { path: ".github/workflows/other.yml" }, null],
  ["similar workflow path", { path: ".github/workflows/rust.yml-copy" }, null],
  ["similar Halo path", { path: ".github/workflows/halo.yml-copy" }, null],
  ["missing queue ref", { head_branch: null }, null],
  ["empty queue ref", { head_branch: "" }, null],
  ["ordinary branch", { head_branch: "feature" }, null],
  ["another queue", { head_branch: "gh-readonly-queue/main/pr-42-abcdef" }, null],
  ["missing queue SHA", { head_sha: null }, null],
  ["empty queue SHA", { head_sha: "" }, null],
  ["non-queue run", { event: "push" }, null],
];

test.each(queueNotifications)(
  "review gate coalesces queue CI by repository, ref and SHA: %s",
  (_name, changes, group) => {
    for (const run of [1001, 1002]) {
      const event = { workflow_run: { ...queueNotice, ...changes } };
      expect(
        evaluateGate(
          gateScheduling.concurrency.group,
          "workflow_run",
          event,
          run,
        ).coerceString(),
      ).toBe(`review-gate-events-${group ?? `run-${run}`}`);
      expect(
        truthy(
          evaluateGate(gateScheduling.jobs.gate.if, "workflow_run", event, run),
        ),
      ).toBe(true);
    }
  },
);

test("review gate preserves running work and global serialization across event groups", () => {
  expect(gateScheduling.concurrency["cancel-in-progress"]).toBe(false);
  expect(gateScheduling.concurrency.queue).toBe("single");
  expect(gateScheduling.jobs.gate.concurrency).toEqual({
    group: "review-gate",
    "cancel-in-progress": false,
    queue: "max",
  });
  expect(gateScheduling.concurrency.group).not.toBe("review-gate");
});

test("organization gate cannot consume personal-repository secrets or run in another repository", () => {
  for (const repository of ["headironc/nucleus", "other/nucleus"])
    expect(truthy(evaluateGate(gateScheduling.jobs.gate.if, "workflow_dispatch", {}, 1001, repository))).toBe(false);
});

test("review gate targets events and reconciles all PRs inside one queued lock", () => {
  const gate = Bun.YAML.parse(
    readFileSync(
      new URL("../.github/workflows/review-gate.yml", import.meta.url),
      "utf8",
    ),
  ) as {
    on: {
      workflow_run: { workflows: string[] };
      issue_comment: { types: string[] };
      pull_request_target: { types: string[]; branches?: string[] };
      schedule: { cron: string }[];
    };
    jobs: {
      gate: {
        concurrency: { group: string; "cancel-in-progress": boolean; queue: string };
        strategy?: unknown;
        if: string;
        steps: {
          id?: string;
          name?: string;
          uses?: string;
          run?: string;
          shell?: string;
          env?: Record<string, string>;
          with?: Record<string, string>;
        }[];
      };
    };
  };
  expect(gate.jobs.gate.concurrency).toEqual({
    group: "review-gate",
    "cancel-in-progress": false,
    queue: "max",
  });
  expect(gate.jobs.gate.strategy).toBeUndefined();
  expect(gate.on.workflow_run.workflows).toContain("Review activity");
  expect(gate.on.workflow_run.workflows).toContain("Halo");
  expect(gate.on.issue_comment.types).toContain("deleted");
  expect(gate.on.pull_request_target.types).toContain("converted_to_draft");
  expect(gate.on.pull_request_target.types).toContain("ready_for_review");
  expect(gate.on.pull_request_target.types).toContain("reopened");
  expect(gate.on.pull_request_target.types).toContain("closed");
  expect(gate.on.pull_request_target.types).toContain("synchronize");
  expect(gate.on.pull_request_target.branches).toEqual(["dev", "main"]);
  expect(gate.on.schedule).toEqual([{ cron: "17 * * * *" }]);
  expect(gate.jobs.gate.if).toContain("github.event_name == 'schedule'");
  expect(
    gate.jobs.gate.steps.some(
      (step) =>
        step.with?.ref === "${{ github.event.repository.default_branch }}",
    ),
  ).toBe(true);
  const resolver = gate.jobs.gate.steps.find((step) => step.id === "target")!;
  const approve = gate.jobs.gate.steps.find(
    (step) => step.name === "Approve clean heads",
  )!;
  expect(resolver.shell).toBe("bash");
  expect(approve.shell).toBe("bash");
  expect(approve.env?.NUMBERS).toBe("${{ steps.target.outputs.numbers }}");
  expect(approve.env?.DEV_REVIEW_QUEUE).toBe("${{ vars.DEV_REVIEW_QUEUE }}");
  expect(Object.hasOwn(gate.on, "merge_group")).toBe(false);
  expect(gate.jobs.gate.steps.find((step) => step.id === "app")?.with?.["permission-checks"])
    .toBe("${{ vars.DEV_REVIEW_QUEUE == 'true' && 'write' || 'read' }}");
  expect(approve.run?.trim()).toBe("bun tools/review-gate.ts");
  expect(resolver.run).toContain("bun tools/review-gate.ts --validate-numbers");
  const steps = gate.jobs.gate.steps;
  const checkoutIndex = steps.findIndex((step) => step.with?.ref === "${{ github.event.repository.default_branch }}");
  const setupIndex = steps.findIndex((step) => step.uses?.startsWith("oven-sh/setup-bun@"));
  const runtimeIndex = steps.findIndex((step) => step.run?.trim() === "bun tools/check-bun.ts");
  const appIndex = steps.findIndex((step) => step.id === "app");
  expect(steps[setupIndex].uses).toBe("oven-sh/setup-bun@0c5077e51419868618aeaa5fe8019c62421857d6");
  expect(steps[setupIndex].with).toEqual({ "bun-version-file": ".tool-versions" });
  expect(checkoutIndex).toBeLessThan(setupIndex);
  expect(setupIndex).toBeLessThan(runtimeIndex);
  expect(runtimeIndex).toBeLessThan(appIndex);
  expect(steps.some((step) => step.run?.includes("bun install"))).toBe(false);
  expect(approve.env?.GH_TOKEN).toBe("${{ steps.app.outputs.token }}");
  expect(approve.env?.GH_ACTIONS_TOKEN).toBe("${{ github.token }}");

  const directory = temporaryDirectory();
  writeFileSync(
    join(directory, "gh"),
    '#!/bin/bash\ntest "$2" = graphql || exit 64\ncase "$*" in *\'after: $endCursor\'*) ;; *) exit 64 ;; esac\nfor arg in "$@"; do test "$arg" != --jq || exit 64; done\nprintf "%s\\n" "${TEST_PULLS}"\n',
  );
  chmodSync(join(directory, "gh"), 0o700);
  const numbers = Array.from({ length: 300 }, (_, index) => index + 1);
  for (const expected of [[], [42, 43], numbers, [-1]]) {
    const output = join(directory, "output");
    writeFileSync(output, "");
    const result = spawnSync(
      "bash",
      ["--noprofile", "--norc", "-e", "-o", "pipefail", "-c", resolver.run!],
      {
        env: {
          ...environment,
          PATH: `${directory}:${process.env.PATH}`,
          GITHUB_REPOSITORY: "owner/repository",
          GITHUB_EVENT_NAME: "schedule",
          GITHUB_OUTPUT: output,
          TEST_PULLS: JSON.stringify(
            Array.from(
              { length: Math.max(1, Math.ceil(expected.length / 100)) },
              (_, index) => expected.slice(index * 100, (index + 1) * 100),
            ).map((page) => ({
              data: {
                repository: {
                  pullRequests: { nodes: page.map((number) => ({ number })) },
                },
              },
            })),
          ),
        },
        encoding: "utf8",
        timeout: 5_000,
      },
    );
    expect(result.error).toBeUndefined();
    if (expected[0] === -1) {
      expect(result.status).not.toBe(0);
      expect(readFileSync(output, "utf8")).toBe("");
    } else {
      expect(result.status, result.stderr).toBe(0);
      expect(
        JSON.parse(
          readFileSync(output, "utf8").trim().slice("numbers=".length),
        ),
      ).toEqual(expected);
    }
  }

  const eventPath = join(directory, "event.json");
  const candidates = [
    { number: 42, headRefName: "feature", headRepository: { nameWithOwner: "fork/repository" } },
    { number: 43, headRefName: "feature", headRepository: { nameWithOwner: "owner/repository" } },
  ];
  const cases: [string, object, number[]][] = [
    ["pull_request_target", { pull_request: { number: 42 } }, [42]],
    ["pull_request_target", { action: "closed", pull_request: { number: 42 } }, [42]],
    ["pull_request_target", { action: "reopened", pull_request: { number: 42 } }, [42]],
    ["issue_comment", { issue: { number: 43 } }, [43]],
    ["workflow_run", { workflow_run: { pull_requests: [{ number: 42 }, { number: 43 }] } }, [42, 43]],
    ["workflow_run", { workflow_run: { event: "merge_group", id: 123, pull_requests: [{ number: 42 }] } }, []],
    ["workflow_run", { workflow_run: { pull_requests: [], path: ".github/workflows/review-context.yml", display_title: "context:42:head:base:dev" } }, [42]],
    ["workflow_run", { workflow_run: { pull_requests: [], path: ".github/workflows/review-activity.yml@dev", display_title: "activity:43" } }, [43]],
    ["workflow_run", { workflow_run: { pull_requests: [], path: ".github/workflows/rust.yml", display_title: "Build", head_branch: "feature", head_repository: { full_name: "fork/repository" } } }, [42]],
    ["workflow_run", { workflow_run: { pull_requests: [], path: ".github/workflows/rust.yml", display_title: "Build", head_branch: "missing", head_repository: { full_name: "fork/repository" } } }, []],
    ["workflow_run", { workflow_run: { pull_requests: [], path: ".github/workflows/rust.yml", display_title: "context:43:fake:context:dev", head_branch: "feature", head_repository: { full_name: "fork/repository" } } }, [42]],
    ["workflow_run", { workflow_run: { pull_requests: [], path: ".github/workflows/docs.yml", display_title: "activity:43", head_branch: "feature", head_repository: { full_name: "fork/repository" } } }, [42]],
    ["workflow_run", { workflow_run: { event: "pull_request", pull_requests: [{ number: 42 }], path: ".github/workflows/halo.yml" } }, [42]],
    ["workflow_run", { workflow_run: { event: "pull_request", pull_requests: [], path: ".github/workflows/halo.yml", head_branch: "feature", head_repository: { full_name: "fork/repository" } } }, [42]],
    ["workflow_run", { workflow_run: { event: "pull_request", pull_requests: [], path: ".github/workflows/halo.yml", head_branch: "missing", head_repository: { full_name: "fork/repository" } } }, []],
    ["workflow_run", { workflow_run: { event: "merge_group", id: 123, pull_requests: [{ number: 99 }], path: ".github/workflows/halo.yml" } }, []],
    ["push", {}, [42, 43]],
    ["schedule", {}, [42, 43]],
    ["workflow_dispatch", {}, [42, 43]],
  ];
  for (const [eventName, event, expected] of cases) {
    const output = join(directory, "output");
    writeFileSync(eventPath, JSON.stringify(event));
    writeFileSync(output, "");
    const result = spawnSync(
      "bash",
      ["--noprofile", "--norc", "-e", "-o", "pipefail", "-c", resolver.run!],
      {
        env: {
          ...environment,
          PATH: `${directory}:${process.env.PATH}`,
          GITHUB_REPOSITORY: "owner/repository",
          GITHUB_OUTPUT: output,
          GITHUB_EVENT_NAME: eventName,
          GITHUB_EVENT_PATH: eventPath,
          TEST_PULLS: JSON.stringify([{ data: { repository: { pullRequests: { nodes: candidates } } } }]),
        },
        encoding: "utf8",
        timeout: 5_000,
      },
    );
    expect(result.status, result.stderr).toBe(0);
    expect(JSON.parse(readFileSync(output, "utf8").trim().slice("numbers=".length))).toEqual(expected);
  }

  // One process owns the sweep and shares the immutable context history.
  writeFileSync(
    join(directory, "bun"),
    '#!/bin/bash\nprintf "%s\\n" "$NUMBERS"\nexit "$TEST_EXIT"\n',
  );
  chmodSync(join(directory, "bun"), 0o700);
  for (const exit of [0, 1]) {
    const result = spawnSync(
      "bash",
      ["--noprofile", "--norc", "-e", "-o", "pipefail", "-c", approve.run!],
      {
        env: {
          ...environment,
          PATH: `${directory}:${process.env.PATH}`,
          NUMBERS: JSON.stringify(numbers),
          TEST_EXIT: String(exit),
        },
        encoding: "utf8",
        timeout: 5_000,
      },
    );
    expect(result.error).toBeUndefined();
    expect(result.status).toBe(exit);
    expect(JSON.parse(result.stdout)).toEqual(numbers);
  }
});

test("formal and inline review activity relay to the trusted gate without credentials", () => {
  const activity = Bun.YAML.parse(
    readFileSync(
      new URL("../.github/workflows/review-activity.yml", import.meta.url),
      "utf8",
    ),
  ) as {
    name: string;
    "run-name": string;
    on: {
      pull_request_review: { types: string[] };
      pull_request_review_comment: { types: string[] };
    };
    permissions: Record<string, string>;
    jobs: { notify: { steps: { run: string }[] } };
  };
  expect(activity.name).toBe("Review activity");
  expect(activity["run-name"]).toBe("activity:${{ github.event.pull_request.number }}");
  expect(activity.on.pull_request_review.types).toEqual([
    "submitted",
    "edited",
    "dismissed",
  ]);
  expect(activity.on.pull_request_review_comment.types).toEqual([
    "created",
    "edited",
    "deleted",
  ]);
  expect(activity.permissions).toEqual({});
  expect(activity.jobs.notify.steps).toEqual([{ run: "true" }]);
});

test("review context records trusted PR events without executing candidate code", () => {
  const context = Bun.YAML.parse(
    readFileSync(
      new URL("../.github/workflows/review-context.yml", import.meta.url),
      "utf8",
    ),
  ) as {
    "run-name": string;
    on: { pull_request_target: { types: string[] } };
    permissions: Record<string, string>;
    jobs: { record: { steps: { run: string }[] } };
  };
  expect(context.on).toEqual({
    pull_request_target: {
      types: ["opened", "synchronize", "edited", "reopened", "closed", "converted_to_draft", "ready_for_review"],
    },
  });
  expect(context.permissions).toEqual({});
  expect(context.jobs.record.steps.map((step) => step.run)).toEqual(["true"]);
  for (const [stack, suffix] of [[null, ""], [{ number: 7, position: 2, size: 2, base: { ref: "dev" } }, ":stack:7:2:2:dev"]] as const) {
    const event = { action: "synchronize", pull_request: { number: 43, head: { sha: "head" }, base: { sha: "parent", ref: "codex/lower" }, stack } };
    const title = context["run-name"].replace(/\$\{\{([\s\S]*?)\}\}/g, (_match, expression: string) =>
      evaluateGate(expression, "pull_request_target", event, 1).coerceString());
    expect(title).toBe(`context:43:head:parent:codex/lower${suffix}`);
    event.action = "edited";
    const notice = context["run-name"].replace(/\$\{\{([\s\S]*?)\}\}/g, (_match, expression: string) =>
      evaluateGate(expression, "pull_request_target", event, 1).coerceString());
    expect(notice).toBe(`context:43:head:parent:codex/lower${suffix}:notice`);
  }
});
