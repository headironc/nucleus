import {
  BOT,
  Gate,
  headChecks,
  type Check,
  type GateOptions,
} from "./review-gate";
import { CHECK } from "./review-queue";
import {
  array,
  encode,
  field,
  object,
  string,
  type Json,
  type Record,
} from "./review-json";

export const REPO = "owner/repository";
export const HEAD = "a".repeat(40),
  OLD = "b".repeat(40),
  BASE = "c".repeat(40);
export const GROUP = "d".repeat(40),
  TREE = "e".repeat(40);
export const BRANCH = "gh-readonly-queue/dev/pr-42-abcdef";
export const VIEW: Record = {
  headRefOid: HEAD,
  baseRefOid: BASE,
  baseRefName: "dev",
  state: "OPEN",
  isDraft: false,
  lifecycle: null,
};
export const UPPER = "f".repeat(40);
export const STACK: Record = {
  id: "stack-7",
  number: 7n,
  size: 2n,
  trunk: "dev",
  position: 1n,
  parent: null,
  members: [
    { id: "layer-1", number: 42n, position: 1n },
    { id: "layer-2", number: 43n, position: 2n },
  ],
};
export const LOWER_VIEW: Record = { ...VIEW, stack: STACK };
export const UPPER_VIEW: Record = {
  ...VIEW,
  headRefOid: UPPER,
  baseRefName: "codex/lower",
  baseRefOid: HEAD,
  stack: {
    ...STACK,
    position: 2n,
    parent: { number: 42n, head: HEAD, ref: "codex/lower" },
  },
};
export function nativeStackView(upper = false): Record {
  return {
    ...(upper ? UPPER_VIEW : LOWER_VIEW),
    headRefName: upper ? "codex/upper" : "codex/lower",
    stackEntry: {
      id: upper ? "layer-2" : "layer-1",
      position: upper ? 2n : 1n,
    },
    stack: {
      id: "stack-7",
      number: 7n,
      size: 2n,
      baseRefName: "dev",
      entries: {
        totalCount: 2n,
        pageInfo: { hasNextPage: false },
        nodes: [
          {
            id: "layer-1",
            position: 1n,
            pullRequest: {
              number: 42n,
              ...VIEW,
              headRefName: "codex/lower",
              headRepository: { nameWithOwner: REPO },
            },
          },
          {
            id: "layer-2",
            position: 2n,
            pullRequest: {
              number: 43n,
              ...UPPER_VIEW,
              baseRef: { target: { oid: HEAD } },
              headRefName: "codex/upper",
              headRepository: { nameWithOwner: REPO },
            },
          },
        ],
      },
    },
  };
}
export const CONTEXT: Record = {
  event: "pull_request_target",
  path: ".github/workflows/review-context.yml",
  repository: { full_name: REPO },
  head_sha: HEAD,
  display_title: `context:42:${HEAD}:${BASE}:dev`,
  status: "completed",
  conclusion: "success",
  created_at: "2026-10-18T00:00:00Z",
};
export const RECORDED = { base: BASE, recorded_at: CONTEXT.created_at };
export const ACTIONS = 15368n;
export const REQUIRED: Check[] = [
  ["docs", ACTIONS],
  ["rust", ACTIONS],
];
export function summary(
  code = "completed",
  security = "completed",
  sha = HEAD,
  user = BOT,
): Record {
  const state = Object.fromEntries(
    [
      ["code_review", code],
      ["security_review", security],
    ].map(([kind, status]) => [
      kind,
      {
        commit_sha: sha,
        status,
        has_any_findings: false,
        has_unresolved_findings: false,
      },
    ]),
  );
  const rows = [
    ["Code Review", code],
    ["Security Review", security],
  ]
    .map(
      ([label, status]) =>
        `| x **${label}** | y **${status[0].toUpperCase() + status.slice(1)}** <relative-time datetime="2026-10-18T00:05:00Z">t</relative-time> | \`${sha.slice(0, 7)}\` | PR opened |\n`,
    )
    .join("");
  const actor = {
    __typename: user === BOT ? "Bot" : "User",
    login: user.replace(/\[bot\]$/, ""),
  };
  return {
    author: actor,
    editor: actor,
    lastEditedAt: "2026-10-18T00:05:00Z",
    updatedAt: "2026-10-18T00:05:00Z",
    body: `<!-- codex-review-state:v1 ${encode(state)} -->\n## Codex Review Summary\n${rows}`,
  };
}
export function run(
  name: string,
  status = "completed",
  conclusion: Json = "success",
  app: Json = ACTIONS,
  id: Json = 1n,
): Record {
  return { id, name, status, conclusion, app: { id: app } };
}
export const PASS = headChecks([run("rust"), run("docs")], []);
export const PROTECTIONS: Json[] = [
  { type: "non_fast_forward" },
  { type: "deletion" },
  {
    type: "pull_request",
    parameters: {
      required_approving_review_count: 1n,
      required_review_thread_resolution: true,
      dismiss_stale_reviews_on_push: true,
      dismissal_restriction: { enabled: false, allowed_actors: [] },
    },
  },
  { type: "required_signatures" },
];
export const QUEUE_RULES: Json[] = [
  ...PROTECTIONS,
  {
    type: "merge_queue",
    parameters: {
      grouping_strategy: "ALLGREEN",
      max_entries_to_build: 1n,
      max_entries_to_merge: 1n,
      min_entries_to_merge: 1n,
      merge_method: "SQUASH",
    },
  },
  {
    type: "required_status_checks",
    parameters: {
      strict_required_status_checks_policy: true,
      required_status_checks: [...REQUIRED, ["website", ACTIONS], CHECK].map(
        ([context, integration_id]) => ({ context, integration_id }),
      ),
    },
  },
];
export const MAIN_RULES: Json[] = [
  ...QUEUE_RULES.filter((rule) => field(rule, "type") !== "merge_queue"),
  {
    type: "workflows",
    parameters: {
      do_not_enforce_on_create: false,
      workflows: [
        {
          repository_id: 1432619931n,
          path: ".github/workflows/review-gate.yml",
          ref: "refs/heads/dev",
        },
      ],
    },
  },
];
export const REVIEW: Record = {
  view: VIEW,
  evidence: {
    rules: QUEUE_RULES,
    reviewed: VIEW,
    integration: { base: BASE, merge_base: BASE },
  },
  approval: 7n,
  reasons: [],
};
export const ENTRY: Record = {
  id: "entry-1",
  enqueuedAt: "2026-10-19T00:00:00Z",
  position: 1n,
  state: "AWAITING_CHECKS",
  headCommit: { oid: GROUP },
  baseCommit: { oid: BASE },
  pullRequest: {
    number: 42n,
    headRefOid: HEAD,
    baseRefOid: BASE,
    baseRefName: "dev",
  },
  mergeQueue: { id: "queue-1", repository: { nameWithOwner: REPO } },
};
export const RUNS: Record[] = [
  ".github/workflows/docs.yml",
  ".github/workflows/rust.yml",
  ".github/workflows/halo.yml",
].map((path, index) => ({
  id: BigInt(index + 1),
  run_attempt: 1n,
  event: "merge_group",
  repository: { full_name: REPO },
  head_sha: GROUP,
  head_branch: BRANCH,
  path,
  status: "completed",
  conclusion: "success",
}));
export function rewritten(
  before: Json = "diverged",
  after: Json = "ahead",
  created: Json = "2026-10-17T23:00:00Z",
): Record {
  return {
    __typename: "HeadRefForcePushedEvent",
    createdAt: created,
    beforeCommit: { oid: OLD },
    afterCommit: { oid: BASE },
    beforeRelationship: before,
    afterRelationship: after,
  };
}
export function viewResponse(value: Record): Record {
  const view = structuredClone(value),
    event = view.lifecycle;
  delete view.lifecycle;
  view.number = 42n;
  view.stack ??= null;
  view.stackEntry ??= null;
  view.timelineItems = {
    nodes:
      event === null
        ? []
        : [{ __typename: field(event, "type"), id: field(event, "id") }],
    pageInfo: { hasNextPage: false },
  };
  return { data: { repository: { nameWithOwner: REPO, pullRequest: view } } };
}
export function approval(lifecycle: Json = null): Record {
  return {
    id: 7n,
    state: "APPROVED",
    commit_id: HEAD,
    submitted_at: "2026-10-18T00:06:00Z",
    user: { login: "cladiron[bot]", type: "Bot" },
    body:
      `Review gate (nucleus-review-gate:v4): machine conditions verified on ${HEAD} against dev at ${BASE}. ` +
      "Both Codex reviews completed with no reported unresolved findings; review material predates completion, " +
      "all threads are resolved and required checks pass.\nReopen event: " +
      (lifecycle === null ? "none" : `id:${field(lifecycle, "id")}`) +
      "\nScope excludes substantive finding disposition and original-report completeness. " +
      "Finding handlers record dispositions in the original review threads. " +
      "Routine dev queue merges consume the current admission result.",
  };
}
export function fakeGate(options: Partial<GateOptions> = {}): Gate {
  return new Gate({
    repo: REPO,
    log() {},
    error() {},
    gh() {
      throw new Error("unexpected GitHub request");
    },
    actions() {
      throw new Error("unexpected Actions request");
    },
    ...options,
  });
}

const checkRuns = [
  run("rust", "completed", "success", ACTIONS, 9007199254740992n),
  run("docs"),
  run("website"),
  run("notify", "completed", "success", ACTIONS, 100n),
  run("docs", "completed", "failure", 999n),
];
const statuses = [
  { context: "external-a", state: "success" },
  { context: "external-b", state: "pending" },
];
const observed = { runs: checkRuns, statuses };
export const CHECK_CHANGES = [
  {
    name: "new successful notification replaces the old successful run",
    before: observed,
    after: {
      ...observed,
      runs: [
        run("notify", "completed", "success", ACTIONS, 101n),
        ...checkRuns,
      ],
    },
    stable: true,
  },
  {
    name: "run order including same-name checks from different Apps",
    before: observed,
    after: { ...observed, runs: checkRuns.toReversed() },
    stable: true,
  },
  {
    name: "distinct legacy status contexts change order",
    before: observed,
    after: { ...observed, statuses: statuses.toReversed() },
    stable: true,
  },
  {
    name: "runs and legacy statuses both change order",
    before: observed,
    after: { runs: checkRuns.toReversed(), statuses: statuses.toReversed() },
    stable: true,
  },
  {
    name: "a precisely higher required run ID fails",
    before: observed,
    after: {
      ...observed,
      runs: [
        run("rust", "completed", "failure", ACTIONS, 9007199254740993n),
        ...checkRuns,
      ],
    },
    stable: false,
  },
  {
    name: "a nonrequired check changes passed",
    before: observed,
    after: {
      ...observed,
      runs: [
        run("notify", "completed", "failure", ACTIONS, 101n),
        ...checkRuns,
      ],
    },
    stable: false,
  },
  {
    name: "a nonrequired check is added",
    before: observed,
    after: { ...observed, runs: [...checkRuns, run("new-check")] },
    stable: false,
  },
  {
    name: "a nonrequired check is removed",
    before: observed,
    after: {
      ...observed,
      runs: checkRuns.filter((run) => run.name !== "notify"),
    },
    stable: false,
  },
  {
    name: "same-name check changes App identity",
    before: observed,
    after: {
      ...observed,
      runs: checkRuns.map((run) =>
        field(run.app, "id") === 999n ? { ...run, app: { id: 1000n } } : run,
      ),
    },
    stable: false,
  },
  {
    name: "a legacy status changes passed",
    before: observed,
    after: {
      ...observed,
      statuses: [{ ...statuses[0], state: "failure" }, statuses[1]],
    },
    stable: false,
  },
  {
    name: "a legacy status is added",
    before: observed,
    after: {
      ...observed,
      statuses: [...statuses, { context: "external-c", state: "success" }],
    },
    stable: false,
  },
  {
    name: "a legacy status is removed",
    before: observed,
    after: { ...observed, statuses: statuses.slice(1) },
    stable: false,
  },
];

export function checkEvidence(
  runs: Json[],
  statuses: Json[],
  view = VIEW,
): Record {
  const gate = fakeGate({
    queueEnabled: true,
    gh(args) {
      if (!args.at(-1)!.endsWith("/check-runs?per_page=100"))
        throw new Error("unexpected check evidence request");
      return encode([{ check_runs: runs }]);
    },
  });
  gate.pages = (path) => {
    if (path.endsWith("/statuses?per_page=100")) return statuses;
    if (path.includes("/rules/branches/"))
      return view.baseRefName === "main" ? MAIN_RULES : QUEUE_RULES;
    if (path.endsWith("/comments")) return [];
    throw new Error(`unexpected evidence route ${path}`);
  };
  gate.reviewEvidence = () => ({
    comments: [summary()],
    reviews: [],
    unresolved: 0n,
  });
  gate.baselineEvents = () => [];
  return gate.evidence(REPO, 42n, view);
}

interface Exercise {
  views?: (Record | Error)[];
  snapshots?: (Record | Error)[];
  approvals?: Record[];
  recorded?: boolean;
  postError?: boolean;
  dismissError?: boolean;
  postEffect?: boolean;
  dismissEffect?: boolean;
  reviewRead?: (review: Record) => Record;
  queueTarget?: Json;
  mainTarget?: Json;
  mergeBases?: { [pair: string]: Json };
  targetRelationship?: string;
  records?: Json[];
  targetReads?: (Json | Error)[];
  reviewPages?: Record[];
  historyPages?: Record[];
}
export function exercise(options: Exercise = {}) {
  const calls: string[][] = [],
    logs: string[] = [];
  const enabled =
    options.queueTarget !== undefined || options.mainTarget !== undefined;
  const views = options.views ?? [VIEW],
    reviews = structuredClone(options.approvals ?? []);
  const clean: Record = {
    comments: [summary()],
    inline: [],
    reviews: [],
    unresolved: 0n,
    checks: enabled
      ? headChecks([run("rust"), run("docs"), run("website")], [])
      : PASS,
    events: [],
    rules: enabled
      ? options.mainTarget !== undefined
        ? MAIN_RULES
        : QUEUE_RULES
      : [
          ...PROTECTIONS,
          {
            type: "required_status_checks",
            parameters: {
              required_status_checks: REQUIRED.map(
                ([context, integration_id]) => ({ context, integration_id }),
              ),
            },
          },
        ],
  };
  const snapshots = (options.snapshots ?? [{}]).map((item) =>
    item instanceof Error ? item : { ...clean, ...item },
  );
  let viewReads = 0,
    evidenceReads = 0,
    targetReads = 0;
  const gate = fakeGate({
    queueEnabled: enabled,
    log: (line) => logs.push(line),
    error: (line) => logs.push(line),
    gh(args) {
      calls.push(args);
      const path = args.at(-1)!;
      if (
        path.endsWith("/git/ref/heads/dev") ||
        path.endsWith("/git/ref/heads/main")
      ) {
        const branch = path.split("/").at(-1)!;
        const target = options.targetReads
          ? options.targetReads[targetReads++]
          : branch === "main"
            ? options.mainTarget!
            : options.queueTarget!;
        if (target instanceof Error) throw target;
        return encode(
          typeof target === "object" && target !== null
            ? target
            : { ref: `refs/heads/${branch}`, object: { sha: target } },
        );
      }
      if (path.includes("/compare/")) {
        const [base, head] = path
          .split("/compare/")[1]
          .split("?")[0]
          .split("...");
        const key = `${base}:${head}`;
        const ancestor =
          options.mergeBases && Object.hasOwn(options.mergeBases, key)
            ? options.mergeBases[key]
            : BASE;
        return encode({
          base_commit: { sha: base },
          merge_base_commit: { sha: ancestor },
          status:
            base === head
              ? "identical"
              : base === BASE && head === HEAD
                ? "ahead"
                : head === options.queueTarget
                  ? (options.targetRelationship ?? "ahead")
                  : "diverged",
        });
      }
      if (args[1] === "graphql") {
        if (options.reviewPages && args[3].includes("timelineItems(first:100"))
          return encode(
            options.historyPages
              ? options.historyPages[
                  Math.min(evidenceReads - 1, options.historyPages.length - 1)
                ]
              : response("timelineItems", []),
          );
        if (options.reviewPages && args[3].includes("comments(first:100")) {
          const page =
            options.reviewPages[
              Math.min(evidenceReads++, options.reviewPages.length - 1)
            ];
          return encode(page);
        }
        const view = views[Math.min(viewReads++, views.length - 1)];
        if (view instanceof Error) throw view;
        return encode(viewResponse(view));
      }
      if (options.reviewPages && path.includes("/check-runs?"))
        return encode([
          {
            check_runs: [
              run("rust"),
              run("docs"),
              ...(enabled ? [run("website")] : []),
            ],
          },
        ]);
      if (args.includes("POST")) {
        if (options.postEffect !== false)
          reviews.push({
            ...approval(),
            id: 8n,
            submitted_at: "2026-10-18T00:10:00Z",
            body: args.find((arg) => arg.startsWith("body="))!.slice(5),
          });
        if (options.postError) throw new Error("approval response lost");
      }
      if (args.includes("PUT")) {
        const id = BigInt(args[3].split("/").at(-2)!);
        if (options.dismissEffect ?? !options.dismissError)
          reviews.find((review) => review.id === id)!.state = "DISMISSED";
        if (options.dismissError) throw new Error("dismissal response lost");
      }
      if (args.length === 2 && args[1].includes("/reviews/")) {
        const id = BigInt(args[1].split("/").at(-1)!);
        let review = structuredClone(
          reviews.find((review) => review.id === id)!,
        );
        if (options.reviewRead) review = options.reviewRead(review);
        return encode(review);
      }
      return "{}";
    },
  });
  gate.pages = (path) =>
    structuredClone(
      options.reviewPages && !path.endsWith("/reviews")
        ? path.includes("/rules/")
          ? array(clean.rules)
          : []
        : reviews,
    );
  if (!options.reviewPages)
    gate.evidence = () => {
      const item = snapshots[Math.min(evidenceReads++, snapshots.length - 1)];
      if (item instanceof Error) throw item;
      return structuredClone(item);
    };
  gate.contexts = () =>
    structuredClone(
      options.records ?? (options.recorded === false ? [] : [CONTEXT]),
    );
  const result = gate.main(42n),
    writes = calls.filter(
      (call) => call.includes("POST") || call.includes("PUT"),
    );
  return {
    result,
    writes,
    methods: writes.map((call) => call[2]),
    calls,
    reviews,
    evidenceReads,
    output: logs.join("\n"),
    gate,
  };
}
export function response(
  connection: string,
  nodes: Json[],
  more: Json = false,
  cursor: Json = null,
  count?: Json,
): Record {
  return {
    data: {
      repository: {
        pullRequest: {
          [connection]: {
            nodes,
            ...(count === undefined ? {} : { totalCount: count }),
            pageInfo: { hasNextPage: more, endCursor: cursor },
          },
        },
      },
    },
  };
}
export function reviewResponse(...pages: Record[]): Record {
  const result = response("comments", []),
    connections = object(
      field(field(field(result, "data"), "repository"), "pullRequest"),
    );
  for (const page of [
    response("reviews", [], false, null, 0n),
    response("reviewThreads", []),
    ...pages,
  ])
    Object.assign(
      connections,
      field(field(field(page, "data"), "repository"), "pullRequest"),
    );
  return result;
}
export function replies(values: Json[]) {
  const calls: string[][] = [];
  const gate = fakeGate({
    gh(args) {
      calls.push(args);
      if (!values.length) throw new Error("unexpected extra request");
      return encode(values.shift()!);
    },
  });
  return { gate, calls };
}
export function body(comment: Record): string {
  return string(comment.body);
}
export function changeState(
  comment: Record,
  kind: string,
  key: string,
  value: Json | undefined,
): Record {
  const match = /codex-review-state:v1 (\{.*?\}) -->/.exec(body(comment))!;
  // Fixture states only contain strings and booleans.
  const state = JSON.parse(match[1]) as Record;
  if (value === undefined) delete object(state[kind])[key];
  else object(state[kind])[key] = value;
  return { ...comment, body: body(comment).replace(match[1], encode(state)) };
}
