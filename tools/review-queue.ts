// Only the protected default-branch controller owns admission and queue writes.
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import type { Gate } from "./review-gate";
import {
  WHITESPACE,
  array,
  compare,
  encode,
  equal,
  field,
  get,
  integer,
  numeric,
  object,
  parse,
  string,
  timestamp,
  truth,
  type Json,
  type Record,
} from "./review-json";

export const CHECK: [string, bigint] = ["Cladiron review gate", 3104501n];
export const CONTRACT = "nucleus-review-gate:v4";
export const WORKFLOWS = new Set([
  ".github/workflows/rust.yml",
  ".github/workflows/docs.yml",
  ".github/workflows/halo.yml",
]);
export class QueueWaiting extends Error {}
export function sha(value: Json): value is string {
  return (
    typeof value === "string" &&
    value.length === 40 &&
    /^[0-9a-f]{40}$/.test(value)
  );
}
export function runTarget(run: Json, repo: string): bigint {
  const branch = string(get(run, "head_branch", ""));
  const path = string(get(run, "path", "")).split("@")[0];
  const match = /^gh-readonly-queue\/dev\/pr-([1-9][0-9]*)-[0-9a-f]+$/.exec(
    branch,
  );
  if (
    get(run, "event") !== "merge_group" ||
    get(get(run, "repository", {}), "full_name") !== repo ||
    !WORKFLOWS.has(path) ||
    !sha(get(run, "head_sha")) ||
    !match ||
    match[0] !== branch
  )
    throw new Error("queue CI run does not identify a supported dev candidate");
  return BigInt(match[1]);
}
export function contract(gate: Gate, rules: Json[], target = "dev"): void {
  if (!["dev", "main"].includes(target))
    throw new Error("admission requires a supported protected target");
  if (!rules.some((rule) => field(rule, "type") === "required_signatures"))
    throw new Error(`${target} must require signed commits`);
  if (
    !rules.some((rule) => {
      if (field(rule, "type") !== "pull_request") return false;
      const count = get(
        field(rule, "parameters"),
        "required_approving_review_count",
      );
      return integer(count) && count >= 1n;
    })
  )
    throw new Error(`${target} must require at least one approval`);
  const required = gate.requiredChecks(rules);
  if (
    [CHECK, ["docs", 15368n], ["rust", 15368n], ["website", 15368n]].some(
      (pair) => !required.some((item) => equal(item, pair)),
    )
  )
    throw new Error(
      `${target} must require docs, rust, website and the exact Cladiron producer`,
    );
  if (
    required.some(([name, app]) => name === CHECK[0] && !equal(app, CHECK[1]))
  )
    throw new Error("Cladiron required check has an ambiguous producer");
  if (target === "main" && gate.mainRules) gate.mainRules(rules);
  if (
    target === "main" && !gate.mainRules &&
    !rules.some((rule) => {
      if (field(rule, "type") !== "workflows") return false;
      const parameters = field(rule, "parameters");
      return (
        get(parameters, "do_not_enforce_on_create") === false &&
        array(field(parameters, "workflows")).some(
          (workflow) =>
            get(workflow, "repository_id") === 1432619931n &&
            get(workflow, "path") === ".github/workflows/review-gate.yml" &&
            get(workflow, "ref") === "refs/heads/dev" &&
            (get(workflow, "sha") === null || get(workflow, "sha") === ""),
        )
      );
    })
  )
    throw new Error("main must require the trusted dev lifecycle workflow");
  const queues = rules
    .filter((rule) => field(rule, "type") === "merge_queue")
    .map((rule) => field(rule, "parameters"));
  if (target === "main" && queues.length)
    throw new Error("main admission requires the direct merge path");
  if (
    target === "dev" &&
    (queues.length !== 1 ||
      Object.entries({
        grouping_strategy: "ALLGREEN",
        max_entries_to_build: 1n,
        max_entries_to_merge: 1n,
        min_entries_to_merge: 1n,
        merge_method: "SQUASH",
      }).some(([key, expected]) => !equal(get(queues[0], key), expected)))
  )
    throw new Error("dev single-entry ALLGREEN squash queue is not configured");
  if (
    !rules.some(
      (rule) =>
        field(rule, "type") === "required_status_checks" &&
        get(
          field(rule, "parameters"),
          "strict_required_status_checks_policy",
        ) === true,
    )
  )
    throw new Error(
      `${target} required checks do not enforce the current baseline`,
    );
}
export function entryIdentity(queued: Json): Json {
  return queued === null
    ? null
    : Object.fromEntries(
        Object.entries(object(queued)).filter(([key]) => key !== "state"),
      );
}
export function binding(
  repo: string,
  number: bigint,
  snapshot: Record,
  queued = false,
): string {
  const review = queued ? object(field(snapshot, "review")) : snapshot;
  const result: Record = {
    contract: CONTRACT,
    repository: repo,
    pr: number,
    kind: queued ? "queue" : "admission",
    candidate: field(review, "view"),
  };
  if (Object.hasOwn(review, "evidence")) {
    result.reviewed = field(review.evidence, "reviewed");
    result.integration = get(review.evidence, "integration");
  }
  if (queued)
    Object.assign(result, {
      entry: field(snapshot, "entry"),
      ref: field(snapshot, "ref"),
      sha: field(snapshot, "head"),
      ...(Object.hasOwn(snapshot, "prefix")
        ? { prefix_digest: digest(encode(snapshot.prefix)) }
        : {}),
      ...(Object.hasOwn(snapshot, "members")
        ? { members: array(snapshot.members).map(member => ({
            number: field(member, "number"),
            candidate: field(field(member, "review"), "view"),
            entry: field(member, "entry"),
            evidence_digest: digest(encode(field(member, "review"))),
          })) }
        : {}),
    });
  return encode(result, true);
}
const digest = (value: string): string =>
  createHash("sha256").update(value).digest("hex");
const maximum = (values: Iterable<bigint>): bigint | null => {
  let result: bigint | null = null;
  for (const value of values)
    if (result === null || value > result) result = value;
  return result;
};

function checkPayload(
  head: Json,
  identity: string,
  conclusion: Json,
  snapshot: Record,
): Record {
  const payload: Record = {
    name: CHECK[0],
    head_sha: head,
    external_id: `${CONTRACT}:${digest(identity)}`,
    status: conclusion === null ? "in_progress" : "completed",
    output: {
      title:
        conclusion === "success"
          ? "Review machine conditions verified"
          : "Review gate is holding merge",
      summary:
        `${identity}\nEvidence digest: ${digest(encode(snapshot))}\n` +
        "Scope: current review state, candidate and baseline binding, threads and required checks. " +
        "Substantive finding disposition and original-report completeness are not verified; " +
        "finding handlers record dispositions in the original review threads. " +
        "Routine dev queue merges consume the current admission result.",
    },
  };
  if (conclusion !== null) payload.conclusion = conclusion;
  return payload;
}

function matchesCheck(run: Json, payload: Record): boolean {
  return (
    Object.entries(payload).every(
      ([key, value]) => key === "output" || equal(get(run, key), value),
    ) &&
    Object.entries(object(payload.output)).every(([key, value]) =>
      equal(
        get(truth(get(run, "output")) ? get(run, "output") : {}, key),
        value,
      ),
    ) &&
    (payload.status !== "in_progress" || get(run, "conclusion") === null)
  );
}
function mergedCandidate(latest: Record, original: Json): boolean {
  return (
    latest.state === "MERGED" &&
    equal(latest.headRefOid, field(original, "headRefOid")) &&
    equal(latest.baseRefName, field(original, "baseRefName"))
  );
}

interface Evaluation {
  readonly id: bigint;
  readonly head: Json;
  readonly identity: string;
}

class CandidateChanged extends Error {
  constructor(
    readonly checkId: bigint | null,
    message: string,
  ) {
    super(message);
  }
}

export class Queue {
  constructor(readonly gate: Gate) {}

  actions(path: string, paginate = false): Json {
    return parse(
      this.gate.actions(
        "api",
        "--hostname",
        "github.com",
        ...(paginate ? ["--paginate", "--slurp"] : []),
        path,
      ),
    );
  }
  eventTargets(): bigint[] {
    if (this.gate.eventName !== "workflow_run") return [];
    if (this.gate.eventPath === undefined)
      throw new Error("missing GITHUB_EVENT_PATH");
    const event = parse(
      new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(
        readFileSync(this.gate.eventPath),
      ),
    );
    const notice = field(event, "workflow_run");
    if (get(notice, "event") !== "merge_group") return [];
    const id = field(notice, "id");
    if (!integer(id) || id <= 0n)
      throw new Error("queue CI run has no exact ID");
    const run = this.actions(`repos/${this.gate.repo}/actions/runs/${id}`);
    if (!equal(get(run, "id"), id))
      throw new Error("queue CI run readback returned another run");
    return [runTarget(run, this.gate.repo)];
  }
  shaPrs(repo: string, head: Json): Record {
    const [owner, name] = repo.split("/");
    const query =
      "query($owner:String!,$name:String!,$after:String){repository(owner:$owner,name:$name)" +
      "{nameWithOwner pullRequests(first:100,after:$after,states:OPEN," +
      "orderBy:{field:CREATED_AT,direction:ASC}){totalCount nodes{number headRefOid baseRefName state " +
      "stack{baseRefName} mergeQueueEntry{id state headCommit{oid}}}pageInfo{hasNextPage endCursor}}}}";
    const pulls: Json[] = [],
      counts: Json[] = [],
      cursors: string[] = [];
    let after: string | null = null;
    while (true) {
      const args = [
        "api",
        "graphql",
        "--hostname",
        "github.com",
        "-f",
        `query=${query}`,
        "-f",
        `owner=${owner}`,
        "-f",
        `name=${name}`,
      ];
      if (after) args.push("-f", `after=${after}`);
      const result = parse(this.gate.gh(...args));
      if (truth(get(result, "errors")))
        throw new Error("same-SHA PR query returned incomplete evidence");
      const repository = field(field(result, "data"), "repository");
      if (field(repository, "nameWithOwner") !== repo)
        throw new Error("same-SHA PR query returned another repository");
      const connection = field(repository, "pullRequests");
      counts.push(field(connection, "totalCount"));
      for (const pull of array(field(connection, "nodes"))) {
        const number = field(pull, "number");
        if (
          !integer(number) ||
          number <= 0n ||
          !sha(field(pull, "headRefOid")) ||
          typeof field(pull, "baseRefName") !== "string" ||
          !truth(field(pull, "baseRefName")) ||
          field(pull, "state") !== "OPEN"
        )
          throw new Error("same-SHA PR identity is incomplete");
        const queued = field(pull, "mergeQueueEntry");
        if (queued !== null) {
          const waiting =
            get(queued, "state") === "QUEUED" &&
            Object.hasOwn(object(queued), "headCommit") &&
            field(queued, "headCommit") === null;
          if (
            typeof get(queued, "id") !== "string" ||
            !truth(field(queued, "id")) ||
            (!waiting &&
              !sha(
                get(
                  truth(get(queued, "headCommit"))
                    ? get(queued, "headCommit")
                    : {},
                  "oid",
                ),
              ))
          )
            throw new Error("same-SHA queue identity is incomplete");
        }
        pulls.push(pull);
      }
      const page = field(connection, "pageInfo");
      if (field(page, "hasNextPage") === false) break;
      const cursor = field(page, "endCursor");
      if (
        field(page, "hasNextPage") !== true ||
        typeof cursor !== "string" ||
        !cursor ||
        cursors.includes(cursor)
      )
        throw new Error("same-SHA PR pagination did not advance");
      cursors.push(cursor);
      after = cursor;
    }
    if (
      new Set(pulls.map((pull) => field(pull, "number"))).size !==
        pulls.length ||
      counts.some((count) => !integer(count) || count !== BigInt(pulls.length))
    )
      throw new Error("same-SHA PR pagination is incomplete or changed");
    const protectedPulls = pulls.filter((pull) => {
      const stack = field(pull, "stack");
      return ["dev", "main"].includes(
        string(
          stack === null
            ? field(pull, "baseRefName")
            : field(stack, "baseRefName"),
        ),
      );
    });
    return {
      heads: protectedPulls
        .filter((pull) => equal(field(pull, "headRefOid"), head))
        .map((pull) => field(pull, "number"))
        .sort((a, b) => compare(numeric(a), numeric(b))),
      entries: protectedPulls
        .filter(
          (pull) =>
            field(pull, "mergeQueueEntry") !== null &&
            equal(
              get(
                truth(field(field(pull, "mergeQueueEntry"), "headCommit"))
                  ? field(field(pull, "mergeQueueEntry"), "headCommit")
                  : {},
                "oid",
              ),
              head,
            ),
        )
        .map((pull) => ({
          number: field(pull, "number"),
          id: field(field(pull, "mergeQueueEntry"), "id"),
        }))
        .sort((a, b) => compare(numeric(a.number), numeric(b.number))),
    };
  }
  reviewSnapshot(repo: string, number: bigint): Record {
    const view = this.gate.prView(repo, number);
    if (
      !["dev", "main"].includes(this.gate.target(view)) ||
      view.state !== "OPEN" ||
      truth(field(view, "isDraft"))
    )
      return { view, reasons: ["PR is not an open ready protected candidate"] };
    const [current, initialReasons] = this.gate.assessment(repo, number, view);
    let reasons = initialReasons;
    contract(this.gate, array(current.rules), this.gate.target(view));
    let approval: Json = null,
      owners: Json = null;
    if (!reasons.length) {
      owners = this.shaPrs(repo, view.headRefOid);
      if (!equal(owners, { heads: [number], entries: [] }))
        reasons = [
          "PR head is shared with another PR or queue candidate, or is no longer current",
        ];
    }
    if (!reasons.length) {
      const updated = this.gate.summaryUpdatedAt(
        array(current.comments),
        string(view.headRefOid),
      );
      const reviews = this.gate.pages(
          `repos/${repo}/pulls/${number}/reviews?per_page=100`,
        ),
        reviewed = object(current.reviewed);
      const valid = reviews.filter((review) =>
        this.gate.matchesApproval(review, reviewed, updated),
      );
      if (valid.length) {
        approval = field(valid.at(-1)!, "id");
        this.gate.confirmReview(
          repo,
          number,
          approval,
          "APPROVED",
          reviewed,
          updated,
        );
      } else reasons = ["no current event-bound Cladiron approval"];
    }
    return { view, evidence: current, approval, head_prs: owners, reasons };
  }
  entry(repo: string, number: bigint): Record | null {
    const [owner, name] = repo.split("/");
    const query =
      "query($owner:String!,$name:String!,$number:Int!){repository(owner:$owner,name:$name)" +
      "{nameWithOwner pullRequest(number:$number){number mergeQueueEntry{id enqueuedAt position state " +
      "baseCommit{oid} headCommit{oid} pullRequest{number headRefOid baseRefOid baseRefName} " +
      "mergeQueue{id repository{nameWithOwner}}}}}}";
    const result = parse(
      this.gate.gh(
        "api",
        "graphql",
        "--hostname",
        "github.com",
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
      throw new Error("queue query returned incomplete evidence");
    const repository = field(field(result, "data"), "repository"),
      pull = field(repository, "pullRequest");
    if (
      field(repository, "nameWithOwner") !== repo ||
      !equal(field(pull, "number"), number)
    )
      throw new Error("queue query returned another PR");
    const value = field(pull, "mergeQueueEntry");
    if (value === null) return null;
    if (
      [get(value, "id"), get(get(value, "mergeQueue", {}), "id")].some(
        (id) => typeof id !== "string" || !id || WHITESPACE.test(id),
      ) ||
      field(
        field(field(value, "mergeQueue"), "repository"),
        "nameWithOwner",
      ) !== repo ||
      !equal(field(field(value, "pullRequest"), "number"), number)
    )
      throw new Error("queue entry has no exact repository, PR and revisions");
    const waiting =
      get(value, "state") === "QUEUED" &&
      Object.hasOwn(object(value), "headCommit") &&
      Object.hasOwn(object(value), "baseCommit") &&
      field(value, "headCommit") === null &&
      field(value, "baseCommit") === null;
    if (
      !waiting &&
      ["headCommit", "baseCommit"].some(
        (key) =>
          !sha(get(truth(get(value, key)) ? get(value, key) : {}, "oid")),
      )
    )
      throw new Error(
        "queue entry has no exact revisions or explicit waiting state",
      );
    timestamp(field(value, "enqueuedAt"));
    return object(value);
  }
  checkRuns(repo: string, head: Json): Json[] {
    const batches = array(
      parse(
        this.gate.gh(
          "api",
          "--hostname",
          "github.com",
          "--paginate",
          "--slurp",
          `repos/${repo}/commits/${head}/check-runs?filter=all&per_page=100`,
        ),
      ),
    );
    const runs = batches.flatMap((batch) => array(field(batch, "check_runs")));
    if (
      !batches.length ||
      batches.some(
        (batch) => !equal(field(batch, "total_count"), BigInt(runs.length)),
      )
    )
      throw new Error("check run pagination is incomplete or changed");
    return runs;
  }
  groupSnapshot(repo: string, number: bigint, expected?: Json): Record {
    const review = this.reviewSnapshot(repo, number);
    if (expected !== undefined && !equal(review.view, expected))
      throw new CandidateChanged(
        null,
        "candidate changed during queue evidence collection",
      );
    const queued = this.entry(repo, number);
    if (queued === null) throw new Error("PR is no longer queued");
    const view = object(review.view),
      position = field(queued, "position"),
      stack = get(view, "stack");
    if (
      (stack === null && integer(position) && position > 1n) ||
      queued.state === "QUEUED"
    )
      throw new QueueWaiting("waiting for the first queue candidate");
    let members: (Record & { number: bigint })[] | null = null;
    if (stack !== null) {
      const active = array(field(stack, "members")),
        index = active.findIndex((member) => field(member, "number") === number);
      if (index < 0) throw new Error("queued PR is not an open stack member");
      members = active.flatMap((member, memberIndex) => {
        const memberNumber = field(member, "number");
        if (!integer(memberNumber))
          throw new Error("stack member number is invalid");
        const memberEntry =
          memberNumber === number ? queued : this.entry(repo, memberNumber);
        // A future layer is not part of this candidate unless GitHub assigns
        // it the same SHA. The lower prefix can pass before the upper is built.
        if (
          memberIndex > index &&
          (memberEntry === null ||
            !equal(memberEntry.headCommit, queued.headCommit))
        )
          return [];
        if (memberEntry === null)
          throw new Error("a required stack layer left the queue");
        const memberReview =
          memberNumber === number
            ? review
            : this.reviewSnapshot(repo, memberNumber);
        const memberStack = field(field(memberReview, "view"), "stack");
        if (
          ["id", "number", "size", "trunk", "members"].some(
            (key) => !equal(field(memberStack, key), field(stack, key)),
          ) ||
          !equal(field(memberStack, "position"), field(member, "position"))
        )
          throw new Error(
            "queue members do not belong to the same native stack",
          );
        if (
          field(memberEntry, "position") !== BigInt(memberIndex + 1) ||
          !equal(
            field(field(memberEntry, "mergeQueue"), "id"),
            field(field(queued, "mergeQueue"), "id"),
          )
        )
          throw new QueueWaiting(
            "waiting for the complete stack at the front of the queue",
          );
        if (
          field(memberEntry, "headCommit") === null ||
          field(memberEntry, "baseCommit") === null
        )
          throw new QueueWaiting("waiting for the covered stack prefix");
        const memberView = field(memberReview, "view"),
          memberPull = field(memberEntry, "pullRequest");
        if (
          !["AWAITING_CHECKS", "MERGEABLE", "LOCKED"].includes(
            string(field(memberEntry, "state")),
          ) ||
          ["headRefOid", "baseRefOid", "baseRefName"].some(
            (key) => !equal(field(memberPull, key), field(memberView, key)),
          )
        )
          throw new Error("stack queue entry differs from its reviewed layer");
        return [
          {
            number: memberNumber,
            review: memberReview,
            entry: entryIdentity(memberEntry),
          },
        ];
      });
      if (
        (members.length === 2 && !equal(
          field(field(field(field(members[1].review, "view"), "stack"), "parent"), "head"),
          field(field(members[0].review, "view"), "headRefOid"),
        )) ||
        members.some(
          (member) =>
            !equal(
              field(
                field(field(member.review, "evidence"), "integration"),
                "base",
              ),
              field(field(review.evidence, "integration"), "base"),
            ),
        )
      )
        throw new Error(
          "stack members changed during queue evidence collection",
        );
    }
    const head = field(queued.headCommit, "oid"),
      base = field(queued.baseCommit, "oid"),
      pull = queued.pullRequest;
    const integration = get(review.evidence, "integration");
    let prefix: Record | null = null;
    if (
      members?.length === 2 &&
      members[1].number === number &&
      !equal(field(field(members[0].entry, "headCommit"), "oid"), head)
    ) {
      const lower = members[0];
      // A distinct upper candidate extends the verified queue prefix, which
      // may still be waiting to land on dev. Never trust its SHA alone.
      prefix = this.groupSnapshot(repo, lower.number);
      if (
        !equal(prefix.review, lower.review) ||
        !equal(prefix.entry, lower.entry) ||
        !equal(prefix.head, field(field(lower.entry, "headCommit"), "oid")) ||
        array(field(prefix, "members")).length !== 1
      )
        throw new Error("lower queue prefix changed during evidence collection");
    }
    if (
      !integer(position) ||
      (stack === null && position !== 1n) ||
      !["AWAITING_CHECKS", "MERGEABLE", "LOCKED"].includes(
        string(field(queued, "state")),
      ) ||
      this.gate.target(view) !== "dev" ||
      !equal(field(pull, "headRefOid"), view.headRefOid) ||
      !equal(field(pull, "baseRefOid"), view.baseRefOid) ||
      equal(head, view.headRefOid) ||
      integration === null ||
      !equal(prefix === null ? field(integration, "base") : prefix.head, base)
    )
      throw new Error("queue entry is not the exact supported dev candidate");
    if (!equal(this.gate.targetHead(repo, "dev"), field(integration, "base")))
      throw new Error("queue baseline is not current dev");
    const batches = array(
      this.actions(
        `repos/${repo}/actions/runs?event=merge_group&head_sha=${head}&per_page=100`,
        true,
      ),
    );
    const runs = batches.flatMap((batch) =>
      array(field(batch, "workflow_runs")),
    );
    if (
      !batches.length ||
      batches.some(
        (batch) => !equal(field(batch, "total_count"), BigInt(runs.length)),
      )
    )
      throw new Error("queue CI pagination is incomplete or changed");
    const latest = new Map<string, Record>();
    for (const run of runs) {
      const path = string(get(run, "path", "")).split("@")[0];
      if (!WORKFLOWS.has(path)) continue;
      if (
        !(
          members === null ? [number] : members.map((member) => member.number)
        ).includes(runTarget(run, repo)) ||
        !equal(field(run, "head_sha"), head)
      )
        throw new Error("queue CI identifies a different candidate");
      const previous = latest.get(path);
      if (
        !previous ||
        compare(numeric(field(run, "id")), numeric(previous.id)) > 0 ||
        (equal(field(run, "id"), previous.id) &&
          compare(
            numeric(field(run, "run_attempt")),
            numeric(previous.run_attempt),
          ) > 0)
      )
        latest.set(
          path,
          Object.fromEntries(
            [
              "id",
              "run_attempt",
              "head_branch",
              "head_sha",
              "path",
              "event",
              "repository",
              "status",
              "conclusion",
            ].map((key) => [key, field(run, key)]),
          ),
        );
    }
    if (!latest.size) throw new QueueWaiting("queue CI has not started");
    if (new Set([...latest.values()].map((run) => run.head_branch)).size !== 1)
      throw new Error("queue CI workflows identify different refs");
    const branch = string(latest.values().next().value!.head_branch);
    const ref = parse(
      this.gate.gh(
        "api",
        "--hostname",
        "github.com",
        `repos/${repo}/git/ref/heads/${branch.split("/").map(encodeURIComponent).join("/")}`,
      ),
    );
    if (
      get(ref, "ref") !== `refs/heads/${branch}` ||
      !equal(get(get(ref, "object", {}), "sha"), head)
    )
      throw new Error("queue ref no longer points to the CI candidate");
    const commit = parse(
      this.gate.gh(
        "api",
        "--hostname",
        "github.com",
        `repos/${repo}/git/commits/${head}`,
      ),
    );
    const finalView =
      members === null ? view : object(field(members.at(-1)!.review, "view"));
    const reviewed = parse(
      this.gate.gh(
        "api",
        "--hostname",
        "github.com",
        `repos/${repo}/git/commits/${finalView.headRefOid}`,
      ),
    );
    const tree = get(
      truth(get(commit, "tree")) ? get(commit, "tree") : {},
      "sha",
    );
    if (
      !equal(get(commit, "sha"), head) ||
      !equal(
        array(field(commit, "parents")).map((parent) => field(parent, "sha")),
        [base],
      ) ||
      !equal(get(reviewed, "sha"), finalView.headRefOid) ||
      !sha(tree) ||
      !sha(
        get(truth(get(reviewed, "tree")) ? get(reviewed, "tree") : {}, "sha"),
      ) ||
      ((members !== null || equal(field(integration, "merge_base"), base)) &&
        !equal(tree, field(field(reviewed, "tree"), "sha")))
    )
      throw new Error(
        "queue commit does not bind exactly the reviewed head and baseline",
      );
    const statuses = new Map<Json, Json>();
    for (const status of this.gate.pages(
      `repos/${repo}/commits/${head}/statuses?per_page=100`,
    ))
      if (!statuses.has(field(status, "context")))
        statuses.set(field(status, "context"), status);
    const checks = this.gate.headChecks(this.checkRuns(repo, head), [
      ...statuses.values(),
    ]);
    const required = this.gate.prerequisites(
      array(field(review.evidence, "rules")),
      view,
    );
    const waiting = this.gate.checkFailures(checks, required),
      owners = this.shaPrs(repo, head);
    const failed = [...latest.values()].some(
      (run) =>
        run.status === "completed" &&
        !this.gate.passing.has(
          string(truth(run.conclusion) ? run.conclusion : "").toUpperCase(),
        ),
    );
    const pending =
      latest.size !== WORKFLOWS.size ||
      [...latest.values()].some((run) => run.status !== "completed");
    const unique = equal(owners, {
      heads: [],
      entries:
        members === null
          ? [{ number, id: queued.id }]
          : members
              .filter((member) =>
                equal(field(field(member.entry, "headCommit"), "oid"), head),
              )
              .map((member) => ({
                number: member.number,
                id: field(member.entry, "id"),
              }))
              .sort((a, b) => compare(numeric(a.number), numeric(b.number))),
    });
    const conclusion =
      truth(review.reasons) ||
      (members !== null &&
        members.some((member) => truth(field(member.review, "reasons")))) ||
      !unique ||
      failed ||
      prefix?.conclusion === "failure"
        ? "failure"
        : waiting.length ||
            pending ||
            (prefix !== null && prefix.conclusion !== "success")
          ? null
          : "success";
    return {
      review,
      entry: entryIdentity(queued),
      ref: branch,
      head,
      runs: [...latest.values()],
      head_prs: owners,
      checks: checks.filter(
        (check) => !equal([field(check, "name"), get(check, "app_id")], CHECK),
      ),
      conclusion,
      ...(members === null ? {} : { members }),
      ...(prefix === null ? {} : { prefix }),
    };
  }
  ownChecks(repo: string, head: Json): Map<bigint, Record> {
    const runs = this.checkRuns(repo, head).filter(
      (run) =>
        field(run, "name") === CHECK[0] &&
        equal(
          get(truth(get(run, "app")) ? get(run, "app") : {}, "id"),
          CHECK[1],
        ),
    );
    const owned = new Map<bigint, Record>();
    for (const run of runs) {
      const id = get(run, "id");
      if (
        !equal(get(run, "head_sha"), head) ||
        get(field(run, "app"), "slug") !== "cladiron" ||
        !integer(id) ||
        id <= 0n ||
        owned.has(id)
      )
        throw new Error(
          "existing check identities are not confirmed for this SHA",
        );
      owned.set(id, object(run));
    }
    return owned;
  }
  setCheck(
    repo: string,
    head: Json,
    identity: string,
    conclusion: Json,
    snapshot: Record,
    checkId: bigint | null = null,
  ): bigint {
    const payload = checkPayload(head, identity, conclusion, snapshot);
    const owned = this.ownChecks(repo, head);
    if (
      checkId !== null &&
      (!integer(checkId) || checkId !== maximum(owned.keys()))
    )
      throw new Error(
        "check evaluation is missing or superseded by a newer run",
      );
    const existing: Record = checkId === null ? {} : (owned.get(checkId) ?? {});
    if (
      conclusion === null &&
      (get(existing, "status") === "completed" ||
        get(existing, "conclusion") !== null)
    )
      throw new Error("a completed check needs a new evaluation run to wait");
    const matches = (run: Json): boolean => matchesCheck(run, payload);
    if (!matches(existing)) {
      const path = `repos/${repo}/check-runs${checkId === null ? "" : `/${checkId}`}`;
      try {
        this.gate.writeCheck(
          [
            "api",
            "--hostname",
            "github.com",
            "-X",
            checkId === null ? "POST" : "PATCH",
            path,
            "--input",
            "-",
          ],
          Object.fromEntries(
            Object.entries(payload).filter(
              ([key]) => key !== "head_sha" || checkId === null,
            ),
          ),
        );
      } catch {
        this.gate.error(
          "check response failed; reading the same target without retry",
        );
      }
      if (checkId === null) {
        const created = [...this.ownChecks(repo, head)].filter(
          ([id, run]) =>
            !owned.has(id) && get(run, "external_id") === payload.external_id,
        );
        if (created.length !== 1)
          throw new Error(
            "check POST did not establish exactly one new bound check",
          );
        checkId = created[0][0];
      }
    }
    const confirmed = parse(
      this.gate.gh(
        "api",
        "--hostname",
        "github.com",
        `repos/${repo}/check-runs/${checkId}`,
      ),
    );
    if (
      !equal(get(confirmed, "id"), checkId) ||
      !equal(get(get(confirmed, "app", {}), "id"), CHECK[1]) ||
      get(get(confirmed, "app", {}), "slug") !== "cladiron" ||
      !matches(confirmed)
    )
      throw new Error(
        "check readback did not confirm its exact identity, binding and result",
      );
    if (
      checkId === null ||
      checkId !== maximum(this.ownChecks(repo, head).keys())
    )
      throw new Error("check evaluation was superseded during publication");
    return checkId;
  }
  private reuseQueueSuccess(
    repo: string,
    number: bigint,
    view: Record,
    queued: Record,
  ): boolean {
    if (queued.headCommit === null) return false;
    const head = field(queued.headCommit, "oid");
    try {
      const owned = this.ownChecks(repo, head),
        id = maximum(owned.keys());
      if (id === null) return false;
      const existing = owned.get(id)!;
      if (existing.status !== "completed" || existing.conclusion !== "success")
        return false;
      const current = this.groupSnapshot(repo, number, view);
      if (
        current.conclusion !== "success" ||
        !equal(current.head, head) ||
        !equal(current.entry, entryIdentity(queued))
      )
        return false;
      const payload = checkPayload(
        head,
        binding(repo, number, current, true),
        "success",
        current,
      );
      if (!matchesCheck(existing, payload)) return false;
      const confirmed = parse(
        this.gate.gh(
          "api",
          "--hostname",
          "github.com",
          `repos/${repo}/check-runs/${id}`,
        ),
      );
      if (
        !equal(get(confirmed, "id"), id) ||
        !equal(get(get(confirmed, "app", {}), "id"), CHECK[1]) ||
        get(get(confirmed, "app", {}), "slug") !== "cladiron" ||
        !matchesCheck(confirmed, payload)
      )
        return false;
      if (!equal(current, this.groupSnapshot(repo, number, view))) return false;
      const latest = this.ownChecks(repo, head);
      if (
        maximum(latest.keys()) !== id ||
        !matchesCheck(latest.get(id) ?? {}, payload)
      )
        return false;
      this.gate.log(
        `#${number} queue evidence unchanged; retained successful check ${id}`,
      );
      return true;
    } catch (error) {
      // A duplicate notification can race with the merge it already allowed.
      // Keep that completed check as history, just as publication does.
      try {
        const latest = this.gate.prView(repo, number);
        if (mergedCandidate(latest, view)) {
          this.gate.log(
            `#${number} merged during queue refresh; retained check is historical`,
          );
          return true;
        }
      } catch {
        /* Fall through to a blocked evaluation when reads are unavailable. */
      }
      this.gate.log(
        `#${number} queue success cannot be reused: ${error instanceof Error ? error.message : "unknown read failure"}`,
      );
      return false;
    }
  }
  private hold(
    repo: string,
    number: bigint,
    snapshot: Record,
    queued = false,
  ): Evaluation {
    const head = queued ? snapshot.head : field(snapshot.view, "headRefOid"),
      identity = binding(repo, number, snapshot, queued);
    return {
      id: this.setCheck(repo, head, identity, null, snapshot),
      head,
      identity,
    };
  }
  private invalidate(
    repo: string,
    evaluation: Evaluation,
    reason: Record,
  ): void {
    this.setCheck(
      repo,
      evaluation.head,
      evaluation.identity,
      "failure",
      reason,
      evaluation.id,
    );
  }
  publish(
    repo: string,
    number: bigint,
    before: Record,
    queued = false,
    evaluation: Evaluation | null = null,
  ): bigint {
    const read = () =>
      queued
        ? this.groupSnapshot(repo, number, field(before.review, "view"))
        : this.reviewSnapshot(repo, number);
    const head = queued ? before.head : field(before.view, "headRefOid"),
      identity = binding(repo, number, before, queued);
    if (evaluation !== null && !equal(head, evaluation.head)) {
      this.invalidate(repo, evaluation, {
        candidate_changed_before_write: true,
      });
      throw new CandidateChanged(
        evaluation.id,
        "candidate changed before check publication",
      );
    }
    let checkId = evaluation?.id ?? null;
    const view = (snapshot: Record): Json =>
      queued ? field(snapshot.review, "view") : snapshot.view;
    let changed: boolean,
      candidateChanged = false;
    try {
      const after = read();
      changed = !equal(before, after);
      candidateChanged = !equal(view(before), view(after));
      if (
        queued &&
        equal(before.review, after.review) &&
        identity === binding(repo, number, after, true)
      ) {
        before = after;
        changed = false;
      }
    } catch (error) {
      checkId = this.setCheck(
        repo,
        head,
        identity,
        "failure",
        { read_failed_before_write: true },
        checkId,
      );
      if (error instanceof CandidateChanged)
        throw new CandidateChanged(checkId, error.message);
      throw error;
    }
    let conclusion = queued
      ? before.conclusion
      : truth(before.reasons)
        ? "failure"
        : "success";
    if (changed) conclusion = "failure";
    checkId = this.setCheck(repo, head, identity, conclusion, before, checkId);
    if (candidateChanged)
      throw new CandidateChanged(
        checkId,
        "candidate changed before check publication",
      );
    if (conclusion === "success") {
      let unchanged: boolean;
      try {
        const after = read();
        if (!queued && mergedCandidate(object(after.view), before.view)) {
          this.gate.log(
            `#${number} merged after confirmed admission; retained check is historical`,
          );
          return checkId;
        }
        unchanged = equal(after, before);
        candidateChanged = !equal(view(before), view(after));
      } catch (error) {
        let latest: Record | null = null;
        try {
          latest = this.gate.prView(repo, number);
        } catch {
          /* Preserve the original read failure. */
        }
        const original = queued ? field(before.review, "view") : before.view;
        if (latest && mergedCandidate(latest, original)) {
          this.gate.log(
            `#${number} merged after confirmed check; retained check is historical`,
          );
          return checkId;
        }
        checkId = this.setCheck(
          repo,
          head,
          identity,
          "failure",
          { read_failed_after_write: true },
          checkId,
        );
        if (error instanceof CandidateChanged)
          throw new CandidateChanged(checkId, error.message);
        throw error;
      }
      if (!unchanged) {
        this.setCheck(
          repo,
          head,
          identity,
          "failure",
          { changed_after_write: true },
          checkId,
        );
        if (candidateChanged)
          throw new CandidateChanged(
            checkId,
            "candidate changed during check publication",
          );
        throw new Error(
          "candidate or review evidence changed during check publication",
        );
      }
    }
    return checkId;
  }
  holdEntry(
    repo: string,
    number: bigint,
    view: Record,
    queued: Record,
  ): Evaluation | null {
    if (field(queued, "headCommit") === null) return null;
    const pending = {
      review: { view },
      entry: queued,
      ref: null,
      head: field(queued.headCommit, "oid"),
    };
    const evaluation = this.hold(repo, number, pending, true);
    if (equal(pending.head, view.headRefOid))
      throw new Error(
        "queue entry reused the PR SHA instead of a distinct candidate",
      );
    return evaluation;
  }
  reconcile(repo: string, number: bigint): number {
    let admissionCheck: Evaluation | null = null,
      groupCheck: Evaluation | null = null;
    try {
      const initial = this.gate.prView(repo, number);
      if (
        !["dev", "main"].includes(this.gate.target(initial)) ||
        initial.state === "MERGED"
      )
        return this.gate.main(number);
      let queued =
        this.gate.target(initial) === "dev" ? this.entry(repo, number) : null;
      const stack = get(initial, "stack");
      if (
        queued !== null &&
        queued.headCommit !== null &&
        stack !== null &&
        array(field(stack, "members")).length === 2 &&
        get(stack, "position") === 2n
      ) {
        const first = field(array(field(stack, "members"))[0], "number");
        if (!integer(first)) throw new Error("stack member number is invalid");
        const lower = this.entry(repo, first);
        if (lower !== null && equal(lower.headCommit, queued.headCommit)) {
          // Canonicalize only the group check. Each member still owns renewal
          // and withdrawal of its approval before the group reads admission.
          const memberResult = this.gate.main(number);
          const groupResult = this.reconcile(repo, first);
          return memberResult || groupResult;
        }
      }
      if (
        queued !== null &&
        this.reuseQueueSuccess(repo, number, initial, queued)
      )
        return 0;
      const admission = {
        view: initial,
        reasons: ["review evaluation in progress"],
      };
      if (queued !== null)
        groupCheck = this.holdEntry(repo, number, initial, queued);
      if (groupCheck === null)
        admissionCheck = this.hold(repo, number, admission);
      const result = this.gate.main(number);
      if (result) {
        if (queued !== null && admissionCheck === null)
          this.setCheck(
            repo,
            initial.headRefOid,
            binding(repo, number, admission),
            "failure",
            admission,
          );
        return result;
      }
      const review = this.reviewSnapshot(repo, number);
      if (!equal(review.view, initial)) {
        const held = admissionCheck ?? groupCheck;
        if (held !== null)
          this.invalidate(repo, held, {
            candidate_changed_during_review: true,
          });
        this.gate.log(
          `#${number} candidate changed during review; original check invalidated`,
        );
        return result;
      }
      const admissionId = this.publish(
        repo,
        number,
        review,
        false,
        admissionCheck,
      );
      admissionCheck = {
        id: admissionId,
        head: field(review.view, "headRefOid"),
        identity: binding(repo, number, review),
      };
      if (initial.baseRefName === "main") return 0;
      const previous = queued;
      queued = this.entry(repo, number);
      if (!equal(entryIdentity(queued), entryIdentity(previous))) {
        if (groupCheck !== null)
          this.invalidate(repo, groupCheck, {
            entry_changed_during_review: true,
          });
        groupCheck =
          queued === null
            ? null
            : this.holdEntry(repo, number, object(review.view), queued);
      }
      if (queued !== null) {
        let group: Record;
        try {
          group = this.groupSnapshot(repo, number, review.view);
        } catch (error) {
          if (error instanceof CandidateChanged) throw error;
          if (error instanceof QueueWaiting) {
            this.gate.log(
              `#${number} ${error.message}; awaiting a verified queue candidate`,
            );
            return 0;
          }
          if (groupCheck !== null) {
            const invalid = {
              review,
              entry: queued,
              ref: null,
              head: field(queued.headCommit, "oid"),
            };
            this.invalidate(repo, groupCheck, invalid);
          }
          throw error;
        }
        if (!equal(field(group.review, "view"), review.view)) {
          const held = groupCheck ?? admissionCheck;
          this.invalidate(repo, held, { candidate_changed_before_queue: true });
          throw new CandidateChanged(
            held.id,
            "candidate changed before queue publication",
          );
        }
        if (groupCheck === null || !equal(group.entry, entryIdentity(queued))) {
          if (groupCheck !== null)
            this.invalidate(repo, groupCheck, {
              entry_changed_during_review: true,
            });
          groupCheck = this.holdEntry(
            repo,
            number,
            object(field(group.review, "view")),
            object(group.entry),
          );
        }
        this.publish(repo, number, group, true, groupCheck);
      }
      return 0;
    } catch (error) {
      // Candidate drift from reads can invalidate every held target. Publication
      // supplies its ID only after confirmed invalidation, never an unknown write.
      if (error instanceof CandidateChanged) {
        try {
          for (const held of [admissionCheck, groupCheck])
            if (held !== null && held.id !== error.checkId)
              this.invalidate(repo, held, {
                candidate_changed_during_publication: true,
              });
        } catch (failure) {
          this.gate.error(
            `#${number} candidate changed; check invalidation failed: ${failure instanceof Error ? failure.message : "unknown error"}`,
          );
          return 1;
        }
      }
      this.gate.error(
        `#${number} queue check failed: ${error instanceof Error ? error.message : "unknown error"}`,
      );
      return 1;
    }
  }
}
