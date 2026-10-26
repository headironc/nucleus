import { isDeepStrictEqual } from "node:util";
import {
  array,
  field,
  object,
  parse,
  string,
  wire,
  type Json,
  type Record,
} from "./review-json";
import { sha } from "./review-queue";
import {
  ownerId,
  repository,
  repositoryId,
  workflow,
  devWorkflow,
  type Ledger,
} from "./merge-deployment";
import { ApiFailure, type Api } from "./merge-credentials";
import { commit } from "./release-candidate";

type Binding = {
  stage: "intent" | "not-sent" | "failed" | "merged";
  previous: bigint | null;
  intent: bigint | null;
  commit: string | null;
  pr: bigint;
  prId: string;
  base: string;
  head: string;
  tree: string;
  lifecycle: Json;
  source: string;
  run: bigint;
  attempt: bigint;
  installation: bigint;
  repository: bigint;
  actor: bigint;
};
export type Operation = Binding &
  (
    | { version: 1n; branch: "main"; dev: string }
    | { version: 2n; branch: "dev" }
  );
export type Entry = { id: bigint; value: Operation };
const prefix = "Nucleus promotion v1\n";
const devPrefix = "Nucleus merge v2\n";
const root = `repos/${repository}`;
const keys = [
  "version",
  "stage",
  "previous",
  "intent",
  "commit",
  "pr",
  "prId",
  "base",
  "head",
  "tree",
  "lifecycle",
  "source",
  "run",
  "attempt",
  "installation",
  "repository",
  "actor",
  "branch",
].sort();
export function operation(value: Json): Operation {
  const row = object(value);
  const main = row.version === 1n && row.branch === "main";
  if (
    (!main && !(row.version === 2n && row.branch === "dev")) ||
    !isDeepStrictEqual(
      Object.keys(row).sort(),
      main ? [...keys, "dev"].sort() : keys,
    ) ||
    (main && !sha(row.dev)) ||
    row.repository !== repositoryId ||
    row.actor !== ownerId ||
    typeof row.prId !== "string" ||
    !row.prId ||
    !["intent", "not-sent", "failed", "merged"].includes(string(row.stage)) ||
    [row.pr, row.run, row.attempt, row.installation].some(
      (id) => typeof id !== "bigint" || id <= 0n,
    ) ||
    [row.base, row.head, row.tree, row.source].some((id) => !sha(id)) ||
    [row.previous, row.intent].some(
      (id) => id !== null && (typeof id !== "bigint" || id <= 0n),
    ) ||
    (row.stage === "intent" ? row.intent !== null : row.intent === null) ||
    (row.stage === "merged" ? !sha(row.commit) : row.commit !== null)
  )
    throw new Error("Invalid promotion operation binding");
  return row as Operation;
}

export function history(entries: Entry[]): Entry | null {
  let pending: Entry | null = null,
    previous: bigint | null = null;
  const ids = new Set<bigint>();
  for (const entry of entries) {
    const value = operation(entry.value);
    if (
      ids.has(entry.id) ||
      typeof entry.id !== "bigint" ||
      entry.id <= 0n ||
      value.previous !== previous
    )
      throw new Error("Promotion operation history is incomplete or reordered");
    ids.add(entry.id);
    if (value.stage === "intent") {
      if (pending !== null) throw new Error("Overlapping promotion intents");
      pending = entry;
    } else {
      if (
        pending === null ||
        value.intent !== pending.id ||
        !isDeepStrictEqual(
          {
            ...value,
            stage: "intent",
            intent: null,
            commit: null,
            previous: pending.value.previous,
          },
          pending.value,
        )
      )
        throw new Error("Promotion result does not bind the original intent");
      pending = null;
    }
    previous = entry.id;
  }
  return pending;
}

export class OperationStore {
  constructor(
    readonly api: Api,
    readonly config: Ledger,
  ) {}
  async read(): Promise<Entry[]> {
    const result: Entry[] = [],
      cursors = new Set<string>();
    let cursor: string | null = null,
      total: bigint | undefined;
    do {
      const response = await this.api("graphql", "POST", {
        query: `query($cursor:String,$number:Int!){repository(owner:"headironc",name:"nucleus"){databaseId issue(number:$number){id number comments(first:100,after:$cursor){totalCount nodes{databaseId body lastEditedAt editor{login} author{__typename login ... on Bot{databaseId}}} pageInfo{hasNextPage endCursor}}}}}`,
        variables: { number: this.config.issue, cursor },
      });
      if (object(response).errors !== undefined)
        throw new Error("Operation history GraphQL failed");
      const repo = field(field(response, "data"), "repository"),
        issue = field(repo, "issue");
      if (
        field(repo, "databaseId") !== repositoryId ||
        field(issue, "id") !== this.config.issueId ||
        field(issue, "number") !== this.config.issue
      )
        throw new Error("Operation issue identity mismatch");
      const comments = field(issue, "comments"),
        count = field(comments, "totalCount");
      if (typeof count !== "bigint" || (total !== undefined && count !== total))
        throw new Error("Operation history count changed");
      total = count;
      for (const node of array(field(comments, "nodes"))) {
        const author = field(node, "author"),
          id = field(node, "databaseId"),
          body = string(field(node, "body"));
        if (
          field(author, "__typename") !== "Bot" ||
          field(author, "databaseId") !== this.config.bot ||
          field(author, "login") !== "cladiron" ||
          field(node, "lastEditedAt") !== null ||
          field(node, "editor") !== null ||
          typeof id !== "bigint" ||
          (!body.startsWith(prefix) && !body.startsWith(devPrefix))
        )
          throw new Error(
            "Operation record has an untrusted author, edit or format",
          );
        const legacy = body.startsWith(prefix);
        const value = operation(
          parse(body.slice(legacy ? prefix.length : devPrefix.length)),
        );
        if ((value.version === 1n) !== legacy)
          throw new Error("Operation record version differs from its envelope");
        result.push({ id, value });
      }
      const page = field(comments, "pageInfo");
      if (field(page, "hasNextPage") === false) cursor = null;
      else {
        cursor = string(field(page, "endCursor"));
        if (
          field(page, "hasNextPage") !== true ||
          !cursor ||
          cursors.has(cursor)
        )
          throw new Error("Operation pagination did not advance");
        cursors.add(cursor);
      }
    } while (cursor !== null);
    if (BigInt(result.length) !== total)
      throw new Error("Operation history is incomplete");
    history(result);
    return result;
  }
  async append(value: Operation): Promise<Entry> {
    operation(value);
    const before = await this.read();
    if (value.previous !== (before.at(-1)?.id ?? null))
      throw new Error("Operation history changed before append");
    history([...before, { id: (before.at(-1)?.id ?? 0n) + 1n, value }]);
    try {
      await this.api(
        `repos/${repository}/issues/${this.config.issue}/comments`,
        "POST",
        { body: (value.version === 1n ? prefix : devPrefix) + wire(value) },
      );
    } catch {
      /* The same target is read once; creation is never retried. */
    }
    const after = await this.read();
    if (
      after.length !== before.length + 1 ||
      !isDeepStrictEqual(after.slice(0, -1), before) ||
      !isDeepStrictEqual(after.at(-1)!.value, value)
    )
      throw new Error(
        "Operation append outcome is unknown; no merge may be sent",
      );
    return after.at(-1)!;
  }
  async finish(
    intent: Entry,
    stage: "not-sent" | "failed" | "merged",
    commit: string | null = null,
  ): Promise<void> {
    await this.append({
      ...intent.value,
      stage,
      intent: intent.id,
      previous: intent.id,
      commit,
    });
  }
}

export async function originalEnded(
  actions: Api,
  intent: Entry,
): Promise<boolean> {
  const value = intent.value;
  const run = await actions(
    `repos/${repository}/actions/runs/${value.run}/attempts/${value.attempt}`,
  );
  if (
    field(run, "id") !== value.run ||
    field(run, "run_attempt") !== value.attempt ||
    field(field(run, "repository"), "id") !== repositoryId ||
    field(run, "path") !== (value.branch === "main" ? workflow : devWorkflow) ||
    field(run, "head_sha") !== value.source ||
    field(run, "head_branch") !== "dev" ||
    field(field(run, "actor"), "id") !== value.actor ||
    field(field(run, "triggering_actor"), "id") !== value.actor ||
    field(run, "event") !== "workflow_dispatch"
  )
    throw new Error("Original promotion run identity is unverified");
  return field(run, "status") === "completed";
}

export function bindPull(
  pull: Json,
  wanted: { pr: bigint; head: string },
  branch: "dev" | "main",
): string {
  const base = field(pull, "base"),
    head = field(pull, "head");
  if (
    field(pull, "number") !== wanted.pr ||
    field(pull, "url") !==
      `https://api.github.com/${root}/pulls/${wanted.pr}` ||
    field(field(base, "repo"), "id") !== repositoryId ||
    field(base, "ref") !== branch ||
    field(field(head, "repo"), "id") !== repositoryId ||
    field(head, "sha") !== wanted.head ||
    (branch === "main" &&
      !/^release\/[a-zA-Z0-9][a-zA-Z0-9._/-]*$/.test(
        string(field(head, "ref")),
      ))
  )
    throw new Error("Merge PR identity, source or target mismatch");
  return string(field(pull, "node_id"));
}

export async function observed(
  read: Api,
  value: Operation,
): Promise<string | null> {
  const pull = await read(`${root}/pulls/${value.pr}`);
  if (bindPull(pull, value, value.branch) !== value.prId)
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
      value.branch === "main" ? [value.base, value.head] : [value.base],
    ) ||
    field(field(field(result, "commit"), "tree"), "sha") !== value.tree ||
    field(field(field(result, "commit"), "verification"), "verified") !== true
  )
    throw new Error(
      "Merged commit parents, tree or signature differ from the accepted candidate",
    );
  const ref = await read(`${root}/git/ref/heads/${value.branch}`);
  if (field(ref, "ref") !== `refs/heads/${value.branch}`)
    throw new Error("Merge target ref identity mismatch");
  const tip = commit(string(field(field(ref, "object"), "sha")));
  const comparison = await read(`${root}/compare/${merged}...${tip}`);
  if (
    field(field(comparison, "base_commit"), "sha") !== merged ||
    !["identical", "ahead"].includes(string(field(comparison, "status")))
  )
    throw new Error("Merged candidate is not reachable from its target");
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
    throw new Error("Original merge execution has not ended");
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
      merge_method: value.branch === "main" ? "merge" : "squash",
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
      ? "GitHub rejected merge; a new explicit request is required"
      : "Merge outcome is unknown; recover the original operation without resending",
  );
}
