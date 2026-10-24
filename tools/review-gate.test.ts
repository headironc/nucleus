import { describe, expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import {
  chmodSync,
  existsSync,
  mkdtempSync,
  rmSync,
  readFileSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  BOT,
  REVIEWS,
  approvalBody,
  baselineFailures,
  checkFailures,
  decide,
  headChecks,
  ghTransport,
  historyFailures,
  requiredChecks,
  reviewContext,
  stackBinding,
  deliveryTarget,
} from "./review-gate";
import {
  array,
  encode,
  field,
  object,
  string,
  type Json,
  type Record,
} from "./review-json";
import {
  ACTIONS,
  BASE,
  CONTEXT,
  CHECK_CHANGES,
  HEAD,
  OLD,
  PASS,
  PROTECTIONS,
  QUEUE_RULES,
  RECORDED,
  REPO,
  REQUIRED,
  VIEW,
  approval,
  body,
  changeState,
  checkEvidence,
  exercise,
  fakeGate,
  replies,
  response,
  reviewResponse,
  rewritten,
  run,
  summary,
  viewResponse,
  nativeStackView,
  LOWER_VIEW,
  UPPER_VIEW,
  UPPER,
} from "./review-fixtures";

test("native layers bind trunk and direct parent without importing upper changes into lower review", () => {
  const lower = nativeStackView(),
    upper = nativeStackView(true);
  expect(stackBinding(REPO, 42n, lower)).toEqual(object(LOWER_VIEW.stack));
  expect(stackBinding(REPO, 43n, upper)).toEqual(object(UPPER_VIEW.stack));
  expect(deliveryTarget(UPPER_VIEW)).toBe("dev");
  const nodes = array(field(field(lower.stack, "entries"), "nodes"));
  object(field(nodes[1], "pullRequest")).headRefOid = OLD;
  expect(stackBinding(REPO, 42n, lower)).toEqual(object(LOWER_VIEW.stack));
  object(field(nodes[0], "pullRequest")).headRefOid = OLD;
  expect(() => stackBinding(REPO, 42n, lower)).toThrow("direct parent");
});

test("native membership rejects incomplete, reordered, foreign and non-dev stacks", () => {
  for (const mutate of [
    (value: Record) => {
      object(value.stack).size = 3n;
    },
    (value: Record) => {
      object(value.stack).baseRefName = "main";
    },
    (value: Record) => {
      object(field(field(value.stack, "entries"), "pageInfo")).hasNextPage =
        true;
    },
    (value: Record) => {
      object(value.stackEntry).position = 1n;
    },
    (value: Record) => {
      object(
        field(
          array(field(field(value.stack, "entries"), "nodes"))[0],
          "pullRequest",
        ),
      ).headRepository = { nameWithOwner: "elsewhere/repo" };
    },
    (value: Record) => {
      object(
        field(
          array(field(field(value.stack, "entries"), "nodes"))[0],
          "pullRequest",
        ),
      ).state = "CLOSED";
    },
  ]) {
    const value = nativeStackView(true);
    mutate(value);
    expect(() => stackBinding(REPO, 43n, value)).toThrow();
  }
});

test("a pinned upper diff baseline can precede the live parent head", () => {
  const raw = nativeStackView(true);
  raw.baseRefOid = OLD;
  object(
    field(array(field(field(raw.stack, "entries"), "nodes"))[1], "pullRequest"),
  ).baseRefOid = OLD;
  expect(stackBinding(REPO, 43n, raw)).toEqual(object(UPPER_VIEW.stack));
});

test("a pinned baseline cannot replace an unavailable live parent ref", () => {
  const raw = nativeStackView(true);
  const upper = array(field(field(raw.stack, "entries"), "nodes"))[1];
  object(field(upper, "pullRequest")).baseRef = null;
  expect(() => stackBinding(REPO, 43n, raw)).toThrow();
});

test("a merged prefix leaves the retargeted upper layer with its own current review identity", () => {
  const raw = nativeStackView(true);
  const nodes = array(field(field(raw.stack, "entries"), "nodes"));
  object(field(nodes[0], "pullRequest")).state = "MERGED";
  expect(() => stackBinding(REPO, 43n, raw)).toThrow("direct parent");
  raw.baseRefName = "dev";
  raw.baseRefOid = BASE;
  Object.assign(object(field(nodes[1], "pullRequest")), { baseRefName: "dev", baseRefOid: BASE });
  const stack = stackBinding(REPO, 43n, raw)!;
  expect(stack.parent).toBeNull();
  expect(stack.members).toEqual([{ id: "layer-2", number: 43n, position: 2n }]);
  const view = { ...UPPER_VIEW, baseRefName: "dev", baseRefOid: BASE, stack };
  expect(approvalBody(view)).not.toBe(approvalBody(UPPER_VIEW));
  const old = { ...CONTEXT, display_title: `context:43:${UPPER}:${HEAD}:codex/lower:stack:7:2:2:dev` };
  expect(reviewContext([old], REPO, 43n, view, true)).toBeNull();
  const current = { ...old, display_title: `context:43:${UPPER}:${BASE}:dev:stack:7:2:2:dev:notice` };
  expect(reviewContext([current], REPO, 43n, view, true)).toEqual({ base: BASE, recorded_at: CONTEXT.created_at });
  object(field(nodes[0], "pullRequest")).state = "CLOSED";
  expect(() => stackBinding(REPO, 43n, raw)).toThrow("closed without merging");
});

test("a compacted one-layer stack binds its current size and position", () => {
  const raw = nativeStackView();
  object(raw.stack).size = 1n;
  const entries = object(field(raw.stack, "entries"));
  entries.nodes = [array(entries.nodes)[0]];
  entries.totalCount = 1n;
  const stack = stackBinding(REPO, 42n, raw)!;
  expect(stack.size).toBe(1n);
  expect(array(stack.members)).toHaveLength(1);
  const view = { ...LOWER_VIEW, stack };
  const old = { ...CONTEXT, display_title: `${CONTEXT.display_title}:stack:7:1:2:dev` };
  expect(reviewContext([old], REPO, 42n, view, true)).toBeNull();
  expect(reviewContext([{ ...old, display_title: `${CONTEXT.display_title}:stack:7:1:1:dev` }], REPO, 42n, view, true)).not.toBeNull();
});

test("stack context cannot borrow a nonstack event or another membership", () => {
  const record = {
    ...CONTEXT,
    head_sha: HEAD,
    display_title: `context:43:${UPPER}:${HEAD}:codex/lower:stack:7:2:2:dev`,
  };
  expect(reviewContext([record], REPO, 43n, UPPER_VIEW, true)).toEqual({
    base: HEAD,
    recorded_at: CONTEXT.created_at,
  });
  expect(
    reviewContext(
      [{ ...record, display_title: `${record.display_title}:notice` }],
      REPO,
      43n,
      UPPER_VIEW,
      true,
    ),
  ).toEqual(reviewContext([record], REPO, 43n, UPPER_VIEW, true));
  expect(
    reviewContext(
      [{ ...CONTEXT, display_title: `${CONTEXT.display_title}:notice` }],
      REPO,
      42n,
      VIEW,
      true,
    ),
  ).toBeNull();
  for (const title of [
    record.display_title.replace(":stack:7:", ":stack:8:"),
    record.display_title.replace(":2:2:dev", ":1:2:dev"),
    record.display_title.split(":stack:")[0],
  ])
    expect(
      reviewContext(
        [{ ...record, display_title: title }],
        REPO,
        43n,
        UPPER_VIEW,
        true,
      ),
    ).toBeNull();
  expect(approvalBody(UPPER_VIEW)).not.toBe(
    approvalBody({
      ...UPPER_VIEW,
      stack: { ...object(UPPER_VIEW.stack), number: 8n },
    }),
  );
});

test("native stack members must have distinct entry and pull identities", () => {
  for (const duplicate of ["entry", "pull"]) {
    const value = nativeStackView(true);
    const [lower, upper] = array(
      field(field(value.stack, "entries"), "nodes"),
    );
    if (duplicate === "entry") object(upper).id = field(lower, "id");
    else
      object(field(upper, "pullRequest")).number = field(
        field(lower, "pullRequest"),
        "number",
      );
    expect(() => stackBinding(REPO, 43n, value)).toThrow("exact direct parent");
  }
});

test("upper layer assessment binds the pinned baseline and current parent event", () => {
  const record: Record = {
    ...CONTEXT,
    head_sha: UPPER,
    display_title: `context:43:${UPPER}:${HEAD}:codex/lower:stack:7:2:2:dev`,
  };
  const parentRecord = {
    ...CONTEXT,
    display_title: `${CONTEXT.display_title}:stack:7:1:2:dev:notice`,
  };
  const assess = ({
    records = [record],
    parents = [parentRecord] as Record[],
    view = UPPER_VIEW,
    parentView = LOWER_VIEW,
    relationship = "ahead",
    events = [] as Json[],
  } = {}) => {
    const gate = fakeGate({ queueEnabled: true });
    gate.evidence = () => ({
      comments: [summary("completed", "completed", UPPER)],
      inline: [],
      reviews: [],
      unresolved: 0n,
      checks: headChecks([run("docs"), run("rust"), run("website")], []),
      rules: QUEUE_RULES,
      events,
    });
    gate.contexts = (_repo, head) => (head === UPPER ? records : parents);
    gate.prView = () => parentView;
    gate.targetHead = () => BASE;
    gate.comparison = (_repo, base) => ({ merge_base: base, status: "ahead" });
    gate.relationship = () => relationship;
    return gate.assessment(REPO, 43n, view);
  };
  expect(assess()[1]).toEqual([]);
  expect(
    assess({
      records: [
        {
          ...record,
          display_title: string(record.display_title).replace(HEAD, OLD),
        },
      ],
      view: { ...UPPER_VIEW, baseRefOid: OLD },
    })[1],
  ).toEqual([]);
  expect(assess({ parents: [] })[1]).toContain(
    "review baseline is unrecorded or changed; synchronize the PR with its target",
  );
  expect(() =>
    assess({ parentView: { ...LOWER_VIEW, headRefOid: OLD } }),
  ).toThrow("stack parent changed");
  expect(() =>
    assess({
      parentView: {
        ...LOWER_VIEW,
        stack: { ...object(LOWER_VIEW.stack), number: 8n },
      },
    }),
  ).toThrow("stack parent changed");
  const late = { ...record, created_at: "2026-10-18T00:10:00Z" };
  expect(assess({ records: [late] })[1]).toContain(
    "code_review does not have a completion after the recorded PR event",
  );
  expect(
    assess({ parents: [{ ...parentRecord, created_at: late.created_at }] })[1],
  ).toContain(
    "code_review does not have a completion after the recorded PR event",
  );
  expect(assess({ relationship: "diverged" })[1]).toContain(
    "PR diff changed after review; establish coverage for the new inputs",
  );
  expect(
    assess({
      parents: [{ ...parentRecord, created_at: "2026-10-18T00:03:00Z" }],
      events: [
        {
          __typename: "BaseRefChangedEvent",
          createdAt: "2026-10-18T00:02:00Z",
        },
      ],
    })[1],
  ).toContain(
    "PR target changed after the recorded event; synchronize with its target",
  );
});

test("invalid live stack retains the candidate for withdrawal instead of hiding its old approval", () => {
  const raw = nativeStackView(true);
  object(raw.stack).size = 3n;
  const response = viewResponse(raw);
  object(field(field(response.data, "repository"), "pullRequest")).number = 43n;
  const gate = fakeGate({ gh: () => encode(response), queueEnabled: true });
  const view = gate.prView(REPO, 43n);
  expect(view.headRefOid).toBe(UPPER);
  expect(gate.target(view)).toBe("dev");
  expect(() => gate.assessment(REPO, 43n, view)).toThrow("two-layer");
});

test("closing the upper layer preserves the lower candidate for approval withdrawal", () => {
  const raw = nativeStackView();
  const upper = array(field(field(raw.stack, "entries"), "nodes"))[1];
  object(field(upper, "pullRequest")).state = "CLOSED";
  const gate = fakeGate({
    gh: () => encode(viewResponse(raw)),
    queueEnabled: true,
  });
  const view = gate.prView(REPO, 42n);
  expect(view.headRefOid).toBe(HEAD);
  expect(gate.target(view)).toBe("dev");
  expect(() => gate.assessment(REPO, 42n, view)).toThrow("closed without merging");
});

const decision = (
  comments: Json[],
  inline: Json[] = [],
  unresolved: Json = 0n,
  checks = PASS,
  reviews: Json[] = [],
) => decide(HEAD, comments, inline, unresolved, checks, REQUIRED, reviews);
const expectWrites = (
  result: ReturnType<typeof exercise>,
  methods: string[],
  status = 0,
) => {
  expect(result.result).toBe(status);
  expect(result.methods).toEqual(methods);
};
const event = { type: "ReopenedEvent", id: "reopen-1" };
const reopened = { ...VIEW, lifecycle: event };
const second = { ...VIEW, lifecycle: { ...event, id: "reopen-2" } };
const closed = { ...VIEW, state: "CLOSED" };
const root: Record = {
  id: 1n,
  user: { login: BOT },
  original_commit_id: HEAD,
  in_reply_to_id: null,
  created_at: "2026-10-18T00:04:00Z",
  updated_at: "2026-10-18T00:04:00Z",
};
const report: Record = {
  id: 19n,
  commit_id: HEAD,
  submitted_at: "2026-10-18T00:04:00Z",
  updated_at: "2026-10-18T00:04:00Z",
};
const formal: Record = {
  fullDatabaseId: "19",
  commit: { oid: HEAD },
  author: { __typename: "Bot", login: BOT.replace("[bot]", "") },
  submittedAt: report.submitted_at,
  updatedAt: report.updated_at,
};
const unavailable = (): never => {
  throw new Error("review readback unavailable");
};
const readEvidenceConnection = (
  gate: ReturnType<typeof fakeGate>,
  connection: string,
) =>
  connection === "timelineItems"
    ? gate.baselineEvents("owner", "repository", 42n, HEAD)
    : gate.reviewEvidence("owner", "repository", 42n);

describe("approval lifecycle and recovery", () => {
  test("dev approval waits for the required website producer", () => {
    const other = [run("rust"), run("docs")];
    for (const website of [
      [],
      [run("website", "in_progress", null)],
      [run("website", "completed", "failure")],
      [run("website", "completed", "cancelled")],
      [run("website", "completed", "success", 999n)],
    ]) {
      const result = exercise({
        queueTarget: BASE,
        snapshots: [{ checks: headChecks([...other, ...website], []) }],
      });
      expectWrites(result, []);
      expect(result.output).toContain("website (app 15368)");
    }
    expectWrites(exercise({ queueTarget: BASE }), ["POST"]);
  });

  test("website failure withdraws dev approval and a later success permits a new one", () => {
    const failed = exercise({
      queueTarget: BASE,
      approvals: [approval()],
      snapshots: [
        {
          checks: headChecks(
            [run("rust"), run("docs"), run("website", "completed", "failure")],
            [],
          ),
        },
      ],
    });
    expectWrites(failed, ["PUT"]);
    expect(failed.reviews[0].state).toBe("DISMISSED");
    const recovered = exercise({
      queueTarget: BASE,
      approvals: failed.reviews,
    });
    expectWrites(recovered, ["POST"]);
    expect(recovered.reviews.at(-1)!.state).toBe("APPROVED");
    expect(recovered.reviews.at(-1)!.id).not.toBe(failed.reviews[0].id);
  });

  for (const phase of ["before", "after"]) {
    test.each(CHECK_CHANGES)(
      `check facts ${phase} approval: $name`,
      ({ before, after, stable }) => {
        const first = checkEvidence(before.runs, before.statuses);
        const next = checkEvidence(after.runs, after.statuses);
        const result = exercise({
          queueTarget: BASE,
          snapshots: phase === "before" ? [first, next] : [first, first, next],
        });
        expectWrites(
          result,
          stable ? ["POST"] : phase === "before" ? [] : ["POST", "PUT"],
        );
        expect(result.output).toContain(
          stable
            ? "aaaaaaaaaaaa approved"
            : `changed ${phase === "before" ? "before" : "during"} approval`,
        );
      },
    );
  }

  test("check normalization preserves duplicate legacy facts and App identities", () => {
    const status = { context: "docs", state: "success" };
    const checks = headChecks(
      [run("docs"), run("docs", "completed", "failure", 999n)],
      [status, status, { ...status, state: "failure" }],
    );
    expect(checks).toHaveLength(5);
    expect(
      checks.filter((check) => field(check, "app_id") === null),
    ).toHaveLength(3);
    expect(checkFailures(checks, [["docs", ACTIONS]])).toEqual([]);
    expect(checkFailures(checks, [["docs", 999n]])).toEqual(["docs (app 999)"]);
    expect(checkFailures(checks, [["docs", null]])).toEqual(["docs"]);
  });

  test("closed approval is inactive without a dismissal write", () => {
    const result = exercise({
      views: [closed],
      approvals: [approval()],
      snapshots: [new Error("must not read")],
    });
    expectWrites(result, []);
    expect(result.reviews[0].state).toBe("APPROVED");
    expect(result.evidenceReads).toBe(0);
    expect(result.output).toContain("inactive");
  });
  test("sweep replaces old approval after a missed reopening", () => {
    const result = exercise({ views: [reopened], approvals: [approval()] });
    expectWrites(result, ["PUT", "POST"]);
    expect(result.reviews.map((review) => review.state)).toEqual([
      "DISMISSED",
      "APPROVED",
    ]);
    expect(result.reviews.at(-1)!.body).toBe(approval(event).body);
    expect(
      result.calls.findIndex(
        (args) => args.at(-1) === `repos/${REPO}/pulls/42/reviews/7`,
      ),
    ).toBeLessThan(result.calls.indexOf(result.writes[1]));
  });
  test("delayed lifecycle event reuses approval for current reopening", () => {
    const result = exercise({
      views: [reopened],
      approvals: [approval(event)],
    });
    expectWrites(result, []);
    expect(result.output).toContain("already approved");
  });
  test("legacy approval without event binding is refreshed", () => {
    const result = exercise({
      approvals: [
        { ...approval(), body: string(approval().body).split("\n")[0] },
      ],
    });
    expectWrites(result, ["PUT", "POST"]);
    expect(result.reviews.at(-1)!.body).toBe(approval().body);
  });
  test("reopening changes invalidate identical refs before approval", () => {
    const result = exercise({ views: [reopened, second] });
    expectWrites(result, []);
    expect(result.output).not.toContain("aaaaaaaaaaaa approved");
  });
  test("reopening after approval invalidates its original event", () => {
    const result = exercise({ views: [reopened, reopened, second] });
    expectWrites(result, ["POST", "PUT"]);
    expect(result.reviews[0].body).toBe(approval(event).body);
    expect(result.reviews[0].state).toBe("DISMISSED");
    expect(result.output).not.toContain("aaaaaaaaaaaa approved");
  });
  test("same second reopenings have distinct approval bindings", () => {
    const first = { ...event, createdAt: "2026-10-18T00:09:00Z" },
      next = { ...first, id: "reopen-2" };
    const result = exercise({
      views: [{ ...VIEW, lifecycle: next }],
      approvals: [approval(first)],
    });
    expectWrites(result, ["PUT", "POST"]);
    expect(result.reviews.at(-1)!.body).toBe(approval(next).body);
  });
  test("latest event read does not traverse older history", () => {
    const value = viewResponse(reopened);
    object(
      field(
        field(field(field(value, "data"), "repository"), "pullRequest"),
        "timelineItems",
      ),
    ).pageInfo = { hasNextPage: false, hasPreviousPage: true };
    const { gate, calls } = replies([value]);
    expect(gate.prView(REPO, 42n)).toEqual(reopened);
    expect(calls).toHaveLength(1);
  });
  test("incomplete or contradictory lifecycle never starts a write", () => {
    const closedEvent = { __typename: "ClosedEvent", id: "closed-1" };
    const timelines: Json[] = [
      null,
      {},
      { nodes: [], pageInfo: null },
      { nodes: [], pageInfo: { hasNextPage: true } },
      { nodes: [], pageInfo: {} },
      { nodes: [closedEvent, closedEvent], pageInfo: { hasNextPage: false } },
      ...[
        closedEvent,
        null,
        { __typename: "ReopenedEvent" },
        { __typename: "ReopenedEvent", id: "" },
        { __typename: "ReopenedEvent", id: "reopen-1\nReopen event: none" },
        { __typename: "IssueComment", id: "unrelated" },
      ].map((item) => ({ nodes: [item], pageInfo: { hasNextPage: false } })),
    ];
    for (const timeline of timelines) {
      const value = viewResponse(VIEW),
        calls: string[][] = [];
      object(
        field(field(field(value, "data"), "repository"), "pullRequest"),
      ).timelineItems = timeline;
      const gate = fakeGate({
        gh(args) {
          calls.push(args);
          return encode(value);
        },
      });
      gate.evidence = () => {
        throw new Error("must not read");
      };
      expect(gate.main(42n)).toBe(1);
      expect(
        calls.every((args) => !args.includes("POST") && !args.includes("PUT")),
      ).toBe(true);
    }
  });
  test("partial or mistargeted PR view is not a candidate", () => {
    const values = [viewResponse(VIEW), viewResponse(VIEW), viewResponse(VIEW)];
    values[0].errors = [{ type: "FORBIDDEN" }];
    object(field(field(values[1], "data"), "repository")).nameWithOwner =
      "other/repository";
    object(
      field(field(field(values[2], "data"), "repository"), "pullRequest"),
    ).number = 43n;
    for (const value of values)
      expect(() => replies([value]).gate.prView(REPO, 42n)).toThrow();
  });
  test("approval is bound to verified head and base", () => {
    const result = exercise();
    expectWrites(result, ["POST"]);
    expect(result.writes[0]).toContain(`commit_id=${HEAD}`);
    expect(
      result.writes[0].some((arg) => arg.includes(BASE) && arg.includes("dev")),
    ).toBe(true);
    expect(approvalBody(VIEW)).toBe(string(approval().body));
  });
  test("old approval scope is replaced and current scope reused", () => {
    const old = {
      ...approval(),
      body:
        `Review gate: both Codex reviews completed on ${HEAD} against dev at ${BASE}, ` +
        "with no new findings, all threads resolved and required checks passing.\nReopen event: none",
    };
    expect(fakeGate().matchesCandidate(old, VIEW)).toBe(false);
    expectWrites(exercise({ approvals: [old] }), ["PUT", "POST"]);
    expectWrites(exercise({ approvals: [approval()] }), []);
  });
  test("missing context never creates approval", () =>
    expectWrites(exercise({ recorded: false }), []));
  test("base change during evaluation withdraws existing approval", () =>
    expectWrites(
      exercise({
        views: [VIEW, { ...VIEW, baseRefOid: OLD }],
        approvals: [approval()],
      }),
      ["PUT"],
    ));
  test("change during approval withdraws new approval", () =>
    expectWrites(
      exercise({ views: [VIEW, VIEW, { ...VIEW, baseRefName: "main" }] }),
      ["POST", "PUT"],
    ));
  test("changed review evidence prevents or withdraws approval", () => {
    for (const changed of [
      { comments: [summary("running", "completed")] },
      { comments: [] },
      { unresolved: 1n },
      { reviews: [{ id: 19n, commit_id: HEAD }] },
      {
        checks: headChecks(
          [run("rust", "completed", "failure"), run("docs")],
          [],
        ),
      },
    ] as Record[]) {
      expectWrites(
        exercise({ snapshots: [{}, changed], approvals: [approval()] }),
        ["PUT"],
      );
      expectWrites(exercise({ snapshots: [{}, {}, changed] }), ["POST", "PUT"]);
    }
  });
  test("forged summary withdraws existing approval", () =>
    expectWrites(
      exercise({
        snapshots: [
          {
            comments: [
              { ...summary(), editor: { __typename: "User", login: "writer" } },
            ],
          },
        ],
        approvals: [approval()],
      }),
      ["PUT"],
    ));
  test("draft PRs lose gate approval", () =>
    expectWrites(
      exercise({
        views: [{ ...VIEW, isDraft: true }],
        approvals: [approval()],
      }),
      ["PUT"],
    ));
  test("merged PRs keep historical approvals", () => {
    const result = exercise({
      views: [{ ...VIEW, state: "MERGED" }],
      approvals: [approval()],
      snapshots: [new Error("must not read")],
    });
    expectWrites(result, []);
    expect(result.output).toContain("historical");
  });
  test("closed PR preserves all reviews without reading evidence", () => {
    const approvals = [
      approval(),
      { ...approval(), id: 10n, user: { login: "writer", type: "User" } },
      { ...approval(), id: 11n, user: { login: "other[bot]", type: "Bot" } },
    ];
    const result = exercise({
      views: [closed],
      approvals,
      snapshots: [new Error("must not read")],
    });
    expectWrites(result, []);
    expect(result.reviews.map((item) => item.state)).toEqual([
      "APPROVED",
      "APPROVED",
      "APPROVED",
    ]);
    expect(result.evidenceReads).toBe(0);
    expect(result.calls).toHaveLength(1);
  });
  test("closed PR without approval never creates one", () => {
    const result = exercise({
      views: [closed],
      snapshots: [new Error("must not read")],
    });
    expectWrites(result, []);
    expect(result.evidenceReads).toBe(0);
  });
  test("reopening after inactive close creates new approval", () => {
    const inactive = exercise({ views: [closed], approvals: [approval()] });
    expectWrites(inactive, []);
    const result = exercise({ views: [reopened], approvals: inactive.reviews });
    expectWrites(result, ["PUT", "POST"]);
    expect(result.reviews.map((item) => [item.id, item.state])).toEqual([
      [7n, "DISMISSED"],
      [8n, "APPROVED"],
    ]);
    expect(result.evidenceReads).toBe(3);
  });
  test("reopening confirms lost dismissal response before approval", () => {
    for (const dismissError of [false, true]) {
      const result = exercise({
        views: [reopened],
        approvals: [approval()],
        dismissError,
        dismissEffect: true,
      });
      expectWrites(result, ["PUT", "POST"]);
      expect(result.output).not.toContain("already approved");
      expect(
        result.calls.findIndex(
          (args) => args.at(-1) === `repos/${REPO}/pulls/42/reviews/7`,
        ),
      ).toBeLessThan(result.calls.indexOf(result.writes[1]));
    }
  });
  test("reopening with invalid evidence only withdraws", () => {
    for (const comments of [
      [],
      [summary("running", "completed")],
      [{ ...summary(), editor: { __typename: "User", login: "writer" } }],
    ]) {
      const result = exercise({
        views: [reopened],
        approvals: [approval()],
        snapshots: [{ comments }],
      });
      expectWrites(result, ["PUT"]);
      expect(result.reviews[0].state).toBe("DISMISSED");
    }
  });
  test("reopening withdrawal failure does not retry or approve", () => {
    for (const dismissError of [false, true])
      expectWrites(
        exercise({
          views: [reopened],
          approvals: [approval()],
          dismissError,
          dismissEffect: false,
        }),
        ["PUT"],
        1,
      );
  });
  test("reopening readback outage stops before approving", () => {
    const result = exercise({
      views: [reopened],
      approvals: [approval()],
      reviewRead: unavailable,
    });
    expectWrites(result, ["PUT"], 1);
    expect(result.output).not.toContain("dismissed approval");
  });
  test("reopening cleanup preserves already bound concurrent approval", () => {
    const result = exercise({
      views: [reopened],
      approvals: [approval(), { ...approval(event), id: 9n }],
    });
    expectWrites(result, ["PUT"]);
    expect(result.reviews.map((item) => [item.id, item.state])).toEqual([
      [7n, "DISMISSED"],
      [9n, "APPROVED"],
    ]);
    expect(result.output).toContain("already approved");
  });
  test("reopening dismisses only app own outdated approvals", () => {
    const result = exercise({
      views: [reopened],
      approvals: [
        approval(),
        { ...approval(), id: 10n, user: { login: "writer", type: "User" } },
        { ...approval(), id: 11n, user: { login: "other[bot]", type: "Bot" } },
        { ...approval(), id: 12n, state: "DISMISSED" },
      ],
    });
    expectWrites(result, ["PUT", "POST"]);
    expect(result.writes[0][3]).toBe(
      `repos/${REPO}/pulls/42/reviews/7/dismissals`,
    );
    expect(result.reviews.map((item) => item.state)).toEqual([
      "DISMISSED",
      "APPROVED",
      "APPROVED",
      "DISMISSED",
      "APPROVED",
    ]);
  });
  test("closure after reopening cleanup preserves unconfirmed failure", () => {
    const result = exercise({
      views: [reopened, reopened, reopened, { ...reopened, state: "CLOSED" }],
      approvals: [approval()],
      dismissError: true,
    });
    expectWrites(result, ["PUT"], 1);
    expect(result.output).not.toContain("dismissed approval");
  });
  test("closure stops second outdated approval dismissal", () => {
    const result = exercise({
      views: [reopened, reopened, reopened, { ...reopened, state: "CLOSED" }],
      approvals: [approval(), { ...approval(), id: 9n }],
    });
    expectWrites(result, ["PUT"]);
    expect(result.reviews.map((item) => item.state)).toEqual([
      "DISMISSED",
      "APPROVED",
    ]);
  });
  test("event change during cleanup prevents approval for either stale view", () => {
    const result = exercise({
      views: [reopened, reopened, reopened, second],
      approvals: [approval()],
    });
    expectWrites(result, ["PUT"]);
    expect(result.output).not.toContain("aaaaaaaaaaaa approved");
  });
  test("read or parse failure withdraws existing approval", () => {
    for (const error of [
      new Error("API unavailable"),
      new Error("malformed review evidence"),
    ])
      expectWrites(
        exercise({ snapshots: [error], approvals: [approval()] }),
        ["PUT"],
        1,
      );
  });
  test("unknown approval outcome is confirmed without retry", () =>
    expectWrites(exercise({ postError: true }), ["POST"]));
  test("unconfirmed approval is reported without retry", () => {
    for (const postError of [false, true]) {
      const result = exercise({ postError, postEffect: false });
      expectWrites(result, ["POST"], 1);
      expect(result.output).not.toContain("aaaaaaaaaaaa approved");
    }
  });
  test("closure does not erase unknown approval outcome", () => {
    for (const postEffect of [false, true]) {
      const result = exercise({
        views: [VIEW, VIEW, closed],
        postError: true,
        postEffect,
        ...(postEffect ? { reviewRead: unavailable } : {}),
      });
      expectWrites(result, ["POST"], 1);
      expect(result.output).not.toContain("aaaaaaaaaaaa approved");
    }
  });
  test("reused approval is read back before reporting success", () => {
    const result = exercise({
      approvals: [approval()],
      reviewRead: (review) => ({ ...review, state: "DISMISSED" }),
    });
    expectWrites(result, ["PUT"], 1);
    expect(result.output).not.toContain("already approved");
  });
  test("approval readback must match exact identity and candidate", () => {
    for (const changes of [
      { id: 99n },
      { commit_id: OLD },
      { body: "unbound approval" },
      { user: { login: "other[bot]", type: "Bot" } },
      { user: { login: "cladiron[bot]", type: "User" } },
    ] as Record[])
      expectWrites(
        exercise({ reviewRead: (review) => ({ ...review, ...changes }) }),
        ["POST", "PUT"],
        1,
      );
  });
  test("wrong baseline approval is not reused", () =>
    expectWrites(
      exercise({
        approvals: [
          { ...approval(), body: string(approval().body).replace(BASE, OLD) },
        ],
      }),
      ["PUT", "POST"],
    ));
  test("valid approval is reused without another write", () => {
    const result = exercise({ approvals: [approval()] });
    expectWrites(result, []);
    expect(result.output).toContain("already approved");
  });
  test("queue target advance reuses original review without writes", () => {
    for (const baseRefOid of [BASE, OLD]) {
      const result = exercise({
        views: [{ ...VIEW, baseRefOid }],
        approvals: [approval()],
        queueTarget: OLD,
      });
      expectWrites(result, []);
      expect(result.output).toContain("already approved");
      expect(result.reviews).toEqual([approval()]);
    }
  });
  test("queue approval after advance still names reviewed baseline", () => {
    const result = exercise({
      views: [{ ...VIEW, baseRefOid: OLD }],
      queueTarget: OLD,
    });
    expectWrites(result, ["POST"]);
    expect(result.reviews.at(-1)!.body).toBe(approval().body);
  });
  test("changed PR diff or rewritten target cannot reuse queue review", () => {
    for (const changes of [
      { mergeBases: { [`${OLD}:${HEAD}`]: "d".repeat(40) } },
      { targetRelationship: "diverged" },
      { targetRelationship: "behind" },
    ]) {
      const result = exercise({
        approvals: [approval()],
        queueTarget: OLD,
        ...changes,
      });
      expectWrites(result, ["PUT"]);
      expect(result.output).not.toContain("already approved");
    }
  });
  test("ambiguous review baselines cannot select convenient context", () =>
    expectWrites(
      exercise({
        approvals: [approval()],
        queueTarget: OLD,
        records: [
          CONTEXT,
          { ...CONTEXT, display_title: `context:42:${HEAD}:${OLD}:dev` },
        ],
      }),
      ["PUT"],
      1,
    ));
  test("reopening after target advance replaces only event binding", () => {
    const result = exercise({
      views: [{ ...reopened, baseRefOid: OLD }],
      approvals: [approval()],
      queueTarget: OLD,
    });
    expectWrites(result, ["PUT", "POST"]);
    expect(result.reviews.at(-1)!.body).toBe(approval(event).body);
  });
  test("queue reuse rejects changed head target findings and protection", () => {
    for (const changes of [
      { views: [{ ...VIEW, headRefOid: OLD }] },
      { views: [{ ...VIEW, baseRefName: "main" }] },
      {
        snapshots: [
          {
            events: [
              {
                __typename: "BaseRefChangedEvent",
                createdAt: "2026-10-18T00:01:00Z",
              },
            ],
          },
        ],
      },
      {
        snapshots: [
          {
            inline: [
              { id: 9n, user: { login: BOT }, original_commit_id: HEAD },
            ],
          },
        ],
      },
      { snapshots: [{ reviews: [{ id: 9n, commit_id: HEAD }] }] },
      { snapshots: [{ unresolved: 1n }] },
      { snapshots: [{ checks: [] }] },
      { snapshots: [{ rules: PROTECTIONS }] },
    ]) {
      const result = exercise({
        approvals: [approval()],
        queueTarget: OLD,
        ...changes,
      });
      expect(result.methods).toEqual(["PUT"]);
      expect(result.output).not.toContain("already approved");
    }
  });
  test("queue target outage or incomplete comparison withdraws approval", () => {
    for (const target of [
      new Error("target unavailable"),
      {},
      { ref: "refs/heads/main", object: { sha: OLD } },
      { ref: "refs/heads/dev", object: { sha: null } },
    ] as (Json | Error)[])
      expectWrites(
        exercise({
          approvals: [approval()],
          queueTarget: OLD,
          targetReads: [target],
        }),
        ["PUT"],
        1,
      );
    for (const ancestor of [null, "", "dev", "a".repeat(39)])
      expectWrites(
        exercise({
          approvals: [approval()],
          queueTarget: OLD,
          mergeBases: { [`${OLD}:${HEAD}`]: ancestor },
        }),
        ["PUT"],
        1,
      );
  });
  test("queue target drift never completes original evaluation", () => {
    for (const [targets, approvals, methods] of [
      [[BASE, OLD], [approval()], ["PUT"]],
      [[BASE, BASE, OLD], [], ["POST", "PUT"]],
      [[BASE, BASE, OLD], [approval()], ["PUT"]],
      [[BASE, BASE, BASE, OLD], [], ["POST", "PUT"]],
    ] as [string[], Record[], string[]][]) {
      const result = exercise({
        approvals,
        queueTarget: OLD,
        targetReads: targets,
      });
      expectWrites(result, methods);
      expect(result.reviews.at(-1)!.state).toBe("DISMISSED");
    }
  });
  test("queue reopening recovers unknown writes at original baseline", () => {
    const result = exercise({
      views: [{ ...reopened, baseRefOid: OLD }],
      approvals: [approval()],
      queueTarget: OLD,
      dismissError: true,
      dismissEffect: true,
      postError: true,
    });
    expectWrites(result, ["PUT", "POST"]);
    expect(result.reviews.at(-1)!.body).toBe(approval(event).body);
    expectWrites(
      exercise({
        views: [{ ...reopened, baseRefOid: OLD }],
        approvals: [approval()],
        queueTarget: OLD,
        dismissError: true,
        dismissEffect: false,
      }),
      ["PUT"],
      1,
    );
  });
  test("queue cannot reuse dismissed or stale approval", () => {
    for (const prior of [
      { ...approval(), state: "DISMISSED" },
      { ...approval(), submitted_at: "2026-10-18T00:01:00Z" },
    ]) {
      const result = exercise({ approvals: [prior], queueTarget: OLD });
      expectWrites(result, ["POST"]);
      expect(result.output).not.toContain("already approved");
    }
  });
  test("reuse does not run post write evidence scan", () => {
    const result = exercise({
      approvals: [approval()],
      snapshots: [{}, {}, new Error("unnecessary post-write scan")],
    });
    expectWrites(result, []);
    expect(result.evidenceReads).toBe(2);
    expect(result.output).toContain("already approved");
  });
  test("summary update after approval creates one fresh approval", () => {
    const updated = {
      ...summary(),
      updatedAt: "2026-10-18T00:07:00Z",
      lastEditedAt: "2026-10-18T00:07:00Z",
    };
    const result = exercise({
      approvals: [approval()],
      snapshots: [{ comments: [summary(), updated] }],
    });
    expectWrites(result, ["POST"]);
    expect(result.output).not.toContain("already approved");
    const renewed = exercise({
      approvals: [
        approval(),
        { ...approval(), id: 8n, submitted_at: "2026-10-18T00:10:00Z" },
      ],
      snapshots: [{ comments: [updated] }],
    });
    expectWrites(renewed, []);
    expect(renewed.evidenceReads).toBe(2);
  });
  test("summary time equal to approval is current", () =>
    expectWrites(
      exercise({
        approvals: [approval()],
        snapshots: [
          { comments: [{ ...summary(), updatedAt: "2026-10-18T00:06:00Z" }] },
        ],
      }),
      [],
    ));
  test("other heads and unrelated comments do not expire approval", () => {
    const old = {
      ...summary("completed", "completed", OLD),
      updatedAt: "2026-10-18T00:20:00Z",
    };
    expectWrites(
      exercise({
        approvals: [approval()],
        snapshots: [
          {
            comments: [
              summary(),
              old,
              { ...old, body: "Unrelated provider comment", updatedAt: null },
            ],
          },
        ],
      }),
      [],
    );
  });
  test("unknown summary time prevents approval and clears existing approval", () => {
    for (const updatedAt of [null, "invalid", "2026-10-18T00:05:00"])
      expectWrites(
        exercise({
          approvals: [approval()],
          snapshots: [{ comments: [{ ...summary(), updatedAt }] }],
        }),
        ["PUT"],
        1,
      );
  });
  test("unknown existing approval time is not reused", () => {
    for (const submitted_at of [null, "invalid", "2026-10-18T00:06:00"])
      expectWrites(exercise({ approvals: [{ ...approval(), submitted_at }] }), [
        "POST",
      ]);
  });
  test("approval time is checked again on exact readback", () => {
    for (const approvals of [[], [approval()]])
      for (const submitted_at of [null, "invalid", "2026-10-18T00:04:00Z"]) {
        const result = exercise({
          approvals,
          reviewRead: (review) => ({ ...review, submitted_at }),
        });
        expectWrites(result, approvals.length ? ["PUT"] : ["POST", "PUT"], 1);
        expect(result.output).not.toContain("aaaaaaaaaaaa approved");
        expect(result.output).not.toContain("already approved");
      }
  });
  test("freshness compares instants across timezone offsets", () =>
    expectWrites(
      exercise({
        approvals: [
          { ...approval(), submitted_at: "2026-10-18T08:06:00+08:00" },
        ],
        snapshots: [
          { comments: [{ ...summary(), updatedAt: "2026-10-18T00:07:00Z" }] },
        ],
      }),
      ["POST"],
    ));
  test("lost dismissal response is confirmed without retry", () =>
    expectWrites(
      exercise({
        recorded: false,
        approvals: [approval()],
        dismissError: true,
        dismissEffect: true,
      }),
      ["PUT"],
    ));
  test("success response without dismissal is not success", () => {
    const result = exercise({
      recorded: false,
      approvals: [approval()],
      dismissEffect: false,
    });
    expectWrites(result, ["PUT"], 1);
    expect(result.output).not.toContain("dismissed approval");
  });
  test("dismissal readback must match review and app", () => {
    for (const changes of [
      { id: 99n },
      { user: { login: "other[bot]", type: "Bot" } },
    ] as Record[]) {
      const result = exercise({
        recorded: false,
        approvals: [approval()],
        reviewRead: (review) => ({ ...review, ...changes }),
      });
      expectWrites(result, ["PUT"], 1);
      expect(result.output).not.toContain("dismissed approval");
    }
  });
  test("readback outage does not retry writes or claim confirmation", () => {
    for (const approvals of [[], [approval()]]) {
      const result = exercise({
        approvals,
        recorded: !approvals.length,
        reviewRead: unavailable,
      });
      expectWrites(result, approvals.length ? ["PUT"] : ["POST", "PUT"], 1);
      expect(result.output).not.toContain("aaaaaaaaaaaa approved");
      expect(result.output).not.toContain("dismissed approval");
    }
  });
  test("closing during approval stops further writes", () => {
    for (const state of ["CLOSED", "MERGED"])
      expectWrites(exercise({ views: [VIEW, VIEW, { ...VIEW, state }] }), [
        "POST",
      ]);
  });
  test("closing during failed withdrawal is not retried", () => {
    for (const state of ["CLOSED", "MERGED"])
      expectWrites(
        exercise({
          views: [VIEW, VIEW, VIEW, { ...VIEW, state }],
          recorded: false,
          approvals: [approval()],
          dismissError: true,
        }),
        ["PUT"],
        state === "CLOSED" ? 1 : 0,
      );
  });
  test("closing before withdrawal stops new write", () => {
    for (const state of ["CLOSED", "MERGED"])
      expectWrites(
        exercise({
          views: [VIEW, VIEW, { ...VIEW, state }],
          recorded: false,
          approvals: [approval()],
        }),
        [],
      );
  });
  test("closing after dismissal uses final lifecycle", () => {
    for (const state of ["CLOSED", "MERGED"]) {
      const result = exercise({
        views: [VIEW, VIEW, VIEW, { ...VIEW, state }],
        recorded: false,
        approvals: [approval()],
      });
      expectWrites(result, ["PUT"]);
      expect(result.output).toContain(
        state === "MERGED" ? "historical" : "inactive",
      );
    }
  });
  test("invalid candidate binding never creates approval", () => {
    for (const changes of [
      { headRefOid: HEAD.slice(0, 12) },
      { baseRefOid: "" },
      { isDraft: null },
    ] as Record[])
      expectWrites(exercise({ views: [{ ...VIEW, ...changes }] }), [], 1);
  });
  test("unknown PR lifecycle is an error", () =>
    expectWrites(
      exercise({
        views: [{ ...VIEW, state: "UNKNOWN" }],
        approvals: [approval()],
      }),
      [],
      1,
    ));
  test("withdrawal failure is reported without retry", () =>
    expectWrites(
      exercise({
        recorded: false,
        approvals: [approval()],
        dismissError: true,
      }),
      ["PUT"],
      1,
    ));
});

describe("sweep and GraphQL pagination", () => {
  for (const stage of ["inline", "rules", "pagination"])
    for (const reuse of [false, true])
      test(`a returning head during ${stage} prevents ${reuse ? "approval reuse" : "new approval"}`, () => {
        const calls: string[][] = [],
          logs: string[] = [],
          reviews = reuse ? [approval()] : [],
          events: Record[] = [];
        let round = 0,
          head = HEAD,
          injected = 0;
        const rewrite = () => {
          events.push({
            __typename: "HeadRefForcePushedEvent",
            createdAt: "2026-10-18T00:01:00Z",
            beforeCommit: { oid: head },
            afterCommit: { oid: OLD },
          });
          head = OLD;
          events.push({
            __typename: "HeadRefForcePushedEvent",
            createdAt: "2026-10-18T00:02:00Z",
            beforeCommit: { oid: head },
            afterCommit: { oid: HEAD },
          });
          head = HEAD;
          injected++;
        };
        const gate = fakeGate({
          log: (line) => logs.push(line),
          error: (line) => logs.push(line),
          actions: () =>
            encode([{ total_count: 1n, workflow_runs: [CONTEXT] }]),
          gh(args) {
            calls.push(args);
            if (args[1] === "graphql") {
              const query = args[3];
              if (query.includes("timelineItems(last:1"))
                return encode(viewResponse({ ...VIEW, headRefOid: head }));
              if (query.includes("comments(first:100")) {
                const later = args.includes("commentsAfter=next");
                if (!later) round++;
                if (round === 2 && stage === "pagination" && later) rewrite();
                const more = round === 2 && stage === "pagination" && !later;
                return encode(
                  reviewResponse(
                    response(
                      "comments",
                      more ? [] : [summary()],
                      more,
                      more ? "next" : null,
                    ),
                    response("timelineItems", events),
                  ),
                );
              }
              if (query.includes("timelineItems(first:100"))
                return encode(response("timelineItems", events));
              throw new Error("unexpected GraphQL request");
            }
            if (args.includes("POST")) {
              reviews.push({
                ...approval(),
                id: 8n,
                submitted_at: "2026-10-18T00:10:00Z",
              });
              return "{}";
            }
            if (args.includes("PUT")) {
              const id = BigInt(args[3].split("/").at(-2)!);
              reviews.find((review) => review.id === id)!.state = "DISMISSED";
              return "{}";
            }
            const path = args.at(-1)!;
            if (path.includes("/compare/")) {
              const base = path.split("/compare/")[1].split("...")[0];
              return encode({
                base_commit: { sha: base },
                merge_base_commit: { sha: BASE },
                status: base === HEAD ? "identical" : "diverged",
              });
            }
            if (path.endsWith("/check-runs?per_page=100"))
              return encode([{ check_runs: [run("rust"), run("docs")] }]);
            if (path.endsWith("/statuses?per_page=100")) return "[[]]";
            if (path === `repos/${REPO}/pulls/42/comments`) {
              if (round === 2 && stage === "inline") rewrite();
              return "[[]]";
            }
            if (path === `repos/${REPO}/rules/branches/dev?per_page=100`) {
              if (round === 2 && stage === "rules") rewrite();
              return encode([
                [
                  ...PROTECTIONS,
                  {
                    type: "required_status_checks",
                    parameters: {
                      required_status_checks: REQUIRED.map(
                        ([context, integration_id]) => ({
                          context,
                          integration_id,
                        }),
                      ),
                    },
                  },
                ],
              ]);
            }
            if (path === `repos/${REPO}/pulls/42/reviews`)
              return encode([reviews]);
            if (path.startsWith(`repos/${REPO}/pulls/42/reviews/`))
              return encode(
                reviews.find(
                  (review) => String(review.id) === path.split("/").at(-1),
                )!,
              );
            throw new Error("unexpected REST request");
          },
        });
        expect(gate.main(42n)).toBe(0);
        expect(injected).toBe(1);
        expect(head).toBe(HEAD);
        expect(calls.filter((args) => args.includes("POST"))).toHaveLength(0);
        expect(calls.filter((args) => args.includes("PUT"))).toHaveLength(
          reuse ? 1 : 0,
        );
        expect(reviews.map((review) => review.state)).toEqual(
          reuse ? ["DISMISSED"] : [],
        );
        expect(logs.join("\n")).toContain("changed before approval");
        expect(logs.join("\n")).not.toContain("already approved");
      });
  test("batched reviews and final history preserve complete evidence and digest bytes", () => {
    const comment = {
        ...summary(),
        id: "summary-1",
        extra: { unicode: "证据", integer: 9007199254740993n, decimal: 1 },
      },
      event = {
        __typename: "BaseRefChangedEvent",
        createdAt: "2026-10-17T00:00:00Z",
      },
      statuses = [
        { context: "custom", state: "success" },
        { context: "custom", state: "failure" },
      ];
    const batch = reviewResponse(
      response("comments", [comment]),
      response(
        "reviews",
        [
          { ...formal, fullDatabaseId: "9007199254740993" },
          formal,
          {
            ...formal,
            fullDatabaseId: "21",
            author: { __typename: "User", login: "writer" },
          },
          { ...formal, fullDatabaseId: "22", submittedAt: null },
        ],
        false,
        null,
        4n,
      ),
      response("reviewThreads", [{ isResolved: true }, { isResolved: false }]),
    );
    const calls: string[][] = [],
      gate = fakeGate({
        gh(args) {
          calls.push(args);
          if (args[1] === "graphql")
            return encode(
              args[3].includes("timelineItems(first:100")
                ? response("timelineItems", [event])
                : batch,
            );
          const path = args.at(-1)!;
          if (path.endsWith("/check-runs?per_page=100"))
            return encode([{ check_runs: [run("rust"), run("docs")] }]);
          if (path.endsWith("/statuses?per_page=100"))
            return encode([statuses]);
          if (path === `repos/${REPO}/pulls/42/comments`)
            return encode([[root]]);
          if (path === `repos/${REPO}/rules/branches/dev?per_page=100`)
            return encode([PROTECTIONS]);
          throw new Error("unexpected evidence request");
        },
      });
    const expected = {
      comments: [comment],
      inline: [root],
      reviews: [report, { ...report, id: 9007199254740993n }],
      unresolved: 1n,
      checks: [{ name: "custom", app_id: null, passed: true }, ...PASS],
      rules: PROTECTIONS,
      events: [event],
    };
    const evidence = gate.evidence(REPO, 42n, VIEW);
    expect(evidence).toEqual(expected);
    expect(encode(evidence)).toBe(encode(expected));
    const queries = calls.filter((args) => args[1] === "graphql");
    expect(queries).toHaveLength(2);
    expect(queries[0]).toEqual([
      "api",
      "graphql",
      "-f",
      expect.any(String),
      "-f",
      "owner=owner",
      "-f",
      "name=repository",
      "-F",
      "number=42",
    ]);
    for (const name of ["comments", "reviews", "reviewThreads"])
      expect(queries[0][3]).toContain(`${name}(first:100,after:$${name}After`);
    expect(queries[0][3]).not.toContain("timelineItems");
    expect(queries[1][3]).toContain("timelineItems(first:100,after:$after");
    expect(queries[0][3]).toContain(
      "lastEditedAt author{__typename login} editor{__typename login}",
    );
    expect(queries[0][3]).toContain(
      "totalCount nodes{fullDatabaseId submittedAt updatedAt author{__typename login} commit{oid}}",
    );
    expect(queries[1][3]).toContain(
      "[BASE_REF_CHANGED_EVENT,HEAD_REF_FORCE_PUSHED_EVENT,HEAD_REF_DELETED_EVENT,HEAD_REF_RESTORED_EVENT]",
    );
  });
  test("mixed page lengths retain independent cursors and complete each connection once", () => {
    const comment = summary();
    const { gate, calls } = replies([
      reviewResponse(
        response("comments", [comment]),
        response(
          "reviews",
          [{ ...formal, fullDatabaseId: "20" }],
          true,
          "page-1",
          2n,
        ),
        response("reviewThreads", [{ isResolved: false }], true, "page-1"),
      ),
      reviewResponse(
        response("comments", [summary("running")]),
        response("reviews", [formal], false, null, 2n),
        response("reviewThreads", [], true, "thread-2"),
      ),
      reviewResponse(
        response("reviews", [formal], false, null, 2n),
        response("reviewThreads", [
          { isResolved: true },
          { isResolved: false },
        ]),
      ),
    ]);
    expect(gate.reviewEvidence("owner", "repository", 42n)).toEqual({
      comments: [comment],
      reviews: [report, { ...report, id: 20n }],
      unresolved: 2n,
    });
    expect(calls).toHaveLength(3);
    const active = [
      ["comments", "reviews", "reviewThreads"],
      ["reviews", "reviewThreads"],
      ["reviewThreads"],
    ];
    calls.forEach((args, index) => {
      for (const name of active[0]) {
        if (active[index].includes(name))
          expect(args[3]).toContain(`${name}(first:100,after:$${name}After`);
        else {
          expect(args[3]).not.toContain(`${name}(`);
          expect(args[3]).not.toContain(`$${name}After`);
          expect(args.some((arg) => arg.startsWith(`${name}After=`))).toBe(
            false,
          );
        }
      }
    });
    expect(calls[1]).toContain("reviewsAfter=page-1");
    expect(calls[1]).toContain("reviewThreadsAfter=page-1");
    expect(calls[2]).toContain("reviewThreadsAfter=thread-2");
  });
  test("partial or missing connection after data never establishes evidence", () => {
    for (const name of [
      "comments",
      "reviews",
      "reviewThreads",
      "timelineItems",
    ]) {
      const initial = reviewResponse(
        response("comments", [summary()]),
        response(
          name,
          name === "comments"
            ? [summary()]
            : name === "reviews"
              ? [formal]
              : name === "reviewThreads"
                ? [{ isResolved: true }]
                : [
                    {
                      __typename: "BaseRefChangedEvent",
                      createdAt: "2026-10-17T00:00:00Z",
                    },
                  ],
          true,
          "next",
          1n,
        ),
      );
      const missing = reviewResponse();
      delete object(
        field(field(field(missing, "data"), "repository"), "pullRequest"),
      )[name];
      for (const later of [
        { ...reviewResponse(), errors: [{ message: "partial after data" }] },
        missing,
      ]) {
        const { gate, calls } = replies([initial, later]);
        expect(() => readEvidenceConnection(gate, name)).toThrow();
        expect(calls).toHaveLength(2);
      }
    }
  });
  test("every connection rejects nonboolean completion and unusable or cycling cursors", () => {
    for (const name of [
      "comments",
      "reviews",
      "reviewThreads",
      "timelineItems",
    ]) {
      for (const more of [null, 0n, 1n, "false", [], {}]) {
        const { gate, calls } = replies([
          reviewResponse(response(name, [], more, "next", 0n)),
        ]);
        expect(() => readEvidenceConnection(gate, name)).toThrow("pagination");
        expect(calls).toHaveLength(1);
      }
      for (const cursors of [
        [null],
        [""],
        [true],
        [1n],
        [[]],
        [{}],
        ["first", "first"],
        ["first", "second", "first"],
      ] as Json[][]) {
        const { gate, calls } = replies(
          cursors.map((cursor) =>
            reviewResponse(response(name, [], true, cursor, 0n)),
          ),
        );
        expect(() => readEvidenceConnection(gate, name)).toThrow("pagination");
        expect(calls).toHaveLength(cursors.length);
      }
    }
  });
  test("all requested connections require nodes and page metadata", () => {
    for (const name of [
      "comments",
      "reviews",
      "reviewThreads",
      "timelineItems",
    ])
      for (const value of [
        null,
        {},
        { nodes: [] },
        { nodes: null, pageInfo: { hasNextPage: false } },
        { nodes: [], pageInfo: {} },
      ] as Json[]) {
        const result = reviewResponse();
        object(
          field(field(field(result, "data"), "repository"), "pullRequest"),
        )[name] = value;
        expect(() =>
          readEvidenceConnection(replies([result]).gate, name),
        ).toThrow();
      }
  });
  test("late summary formal review and final history changes still withdraw approvals", () => {
    const initial = reviewResponse(
      response("comments", [summary()]),
      response("reviews", [formal], false, null, 1n),
    );
    for (const change of [
      response("comments", [
        { ...summary(), updatedAt: "2026-10-18T00:06:00Z" },
      ]),
      response(
        "reviews",
        [{ ...formal, updatedAt: "2026-10-18T00:06:00Z" }],
        false,
        null,
        1n,
      ),
      response("timelineItems", [
        {
          __typename: "BaseRefChangedEvent",
          createdAt: "2026-10-18T00:01:00Z",
        },
      ]),
    ]) {
      const changed = reviewResponse(
        response("comments", [summary()]),
        response("reviews", [formal], false, null, 1n),
        change,
      );
      const history = Object.hasOwn(
        object(
          field(field(field(change, "data"), "repository"), "pullRequest"),
        ),
        "timelineItems",
      )
        ? change
        : response("timelineItems", []);
      const before = exercise({
        reviewPages: [initial, changed],
        historyPages: [response("timelineItems", []), history],
        approvals: [approval()],
      });
      expectWrites(before, ["PUT"]);
      expect(before.evidenceReads).toBe(2);
      const during = exercise({
        reviewPages: [initial, initial, changed],
        historyPages: [
          response("timelineItems", []),
          response("timelineItems", []),
          history,
        ],
      });
      expectWrites(during, ["POST", "PUT"]);
      expect(during.evidenceReads).toBe(3);
    }
  });
  test("sweep reuses context history and continues after a failure", () => {
    let reads = 0;
    const gate = fakeGate({
      actions: () => {
        reads++;
        return encode([{ total_count: 1n, workflow_runs: [CONTEXT] }]);
      },
    });
    const seen: bigint[] = [];
    gate.main = (number) => {
      seen.push(number);
      expect(gate.contexts(REPO, HEAD)).toEqual([CONTEXT]);
      return Number(number === 1n);
    };
    const numbers = Array.from({ length: 300 }, (_, index) =>
      BigInt(index + 1),
    );
    expect(gate.sweep(numbers)).toBe(1);
    expect(reads).toBe(1);
    expect(seen).toEqual(numbers);
    gate.sweep([2n]);
    expect(reads).toBe(2);
  });
  test("context requests and cache entries are isolated by head", () => {
    const reads: string[] = [];
    const gate = fakeGate({
      actions: (args) => {
        reads.push(args.at(-1)!);
        return encode([{ total_count: 1n, workflow_runs: [CONTEXT] }]);
      },
    });
    gate.contexts(REPO, HEAD);
    gate.contexts(REPO, OLD);
    gate.contexts(REPO, HEAD);
    expect(reads).toEqual(
      [HEAD, OLD].map(
        (head) =>
          `repos/${REPO}/actions/workflows/review-context.yml/runs?event=pull_request_target&head_sha=${head}&per_page=100`,
      ),
    );
  });
  test("summaries preserve edit provenance across pages", () => {
    const expected = summary(),
      { gate, calls } = replies([
        reviewResponse(response("comments", [], true, "next")),
        response("comments", [expected]),
      ]);
    expect(gate.reviewEvidence("owner", "repository", 42n).comments).toEqual([
      expected,
    ]);
    expect(calls.at(-1)).toContain("commentsAfter=next");
  });
  test("empty GraphQL errors retain Python truthiness and explicit null user fails closed", () => {
    for (const errors of [[], {}])
      expect(
        replies([{ errors, ...reviewResponse() }]).gate.reviewEvidence(
          "owner",
          "repository",
          42n,
        ).comments,
      ).toEqual([]);
    const result = exercise({ approvals: [{ ...approval(), user: null }] });
    expectWrites(result, [], 1);
    expect(() => decision([summary()], [{ ...root, user: null }])).toThrow();
  });
  test("failed reads are not cached and caches belong to one Gate sweep", () => {
    let contexts = 0,
      comparisons = 0;
    const gate = fakeGate({
      actions() {
        contexts++;
        return encode([
          { total_count: contexts === 1 ? 2n : 1n, workflow_runs: [CONTEXT] },
        ]);
      },
      gh() {
        comparisons++;
        if (comparisons === 1) throw new Error("comparison unavailable");
        return encode({
          base_commit: { sha: BASE },
          merge_base_commit: { sha: BASE },
          status: "ahead",
        });
      },
    });
    expect(() => gate.contexts(REPO, HEAD)).toThrow();
    expect(gate.contexts(REPO, HEAD)).toEqual([CONTEXT]);
    gate.contexts(REPO, HEAD);
    expect(contexts).toBe(2);
    expect(() => gate.comparison(REPO, BASE, HEAD)).toThrow();
    expect(gate.comparison(REPO, BASE, HEAD)).toEqual({
      status: "ahead",
      merge_base: BASE,
    });
    gate.comparison(REPO, BASE, HEAD);
    expect(comparisons).toBe(2);
    gate.main = () => {
      gate.contexts(REPO, HEAD);
      gate.comparison(REPO, BASE, HEAD);
      return 0;
    };
    expect(gate.sweep([42n, 43n])).toBe(0);
    expect(contexts).toBe(3);
    expect(comparisons).toBe(3);
    expect(() => fakeGate().contexts(REPO, HEAD)).toThrow(
      "unexpected Actions request",
    );
  });
  test("formal review identities and counts keep strict lexical integer checks", () => {
    const id = "9007199254740993";
    expect(
      array(
        replies([
          reviewResponse(
            response(
              "reviews",
              [{ ...formal, fullDatabaseId: id }],
              false,
              null,
              1n,
            ),
          ),
        ]).gate.reviewEvidence("owner", "repository", 42n).reviews,
      )[0],
    ).toEqual({ ...report, id: 9007199254740993n });
    for (const fullDatabaseId of [
      true,
      19,
      "019",
      "19\n",
      "19\r",
      "19\u2028",
      0n,
      -1n,
    ])
      expect(() =>
        replies([
          reviewResponse(
            response(
              "reviews",
              [{ ...formal, fullDatabaseId }],
              false,
              null,
              1n,
            ),
          ),
        ]).gate.reviewEvidence("owner", "repository", 42n),
      ).toThrow();
    for (const count of [1, true, null])
      expect(() =>
        replies([
          reviewResponse(response("reviews", [formal], false, null, count)),
        ]).gate.reviewEvidence("owner", "repository", 42n),
      ).toThrow();
  });
  test("original non-strict count and App comparisons preserve numeric equality", () => {
    for (const count of [1, true])
      expect(
        fakeGate({
          actions: () =>
            encode([{ total_count: count, workflow_runs: [CONTEXT] }]),
        }).contexts(REPO, HEAD),
      ).toEqual([CONTEXT]);
    expect(
      checkFailures(
        [{ name: "check", app_id: 1, passed: true }],
        [["check", true]],
      ),
    ).toEqual([]);
    const value = viewResponse(VIEW);
    object(
      field(field(field(value, "data"), "repository"), "pullRequest"),
    ).number = 42;
    expect(replies([value]).gate.prView(REPO, 42n)).toEqual(VIEW);
  });
});

test("validation CLI runs before repository credentials and preserves exact numbers", () => {
  const path = new URL("./review-gate.ts", import.meta.url).pathname;
  for (const [input, expected] of [
    ["[]", "[]\n"],
    ["[3,9007199254740993,1,3]", "[3,9007199254740993,1,3]\n"],
  ]) {
    const result = spawnSync(process.execPath, [path, "--validate-numbers"], {
      input,
      encoding: "utf8",
      env: {},
      timeout: 5000,
    });
    expect(result.status, result.stderr).toBe(0);
    expect(result.stdout).toBe(expected);
  }
  for (const input of [
    "[1.0]",
    "[1e0]",
    "[true]",
    "[0]",
    "[-1]",
    "{}",
    "not JSON",
  ]) {
    const result = spawnSync(process.execPath, [path, "--validate-numbers"], {
      input,
      encoding: "utf8",
      env: {},
      timeout: 5000,
    });
    expect(result.status).toBe(1);
    expect(result.stdout).toBe("");
    expect(result.stderr).not.toContain("GITHUB_REPOSITORY");
  }
  const invalid = spawnSync(process.execPath, [path], {
    encoding: "utf8",
    env: { NUMBERS: "[1.0]" },
    timeout: 5000,
  });
  expect(invalid.status).toBe(1);
  expect(invalid.stderr).toContain("NUMBERS must contain positive PR numbers");
});
test("gh transport uses literal argv stdin strict UTF-8 and child-only Actions token", () => {
  const directory = mkdtempSync(join(tmpdir(), "review-transport-")),
    executable = join(directory, "gh");
  const previousPath = process.env.PATH,
    previousToken = process.env.GH_TOKEN;
  try {
    writeFileSync(
      executable,
      `#!${process.execPath}
const mode = process.argv[2];
if (mode === "bytes") process.stdout.write(Buffer.from([255]));
else if (mode === "bom") process.stdout.write(Buffer.from([239,187,191,123,125]));
else if (mode === "large") process.stdout.write("x".repeat(2 * 1024 * 1024));
else if (mode === "fail") { console.error("private-response-and-token"); process.exitCode = 17; }
else console.log(JSON.stringify({args:process.argv.slice(2),input:await Bun.stdin.text(),token:process.env.GH_TOKEN}));
`,
    );
    chmodSync(executable, 0o700);
    process.env.PATH = `${directory}:${previousPath}`;
    process.env.GH_TOKEN = "ordinary-fixture-token";
    const literal = `$(touch ${join(directory, "should-not-exist")})`;
    const ordinary = JSON.parse(
      ghTransport()(["api", literal], "nested\nstdin"),
    ) as { args: string[]; input: string; token: string };
    expect(ordinary).toEqual({
      args: ["api", literal],
      input: "nested\nstdin",
      token: "ordinary-fixture-token",
    });
    expect(existsSync(join(directory, "should-not-exist"))).toBe(false);
    expect(
      JSON.parse(ghTransport("actions-fixture-token")(["api"], "")).token,
    ).toBe("actions-fixture-token");
    expect(process.env.GH_TOKEN).toBe("ordinary-fixture-token");
    expect(ghTransport()(["large"])).toHaveLength(2 * 1024 * 1024);
    expect(() => ghTransport()(["bytes"])).toThrow("not valid UTF-8");
    expect(ghTransport()(["bom"])).toBe("\ufeff{}");
    expect(() => ghTransport()(["fail", "private-request"])).toThrow(
      "GitHub API request failed",
    );
    try {
      ghTransport()(["fail", "private-request"]);
    } catch (error) {
      expect(String(error)).not.toContain("private-response");
      expect(String(error)).not.toContain("private-request");
    }
  } finally {
    if (previousPath === undefined) delete process.env.PATH;
    else process.env.PATH = previousPath;
    if (previousToken === undefined) delete process.env.GH_TOKEN;
    else process.env.GH_TOKEN = previousToken;
    rmSync(directory, { recursive: true, force: true });
  }
});

test("provider text keeps Python line and whitespace rejection boundaries", () => {
  const url = "https://github.com/owner/repository/pull/42#discussion_r1";
  const old = { ...root, html_url: url, original_commit_id: OLD };
  const clean = summary();
  const section = `\n### Security findings\n#### Advisory findings (1)\n- [Finding](${url}) · **High**\n<details>About</details>`;
  expect(decision([{ ...clean, body: body(clean) + section }], [old])).toEqual(
    [],
  );
  for (const marker of ["#### Advisory", "- [Finding]"])
    expect(
      decision(
        [
          {
            ...clean,
            body: body(clean) + section.replace(marker, `\ufeff${marker}`),
          },
        ],
        [old],
      ),
    ).not.toEqual([]);
  expect(
    decision(
      [
        {
          ...clean,
          body:
            body(clean) +
            section.replace("#### Advisory", "\u001f#### Advisory"),
        },
      ],
      [old],
    ),
  ).toEqual([]);
  const badUrl = url.replace("/pull/", "/\u0085pull/");
  expect(
    decision(
      [{ ...clean, body: body(clean) + section.replace(url, badUrl) }],
      [{ ...old, html_url: badUrl }],
    ),
  ).not.toEqual([]);
  const state = body(clean).replace(
    '"code_review": {',
    '"code_review": {"unknown": "x\u2028y",',
  );
  expect(decision([{ ...clean, body: state }])).toEqual([]);
  expect(
    decision([
      {
        ...clean,
        body: body(clean).replace("y **Completed**", "y\u2028**Completed**"),
      },
    ]),
  ).toEqual([]);
});

test("lifecycle IDs reject Python whitespace without treating BOM as whitespace", () => {
  for (const separator of ["\u0085", "\u001c", "\u001d", "\u001e", "\u001f"]) {
    const view = {
      ...VIEW,
      lifecycle: { type: "ReopenedEvent", id: `id${separator}suffix` },
    };
    expect(() =>
      replies([viewResponse(view)]).gate.prView(REPO, 42n),
    ).toThrow();
  }
  const view = {
    ...VIEW,
    lifecycle: { type: "ReopenedEvent", id: "id\ufeffsuffix" },
  };
  expect(replies([viewResponse(view)]).gate.prView(REPO, 42n)).toEqual(view);
});

describe("review evidence and baseline", () => {
  test("clean head is approved", () =>
    expect(decision([summary()])).toEqual([]));
  test("review context requires the exact trusted event", () => {
    expect(reviewContext([CONTEXT], REPO, 42n, VIEW)).toEqual(RECORDED);
    for (const [key, value] of [
      ["event", "pull_request"],
      ["event", "workflow_dispatch"],
      ["path", ".github/workflows/impostor.yml"],
      ["repository", { full_name: "attacker/repository" }],
      ["display_title", `context:43:${HEAD}:${BASE}:dev`],
      ["status", "in_progress"],
      ["conclusion", "failure"],
    ] as [string, Json][])
      expect(
        reviewContext([{ ...CONTEXT, [key]: value }], REPO, 42n, VIEW),
      ).toBeNull();
    for (const changed of [
      { head_sha: OLD },
      { path: `${CONTEXT.path}@dev` },
    ] as Record[])
      expect(
        reviewContext([{ ...CONTEXT, ...changed }], REPO, 42n, VIEW),
      ).toEqual(RECORDED);
    expect(
      reviewContext(
        [{ ...CONTEXT, path: ".github/workflows/impostor.yml@dev" }],
        REPO,
        42n,
        VIEW,
      ),
    ).toBeNull();
  });
  test("retarget or base advance cannot reuse old context", () => {
    for (const changed of [
      { baseRefName: "main" },
      { baseRefOid: OLD },
      { headRefOid: OLD },
    ] as Record[])
      expect(
        reviewContext([CONTEXT], REPO, 42n, { ...VIEW, ...changed }),
      ).toBeNull();
    expect(baselineFailures([summary()], HEAD, null)).not.toEqual([]);
  });
  test("reviews must finish after the recorded event", () => {
    expect(baselineFailures([summary()], HEAD, CONTEXT.created_at)).toEqual([]);
    expect(
      baselineFailures([summary()], HEAD, "2026-10-18T00:06:00Z"),
    ).not.toEqual([]);
    expect(baselineFailures([], HEAD, CONTEXT.created_at)).not.toEqual([]);
  });
  test("returning to a previous target does not reuse context", () => {
    const events = [1, 2].map((minute) => ({
      __typename: "BaseRefChangedEvent",
      createdAt: `2026-10-18T00:0${minute}:00Z`,
    }));
    expect(
      historyFailures(events, CONTEXT.created_at, PROTECTIONS),
    ).not.toEqual([]);
    expect(
      historyFailures(events, "2026-10-18T00:03:00Z", PROTECTIONS),
    ).toEqual([]);
  });
  test("reintroduced heads and unprotected targets cannot bind a review", () => {
    for (const kind of [
      "HeadRefForcePushedEvent",
      "HeadRefDeletedEvent",
      "HeadRefRestoredEvent",
    ])
      expect(
        historyFailures(
          [{ __typename: kind }],
          CONTEXT.created_at,
          PROTECTIONS,
        ),
      ).not.toEqual([]);
    expect(historyFailures([], CONTEXT.created_at, [])).not.toEqual([]);
    expect(historyFailures([], CONTEXT.created_at, PROTECTIONS)).toEqual([]);
    for (const key of [
      "required_review_thread_resolution",
      "dismiss_stale_reviews_on_push",
    ])
      expect(
        historyFailures([], CONTEXT.created_at, [
          ...PROTECTIONS.slice(0, 2),
          {
            type: "pull_request",
            parameters: {
              ...object(field(PROTECTIONS[2], "parameters")),
              [key]: false,
            },
          },
        ]),
      ).not.toEqual([]);
  });
  test("target history reads later pages", () => {
    const event = {
      __typename: "BaseRefChangedEvent",
      createdAt: "2026-10-18T00:01:00Z",
    };
    const { gate, calls } = replies([
      response("timelineItems", [], true, "next"),
      response("timelineItems", [event]),
    ]);
    expect(gate.baselineEvents("owner", "repository", 42n, HEAD)).toEqual([
      event,
    ]);
    expect(calls.at(-1)).toContain("after=next");
  });
  test("new head after a rewrite can receive its own approval", () => {
    for (const before of ["ahead", "diverged"]) {
      const result = exercise({ snapshots: [{ events: [rewritten(before)] }] });
      expectWrites(result, ["POST"]);
      expect(result.writes[0]).toContain(`commit_id=${HEAD}`);
    }
  });
  test("old head or rewrite after context blocks approval", () => {
    for (const event of [
      rewritten("behind"),
      rewritten("identical"),
      rewritten("diverged", "identical"),
      rewritten("diverged", "behind"),
      rewritten("diverged", "diverged"),
      rewritten("diverged", "ahead", CONTEXT.created_at),
      rewritten("diverged", "ahead", "2026-10-18T00:01:00Z"),
      rewritten(null),
      rewritten("diverged", null),
    ])
      expectWrites(
        exercise({ snapshots: [{ events: [event] }], approvals: [approval()] }),
        ["PUT"],
      );
  });
  test("every removed history is checked and only the latest tip must be extended", () => {
    const older = rewritten("diverged", "diverged", "2026-10-17T22:00:00Z");
    expect(
      historyFailures([older, rewritten()], CONTEXT.created_at, PROTECTIONS),
    ).toEqual([]);
    expect(
      historyFailures(
        [{ ...older, beforeRelationship: "behind" }, rewritten()],
        CONTEXT.created_at,
        PROTECTIONS,
      ),
    ).not.toEqual([]);
  });
  test("same second rewrites use timeline order", () => {
    const earlier = rewritten("diverged", "diverged"),
      final = rewritten();
    expect(
      historyFailures([earlier, final], CONTEXT.created_at, PROTECTIONS),
    ).toEqual([]);
    expect(
      historyFailures([final, earlier], CONTEXT.created_at, PROTECTIONS),
    ).not.toEqual([]);
    expect(
      historyFailures(
        [final, rewritten("diverged", "ahead", "2026-10-17T22:00:00Z")],
        CONTEXT.created_at,
        PROTECTIONS,
      ),
    ).not.toEqual([]);
  });
  test("history rereads only reuse immutable comparisons", () => {
    const older = "d".repeat(40),
      final = "e".repeat(40),
      reads: string[][] = [],
      comparisons: string[] = [];
    const events: Record[] = [
      rewritten(),
      {
        ...rewritten(),
        beforeCommit: { oid: older },
        afterCommit: { oid: final },
      },
    ];
    object(events[0]).afterCommit = { oid: older };
    for (const event of events) {
      delete event.beforeRelationship;
      delete event.afterRelationship;
    }
    const gate = fakeGate({
      gh(args) {
        if (args[1] === "graphql") {
          reads.push(args);
          const later = args.includes("after=next");
          return encode(
            response("timelineItems", [events[Number(later)]], !later, "next"),
          );
        }
        const path = args.at(-1)!,
          base = path.split("/compare/")[1].split("...")[0];
        comparisons.push(path);
        return encode({
          base_commit: { sha: base },
          merge_base_commit: { sha: BASE },
          status: base === final ? "ahead" : "diverged",
        });
      },
    });
    for (const number of [42n, 42n, 43n])
      expect(
        historyFailures(
          gate.baselineEvents("owner", "repository", number, HEAD),
          CONTEXT.created_at,
          PROTECTIONS,
        ),
      ).toEqual([]);
    expect(reads).toHaveLength(6);
    expect(comparisons).toEqual(
      [OLD, older, final].map(
        (base) => `repos/${REPO}/compare/${base}...${HEAD}?per_page=1`,
      ),
    );
    gate.baselineEvents("owner", "repository", 42n, "f".repeat(40));
    expect(comparisons).toHaveLength(6);
  });
  test("history binds both sides after pagination", () => {
    const event = rewritten();
    delete event.beforeRelationship;
    delete event.afterRelationship;
    const { gate, calls } = replies([
      response("timelineItems", [], true, "next"),
      response("timelineItems", [event]),
      ...[
        [OLD, "diverged"],
        [BASE, "ahead"],
      ].map(([base, status]) => ({
        base_commit: { sha: base },
        status,
        merge_base_commit: { sha: BASE },
      })),
    ]);
    expect(gate.baselineEvents("owner", "repository", 42n, HEAD)).toEqual([
      rewritten(),
    ]);
    expect(calls[0][3]).toContain("beforeCommit{oid} afterCommit{oid}");
    expect(calls.slice(-2)).toEqual(
      [OLD, BASE].map((base) => [
        "api",
        `repos/${REPO}/compare/${base}...${HEAD}?per_page=1`,
      ]),
    );
  });
  test("missing commit and invalid comparisons cannot establish ancestry", () => {
    for (const sha of [null, "", "branch", "../unexpected"]) {
      const { gate, calls } = replies([]);
      expect(() => gate.relationship(REPO, sha, HEAD)).toThrow();
      expect(calls).toEqual([]);
    }
    for (const value of [
      {},
      { base_commit: { sha: HEAD }, status: "ahead" },
      { base_commit: { sha: OLD }, status: "unknown" },
    ] as Record[])
      expect(() =>
        replies([value]).gate.relationship(REPO, OLD, HEAD),
      ).toThrow();
    expect(() =>
      fakeGate({
        gh() {
          throw new Error("comparison unavailable");
        },
      }).relationship(REPO, OLD, HEAD),
    ).toThrow("comparison unavailable");
  });
  test("incomplete rewrite history fails closed", () => {
    for (const cursors of [
      [null],
      ["first", "first"],
      ["first", "second", "first"],
    ])
      expect(() =>
        replies(
          cursors.map((cursor) => response("timelineItems", [], true, cursor)),
        ).gate.baselineEvents("owner", "repository", 42n, HEAD),
      ).toThrow("pagination");
    expect(() =>
      replies([
        {
          errors: [{ message: "partial history" }],
          ...response("timelineItems", []),
        },
      ]).gate.baselineEvents("owner", "repository", 42n, HEAD),
    ).toThrow("incomplete evidence");
  });
  test("new rewrite during approval revalidation prevents write", () => {
    expectWrites(
      exercise({
        snapshots: [
          { events: [rewritten()] },
          {
            events: [
              rewritten(),
              rewritten("diverged", "ahead", "2026-10-18T00:01:00Z"),
            ],
          },
        ],
        approvals: [approval()],
      }),
      ["PUT"],
    );
  });
  test("restricted or unknown dismissal authority blocks approval", () => {
    for (const restriction of [
      {},
      { enabled: true, allowed_actors: [] },
    ] as Record[])
      expect(
        historyFailures([], CONTEXT.created_at, [
          ...PROTECTIONS.slice(0, 2),
          {
            type: "pull_request",
            parameters: {
              ...object(field(PROTECTIONS[2], "parameters")),
              dismissal_restriction: restriction,
            },
          },
        ]),
      ).not.toEqual([]);
  });
  test("incomplete context pagination is rejected", () => {
    expect(() =>
      fakeGate({
        actions: () => encode([{ total_count: 2n, workflow_runs: [CONTEXT] }]),
      }).contexts(REPO, HEAD),
    ).toThrow("pagination is incomplete");
  });
});

describe("provider summaries and material", () => {
  test("threadless and loading security findings block", () => {
    const comment = summary();
    expect(
      decision([
        {
          ...comment,
          body:
            body(comment) +
            "\n### Security findings\nFinding details are still loading.",
        },
      ]),
    ).not.toEqual([]);
    expect(
      decision([
        {
          ...comment,
          body: body(comment).replaceAll(
            '"has_unresolved_findings": false',
            '"has_unresolved_findings": true',
          ),
        },
      ]),
    ).not.toEqual([]);
  });
  test("clean review can retain resolved security findings from older or current head", () => {
    const url = "https://github.com/owner/repository/pull/42#discussion_r1";
    const old = { ...root, html_url: url, original_commit_id: OLD };
    const comment = summary();
    comment.body =
      body(comment) +
      `\n### Security findings\n\n#### Advisory findings (1)\n\n- 🟡 [Previous finding](${url}) · **High**\n\n<details>About</details>`;
    expect(decision([comment], [old])).toEqual([]);
    expect(decision([comment], [old], 1n)).not.toEqual([]);
    expect(
      decision([comment], [{ ...old, original_commit_id: HEAD }]),
    ).not.toEqual([]);
    expect(decision([comment])).not.toEqual([]);
    expect(
      decision(
        [
          {
            ...comment,
            body: body(comment).replace(
              "<details>",
              "New unlinked finding\n<details>",
            ),
          },
        ],
        [old],
      ),
    ).not.toEqual([]);
    const resolved = changeState(
      {
        ...comment,
        body: body(comment).replace("**High**", "**High** · **Resolved**"),
      },
      "security_review",
      "has_any_findings",
      true,
    );
    expect(decision([resolved], [old])).toEqual([]);
    expect(decision([resolved], [old], 1n)).not.toEqual([]);
    expect(
      decision([resolved], [{ ...old, original_commit_id: HEAD }]),
    ).toEqual([]);
    expect(decision([resolved])).not.toEqual([]);
    expect(
      decision(
        [
          changeState(
            resolved,
            "security_review",
            "has_unresolved_findings",
            true,
          ),
        ],
        [old],
      ),
    ).not.toEqual([]);
    expect(
      decision(
        [
          {
            ...resolved,
            body: body(resolved).replace(
              "<details>",
              "### Security findings\nUnknown\n<details>",
            ),
          },
        ],
        [old],
      ),
    ).not.toEqual([]);
    for (const separator of ["\r", "\u0085", "\u2028", "\u2029"])
      expect(
        decision(
          [{ ...resolved, body: body(resolved).replaceAll("\n", separator) }],
          [old],
        ),
      ).toEqual([]);
  });
  test("each review must complete on this head", () => {
    for (const comments of [
      [summary("completed", "running")],
      [summary("completed", "failed")],
      [summary("completed", "completed", OLD)],
      [],
    ])
      expect(decision(comments)).not.toEqual([]);
  });
  test("code review history flag is not a disposition or permanent blocker", () => {
    const comment = changeState(
        summary(),
        "code_review",
        "has_any_findings",
        true,
      ),
      old = { ...root, original_commit_id: OLD };
    expect(decision([comment], [old])).toEqual([]);
    expect(decision([comment])).toEqual([]);
    expect(decision([comment], [old], 1n)).not.toEqual([]);
    expect(decision([comment], [old, { ...root, id: 2n }])).toEqual([]);
    for (const value of [true, null])
      expect(
        decision(
          [
            changeState(
              comment,
              "code_review",
              "has_unresolved_findings",
              value,
            ),
          ],
          [old],
        ),
      ).not.toEqual([]);
  });
  test("summary from another user is ignored", () =>
    expect(
      decision([summary("completed", "completed", HEAD, "someone")]),
    ).not.toEqual([]));
  test("summary edited by a writer is not provider evidence", () => {
    for (const editor of [
      null,
      { __typename: "User", login: "writer" },
      { __typename: "User", login: BOT.replace("[bot]", "") },
    ]) {
      const forged = { ...summary(), editor };
      expect(decision([forged])).not.toEqual([]);
      expect(baselineFailures([forged], HEAD, CONTEXT.created_at)).not.toEqual(
        [],
      );
    }
    expect(
      decision([{ ...summary(), editor: null, lastEditedAt: null }]),
    ).toEqual([]);
  });
  test("new inline findings block but replies and moved comments do not", () => {
    const finding = {
      id: 1n,
      user: { login: BOT },
      original_commit_id: HEAD,
      in_reply_to_id: null,
    };
    expect(decision([summary()], [finding])).not.toEqual([]);
    expect(
      decision(
        [summary()],
        [
          { ...finding, id: 2n, in_reply_to_id: 1n },
          { ...finding, id: 3n, original_commit_id: OLD, commit_id: HEAD },
          { ...finding, id: 4n, user: { login: "headiron" } },
        ],
      ),
    ).toEqual([]);
  });
  test("unresolved threads block", () =>
    expect(decision([summary()], [], 1n)).not.toEqual([]));
  test("required checks must pass including latest reruns", () => {
    for (const runs of [
      [run("rust", "in_progress", null), run("docs")],
      [run("rust", "completed", "failure"), run("docs")],
      [run("docs")],
      [
        run("rust"),
        run("rust", "completed", "failure", ACTIONS, 2n),
        run("docs"),
      ],
    ])
      expect(decision([summary()], [], 0n, headChecks(runs, []))).not.toEqual(
        [],
      );
    for (const runs of [
      [run("rust", "completed", "skipped"), run("docs")],
      [
        run("rust", "completed", "failure"),
        run("rust", "completed", "success", ACTIONS, 2n),
        run("docs"),
      ],
    ])
      expect(decision([summary()], [], 0n, headChecks(runs, []))).toEqual([]);
  });
  test("required checks must come from pinned integration", () => {
    expect(
      decision(
        [summary()],
        [],
        0n,
        headChecks(
          [run("rust", "completed", "success", 999n), run("docs")],
          [],
        ),
      ),
    ).not.toEqual([]);
    expect(
      decision(
        [summary()],
        [],
        0n,
        headChecks(
          [run("rust", "completed", "success", 999n), run("rust"), run("docs")],
          [],
        ),
      ),
    ).toEqual([]);
    const unpinned = headChecks(
      [run("docs")],
      [{ context: "rust", state: "success" }],
    );
    expect(checkFailures(unpinned, [["rust", null]])).toEqual([]);
    expect(checkFailures(unpinned, [["rust", ACTIONS]])).not.toEqual([]);
  });
  test("overlapping rules keep pinned and unpinned requirements", () => {
    const required = requiredChecks([
      { type: "required_signatures" },
      {
        type: "required_status_checks",
        parameters: {
          required_status_checks: [
            { context: "rust" },
            { context: "rust", integration_id: ACTIONS },
            { context: "rust", integration_id: ACTIONS },
          ],
        },
      },
    ]);
    expect(required).toEqual([
      ["rust", null],
      ["rust", ACTIONS],
    ]);
    expect(
      checkFailures(
        headChecks([run("rust", "completed", "success", 999n)], []),
        required,
      ),
    ).toEqual([`rust (app ${ACTIONS})`]);
    expect(checkFailures(PASS, required)).toEqual([]);
  });
  test("hidden state must match published summary", () => {
    expect(
      decision([
        {
          ...summary(),
          body: body(summary()).replace("**Completed**", "**Failed**"),
        },
      ]),
    ).not.toEqual([]);
    expect(
      decision([{ ...summary(), body: body(summary()).split("## Codex")[0] }]),
    ).not.toEqual([]);
  });
  test("submitted review with unverified freshness blocks", () => {
    expectWrites(
      exercise({
        snapshots: [{ reviews: [{ id: 19n, commit_id: HEAD }] }],
        approvals: [approval()],
      }),
      ["PUT"],
    );
    expect(
      decision([summary()], [], 0n, PASS, [{ id: 19n, commit_id: OLD }]),
    ).toEqual([]);
  });
  test("formal metadata tracks edits without judging body or dismissal", () => {
    for (const [state, body] of [
      ["COMMENTED", "Finding"],
      ["DISMISSED", ""],
    ]) {
      const records = [
        { ...formal, state, body },
        {
          ...formal,
          fullDatabaseId: "20",
          author: { __typename: "Bot", login: "cladiron" },
          state: "APPROVED",
        },
      ];
      expect(
        replies([
          reviewResponse(response("reviews", records, false, null, 2n)),
        ]).gate.reviewEvidence("owner", "repository", 42n).reviews,
      ).toEqual([report]);
    }
  });
  test("explicit boolean finding flags are required", () => {
    for (const kind of REVIEWS)
      for (const key of ["has_any_findings", "has_unresolved_findings"])
        for (const value of [null, "false", 0n, undefined])
          expect(
            decision([changeState(summary(), kind, key, value)]),
          ).not.toEqual([]);
  });
  test("same head retained reports pass only machine scope", () => {
    const comment = {
      ...summary(),
      body: body(summary()).replaceAll(
        '"has_any_findings": false',
        '"has_any_findings": true',
      ),
    };
    const result = exercise({
      snapshots: [{ comments: [comment], reviews: [report], inline: [root] }],
      postError: true,
    });
    expectWrites(result, ["POST"]);
    expect(result.reviews[0].body).toContain(
      "Scope excludes substantive finding disposition",
    );
    expect(result.reviews[0].body).toContain(
      "Finding handlers record dispositions in the original review threads.",
    );
    expect(decision([comment], [root], 0n, PASS, [report])).toEqual([]);
    expect(decision([comment], [root], 1n, PASS, [report])).not.toEqual([]);
    expect(decision([comment], [], 0n, PASS, [report])).toEqual([]);
    expect(
      decision(
        [comment],
        [{ ...root, body: "Edited before completion" }],
        0n,
        PASS,
        [report],
      ),
    ).toEqual([]);
  });
  test("late or unknown material cannot become history on next poll", () => {
    const comment = {
      ...summary(),
      body: body(summary()).replace(
        'datetime="2026-10-18T00:05:00Z"',
        'datetime="2026-10-18T00:07:00Z"',
      ),
    };
    for (const time of [
      null,
      "invalid",
      "2026-10-18T00:05:00Z",
      "2026-10-18T00:06:00Z",
      "2026-10-18T00:03:00Z",
    ])
      for (let poll = 0; poll < 2; poll++) {
        expect(
          decision([comment], [{ ...root, updated_at: time }], 0n, PASS, [
            report,
          ]),
        ).not.toEqual([]);
        expect(
          decision([comment], [root], 0n, PASS, [
            { ...report, updated_at: time },
          ]),
        ).not.toEqual([]);
      }
    expect(
      decision(
        [
          {
            ...comment,
            body: body(comment).replace("00:05:00Z", "00:05:00.900000Z"),
          },
        ],
        [root],
        0n,
        PASS,
        [{ ...report, updated_at: "2026-10-18T00:05:00Z" }],
      ),
    ).not.toEqual([]);
  });
  test("new material withdraws approval and later completion restores it", () => {
    const changed = {
      reviews: [
        report,
        {
          ...report,
          id: 20n,
          submitted_at: "2026-10-18T00:06:00Z",
          updated_at: "2026-10-18T00:06:00Z",
        },
      ],
    };
    expectWrites(
      exercise({
        snapshots: [{ reviews: [report] }, { reviews: [report] }, changed],
      }),
      ["POST", "PUT"],
    );
    expectWrites(exercise({ snapshots: [changed], approvals: [approval()] }), [
      "PUT",
    ]);
    const later = {
      ...summary(),
      body: body(summary()).replaceAll("00:05:00Z", "00:07:00Z"),
      updatedAt: "2026-10-18T00:07:00Z",
    };
    expectWrites(exercise({ snapshots: [{ ...changed, comments: [later] }] }), [
      "POST",
    ]);
  });
  test("formal pagination requires complete unique metadata", () => {
    const { gate, calls } = replies([
      reviewResponse(response("reviews", [], true, "next", 1n)),
      response("reviews", [formal], false, null, 1n),
    ]);
    expect(
      field(
        array(gate.reviewEvidence("owner", "repository", 42n).reviews)[0],
        "id",
      ),
    ).toBe(19n);
    expect(calls.at(-1)).toContain("reviewsAfter=next");
    for (const values of [
      [response("reviews", [formal])],
      [response("reviews", [formal], false, null, 2n)],
      [response("reviews", [formal, formal], false, null, 2n)],
      [
        response("reviews", [formal], true, "next", 1n),
        response("reviews", [], false, null, 0n),
      ],
      [
        response(
          "reviews",
          [
            { ...formal, author: { __typename: "User", login: "writer" } },
            formal,
          ],
          false,
          null,
          2n,
        ),
      ],
      [
        response("reviews", [], true, "next", 0n),
        response("reviews", [], true, "next", 0n),
      ],
      [
        response(
          "reviews",
          [{ ...formal, fullDatabaseId: true }],
          false,
          null,
          1n,
        ),
      ],
    ]) {
      const { gate, calls } = replies(
        values.map((value) => reviewResponse(value)),
      );
      expect(() => gate.reviewEvidence("owner", "repository", 42n)).toThrow();
      expect(calls.length).toBeGreaterThan(0);
    }
  });
});
