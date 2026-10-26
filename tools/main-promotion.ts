import { readFileSync } from "node:fs";
import { isDeepStrictEqual } from "node:util";
import { Gate, ghTransport, type Transport } from "./review-gate";
import { Queue } from "./review-queue";
import {
  array,
  field,
  get,
  object,
  parse,
  string,
  type Json,
  type Record,
} from "./review-json";
import {
  appId,
  deployment,
  ownerId,
  repository,
  repositoryId,
  workflow,
  type PromotionDeployment,
} from "./promotion-deployment";
import {
  api,
  ApiFailure,
  mint,
  revoke,
  verifyRepository,
  type Api,
  type Credentials,
} from "./promotion-credentials";
import {
  history,
  OperationStore,
  originalEnded,
  type Operation,
} from "./promotion-operation";
import { commit, git, validateRelease } from "./release-candidate";

export type Request = { pr: bigint; dev: string; base: string; head: string };
export type Execution = { source: string; run: bigint; attempt: bigint };
const root = `repos/${repository}`;
let stage = "deployment";
let diagnostic = "";

export function request(value: Json): Request {
  const input = object(value);
  if (!/^[1-9][0-9]*$/.test(string(input.pr)) || input.accepted_dev !== "true")
    throw new Error(
      "Promotion requires an explicit accepted-dev declaration and PR",
    );
  return {
    pr: BigInt(string(input.pr)),
    dev: commit(string(input.dev)),
    base: commit(string(input.base)),
    head: commit(string(input.head)),
  };
}

export async function preflight(
  read: Api,
  env: NodeJS.ProcessEnv,
): Promise<Execution> {
  if (
    env.GITHUB_REPOSITORY !== repository ||
    env.GITHUB_REPOSITORY_ID !== String(repositoryId) ||
    env.GITHUB_REPOSITORY_OWNER_ID !== String(ownerId) ||
    env.GITHUB_EVENT_NAME !== "workflow_dispatch" ||
    env.GITHUB_REF !== "refs/heads/dev" ||
    env.GITHUB_WORKFLOW_REF !== `${repository}/${workflow}@refs/heads/dev` ||
    env.GITHUB_ACTOR_ID !== String(ownerId) ||
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
    field(run, "head_sha") !== execution.source ||
    field(run, "head_branch") !== "dev" ||
    field(run, "path") !== workflow ||
    field(run, "event") !== "workflow_dispatch" ||
    field(run, "status") !== "in_progress" ||
    field(field(run, "repository"), "id") !== repositoryId ||
    field(field(run, "actor"), "id") !== ownerId ||
    field(field(run, "triggering_actor"), "id") !== ownerId
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
  config: PromotionDeployment,
): Promise<Json[]> {
  const rules = await pages(read, `${root}/rules/branches/main`);
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
          "merge",
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

export function bindPull(pull: Json, wanted: Request): string {
  const base = field(pull, "base"),
    head = field(pull, "head");
  if (
    field(pull, "number") !== wanted.pr ||
    field(pull, "url") !==
      `https://api.github.com/${root}/pulls/${wanted.pr}` ||
    field(field(base, "repo"), "id") !== repositoryId ||
    field(base, "ref") !== "main" ||
    field(field(head, "repo"), "id") !== repositoryId ||
    field(head, "sha") !== wanted.head ||
    !/^release\/[a-zA-Z0-9][a-zA-Z0-9._/-]*$/.test(string(field(head, "ref")))
  )
    throw new Error("Promotion PR identity, source or target mismatch");
  return string(field(pull, "node_id"));
}

export async function observed(
  read: Api,
  value: Operation,
): Promise<string | null> {
  const pull = await read(`${root}/pulls/${value.pr}`);
  if (bindPull(pull, value) !== value.prId)
    throw new Error("Original PR identity changed");
  const mergedState = field(pull, "merged");
  if (mergedState === false) return null;
  if (mergedState !== true)
    throw new Error("Original PR merge state is unknown");
  const merged = commit(string(field(pull, "merge_commit_sha")));
  const result = await read(`${root}/commits/${merged}`);
  if (
    field(result, "sha") !== merged ||
    !isDeepStrictEqual(
      array(field(result, "parents")).map((parent) => field(parent, "sha")),
      [value.base, value.head],
    ) ||
    field(field(field(result, "commit"), "tree"), "sha") !== value.tree ||
    field(field(field(result, "commit"), "verification"), "verified") !== true
  )
    throw new Error(
      "Merged commit parents, tree or signature differ from the accepted release",
    );
  const ref = await read(`${root}/git/ref/heads/main`);
  if (field(ref, "ref") !== "refs/heads/main")
    throw new Error("Main ref identity mismatch");
  const tip = commit(string(field(field(ref, "object"), "sha")));
  const comparison = await read(`${root}/compare/${merged}...${tip}`);
  if (
    field(field(comparison, "base_commit"), "sha") !== merged ||
    !["identical", "ahead"].includes(string(field(comparison, "status")))
  )
    throw new Error("Merged release is not reachable from main");
  return merged;
}

export async function recover(
  store: OperationStore,
  read: Api,
  actions: Api,
  installation: bigint,
): Promise<boolean> {
  const pending = history(await store.read());
  if (pending === null) return false;
  if (pending.value.installation !== installation)
    throw new Error("Original operation belongs to another installation");
  if (!(await originalEnded(actions, pending)))
    throw new Error("Original promotion execution has not ended");
  const result = await observed(read, pending.value);
  if (result === null)
    throw new Error(
      "Original merge outcome remains unknown; no new write is permitted",
    );
  await store.finish(pending, "merged", result);
  return true;
}

export async function execute(
  store: OperationStore,
  merge: Api,
  value: Operation,
  final: () => Promise<void>,
): Promise<string> {
  if (history(await store.read()) !== null)
    throw new Error("An earlier merge is unresolved");
  const intent = await store.append(value);
  try {
    await final();
  } catch (error) {
    await store.finish(intent, "not-sent");
    throw error;
  }
  let rejection = false;
  try {
    await merge(`${root}/pulls/${value.pr}/merge`, "PUT", {
      sha: value.head,
      merge_method: "merge",
    });
  } catch (error) {
    rejection =
      error instanceof ApiFailure && [405, 409, 422].includes(error.status);
  }
  const result = await observed(merge, value);
  if (result !== null) {
    await store.finish(intent, "merged", result);
    return result;
  }
  if (rejection) await store.finish(intent, "failed");
  throw new Error(
    rejection
      ? "GitHub rejected promotion; a new explicit request is required"
      : "Merge outcome is unknown; recover the original operation without resending",
  );
}

// This adapter is the existing Gate's sole personal-main contract consumer.
export function admission(
  credentials: Credentials,
  actions: Transport,
  rules: Json[],
  review = ghTransport(credentials.tokens.review),
): Gate {
  return new Gate({
    repo: repository,
    queueEnabled: true,
    gh: review,
    actions,
    mainRules(actual) {
      if (!isDeepStrictEqual(actual, rules))
        throw new Error("Main rules changed during review");
    },
  });
}

export async function ci(read: Api, wanted: Request): Promise<Json[]> {
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
          field(field(pr, "base"), "ref") === "main",
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

export async function release(
  wanted: Request,
  source: string,
  read: Api,
  cwd?: string,
): Promise<Record> {
  const pull = await read(`${root}/pulls/${wanted.pr}`),
    id = bindPull(pull, wanted);
  if (
    field(pull, "state") !== "open" ||
    field(pull, "draft") !== false ||
    field(pull, "merged") !== false ||
    field(field(pull, "base"), "sha") !== wanted.base
  )
    throw new Error("Promotion PR is not the requested open release candidate");
  const main = await read(`${root}/git/ref/heads/main`),
    dev = await read(`${root}/git/ref/heads/dev`);
  if (
    field(main, "ref") !== "refs/heads/main" ||
    field(field(main, "object"), "sha") !== wanted.base ||
    field(dev, "ref") !== "refs/heads/dev" ||
    field(field(dev, "object"), "sha") !== source
  )
    throw new Error("Promotion baseline or controller changed");
  const tree = validateRelease(
    wanted.dev,
    wanted.base,
    wanted.head,
    source,
    cwd,
  );
  const head = await read(`${root}/commits/${wanted.head}`);
  if (
    field(head, "sha") !== wanted.head ||
    field(field(field(head, "commit"), "verification"), "verified") !== true
  )
    throw new Error("Release candidate has no verified signature");
  return { id, tree, headRef: field(field(pull, "head"), "ref") };
}

export function reviewSnapshot(queue: Queue, wanted: Request): Record {
  const review = queue.reviewSnapshot(repository, wanted.pr);
  if (
    array(field(review, "reasons")).length ||
    field(field(review, "view"), "headRefOid") !== wanted.head ||
    field(field(review, "view"), "baseRefName") !== "main" ||
    field(field(review, "view"), "baseRefOid") !== wanted.base ||
    get(field(review, "view"), "stack") !== null
  )
    throw new Error(
      "Release review is missing, stale or bound to another candidate",
    );
  return review;
}

async function snapshot(
  wanted: Request,
  credentials: Credentials,
  actions: Transport,
  config: PromotionDeployment,
  source: string,
  read: Api,
  actionsRead: Api,
): Promise<Record> {
  const rules = await protection(read, config);
  const candidate = await release(wanted, source, read);
  const gate = admission(credentials, actions, rules),
    review = reviewSnapshot(new Queue(gate), wanted);
  return { ...candidate, review, rules, ci: await ci(actionsRead, wanted) };
}

export async function main(
  env: NodeJS.ProcessEnv = process.env,
): Promise<void> {
  const config = deployment();
  stage = "trusted invocation";
  const actionsToken = string(env.GH_ACTIONS_TOKEN),
    actions = api(actionsToken);
  const execution = await preflight(actions, env);
  diagnostic = ` ${repository} run ${execution.run}/${execution.attempt} source ${execution.source}.`;
  if (git(["rev-parse", "HEAD"]) !== execution.source)
    throw new Error("Checkout is not the trusted controller");
  if (process.argv.includes("--preflight")) return;
  const event = parse(readFileSync(string(env.GITHUB_EVENT_PATH), "utf8"));
  const input = field(event, "inputs"),
    mode = field(input, "mode");
  if (mode !== "merge" && mode !== "recover")
    throw new Error("Unsupported promotion mode");
  if (mode === "merge" && execution.attempt !== 1n)
    throw new Error("Reruns cannot send a merge; dispatch recovery instead");
  const wanted = mode === "merge" ? request(input) : null;
  if (wanted !== null)
    diagnostic += ` PR ${wanted.pr} main ${wanted.base} candidate ${wanted.head}.`;
  const key = string(env.NUCLEUS_PROMOTION_PRIVATE_KEY);
  delete process.env.NUCLEUS_PROMOTION_PRIVATE_KEY;
  stage = "credential issuance";
  const credentials = await mint(key);
  try {
    stage = "original operation recovery";
    const read = api(credentials.tokens.merge),
      store = new OperationStore(api(credentials.tokens.record), config);
    await recover(store, read, actions, credentials.installation);
    if (wanted === null) return;
    stage = "release object verification";
    // Fetch objects through the read-only Actions token; no candidate checkout,
    // dependency install, hook or local action is executed in this process.
    const previousToken = process.env.GH_TOKEN;
    process.env.GH_TOKEN = actionsToken;
    try {
      git([
        "-c",
        "credential.helper=",
        "-c",
        "credential.helper=!gh auth git-credential",
        "fetch",
        "--no-tags",
        `https://github.com/${repository}.git`,
        wanted.dev,
        wanted.base,
        wanted.head,
      ]);
    } finally {
      if (previousToken === undefined) delete process.env.GH_TOKEN;
      else process.env.GH_TOKEN = previousToken;
    }
    const rules = await protection(read, config);
    // Establish release eligibility before publishing any approval.
    await release(wanted, execution.source, read);
    stage = "review admission";
    const gate = admission(credentials, ghTransport(actionsToken), rules);
    if (new Queue(gate).reconcile(repository, wanted.pr) !== 0)
      throw new Error("Promotion admission is not ready");
    const initial = await snapshot(
      wanted,
      credentials,
      ghTransport(actionsToken),
      config,
      execution.source,
      read,
      actions,
    );
    const entries = await store.read();
    const value: Operation = {
      version: 1n,
      stage: "intent",
      previous: entries.at(-1)?.id ?? null,
      intent: null,
      commit: null,
      ...wanted,
      prId: string(initial.id),
      tree: string(initial.tree),
      lifecycle: field(field(initial.review, "view"), "lifecycle"),
      ...execution,
      installation: credentials.installation,
      repository: repositoryId,
      actor: ownerId,
      branch: "main",
    };
    stage = "merge intent, final verification and result readback";
    const result = await execute(store, read, value, async () => {
      await preflight(actions, env);
      if (
        !isDeepStrictEqual(
          await snapshot(
            wanted,
            credentials,
            ghTransport(actionsToken),
            config,
            execution.source,
            read,
            actions,
          ),
          initial,
        )
      )
        throw new Error("Release evidence changed before merge");
    });
    console.log(`Confirmed main promotion #${wanted.pr}: ${result}`);
  } finally {
    try {
      await revoke(Object.values(credentials.tokens));
    } catch (error) {
      stage = "credential cleanup (revocation unconfirmed)";
      throw error;
    }
  }
}

if (import.meta.main) {
  try {
    await main();
  } catch (error) {
    // Fetch, crypto and child-process errors may contain secret-bearing inputs.
    console.error(
      `Main promotion stopped during ${stage}; no automatic retry.${diagnostic} ${error instanceof ApiFailure ? error.message : "Inspect the bound operation and trusted inputs."}`,
    );
    process.exitCode = 1;
  }
}
