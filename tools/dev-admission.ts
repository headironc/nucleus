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
  deployment,
  ownerId,
  repository,
  repositoryId,
  type Protection,
} from "./merge-deployment";
import {
  api,
  ApiFailure,
  mint,
  mintReview,
  revoke,
  type Api,
  type Credentials,
} from "./merge-credentials";
import { ci, pages, preflight, protection } from "./merge-admission";
import {
  bindPull,
  execute,
  recover,
  OperationStore,
  type Operation,
} from "./merge-operation";
import { commit, git } from "./release-candidate";

const root = `repos/${repository}`;
export type Request = { pr: bigint; head: string };
let stage = "deployment";
let diagnostic = "";

export function request(input: Json): Request {
  const pr = string(field(input, "pr"));
  if (!/^[1-9][0-9]*$/.test(pr))
    throw new Error("An exact PR number is required");
  return { pr: BigInt(pr), head: commit(string(field(input, "head"))) };
}

export async function candidate(
  wanted: Request,
  source: string,
  read: Api,
): Promise<Record> {
  const pull = await read(`${root}/pulls/${wanted.pr}`);
  const id = bindPull(pull, wanted, "dev");
  if (
    field(pull, "state") !== "open" ||
    field(pull, "draft") !== false ||
    field(pull, "merged") !== false ||
    field(field(pull, "base"), "sha") !== source
  )
    throw new Error(
      "PR is not the requested open candidate on the current dev baseline",
    );
  const target = await read(`${root}/git/ref/heads/dev`);
  if (
    field(target, "ref") !== "refs/heads/dev" ||
    field(field(target, "object"), "sha") !== source
  )
    throw new Error("Dev advanced; synchronize the PR and renew its evidence");
  const comparison = await read(`${root}/compare/${source}...${wanted.head}`);
  if (
    !["ahead", "identical"].includes(string(field(comparison, "status"))) ||
    field(field(comparison, "merge_base_commit"), "sha") !== source
  )
    throw new Error("Candidate must contain the current dev baseline");
  const head = await read(`${root}/commits/${wanted.head}`);
  if (
    field(head, "sha") !== wanted.head ||
    field(field(field(head, "commit"), "verification"), "verified") !== true
  )
    throw new Error("Candidate has no verified signature");
  return {
    id,
    base: source,
    tree: commit(string(field(field(field(head, "commit"), "tree"), "sha"))),
    headRef: field(field(pull, "head"), "ref"),
  };
}

export function admission(
  credentials: Credentials<"review">,
  actions: Transport,
  rules: Json[],
  wanted: Request,
  base: string,
  review = ghTransport(credentials.tokens.review),
): Gate {
  return new Gate({
    repo: repository,
    queueEnabled: true,
    gh: review,
    actions,
    devRules(actual, view) {
      if (!isDeepStrictEqual(actual, rules))
        throw new Error("Dev rules changed during review");
      if (view && (view.headRefOid !== wanted.head || view.baseRefOid !== base))
        throw new Error("Dev candidate changed after eligibility verification");
    },
  });
}

export function reviewSnapshot(
  queue: Queue,
  wanted: Request,
  base: string,
): Record {
  const review = queue.reviewSnapshot(repository, wanted.pr),
    view = field(review, "view");
  if (
    array(field(review, "reasons")).length ||
    field(view, "headRefOid") !== wanted.head ||
    field(view, "baseRefName") !== "dev" ||
    field(view, "baseRefOid") !== base ||
    get(view, "stack") !== null
  )
    throw new Error(
      "Dev review is missing, stale or bound to another candidate",
    );
  return review;
}

// All notifications select existing PRs only. They confer no merge authority.
export async function targets(
  event: Json,
  name: string,
  read: Api,
): Promise<bigint[]> {
  const number = (value: Json): bigint => {
    if (typeof value !== "bigint" || value <= 0n)
      throw new Error("Invalid event PR number");
    return value;
  };
  if (name === "pull_request_target")
    return [number(field(field(event, "pull_request"), "number"))];
  if (name === "issue_comment") {
    const issue = field(event, "issue");
    return get(issue, "pull_request") === null
      ? []
      : [number(field(issue, "number"))];
  }
  let run: Json = null;
  if (name === "workflow_run") {
    run = field(event, "workflow_run");
    if (
      field(field(run, "repository"), "id") !== repositoryId ||
      field(run, "event") === "merge_group"
    )
      return [];
    const prs = array(field(run, "pull_requests"));
    if (prs.length)
      return [...new Set(prs.map((pr) => number(field(pr, "number"))))];
    const path = string(field(run, "path")).split("@")[0];
    const title = string(field(run, "display_title"));
    const match =
      path === ".github/workflows/review-context.yml"
        ? /^context:([1-9][0-9]*):/.exec(title)
        : path === ".github/workflows/review-activity.yml"
          ? /^activity:([1-9][0-9]*)$/.exec(title)
          : null;
    if (match) return [BigInt(match[1]!)];
  }
  const pulls = await pages(read, `${root}/pulls?state=open&base=dev`);
  return pulls
    .filter(
      (pull) =>
        run === null ||
        (field(field(pull, "head"), "ref") === field(run, "head_branch") &&
          field(field(field(pull, "head"), "repo"), "id") ===
            field(field(run, "head_repository"), "id")),
    )
    .map((pull) => number(field(pull, "number")));
}

export async function evaluate(
  wanted: Request,
  source: string,
  read: Api,
  actions: Api,
  queue: Queue,
  config: Protection,
): Promise<Record> {
  const view = queue.gate.prView(repository, wanted.pr);
  try {
    const snapshot = async () => ({
      candidate: await candidate(wanted, source, read),
      rules: await protection(read, config, "dev"),
      ci: await ci(actions, wanted, "dev"),
    });
    const initial = await snapshot();
    if (queue.reconcile(repository, wanted.pr) !== 0)
      throw new Error("Dev admission is not ready");
    const review = reviewSnapshot(queue, wanted, source);
    if (!isDeepStrictEqual(await snapshot(), initial))
      throw new Error("Dev eligibility changed during admission");
    return { ...initial, review };
  } catch (error) {
    // A stale green check must not survive failed eligibility or readback.
    queue.reject(repository, wanted.pr, view);
    throw error;
  }
}

export async function main(
  env: NodeJS.ProcessEnv = process.env,
): Promise<void> {
  const config = deployment();
  stage = "trusted invocation";
  const actionsToken = string(env.GH_ACTIONS_TOKEN),
    actions = api(actionsToken);
  const execution = await preflight(actions, env, "dev");
  diagnostic = ` ${repository} run ${execution.run}/${execution.attempt} source ${execution.source}.`;
  if (git(["rev-parse", "HEAD"]) !== execution.source)
    throw new Error("Checkout is not the trusted controller");
  if (process.argv.includes("--preflight")) return;
  const event = parse(readFileSync(string(env.GITHUB_EVENT_PATH), "utf8"));
  const manual = env.GITHUB_EVENT_NAME === "workflow_dispatch";
  const mode = manual ? field(field(event, "inputs"), "mode") : "evaluate";
  if (!["evaluate", "merge", "recover"].includes(string(mode)))
    throw new Error("Unsupported dev mode");
  if (mode === "merge" && execution.attempt !== 1n)
    throw new Error("Reruns cannot send a merge; dispatch recovery instead");
  const wanted = mode === "merge" ? request(field(event, "inputs")) : null;
  const key = string(env.NUCLEUS_DEV_PRIVATE_KEY);
  delete process.env.NUCLEUS_DEV_PRIVATE_KEY;
  stage = "credential issuance";
  const credentials =
    mode === "evaluate" ? await mintReview(key) : await mint(key);
  try {
    let store: OperationStore | undefined;
    let merge: Api | undefined;
    if (mode !== "evaluate") {
      const full = credentials as Credentials;
      merge = api(full.tokens.merge);
      store = new OperationStore(api(full.tokens.record), config.ledger);
      stage = "original operation recovery";
      await recover(store, merge, actions, credentials.installation);
      if (mode === "recover") return;
    }
    const reviewRead = api(credentials.tokens.review);
    const rules = await protection(reviewRead, config.dev, "dev");
    const queueFor = (wanted: Request) =>
      new Queue(
        admission(
          credentials,
          ghTransport(actionsToken),
          rules,
          wanted,
          execution.source,
        ),
      );
    if (mode === "evaluate") {
      let failed = false;
      for (const pr of await targets(
        event,
        string(env.GITHUB_EVENT_NAME),
        reviewRead,
      )) {
        stage = `admission for PR ${pr}`;
        try {
          const pull = await reviewRead(`${root}/pulls/${pr}`);
          if (field(pull, "merged") === true) continue;
          const wanted = {
            pr,
            head: commit(string(field(field(pull, "head"), "sha"))),
          };
          await evaluate(
            wanted,
            execution.source,
            reviewRead,
            actions,
            queueFor(wanted),
            config.dev,
          );
        } catch {
          failed = true;
          console.error(
            `PR ${pr}: admission not established; no merge requested.`,
          );
        }
      }
      if (failed) throw new Error("Some candidates are not admitted");
      return;
    }
    if (wanted === null || !store || !merge)
      throw new Error("Explicit merge context is missing");
    const read = merge;
    const queue = queueFor(wanted);
    diagnostic += ` PR ${wanted.pr} candidate ${wanted.head}.`;
    stage = "candidate admission";
    const initial = await evaluate(
      wanted,
      execution.source,
      read,
      actions,
      queue,
      config.dev,
    );
    const eligible = field(initial, "candidate"),
      entries = await store.read();
    const value: Operation = {
      version: 2n,
      branch: "dev",
      stage: "intent",
      previous: entries.at(-1)?.id ?? null,
      intent: null,
      commit: null,
      ...wanted,
      base: execution.source,
      prId: string(field(eligible, "id")),
      tree: string(field(eligible, "tree")),
      lifecycle: field(field(field(initial, "review"), "view"), "lifecycle"),
      ...execution,
      installation: credentials.installation,
      repository: repositoryId,
      actor: ownerId,
    };
    stage = "merge intent, final verification and result readback";
    const result = await execute(store, read, value, async () => {
      try {
        await preflight(actions, env, "dev");
        const final = {
          candidate: await candidate(wanted, execution.source, read),
          rules: await protection(read, config.dev, "dev"),
          ci: await ci(actions, wanted, "dev"),
          review: reviewSnapshot(queue, wanted, execution.source),
        };
        if (!isDeepStrictEqual(final, initial))
          throw new Error("Dev evidence changed before merge");
      } catch (error) {
        queue.reject(
          repository,
          wanted.pr,
          object(field(field(initial, "review"), "view")),
        );
        throw error;
      }
    });
    console.log(`Confirmed dev squash #${wanted.pr}: ${result}`);
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
    console.error(
      `Dev admission stopped during ${stage}; no automatic retry.${diagnostic} ${error instanceof ApiFailure ? error.message : "Inspect the bound operation and trusted inputs."}`,
    );
    process.exitCode = 1;
  }
}
