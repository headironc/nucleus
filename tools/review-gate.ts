// Verify admission conditions; finding handlers own substantive dispositions.
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import {
  WHITESPACE,
  array,
  compare,
  equal,
  field,
  get,
  integer,
  numeric,
  object,
  parse,
  parseNumbers,
  second,
  string,
  timestamp,
  truth,
  wire,
  type Json,
  type Record,
} from "./review-json";
import { CHECK, CONTRACT, Queue, contract, sha } from "./review-queue";

export const BOT = "chatgpt-codex-connector[bot]";
export const REVIEWS = ["code_review", "security_review"];
const LABELS: { [key: string]: string } = {
  code_review: "Code Review",
  security_review: "Security Review",
};
const STATE = /codex-review-state:v1 (\{[^\n]*?\}) -->/;
export const PASSING = new Set(["SUCCESS", "SKIPPED", "NEUTRAL"]);
const provider = { __typename: "Bot", login: BOT.replace(/\[bot\]$/, "") };
export type Check = [Json, Json];
export type Transport = (args: string[], input?: string) => string;
export class WithdrawalError extends Error {}

export function deliveryTarget(view: Json): string {
  const stack = get(view, "stack");
  return string(
    stack === null ? field(view, "baseRefName") : field(stack, "trunk"),
  );
}

// Keep upper-layer changes out of the lower layer's review identity.
export function stackBinding(
  repo: string,
  number: bigint,
  view: Record,
): Record | null {
  const stack = field(view, "stack"),
    entry = field(view, "stackEntry");
  if (stack === null && entry === null) return null;
  if (stack === null || entry === null)
    throw new Error("stack membership is incomplete");
  const connection = field(stack, "entries"),
    nodes = array(field(connection, "nodes"));
  if (
    field(stack, "baseRefName") !== "dev" ||
    (field(stack, "size") !== 1n && field(stack, "size") !== 2n) ||
    field(connection, "totalCount") !== field(stack, "size") ||
    BigInt(nodes.length) !== field(stack, "size") ||
    field(field(connection, "pageInfo"), "hasNextPage") !== false ||
    typeof field(stack, "id") !== "string" ||
    !truth(field(stack, "id")) ||
    !integer(field(stack, "number")) ||
    numeric(field(stack, "number")) <= 0n
  )
    throw new Error("only a complete two-layer dev stack is supported");
  const pulls = nodes.map((node) => field(node, "pullRequest"));
  for (const [index, node] of nodes.entries()) {
    const pull = pulls[index];
    if (
      field(node, "position") !== BigInt(index + 1) ||
      typeof field(node, "id") !== "string" ||
      !truth(field(node, "id")) ||
      !integer(field(pull, "number")) ||
      numeric(field(pull, "number")) <= 0n ||
      !["OPEN", "MERGED"].includes(string(field(pull, "state"))) ||
      field(field(pull, "headRepository"), "nameWithOwner") !== repo ||
      !sha(field(pull, "headRefOid")) ||
      !sha(field(pull, "baseRefOid")) ||
      typeof field(pull, "headRefName") !== "string" ||
      !truth(field(pull, "headRefName"))
    )
      throw new Error("stack layer identity is incomplete or closed without merging");
  }
  const active = nodes.filter((node) => field(field(node, "pullRequest"), "state") === "OPEN"),
    first = nodes.length - active.length;
  if (
    !active.length ||
    nodes.slice(0, first).some((node) => field(field(node, "pullRequest"), "state") !== "MERGED") ||
    nodes.slice(first).some((node) => field(field(node, "pullRequest"), "state") !== "OPEN") ||
    new Set(pulls.map((pull) => field(pull, "number"))).size !== pulls.length ||
    new Set(nodes.map((node) => field(node, "id"))).size !== nodes.length ||
    field(pulls[first], "baseRefName") !== "dev" ||
    (active.length === 2 && (
      !equal(field(pulls[1], "baseRefName"), field(pulls[0], "headRefName")) ||
      !equal(
        field(field(field(pulls[1], "baseRef"), "target"), "oid"),
        field(pulls[0], "headRefOid"),
      )
    ))
  )
    throw new Error("stack does not bind the exact direct parent");
  const index = pulls.findIndex((pull) => equal(field(pull, "number"), number));
  if (
    index < first ||
    !equal(field(entry, "id"), field(nodes[index], "id")) ||
    !equal(field(entry, "position"), field(nodes[index], "position")) ||
    ["headRefOid", "baseRefOid", "baseRefName", "headRefName"].some(
      (key) => !equal(field(view, key), field(pulls[index], key)),
    )
  )
    throw new Error("PR differs from its stack entry");
  return {
    id: field(stack, "id"),
    number: field(stack, "number"),
    size: field(stack, "size"),
    trunk: "dev",
    position: field(entry, "position"),
    members: active.map((node) => ({
      id: field(node, "id"),
      number: field(field(node, "pullRequest"), "number"),
      position: field(node, "position"),
    })),
    parent:
      index === first
        ? null
        : {
            number: field(pulls[0], "number"),
            head: field(pulls[0], "headRefOid"),
            ref: field(pulls[0], "headRefName"),
          },
  };
}

export function trusted(comment: Json): boolean {
  return (
    equal(get(comment, "author"), provider) &&
    (equal(get(comment, "editor"), provider) ||
      (get(comment, "editor") === null &&
        Object.hasOwn(object(comment), "lastEditedAt") &&
        field(comment, "lastEditedAt") === null))
  );
}

export function published(body: string, kind: string, head: string): Record {
  const row = new RegExp(
    `\\*\\*${LABELS[kind]}\\*\\* \\| ([^\\n]*?) \\| \`([0-9a-f]+)\` \\|`,
  ).exec(body);
  if (!row || !head.startsWith(row[2])) return {};
  const status = /\*\*(\w+)\*\*/.exec(row[1]);
  const time = /<relative-time datetime="([^"]+)"/.exec(row[1]);
  return {
    status: status ? status[1].toLowerCase() : null,
    time: time ? time[1] : null,
  };
}

export function retainedSecurityFindings(
  body: string,
  head: string,
  comments: Json[],
): boolean {
  const section = body
    .slice(
      body.indexOf("### Security findings") + "### Security findings".length,
    )
    .split("<details>", 1)[0];
  const known = comments.filter(
    (comment) =>
      get(get(comment, "user", {}), "login") === BOT &&
      get(comment, "original_commit_id") !== null &&
      get(comment, "in_reply_to_id") === null,
  );
  let expected = 0n,
    found = 0n;
  for (const raw of section.split(
    /\r\n|[\n\r\v\f\u001c-\u001e\u0085\u2028\u2029]/,
  )) {
    const line = raw.replace(
      /^[\p{White_Space}\u001c-\u001f]+|[\p{White_Space}\u001c-\u001f]+$/gu,
      "",
    );
    if (!line) continue;
    const heading = /^#### (?:Advisory|Blocking) findings \(([0-9]+)\)$/.exec(
      line,
    );
    if (heading) {
      expected += BigInt(heading[1]);
      continue;
    }
    const finding =
      /^- [^\[]*\[[^\]]+\]\((https:\/\/github\.com\/[^\p{White_Space}\u001c-\u001f)]+#discussion_r[0-9]+)\) · \*\*(?:Critical|High|Medium|Low|Informational)\*\*(?: · \*\*Resolved\*\*)?$/u.exec(
        line,
      );
    const comment =
      finding && known.findLast((item) => get(item, "html_url") === finding[1]);
    if (
      !finding ||
      !comment ||
      (get(comment, "original_commit_id") === head &&
        !line.endsWith(" · **Resolved**"))
    )
      return false;
    found++;
  }
  return found > 0n && found === expected;
}

export function reviewStates(
  comments: Json[],
  head: string,
  inline: Json[] = [],
): Record {
  const states: Record = {};
  for (const comment of comments) {
    const body = string(
      truth(get(comment, "body")) ? get(comment, "body") : "",
    );
    const match = STATE.exec(body);
    if (!match || !trusted(comment)) continue;
    for (const [kind, value] of Object.entries(object(parse(match[1])))) {
      if (
        !REVIEWS.includes(kind) ||
        !truth(value) ||
        get(value, "commit_sha") !== head
      )
        continue;
      const hidden = get(value, "status");
      states[kind] = equal(get(published(body, kind, head), "status"), hidden)
        ? hidden
        : `unpublished ${hidden}`;
      if (
        typeof get(value, "has_any_findings") !== "boolean" ||
        get(value, "has_unresolved_findings") !== false
      )
        states[kind] = "unresolved findings or unverified finding flags";
      if (
        kind === "security_review" &&
        body.includes("### Security findings") &&
        !retainedSecurityFindings(body, head, inline)
      )
        states[kind] =
          "published security findings are unresolved or unrecognized";
    }
  }
  return states;
}

export function headFindings(comments: Json[], head: string): Json[] {
  return comments
    .filter(
      (comment) =>
        get(get(comment, "user", {}), "login") === BOT &&
        get(comment, "original_commit_id") === head &&
        get(comment, "in_reply_to_id") === null,
    )
    .map((comment) => field(comment, "id"));
}

export function reviewCompletions(
  comments: Json[],
  head: string,
): { [kind: string]: bigint } {
  const completed: { [kind: string]: bigint } = {};
  for (const comment of comments) {
    if (!trusted(comment)) continue;
    const body = string(
      truth(get(comment, "body")) ? get(comment, "body") : "",
    );
    const match = STATE.exec(body);
    if (!match) continue;
    for (const [kind, value] of Object.entries(object(parse(match[1])))) {
      if (
        !REVIEWS.includes(kind) ||
        !truth(value) ||
        get(value, "commit_sha") !== head
      )
        continue;
      const row = published(body, kind, head);
      if (
        get(value, "status") === "completed" &&
        get(row, "status") === "completed"
      )
        completed[kind] = timestamp(get(row, "time"));
      else delete completed[kind];
    }
  }
  return completed;
}

export function reviewFreshness(
  comments: Json[],
  head: string,
  inline: Json[],
  reviews: Json[],
): string[] {
  const findings = headFindings(inline, head);
  const roots = inline.filter((comment) =>
    findings.some((id) => equal(id, field(comment, "id"))),
  );
  const reports = reviews.filter(
    (review) => field(review, "commit_id") === head,
  );
  if (!roots.length && !reports.length) return [];
  try {
    const completed = reviewCompletions(comments, head);
    if (REVIEWS.some((kind) => !Object.hasOwn(completed, kind)))
      return ["current review material has no complete summary timestamps"];
    const cutoff = second(
      Object.values(completed).reduce((a, b) => (a < b ? a : b)),
    );
    const times = [
      ...roots.map((item) => [
        get(item, "created_at"),
        get(item, "updated_at"),
      ]),
      ...reports.map((item) => [
        get(item, "submitted_at"),
        get(item, "updated_at"),
      ]),
    ];
    for (const [created, updated] of times) {
      const a = timestamp(created),
        b = timestamp(updated);
      if (b < a || (a > b ? a : b) >= cutoff)
        return [
          "Codex review material is not known to precede both current completions",
        ];
    }
  } catch {
    return ["Codex review material has unverified timestamps"];
  }
  return [];
}

export function checkFailures(checks: Json[], required: Check[]): string[] {
  const failures: string[] = [];
  for (const [context, integration] of required) {
    const results = checks.filter(
      (check) =>
        equal(field(check, "name"), context) &&
        (integration === null || equal(get(check, "app_id"), integration)),
    );
    if (
      !results.length ||
      !results.every((check) => truth(field(check, "passed")))
    )
      failures.push(
        integration === null
          ? string(context)
          : `${context} (app ${integration})`,
      );
  }
  return failures.sort();
}

export function headChecks(runs: Json[], statuses: Json[]): Json[] {
  const latest: { name: Json; app: Json; run: Json }[] = [];
  for (const run of runs) {
    const name = field(run, "name");
    const app = get(truth(get(run, "app")) ? get(run, "app") : {}, "id");
    const previous = latest.find(
      (entry) => equal(entry.name, name) && equal(entry.app, app),
    );
    if (!previous) latest.push({ name, app, run });
    else if (
      compare(
        numeric(get(run, "id", 0n)),
        numeric(get(previous.run, "id", 0n)),
      ) > 0
    )
      previous.run = run;
  }
  // Reruns can change API enumeration order without changing these facts.
  return [
    ...latest.map(({ name, app, run }) => ({
      name,
      app_id: app,
      passed:
        get(run, "status") === "completed" &&
        PASSING.has(
          string(
            truth(get(run, "conclusion")) ? get(run, "conclusion") : "",
          ).toUpperCase(),
        ),
    })),
    ...statuses.map((status) => ({
      name: field(status, "context"),
      app_id: null,
      passed: get(status, "state") === "success",
    })),
  ].sort((left, right) => {
    const a = wire(left), b = wire(right);
    return a < b ? -1 : a > b ? 1 : 0;
  });
}

export function requiredChecks(rules: Json[]): Check[] {
  const required: Check[] = [];
  for (const rule of rules) {
    if (field(rule, "type") !== "required_status_checks") continue;
    for (const check of array(
      field(field(rule, "parameters"), "required_status_checks"),
    )) {
      const pair: Check = [
        field(check, "context"),
        get(check, "integration_id"),
      ];
      if (!required.some((item) => equal(item, pair))) required.push(pair);
    }
  }
  return required;
}

export function reviewContext(
  records: Json[],
  repo: string,
  number: bigint,
  view: Record,
  reuse = false,
): Record | null {
  const matches: Record[] = [];
  for (const record of records) {
    if (
      get(record, "event") !== "pull_request_target" ||
      string(get(record, "path", "")).split("@", 1)[0] !==
        ".github/workflows/review-context.yml" ||
      get(get(record, "repository", {}), "full_name") !== repo ||
      get(record, "status") !== "completed" ||
      get(record, "conclusion") !== "success" ||
      !["dev", "main"].includes(deliveryTarget(view))
    )
      continue;
    const title = string(get(record, "display_title", ""));
    const match =
      /^context:([1-9][0-9]*):([0-9a-f]{40}):([0-9a-f]{40}):([^:]+)(?::stack:([1-9][0-9]*):([12]):([12]):dev(?::notice)?)?$/.exec(
        title,
      );
    if (
      match &&
      match[0] === title &&
      BigInt(match[1]) === number &&
      match[2] === field(view, "headRefOid") &&
      match[4] === field(view, "baseRefName") &&
      (get(view, "stack") === null
        ? match[5] === undefined
        : match[5] === String(field(view.stack, "number")) &&
          match[6] === String(field(view.stack, "position")) &&
          match[7] === String(field(view.stack, "size"))) &&
      ((reuse &&
        (get(view, "stack") === null || get(view.stack, "parent") === null)) ||
        match[3] === field(view, "baseRefOid"))
    )
      matches.push({
        base: match[3],
        recorded_at: field(record, "created_at"),
      });
  }
  if (new Set(matches.map((match) => match.base)).size > 1)
    throw new Error("review context has ambiguous baselines for the same head");
  return (
    matches.sort((a, b) =>
      compare(timestamp(a.recorded_at), timestamp(b.recorded_at)),
    )[0] ?? null
  );
}

export function baselineFailures(
  comments: Json[],
  head: string,
  recorded: Json,
): string[] {
  if (recorded === null)
    return [
      "review baseline is unrecorded or changed; synchronize the PR with its target",
    ];
  const completed = reviewCompletions(comments, head),
    start = timestamp(recorded);
  return REVIEWS.filter(
    (kind) => !Object.hasOwn(completed, kind) || completed[kind] <= start,
  ).map(
    (kind) => `${kind} does not have a completion after the recorded PR event`,
  );
}

export function historyFailures(
  events: Json[],
  recorded: Json,
  rules: Json[],
): string[] {
  const reasons: string[] = [];
  if (
    ["non_fast_forward", "deletion"].some(
      (type) => !rules.some((rule) => field(rule, "type") === type),
    )
  )
    reasons.push("target rules do not prevent history rewrites and deletion");
  for (const protection of [
    "required_review_thread_resolution",
    "dismiss_stale_reviews_on_push",
  ])
    if (
      !rules.some(
        (rule) =>
          field(rule, "type") === "pull_request" &&
          get(field(rule, "parameters"), protection) === true,
      )
    )
      reasons.push(`target rules do not enforce ${protection}`);
  if (
    rules.some(
      (rule) =>
        field(rule, "type") === "pull_request" &&
        get(
          get(field(rule, "parameters"), "dismissal_restriction", {}),
          "enabled",
        ) !== false,
    )
  )
    reasons.push(
      "target review dismissal restrictions do not establish the gate's cleanup authority",
    );
  const rewrites = events.filter(
    (event) => field(event, "__typename") === "HeadRefForcePushedEvent",
  );
  if (
    events.some(
      (event) =>
        !["BaseRefChangedEvent", "HeadRefForcePushedEvent"].includes(
          string(field(event, "__typename")),
        ),
    )
  )
    reasons.push(
      "PR head was deleted or restored; automatic baseline binding is unavailable",
    );
  if (rewrites.length) {
    if (
      !truth(recorded) ||
      rewrites.some((event) => !truth(get(event, "createdAt")))
    )
      reasons.push("force-push history has no usable context or event time");
    else {
      const start = timestamp(recorded),
        times = rewrites.map((event) => timestamp(field(event, "createdAt")));
      if (!equal(times, times.toSorted(compare)))
        reasons.push("force-push timeline order is inconsistent");
      if (times.some((time) => time >= start))
        reasons.push(
          "PR head was rewritten after the recorded event; synchronize with its target",
        );
      if (
        rewrites.some(
          (event) =>
            !(["ahead", "diverged"] as Json[]).includes(
              get(event, "beforeRelationship"),
            ),
        )
      )
        reasons.push("PR head may reuse a commit from rewritten history");
      if (get(rewrites.at(-1)!, "afterRelationship") !== "ahead")
        reasons.push(
          "PR head must extend the latest force-pushed tip with a new commit",
        );
    }
  }
  if (
    truth(recorded) &&
    events.some(
      (event) =>
        field(event, "__typename") === "BaseRefChangedEvent" &&
        timestamp(field(event, "createdAt")) >= timestamp(recorded),
    )
  )
    reasons.push(
      "PR target changed after the recorded event; synchronize with its target",
    );
  return reasons;
}

export function decide(
  head: string,
  comments: Json[],
  inline: Json[],
  unresolved: Json,
  checks: Json[],
  required: Check[],
  reviews: Json[] = [],
): string[] {
  const states = reviewStates(comments, head, inline);
  const reasons = REVIEWS.filter(
    (kind) => get(states, kind) !== "completed",
  ).map(
    (kind) =>
      `${kind} is ${truth(get(states, kind)) ? get(states, kind) : "missing"} on ${head.slice(0, 12)}`,
  );
  reasons.push(...reviewFreshness(comments, head, inline, reviews));
  if (truth(unresolved))
    reasons.push(`${unresolved} unresolved review thread(s)`);
  const failures = checkFailures(checks, required);
  if (failures.length)
    reasons.push(`required checks not passing: ${failures.join(", ")}`);
  return reasons;
}

export function approvalBody(view: Record): string {
  const event = field(view, "lifecycle"),
    binding = event === null ? "none" : `id:${field(event, "id")}`;
  return (
    `Review gate (${CONTRACT}): machine conditions verified on ${field(view, "headRefOid")} against ` +
    `${field(view, "baseRefName")} at ${field(view, "baseRefOid")}. Both Codex reviews completed with no ` +
    "reported unresolved findings; review material predates completion, all threads are resolved " +
    `and required checks pass.\nReopen event: ${binding}\n` +
    "Scope excludes substantive finding disposition and original-report completeness. " +
    "Finding handlers record dispositions in the original review threads. " +
    "Routine dev queue merges consume the current admission result." +
    (get(view, "stack") === null ? "" : `\nStack layer: ${wire(view.stack)}`)
  );
}

export function summaryUpdatedAt(comments: Json[], head: string): bigint {
  const updates: bigint[] = [];
  for (const comment of comments) {
    const match = STATE.exec(
      string(truth(get(comment, "body")) ? get(comment, "body") : ""),
    );
    if (!match || !trusted(comment)) continue;
    if (
      Object.entries(object(parse(match[1]))).some(
        ([kind, value]) =>
          REVIEWS.includes(kind) &&
          truth(value) &&
          get(value, "commit_sha") === head,
      )
    )
      updates.push(timestamp(get(comment, "updatedAt")));
  }
  if (!updates.length)
    throw new Error("current review summaries have no verified update time");
  return updates.reduce((a, b) => (a > b ? a : b));
}

export function ghTransport(token?: string): Transport {
  return (args, input) => {
    const result = spawnSync("gh", args, {
      maxBuffer: Infinity,
      input,
      ...(token === undefined
        ? {}
        : { env: { ...process.env, GH_TOKEN: token } }),
    });
    if (result.error || result.status !== 0)
      throw new Error(
        `GitHub ${args.includes("graphql") ? "GraphQL" : "API"} request failed`,
      );
    try {
      return new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(
        result.stdout,
      );
    } catch {
      throw new Error("GitHub response is not valid UTF-8");
    }
  };
}

export interface GateOptions {
  repo: string;
  approver?: string;
  queueEnabled?: boolean;
  gh?: Transport;
  actions?: Transport;
  log?: (message: string) => void;
  error?: (message: string) => void;
  eventName?: string;
  eventPath?: string;
  mainRules?: (rules: Json[]) => void;
}

function nextCursor(
  connection: Json,
  cursors: Set<string>,
  stage: string,
): string | null {
  const page = field(connection, "pageInfo"),
    more = field(page, "hasNextPage");
  if (more === false) return null;
  const cursor = field(page, "endCursor");
  if (
    more !== true ||
    typeof cursor !== "string" ||
    !cursor ||
    cursors.has(cursor)
  )
    throw new Error(`PR ${stage} pagination did not advance`);
  cursors.add(cursor);
  return cursor;
}

export class Gate {
  readonly repo: string;
  readonly approver: string;
  readonly queueEnabled: boolean;
  private readonly transport: Transport;
  private readonly actionsTransport: Transport;
  readonly log: (message: string) => void;
  readonly error: (message: string) => void;
  readonly eventName: string | undefined;
  readonly eventPath: string | undefined;
  readonly mainRules: ((rules: Json[]) => void) | undefined;
  private readonly contextCache = new Map<string, Json[]>();
  private readonly comparisonCache = new Map<string, Record>();

  constructor(options: GateOptions) {
    this.repo = options.repo;
    this.approver = options.approver ?? "cladiron[bot]";
    this.queueEnabled = options.queueEnabled ?? false;
    this.transport = options.gh ?? ghTransport();
    this.actionsTransport =
      options.actions ??
      (() => {
        throw new Error("Actions read transport is unavailable");
      });
    this.log = options.log ?? console.log;
    this.error = options.error ?? console.error;
    this.eventName = options.eventName;
    this.eventPath = options.eventPath;
    this.mainRules = options.mainRules;
  }

  gh(...args: string[]): string {
    return this.transport(args);
  }
  actions(...args: string[]): string {
    return this.actionsTransport(args);
  }
  writeCheck(args: string[], payload: Json): string {
    return this.transport(args, wire(payload));
  }
  requiredChecks = requiredChecks;
  headChecks = headChecks;
  checkFailures = checkFailures;
  summaryUpdatedAt = summaryUpdatedAt;
  readonly passing = PASSING;
  target = deliveryTarget;

  prerequisites(rules: Json[], view: Record): Check[] {
    return requiredChecks(rules).filter(
      (check) =>
        !(
          this.queueEnabled &&
          ["dev", "main"].includes(deliveryTarget(view)) &&
          equal(check, CHECK)
        ),
    );
  }
  gateReview(review: Json): boolean {
    return (
      get(get(review, "user", {}), "login") === this.approver &&
      get(field(review, "user"), "type") === "Bot"
    );
  }
  pages(path: string): Json[] {
    return array(parse(this.gh("api", "--paginate", "--slurp", path))).flatMap(
      array,
    );
  }
  contexts(repo: string, head: string): Json[] {
    const key = `${repo}:${head}`;
    const cached = this.contextCache.get(key);
    if (cached) return cached;
    const batches = array(
      parse(
        this.actions(
          "api",
          "--paginate",
          "--slurp",
          `repos/${repo}/actions/workflows/review-context.yml/runs?event=pull_request_target&head_sha=${head}&per_page=100`,
        ),
      ),
    );
    const records = batches.flatMap((batch) =>
      array(field(batch, "workflow_runs")),
    );
    if (
      !batches.length ||
      batches.some(
        (batch) => !equal(field(batch, "total_count"), BigInt(records.length)),
      )
    )
      throw new Error(
        "review context pagination is incomplete or changed during retrieval",
      );
    this.contextCache.set(key, records);
    return records;
  }
  reviewEvidence(owner: string, name: string, number: bigint): Record {
    const comments: Json[] = [],
      reviews: Json[] = [],
      threads: Json[] = [],
      counts: Json[] = [];
    const connections = [
      {
        name: "comments",
        selection:
          "nodes{id body updatedAt lastEditedAt author{__typename login} editor{__typename login}}",
        nodes: comments,
      },
      {
        name: "reviews",
        selection:
          "totalCount nodes{fullDatabaseId submittedAt updatedAt author{__typename login} commit{oid}}",
        nodes: reviews,
      },
      {
        name: "reviewThreads",
        selection: "nodes{isResolved}",
        nodes: threads,
      },
    ].map((connection) => ({
      ...connection,
      after: null as string | null,
      cursors: new Set<string>(),
      complete: false,
    }));
    while (connections.some((connection) => !connection.complete)) {
      const pending = connections.filter((connection) => !connection.complete);
      const query =
        "query($owner:String!,$name:String!,$number:Int!" +
        pending.map(({ name }) => `,$${name}After:String`).join("") +
        "){repository(owner:$owner,name:$name){pullRequest(number:$number){" +
        pending
          .map(
            ({ name, selection }) =>
              `${name}(first:100,after:$${name}After){${selection}pageInfo{hasNextPage endCursor}}`,
          )
          .join("") +
        "}}}";
      const args = [
        "api",
        "graphql",
        "-f",
        `query=${query}`,
        "-f",
        `owner=${owner}`,
        "-f",
        `name=${name}`,
        "-F",
        `number=${number}`,
      ];
      for (const connection of pending)
        if (connection.after !== null)
          args.push("-f", `${connection.name}After=${connection.after}`);
      const response = parse(this.gh(...args));
      if (truth(get(response, "errors")))
        throw new Error("PR review query returned incomplete evidence");
      const result = field(
        field(field(response, "data"), "repository"),
        "pullRequest",
      );
      for (const connection of pending) {
        const value = field(result, connection.name);
        connection.nodes.push(...array(field(value, "nodes")));
        if (connection.name === "reviews")
          counts.push(field(value, "totalCount"));
        connection.after = nextCursor(
          value,
          connection.cursors,
          connection.name,
        );
        connection.complete = connection.after === null;
      }
    }
    const reports: Record[] = [],
      identities: bigint[] = [];
    for (const review of reviews) {
      const raw = field(review, "fullDatabaseId");
      if (
        !(typeof raw === "string" || integer(raw)) ||
        /^[1-9][0-9]*/.exec(String(raw))?.[0] !== String(raw)
      )
        throw new Error("formal review has no exact identity");
      const id = BigInt(raw);
      identities.push(id);
      if (
        equal(field(review, "author"), provider) &&
        truth(field(review, "submittedAt"))
      )
        reports.push({
          id,
          commit_id: field(field(review, "commit"), "oid"),
          submitted_at: field(review, "submittedAt"),
          updated_at: field(review, "updatedAt"),
        });
    }
    if (
      counts.some(
        (count) => !integer(count) || count !== BigInt(identities.length),
      ) ||
      new Set(identities).size !== identities.length
    )
      throw new Error("PR review pagination is incomplete or changed");
    return {
      comments,
      reviews: reports.sort((a, b) => compare(numeric(a.id), numeric(b.id))),
      unresolved: BigInt(
        threads.filter((node) => !truth(field(node, "isResolved"))).length,
      ),
    };
  }
  baselineEvents(
    owner: string,
    name: string,
    number: bigint,
    head: string,
  ): Json[] {
    const query =
      "query($owner:String!,$name:String!,$number:Int!,$after:String){repository(owner:$owner,name:$name)" +
      "{pullRequest(number:$number){timelineItems(first:100,after:$after,itemTypes:" +
      "[BASE_REF_CHANGED_EVENT,HEAD_REF_FORCE_PUSHED_EVENT,HEAD_REF_DELETED_EVENT,HEAD_REF_RESTORED_EVENT])" +
      "{nodes{__typename ... on BaseRefChangedEvent{createdAt}" +
      " ... on HeadRefForcePushedEvent{createdAt beforeCommit{oid} afterCommit{oid}}}" +
      "pageInfo{hasNextPage endCursor}}}}}";
    const events: Json[] = [],
      cursors = new Set<string>();
    let after: string | null = null;
    do {
      const args = [
        "api",
        "graphql",
        "-f",
        `query=${query}`,
        "-f",
        `owner=${owner}`,
        "-f",
        `name=${name}`,
        "-F",
        `number=${number}`,
      ];
      if (after !== null) args.push("-f", `after=${after}`);
      const response = parse(this.gh(...args));
      if (truth(get(response, "errors")))
        throw new Error("PR history query returned incomplete evidence");
      const result = field(
        field(field(field(response, "data"), "repository"), "pullRequest"),
        "timelineItems",
      );
      events.push(...array(field(result, "nodes")));
      after = nextCursor(result, cursors, "history");
    } while (after !== null);
    const rewrites = events
      .filter(
        (event) => field(event, "__typename") === "HeadRefForcePushedEvent",
      )
      .map(object);
    for (const event of rewrites)
      event.beforeRelationship = this.relationship(
        `${owner}/${name}`,
        get(
          truth(get(event, "beforeCommit")) ? get(event, "beforeCommit") : {},
          "oid",
        ),
        head,
      );
    if (rewrites.length) {
      const latest = rewrites.at(-1)!;
      latest.afterRelationship = this.relationship(
        `${owner}/${name}`,
        get(
          truth(get(latest, "afterCommit")) ? get(latest, "afterCommit") : {},
          "oid",
        ),
        head,
      );
    }
    return events;
  }
  comparison(repo: string, base: Json, head: Json): Record {
    if (!sha(base) || !sha(head))
      throw new Error("commit comparison is missing an exact commit");
    const key = `${repo}:${base}:${head}`,
      cached = this.comparisonCache.get(key);
    if (cached) return cached;
    const result = parse(
      this.gh("api", `repos/${repo}/compare/${base}...${head}?per_page=1`),
    );
    const mergeBase = get(
      truth(get(result, "merge_base_commit"))
        ? get(result, "merge_base_commit")
        : {},
      "sha",
    );
    if (
      get(get(result, "base_commit", {}), "sha") !== base ||
      !["ahead", "behind", "identical", "diverged"].includes(
        string(get(result, "status", "")),
      ) ||
      !sha(mergeBase)
    )
      throw new Error(
        "commit comparison did not establish the requested ancestry",
      );
    const comparison = {
      status: field(result, "status"),
      merge_base: mergeBase,
    };
    this.comparisonCache.set(key, comparison);
    return comparison;
  }
  relationship(repo: string, base: Json, head: Json): Json {
    return this.comparison(repo, base, head).status;
  }
  targetHead(repo: string, branch: string): string {
    const result = parse(
      this.gh(
        "api",
        "--hostname",
        "github.com",
        `repos/${repo}/git/ref/heads/${encodeURIComponent(branch)}`,
      ),
    );
    const head = get(
      truth(get(result, "object")) ? get(result, "object") : {},
      "sha",
    );
    if (get(result, "ref") !== `refs/heads/${branch}` || !sha(head))
      throw new Error("target branch did not return an exact current commit");
    return head;
  }
  evidence(repo: string, number: bigint, view: Record): Record {
    const [owner, name] = repo.split("/"),
      head = string(field(view, "headRefOid"));
    const runs = array(
      parse(
        this.gh(
          "api",
          "--paginate",
          "--slurp",
          `repos/${repo}/commits/${head}/check-runs?per_page=100`,
        ),
      ),
    ).flatMap((page) => array(field(page, "check_runs")));
    const latest = new Map<Json, Json>();
    for (const status of this.pages(
      `repos/${repo}/commits/${head}/statuses?per_page=100`,
    ))
      if (!latest.has(field(status, "context")))
        latest.set(field(status, "context"), status);
    const review = this.reviewEvidence(owner, name, number);
    return {
      comments: review.comments,
      inline: this.pages(`repos/${repo}/pulls/${number}/comments`),
      reviews: review.reviews,
      unresolved: review.unresolved,
      checks: headChecks(runs, [...latest.values()]).filter(
        (check) =>
          !(
            this.queueEnabled &&
            ["dev", "main"].includes(deliveryTarget(view)) &&
            equal([field(check, "name"), field(check, "app_id")], CHECK)
          ),
      ),
      rules: this.pages(
        `repos/${repo}/rules/branches/${encodeURIComponent(deliveryTarget(view))}?per_page=100`,
      ),
      // Keep history last so a returning head during other reads is observed.
      events: this.baselineEvents(owner, name, number, head),
    };
  }
  prView(repo: string, number: bigint): Record {
    const [owner, name] = repo.split("/");
    const query =
      "query($owner:String!,$name:String!,$number:Int!){repository(owner:$owner,name:$name)" +
      "{nameWithOwner pullRequest(number:$number){number headRefOid headRefName state isDraft baseRefName baseRefOid " +
      "stackEntry{id position} stack{id number size baseRefName entries(first:3){totalCount nodes{id position " +
      "pullRequest{number headRefName headRefOid baseRefName baseRefOid baseRef{target{oid}} state headRepository{nameWithOwner}}}pageInfo{hasNextPage}}} " +
      "timelineItems(last:1,itemTypes:[CLOSED_EVENT,REOPENED_EVENT]){nodes{__typename " +
      "... on ClosedEvent{id} ... on ReopenedEvent{id}}pageInfo{hasNextPage}}}}}";
    const result = parse(
      this.gh(
        "api",
        "graphql",
        "-f",
        `query=${query}`,
        "-f",
        `owner=${owner}`,
        "-f",
        `name=${name}`,
        "-F",
        `number=${number}`,
      ),
    );
    if (truth(get(result, "errors")))
      throw new Error("PR lifecycle query returned incomplete evidence");
    const repository = field(field(result, "data"), "repository"),
      view = object(field(repository, "pullRequest"));
    if (
      field(repository, "nameWithOwner") !== repo ||
      !equal(field(view, "number"), number)
    )
      throw new Error("PR lifecycle query returned a different target");
    delete view.number;
    let stack: Record | null = null;
    if (view.state === "OPEN") {
      try {
        stack = stackBinding(repo, number, view);
      } catch (error) {
        // Preserve the candidate so an invalid membership can revoke its old
        // approval and head check instead of aborting before withdrawal.
        const raw = get(view, "stack");
        stack = {
          trunk:
            raw !== null && typeof get(raw, "baseRefName") === "string"
              ? field(raw, "baseRefName")
              : view.baseRefName,
          invalid:
            error instanceof Error ? error.message : "invalid stack membership",
        };
      }
    }
    delete view.stack;
    delete view.stackEntry;
    delete view.headRefName;
    if (stack !== null) view.stack = stack;
    if (!["OPEN", "CLOSED", "MERGED"].includes(string(field(view, "state"))))
      throw new Error("PR lifecycle is unknown");
    if (
      view.state === "OPEN" &&
      (typeof field(view, "isDraft") !== "boolean" ||
        !truth(field(view, "baseRefName")) ||
        !sha(field(view, "headRefOid")) ||
        !sha(field(view, "baseRefOid")))
    )
      throw new Error(
        "PR candidate or baseline is not bound to exact revisions",
      );
    const timeline = object(get(view, "timelineItems"));
    delete view.timelineItems;
    if (
      get(field(timeline, "pageInfo"), "hasNextPage") !== false ||
      !Array.isArray(get(timeline, "nodes")) ||
      array(timeline.nodes).length > 1
    )
      throw new Error("Latest PR lifecycle event is unverified");
    let lifecycle: Json = null;
    if (array(timeline.nodes).length) {
      const event = object(array(timeline.nodes)[0]),
        id = get(event, "id");
      if (
        !["ClosedEvent", "ReopenedEvent"].includes(
          string(get(event, "__typename", "")),
        ) ||
        typeof id !== "string" ||
        !id ||
        WHITESPACE.test(id)
      )
        throw new Error("Latest PR lifecycle event is invalid");
      lifecycle = { type: event.__typename, id };
    }
    if (
      view.state === "OPEN" &&
      lifecycle !== null &&
      field(lifecycle, "type") === "ClosedEvent"
    )
      throw new Error("Open PR contradicts its latest lifecycle event");
    view.lifecycle = lifecycle;
    return view;
  }
  merged(number: bigint, view: Record): boolean {
    if (field(view, "state") !== "MERGED") return false;
    this.log(`#${number} is merged; retained approvals are historical`);
    return true;
  }
  matchesCandidate(review: Json, view: Record): boolean {
    return (
      this.gateReview(review) &&
      field(review, "state") === "APPROVED" &&
      get(review, "commit_id") === field(view, "headRefOid") &&
      get(review, "body") === approvalBody(view)
    );
  }
  matchesApproval(review: Json, view: Record, updated: bigint): boolean {
    if (!this.matchesCandidate(review, view)) return false;
    try {
      return timestamp(get(review, "submitted_at")) >= updated;
    } catch {
      return false;
    }
  }
  confirmReview(
    repo: string,
    number: bigint,
    id: Json,
    state: string,
    view?: Record,
    updated?: bigint,
  ): void {
    const review = parse(
      this.gh("api", `repos/${repo}/pulls/${number}/reviews/${id}`),
    );
    if (
      !equal(get(review, "id"), id) ||
      !this.gateReview(review) ||
      get(review, "state") !== state ||
      (view !== undefined && !this.matchesApproval(review, view, updated!))
    )
      throw new Error(
        `review ${id} readback did not confirm the expected ${state} binding`,
      );
  }
  assessment(repo: string, number: bigint, view: Record): [Record, string[]] {
    if (get(view, "stack") !== null && get(view.stack, "invalid") !== null)
      throw new Error(string(field(view.stack, "invalid")));
    const current = this.evidence(repo, number, view),
      reuse = this.queueEnabled && deliveryTarget(view) === "dev";
    if (this.queueEnabled)
      contract(this, array(current.rules), deliveryTarget(view));
    const head = string(field(view, "headRefOid")),
      context = reviewContext(
        this.contexts(repo, head),
        repo,
        number,
        view,
        reuse,
      );
    const recorded = context ? context.recorded_at : null,
      parent = get(view, "stack") === null ? null : get(view.stack, "parent");
    let covered = recorded;
    if (parent !== null) {
      const parentNumber = field(parent, "number");
      if (!integer(parentNumber))
        throw new Error("invalid stack parent number");
      const parentView = this.prView(repo, parentNumber),
        parentStack = get(parentView, "stack");
      if (
        parentView.state !== "OPEN" ||
        !equal(parentView.headRefOid, field(parent, "head")) ||
        parentStack === null ||
        get(parentStack, "invalid") !== null ||
        field(parentStack, "position") !== 1n ||
        ["id", "number", "size", "trunk", "members"].some(
          (key) => !equal(field(parentStack, key), field(view.stack, key)),
        )
      )
        throw new Error(
          "stack parent changed during review evidence collection",
        );
      const parentContext = reviewContext(
        this.contexts(repo, string(parentView.headRefOid)),
        repo,
        parentNumber,
        parentView,
        reuse,
      );
      current.parent_context = parentContext;
      covered =
        recorded === null || parentContext === null
          ? null
          : timestamp(parentContext.recorded_at) > timestamp(recorded)
            ? parentContext.recorded_at
            : recorded;
    }
    current.reviewed = context ? { ...view, baseRefOid: context.base } : null;
    const reasons = baselineFailures(array(current.comments), head, covered);
    reasons.push(
      ...historyFailures(array(current.events), recorded, array(current.rules)),
    );
    if (reuse && context) {
      const target = this.targetHead(repo, "dev");
      const diff = this.comparison(
        repo,
        parent === null ? target : field(parent, "head"),
        head,
      );
      current.integration = { base: target, merge_base: diff.merge_base };
      if (parent !== null && !equal(diff.merge_base, field(parent, "head")))
        reasons.push("stack head does not extend its exact direct parent");
      if (
        !["ahead", "identical"].includes(
          string(
            this.relationship(
              repo,
              parent === null ? context.base : target,
              parent === null ? target : head,
            ),
          ),
        )
      )
        reasons.push(
          "integration target no longer extends the recorded review baseline",
        );
      if (
        parent === null
          ? !equal(
              diff.merge_base,
              this.comparison(repo, context.base, head).merge_base,
            )
          : !["ahead", "identical"].includes(
              string(
                this.relationship(repo, context.base, field(parent, "head")),
              ),
            )
      )
        reasons.push(
          "PR diff changed after review; establish coverage for the new inputs",
        );
    }
    if (this.queueEnabled && view.baseRefName === "main") {
      const target = this.targetHead(repo, "main");
      current.integration = { base: target };
      if (!equal(target, view.baseRefOid))
        reasons.push(
          "main target advanced beyond the recorded review baseline",
        );
    }
    reasons.push(
      ...decide(
        head,
        array(current.comments),
        array(current.inline),
        current.unresolved,
        array(current.checks),
        this.prerequisites(array(current.rules), view),
        array(current.reviews),
      ),
    );
    return [current, reasons];
  }
  evaluate(repo: string, number: bigint): boolean {
    const view = this.prView(repo, number);
    if (this.merged(number, view)) return true;
    if (view.baseRefName === "main" && !this.queueEnabled)
      throw new Error("main admission requires the check controller");
    if (view.state === "CLOSED") {
      this.log(`#${number} is closed; retained approvals are inactive`);
      return true;
    }
    if (truth(field(view, "isDraft"))) {
      this.log(`#${number} is a draft pull request`);
      return false;
    }
    if (!["dev", "main"].includes(deliveryTarget(view))) {
      this.log(`#${number} does not target a protected delivery branch`);
      return false;
    }
    const head = string(field(view, "headRefOid")),
      [current, reasons] = this.assessment(repo, number, view);
    if (reasons.length) {
      this.log(`#${number} ${head.slice(0, 12)} not approved:`);
      for (const reason of reasons) this.log(`- ${reason}`);
      return false;
    }
    const reviewed = object(current.reviewed),
      updated = summaryUpdatedAt(array(current.comments), head);
    let reviews = this.pages(`repos/${repo}/pulls/${number}/reviews`);
    if (
      reviews.some(
        (review) =>
          this.gateReview(review) &&
          field(review, "state") === "APPROVED" &&
          !this.matchesCandidate(review, reviewed),
      )
    ) {
      let latest: Record;
      try {
        latest = this.withdraw(repo, number, reviewed, view);
      } catch {
        throw new WithdrawalError("outdated approval withdrawal failed");
      }
      if (this.merged(number, latest)) return true;
      if (!equal(latest, view)) {
        this.log(`#${number} changed while clearing outdated approvals`);
        return false;
      }
      reviews = this.pages(`repos/${repo}/pulls/${number}/reviews`);
    }
    let [refreshed, failures] = this.assessment(repo, number, view);
    if (
      failures.length ||
      !equal(refreshed, current) ||
      !equal(this.prView(repo, number), view)
    ) {
      this.log(`#${number} changed before approval`);
      return false;
    }
    const existing = reviews
      .filter((review) => this.matchesApproval(review, reviewed, updated))
      .map((review) => field(review, "id"));
    let id: Json;
    if (existing.length) id = existing.at(-1)!;
    else {
      const before = reviews.map((review) => field(review, "id"));
      try {
        this.gh(
          "api",
          "-X",
          "POST",
          `repos/${repo}/pulls/${number}/reviews`,
          "-f",
          `commit_id=${head}`,
          "-f",
          "event=APPROVE",
          "-f",
          `body=${approvalBody(reviewed)}`,
        );
      } catch {
        this.error(
          `#${number} approval response failed; checking the same target without retry`,
        );
      }
      const created = this.pages(
        `repos/${repo}/pulls/${number}/reviews`,
      ).filter(
        (review) =>
          !before.some((prior) => equal(prior, field(review, "id"))) &&
          this.matchesApproval(review, reviewed, updated),
      );
      if (created.length !== 1)
        throw new Error(
          "approval write did not establish exactly one new bound approval",
        );
      id = field(created[0], "id");
      [refreshed, failures] = this.assessment(repo, number, view);
      if (failures.length || !equal(refreshed, current)) {
        this.log(`#${number} changed during approval`);
        return false;
      }
    }
    this.confirmReview(repo, number, id, "APPROVED", reviewed, updated);
    const latest = this.prView(repo, number);
    if (this.merged(number, latest)) return true;
    if (
      !equal(latest, view) ||
      (Object.hasOwn(current, "integration") &&
        this.targetHead(repo, deliveryTarget(view)) !==
          field(current.integration, "base"))
    ) {
      this.log(`#${number} changed during approval`);
      return false;
    }
    this.log(
      `#${number} ${head.slice(0, 12)} ${existing.length ? "already approved" : "approved"}`,
    );
    return true;
  }
  withdraw(
    repo: string,
    number: bigint,
    keep?: Record,
    expected?: Record,
  ): Record {
    let view = this.prView(repo, number);
    if (this.merged(number, view) || view.state === "CLOSED") return view;
    for (const approval of this.pages(
      `repos/${repo}/pulls/${number}/reviews`,
    )) {
      if (
        !this.gateReview(approval) ||
        field(approval, "state") !== "APPROVED" ||
        (keep !== undefined && this.matchesCandidate(approval, keep))
      )
        continue;
      view = this.prView(repo, number);
      if (
        this.merged(number, view) ||
        view.state === "CLOSED" ||
        (keep !== undefined && !equal(view, expected!))
      )
        return view;
      const id = field(approval, "id");
      try {
        this.gh(
          "api",
          "-X",
          "PUT",
          `repos/${repo}/pulls/${number}/reviews/${id}/dismissals`,
          "-f",
          "event=DISMISS",
          "-f",
          "message=Review gate: current evidence does not establish approval.",
        );
      } catch {
        this.error(
          `#${number} dismissal response failed; checking review ${id} without retry`,
        );
      }
      try {
        this.confirmReview(repo, number, id, "DISMISSED");
      } catch (error) {
        view = this.prView(repo, number);
        if (this.merged(number, view)) return view;
        throw error;
      }
      this.log(`#${number} dismissed approval ${id}`);
    }
    view = this.prView(repo, number);
    if (!this.merged(number, view) && view.state === "CLOSED")
      this.log(`#${number} is closed; retained approvals are inactive`);
    return view;
  }
  main(number: bigint): number {
    let failed = 0,
      passed: boolean;
    try {
      passed = this.evaluate(this.repo, number);
    } catch (error) {
      if (error instanceof WithdrawalError) {
        this.error(`#${number} ${error.message}`);
        return 1;
      }
      this.error(
        `#${number} evaluation failed: ${error instanceof Error ? error.message : "unknown error"}`,
      );
      passed = false;
      failed = 1;
    }
    if (!passed) {
      try {
        if (this.withdraw(this.repo, number).state === "MERGED") return 0;
      } catch {
        this.error(`#${number} approval withdrawal failed`);
        failed = 1;
      }
    }
    return failed;
  }
  sweep(numbers: bigint[]): number {
    this.contextCache.clear();
    this.comparisonCache.clear();
    const queue = new Queue(this);
    if (this.queueEnabled)
      numbers = [...new Set([...numbers, ...queue.eventTargets()])];
    let failed = 0;
    for (const number of numbers)
      failed |= this.queueEnabled
        ? queue.reconcile(this.repo, number)
        : this.main(number);
    return failed;
  }
}

export function cli(args = process.argv.slice(2)): number {
  // Validation precedes environment access and construction of any transport.
  if (args.length === 1 && args[0] === "--validate-numbers") {
    console.log(wire(parseNumbers(readFileSync(0, "utf8"))));
    return 0;
  }
  if (args.length) throw new Error("unexpected review gate arguments");
  const numbers = parseNumbers(requiredEnv("NUMBERS"));
  return new Gate({
    repo: requiredEnv("GITHUB_REPOSITORY"),
    approver: process.env.APPROVER,
    queueEnabled: process.env.DEV_REVIEW_QUEUE === "true",
    actions: (args, input) =>
      ghTransport(requiredEnv("GH_ACTIONS_TOKEN"))(args, input),
    eventName: process.env.GITHUB_EVENT_NAME,
    eventPath: process.env.GITHUB_EVENT_PATH,
  }).sweep(numbers);
}

function requiredEnv(name: string): string {
  const value = process.env[name];
  if (value === undefined) throw new Error(`missing ${name}`);
  return value;
}

if (import.meta.main) {
  try {
    process.exitCode = cli();
  } catch (error) {
    console.error(
      error instanceof Error ? error.message : "review gate failed",
    );
    process.exitCode = 1;
  }
}
