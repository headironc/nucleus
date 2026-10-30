import { spawnSync } from "node:child_process";
import { writeFileSync } from "node:fs";
import {
  actionsId,
  repository,
  repositoryId,
  requireThat,
  sha,
  type Binding,
} from "./identity.ts";
import type { Record } from "./providers.ts";

export const stableWorkflows = [
  "docs.yml",
  "maintenance-tools.yml",
  "rust-checks.yml",
  "packages.yml",
];
export type Receipt = {
  version: 1;
  repository: string;
  repositoryId: number;
  pr: number;
  target: string;
  head: string;
  merge: string;
  parents: string[];
  tree: string;
  workflow: string;
  run: number;
  attempt: number;
};
export function produceReceipt(
  env: NodeJS.ProcessEnv,
  event: Record,
  cwd: string,
): Receipt {
  const git = (args: string[]) => {
    const result = spawnSync("git", ["--no-replace-objects", ...args], {
      cwd,
      env: {
        PATH: env.PATH,
        GIT_CONFIG_NOSYSTEM: "1",
        GIT_CONFIG_GLOBAL: "/dev/null",
      },
      encoding: "utf8",
      timeout: 10000,
    });
    requireThat(result.status === 0, "Cannot read executed Git candidate");
    return result.stdout.trim();
  };
  // Raw commit headers retain parents at a depth-one checkout's shallow boundary.
  const headers = git(["cat-file", "-p", "HEAD"])
    .split("\n\n", 1)[0]
    .split("\n");
  const parents = headers
    .filter((line) => line.startsWith("parent "))
    .map((line) => line.slice(7));
  const merge = git(["rev-parse", "HEAD"]);
  const tree = git(["rev-parse", "HEAD^{tree}"]);
  const workflow = env.GITHUB_WORKFLOW_REF?.split("@")[0].replace(
    `${repository}/`,
    "",
  );
  requireThat(
    env.GITHUB_EVENT_NAME === "pull_request" &&
      env.GITHUB_REPOSITORY === repository &&
      Number(env.GITHUB_REPOSITORY_ID) === repositoryId &&
      event.repository?.id === repositoryId,
    "Receipt requires the native repository PR event",
  );
  requireThat(
    sha(merge) &&
      merge === env.GITHUB_SHA &&
      sha(tree) &&
      parents.length === 2 &&
      parents.every(sha) &&
      parents[1] === event.pull_request?.head?.sha,
    "Executed merge parents/head differ from event",
  );
  requireThat(
    event.pull_request?.base?.repo?.id === repositoryId &&
      ["dev", "main"].includes(event.pull_request?.base?.ref) &&
      Number.isSafeInteger(event.number) &&
      event.number > 0,
    "Invalid receipt PR target",
  );
  requireThat(
    workflow &&
      (stableWorkflows
        .map((f) => `.github/workflows/${f}`)
        .includes(workflow) ||
        workflow === ".github/workflows/halo-publish-checks.yml"),
    "Unknown receipt workflow",
  );
  const run = Number(env.GITHUB_RUN_ID),
    attempt = Number(env.GITHUB_RUN_ATTEMPT);
  requireThat(
    Number.isSafeInteger(run) &&
      run > 0 &&
      Number.isSafeInteger(attempt) &&
      attempt > 0,
    "Invalid receipt run/attempt",
  );
  return {
    version: 1,
    repository,
    repositoryId,
    pr: event.number,
    target: event.pull_request.base.ref,
    head: parents[1],
    merge,
    parents,
    tree,
    workflow,
    run,
    attempt,
  };
}

export function verifyReceipt(
  r: Receipt,
  run: Record,
  b: Binding,
  tree: string,
  path: string,
) {
  requireThat(
    r.version === 1 &&
      r.repository === repository &&
      r.repositoryId === repositoryId &&
      r.pr === b.pr &&
      r.target === "dev" &&
      r.head === b.head &&
      r.merge === b.merge &&
      r.parents?.length === 2 &&
      r.parents[0] === b.base &&
      r.parents[1] === b.head &&
      r.tree === tree &&
      r.workflow === path &&
      r.run === run.id &&
      r.attempt === run.run_attempt,
    "CI receipt is not the current tested H/B/M/run/attempt",
  );
}

export async function selectRun(
  runs: Record[],
  path: string,
  b: Binding,
  readReceipt: (run: Record) => Promise<Receipt>,
) {
  const candidates = runs.filter(
    (r) =>
      r.path === path && r.head_sha === b.head && r.event === "pull_request",
  );
  requireThat(candidates.length > 0, `Missing CI ${path}`);
  candidates.sort(
    (a, b) => b.run_number - a.run_number || b.run_attempt - a.run_attempt,
  );
  for (const run of candidates) {
    const associations: Record[] | undefined = Array.isArray(run.pull_requests)
      ? run.pull_requests
      : undefined;
    const complete = (pr: Record) =>
      Number.isSafeInteger(pr.number) &&
      pr.number > 0 &&
      Number.isSafeInteger(pr.base?.repo?.id) &&
      pr.base.repo.id > 0 &&
      Number.isSafeInteger(pr.head?.repo?.id) &&
      pr.head.repo.id > 0 &&
      typeof pr.base?.ref === "string" &&
      pr.base.ref.length > 0 &&
      sha(pr.base?.sha) &&
      sha(pr.head?.sha);
    const current = associations?.some(
      (pr) =>
        complete(pr) &&
        pr.number === b.pr &&
        pr.base.repo.id === repositoryId &&
        pr.base.ref === "dev" &&
        pr.head.repo.id === repositoryId &&
        pr.head.sha === b.head,
    );
    if (!current && associations?.length && associations.every(complete))
      continue;
    requireThat(
      run.repository?.id === repositoryId &&
        run.head_repository?.id === repositoryId &&
        run.status === "completed" &&
        run.conclusion === "success",
      `Latest CI not successful or PR association unknown: ${path}`,
    );
    const receipt = await readReceipt(run);
    requireThat(
      receipt.version === 1 &&
        receipt.repository === repository &&
        receipt.repositoryId === repositoryId &&
        Number.isSafeInteger(receipt.pr) &&
        receipt.pr > 0 &&
        ["dev", "main"].includes(receipt.target) &&
        receipt.head === b.head &&
        sha(receipt.merge) &&
        sha(receipt.tree) &&
        receipt.parents?.length === 2 &&
        receipt.parents.every(sha) &&
        receipt.parents[1] === b.head &&
        receipt.workflow === path &&
        receipt.run === run.id &&
        receipt.attempt === run.run_attempt,
      "Unknown PR association has no trustworthy run receipt",
    );
    const ours = receipt.pr === b.pr && receipt.target === "dev";
    requireThat(
      !current || ours,
      "Explicit PR association contradicts receipt",
    );
    if (ours) return run;
  }
  throw new Error(`Missing CI for requested PR: ${path}`);
}

export function verifyJobs(
  jobs: Record[],
  checks: Record[],
  run: Record,
  names: string[],
) {
  for (const name of names) {
    const matches = jobs.filter((j) => j.name === name);
    requireThat(
      matches.length === 1 &&
        matches[0].status === "completed" &&
        matches[0].conclusion === "success" &&
        matches[0].run_id === run.id,
      `Missing successful stable job ${name}`,
    );
    const check = checks.filter(
      (c) => c.id === Number(matches[0].check_run_url?.split("/").at(-1)),
    );
    requireThat(
      check.length === 1 &&
        check[0].app?.id === actionsId &&
        check[0].check_suite?.id === run.check_suite_id &&
        check[0].conclusion === "success",
      `Untrusted Actions job ${name}`,
    );
  }
}

if (import.meta.main) {
  const event = await Bun.file(process.env.GITHUB_EVENT_PATH!).json();
  writeFileSync(
    process.env.CI_RECEIPT_PATH!,
    JSON.stringify(produceReceipt(process.env, event, process.cwd())) + "\n",
    { flag: "wx" },
  );
}
