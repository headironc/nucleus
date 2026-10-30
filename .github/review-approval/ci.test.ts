import { afterEach, expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { produceReceipt, selectRun, verifyReceipt, verifyJobs } from "./ci.ts";
import { b } from "./fixtures.ts";
import { numericId } from "./identity.ts";

const directories: string[] = [];
test("GraphQL fullDatabaseId keeps current 64-bit IDs exact and rejects rounding", () => {
  expect(numericId("6000000000")).toBe(6000000000);
  expect(() => numericId("9007199254740993")).toThrow("exactly");
});
afterEach(() => {
  for (const path of directories.splice(0))
    rmSync(path, { recursive: true, force: true });
});
test("receipt independently binds real Git merge parents and tree, not event's historical base", () => {
  const dir = mkdtempSync(join(tmpdir(), "cladiron-ci-"));
  directories.push(dir);
  const env = {
    PATH: process.env.PATH,
    GIT_CONFIG_GLOBAL: "/dev/null",
    GIT_CONFIG_NOSYSTEM: "1",
    GIT_AUTHOR_NAME: "Fixture",
    GIT_AUTHOR_EMAIL: "fixture@example.invalid",
    GIT_COMMITTER_NAME: "Fixture",
    GIT_COMMITTER_EMAIL: "fixture@example.invalid",
  };
  function git(...args: string[]) {
    const r = spawnSync("git", args, {
      cwd: dir,
      env,
      encoding: "utf8",
      input: "fixture\n",
    });
    expect(r.status).toBe(0);
    return r.stdout.trim();
  }
  git("init", "--quiet");
  writeFileSync(join(dir, "file"), "base\n");
  git("add", "file");
  git("commit", "--quiet", "--no-gpg-sign", "-m", "fixture base");
  const base = git("rev-parse", "HEAD");
  writeFileSync(join(dir, "file"), "candidate\n");
  git("add", "file");
  git("commit", "--quiet", "--no-gpg-sign", "-m", "fixture head");
  const head = git("rev-parse", "HEAD"),
    tree = git("rev-parse", "HEAD^{tree}");
  const merge = git("commit-tree", tree, "-p", base, "-p", head);
  git("checkout", "--quiet", "--detach", merge);
  const runtime = {
    ...process.env,
    GITHUB_EVENT_NAME: "pull_request",
    GITHUB_REPOSITORY: "headironc/nucleus",
    GITHUB_REPOSITORY_ID: "1432619931",
    GITHUB_SHA: merge,
    GITHUB_WORKFLOW_REF:
      "headironc/nucleus/.github/workflows/docs.yml@refs/pull/27/merge",
    GITHUB_RUN_ID: "42",
    GITHUB_RUN_ATTEMPT: "2",
  };
  const event = {
    number: 27,
    repository: { id: 1432619931 },
    pull_request: {
      head: { sha: head },
      base: { sha: "0".repeat(40), ref: "dev", repo: { id: 1432619931 } },
    },
  };
  const receipt = produceReceipt(runtime, event, dir);
  expect(receipt.parents).toEqual([base, head]);
  expect(receipt.tree).toBe(tree);
  expect(git("cat-file", "-p", merge).split("\n").slice(0, 3)).toEqual([
    `tree ${tree}`,
    `parent ${base}`,
    `parent ${head}`,
  ]);
  const run = { id: 42, run_attempt: 2 };
  const binding = { pr: 27, head, base, merge };
  verifyReceipt(receipt, run, binding, tree, ".github/workflows/docs.yml");
  expect(() =>
    verifyReceipt(
      { ...receipt, parents: [head, base] },
      run,
      binding,
      tree,
      ".github/workflows/docs.yml",
    ),
  ).toThrow();
  expect(() =>
    verifyReceipt(
      receipt,
      { ...run, run_attempt: 3 },
      binding,
      tree,
      ".github/workflows/docs.yml",
    ),
  ).toThrow();
  expect(() =>
    produceReceipt({ ...runtime, GITHUB_SHA: head }, event, dir),
  ).toThrow();
  const shallow = mkdtempSync(join(tmpdir(), "cladiron-shallow-"));
  directories.push(shallow);
  const clone = spawnSync(
    "git",
    ["clone", "--quiet", "--depth", "1", `file://${dir}`, shallow],
    { env, encoding: "utf8" },
  );
  expect(clone.status).toBe(0);
  expect(
    spawnSync("git", ["show", "-s", "--format=%P", "HEAD"], {
      cwd: shallow,
      env,
      encoding: "utf8",
    }).stdout.trim(),
  ).toBe("");
  expect(produceReceipt(runtime, event, shallow).parents).toEqual([base, head]);
});
test("latest run/attempt replaces old failure; newer failure never borrows old green", () => {
  const run = {
    id: 10,
    path: ".github/workflows/docs.yml",
    head_sha: b.head,
    event: "pull_request",
    run_number: 10,
    run_attempt: 2,
    repository: { id: 1432619931 },
    head_repository: { id: 1432619931 },
    status: "completed",
    conclusion: "success",
  };
  expect(
    selectRun(
      [{ ...run, id: 9, run_number: 9, conclusion: "failure" }, run],
      run.path,
      b.head,
    ).id,
  ).toBe(10);
  expect(() =>
    selectRun(
      [run, { ...run, id: 11, run_number: 11, conclusion: "cancelled" }],
      run.path,
      b.head,
    ),
  ).toThrow();
  expect(() =>
    selectRun(
      [
        { ...run, run_attempt: 1 },
        { ...run, run_attempt: 2, conclusion: "failure" },
      ],
      run.path,
      b.head,
    ),
  ).toThrow();
});
test("stable check job requires Actions App, suite and successful exact job", () => {
  const run = { id: 10, check_suite_id: 50 };
  const jobs = [
    {
      name: "Nucleus Docs",
      status: "completed",
      conclusion: "success",
      run_id: 10,
      check_run_url:
        "https://api.github.com/repos/headironc/nucleus/check-runs/70",
    },
  ];
  const checks = [
    {
      id: 70,
      app: { id: 15368 },
      check_suite: { id: 50 },
      conclusion: "success",
    },
  ];
  verifyJobs(jobs, checks, run, ["Nucleus Docs"]);
  expect(() =>
    verifyJobs(jobs, [{ ...checks[0], app: { id: 1 } }], run, ["Nucleus Docs"]),
  ).toThrow();
  expect(() => verifyJobs([], checks, run, ["Nucleus Docs"])).toThrow();
});
