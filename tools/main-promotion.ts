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
  revoke,
  type Api,
  type Credentials,
} from "./merge-credentials";
import {
  bindPull,
  execute,
  recover,
  OperationStore,
  type Operation,
} from "./merge-operation";
import { commit, git, validateRelease } from "./release-candidate";
import { preflight, protection, ci } from "./merge-admission";

export type Request = { pr: bigint; dev: string; base: string; head: string };
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

export async function release(
  wanted: Request,
  source: string,
  read: Api,
  cwd?: string,
): Promise<Record> {
  const pull = await read(`${root}/pulls/${wanted.pr}`),
    id = bindPull(pull, wanted, "main");
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
  config: Protection,
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
      store = new OperationStore(api(credentials.tokens.record), config.ledger);
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
    const rules = await protection(read, config.main);
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
      config.main,
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
            config.main,
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
