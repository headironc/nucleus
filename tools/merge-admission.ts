import { isDeepStrictEqual } from "node:util";
import { array, field, object, string, type Json } from "./review-json";
import {
  appId,
  ownerId,
  repository,
  repositoryId,
  workflow,
  devWorkflow,
  type Protection,
} from "./merge-deployment";
import { verifyRepository, type Api } from "./merge-credentials";
import { commit } from "./release-candidate";

export type Execution = { source: string; run: bigint; attempt: bigint };
type Request = { pr: bigint; head: string };
const root = `repos/${repository}`;

export async function preflight(
  read: Api,
  env: NodeJS.ProcessEnv,
  target: "main" | "dev" = "main",
): Promise<Execution> {
  const path = target === "main" ? workflow : devWorkflow;
  const manual = env.GITHUB_EVENT_NAME === "workflow_dispatch";
  const events = [
    "pull_request_target",
    "push",
    "workflow_run",
    "issue_comment",
    "schedule",
    "workflow_dispatch",
  ];
  if (
    env.GITHUB_REPOSITORY !== repository ||
    env.GITHUB_REPOSITORY_ID !== String(repositoryId) ||
    env.GITHUB_REPOSITORY_OWNER_ID !== String(ownerId) ||
    (target === "main"
      ? !manual
      : !events.includes(env.GITHUB_EVENT_NAME ?? "")) ||
    env.GITHUB_REF !== "refs/heads/dev" ||
    env.GITHUB_WORKFLOW_REF !== `${repository}/${path}@refs/heads/dev` ||
    (manual && env.GITHUB_ACTOR_ID !== String(ownerId)) ||
    !env.GITHUB_SHA ||
    env.GITHUB_WORKFLOW_SHA !== env.GITHUB_SHA ||
    !/^[1-9][0-9]*$/.test(env.GITHUB_RUN_ID ?? "") ||
    !/^[1-9][0-9]*$/.test(env.GITHUB_RUN_ATTEMPT ?? "")
  )
    throw new Error(
      "Promotion requires the trusted dev workflow and authorized requester",
    );
  const execution = {
    source: commit(env.GITHUB_SHA),
    run: BigInt(env.GITHUB_RUN_ID!),
    attempt: BigInt(env.GITHUB_RUN_ATTEMPT!),
  };
  verifyRepository(await read(root));
  const dev = await read(`${root}/git/ref/heads/dev`);
  if (
    field(dev, "ref") !== "refs/heads/dev" ||
    field(field(dev, "object"), "sha") !== execution.source
  )
    throw new Error("Controller version is no longer the trusted dev tip");
  const run = await read(
    `${root}/actions/runs/${execution.run}/attempts/${execution.attempt}`,
  );
  if (
    field(run, "id") !== execution.run ||
    field(run, "run_attempt") !== execution.attempt ||
    (env.GITHUB_EVENT_NAME !== "pull_request_target" &&
      (field(run, "head_sha") !== execution.source ||
        field(run, "head_branch") !== "dev")) ||
    field(run, "path") !== path ||
    field(run, "event") !== env.GITHUB_EVENT_NAME ||
    field(run, "status") !== "in_progress" ||
    field(field(run, "repository"), "id") !== repositoryId ||
    (manual &&
      (field(field(run, "actor"), "id") !== ownerId ||
        field(field(run, "triggering_actor"), "id") !== ownerId))
  )
    throw new Error(
      "Promotion run identity differs from the trusted invocation",
    );
  return execution;
}

export async function pages(read: Api, path: string): Promise<Json[]> {
  const rows: Json[] = [];
  for (let page = 1; ; page++) {
    const batch = array(
      await read(
        `${path}${path.includes("?") ? "&" : "?"}per_page=100&page=${page}`,
      ),
    );
    rows.push(...batch);
    if (batch.length < 100) return rows;
  }
}

export async function protection(
  read: Api,
  config: Protection,
  target: "main" | "dev" = "main",
): Promise<Json[]> {
  const rules = await pages(read, `${root}/rules/branches/${target}`);
  const ids = [
    ...new Set(rules.map((rule) => field(rule, "ruleset_id"))),
  ].sort();
  if (
    !isDeepStrictEqual(ids, [...config.rules].sort()) ||
    !ids.includes(config.updateRule)
  )
    throw new Error("Effective main rules differ from trusted deployment");
  for (const id of ids) {
    const rule = await read(`${root}/rulesets/${id}`);
    if (
      field(rule, "id") !== id ||
      field(rule, "source_type") !== "Repository" ||
      field(rule, "source") !== repository ||
      field(rule, "target") !== "branch" ||
      field(rule, "enforcement") !== "active"
    )
      throw new Error("Main ruleset identity or enforcement mismatch");
    // GitHub omits bypass_actors for credentials without ruleset write access.
    // The complete bypass policy is a deployment prerequisite, not inferred
    // from an omitted field. Validate it whenever the API exposes it.
    const bypass = Object.hasOwn(object(rule), "bypass_actors")
      ? array(field(rule, "bypass_actors"))
      : undefined;
    if (id === config.updateRule) {
      const updates = array(field(rule, "rules"));
      if (
        updates.length !== 1 ||
        field(updates[0], "type") !== "update" ||
        (Object.hasOwn(object(updates[0]), "parameters") &&
          !isDeepStrictEqual(field(updates[0], "parameters"), {
            update_allows_fetch_and_merge: false,
          })) ||
        (bypass !== undefined &&
          !isDeepStrictEqual(bypass, [
            {
              actor_id: appId,
              actor_type: "Integration",
              bypass_mode: "pull_request",
            },
          ]))
      )
        throw new Error(
          "Only Cladiron may bypass the update-only rule through a PR",
        );
    } else if (bypass !== undefined && bypass.length)
      throw new Error("Main protection has another bypass");
  }
  for (const type of ["non_fast_forward", "deletion", "required_signatures"])
    if (!rules.some((rule) => field(rule, "type") === type))
      throw new Error(`Main requires ${type}`);
  const pulls = rules.filter((rule) => field(rule, "type") === "pull_request");
  if (
    !pulls.length ||
    pulls.some((rule) => {
      const parameters = field(rule, "parameters");
      return (
        !isDeepStrictEqual(field(parameters, "allowed_merge_methods"), [
          target === "main" ? "merge" : "squash",
        ]) ||
        field(parameters, "required_review_thread_resolution") !== true ||
        field(parameters, "dismiss_stale_reviews_on_push") !== true ||
        field(parameters, "require_code_owner_review") !== true
      );
    })
  )
    throw new Error(
      "Main PR protections or merge method differ from deployment",
    );
  return rules;
}

export async function ci(
  read: Api,
  wanted: Request,
  target: "main" | "dev" = "main",
): Promise<Json[]> {
  const result: Json[] = [];
  for (const path of ["docs.yml", "rust.yml", "halo.yml"]) {
    const runs: Json[] = [];
    let total: bigint | undefined;
    for (let page = 1; ; page++) {
      const response = await read(
        `${root}/actions/workflows/${path}/runs?event=pull_request&head_sha=${wanted.head}&per_page=100&page=${page}`,
      );
      const count = field(response, "total_count"),
        batch = array(field(response, "workflow_runs"));
      if (typeof count !== "bigint" || (total !== undefined && count !== total))
        throw new Error("CI pagination changed");
      total = count;
      runs.push(...batch);
      if (batch.length < 100) break;
    }
    if (BigInt(runs.length) !== total || !runs.length)
      throw new Error("Release CI coverage is incomplete");
    const matching = runs.filter((run) =>
      array(field(run, "pull_requests")).some(
        (pr) =>
          field(pr, "number") === wanted.pr &&
          field(field(pr, "head"), "sha") === wanted.head &&
          field(field(pr, "base"), "ref") === target,
      ),
    );
    matching.sort((left, right) => {
      const a = field(left, "id"),
        b = field(right, "id");
      if (typeof a !== "bigint" || typeof b !== "bigint")
        throw new Error("CI run identity is invalid");
      return a < b ? 1 : a > b ? -1 : 0;
    });
    const latest = matching[0];
    if (
      !latest ||
      field(latest, "head_sha") !== wanted.head ||
      field(latest, "event") !== "pull_request" ||
      field(latest, "path") !== `.github/workflows/${path}` ||
      field(field(latest, "repository"), "id") !== repositoryId ||
      typeof field(latest, "id") !== "bigint" ||
      (field(latest, "id") as bigint) <= 0n ||
      typeof field(latest, "run_attempt") !== "bigint" ||
      (field(latest, "run_attempt") as bigint) <= 0n ||
      field(latest, "status") !== "completed" ||
      field(latest, "conclusion") !== "success"
    )
      throw new Error("Latest release CI has not completed successfully");
    result.push({
      id: field(latest, "id"),
      attempt: field(latest, "run_attempt"),
      path: field(latest, "path"),
      head: wanted.head,
    });
  }
  return result;
}
