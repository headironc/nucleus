import { expect, test } from "bun:test";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { approvalBody, headChecks } from "./review-gate";
import { binding, CHECK, contract, Queue, QueueWaiting } from "./review-queue";
import {
  array,
  encode,
  field,
  object,
  parse,
  string,
  type Json,
  type Record,
} from "./review-json";
import {
  approval,
  BASE,
  CONTEXT,
  BRANCH,
  CHECK_CHANGES,
  ENTRY,
  fakeGate,
  exercise,
  checkEvidence,
  GROUP,
  HEAD,
  OLD,
  MAIN_RULES,
  QUEUE_RULES,
  REPO,
  REVIEW,
  RUNS,
  run,
  summary,
  TREE,
  VIEW,
  LOWER_VIEW,
  UPPER_VIEW,
} from "./review-fixtures";

interface GroupOptions {
  number?: bigint;
  branch?: string;
  reviewReads?: bigint[];
  queued?: Record | null;
  runs?: Record[];
  review?: Record;
  ref?: Json;
  target?: Json;
  parents?: Json[];
  tree?: Json;
  reviewedTree?: Json;
  checks?: Json[];
  statuses?: Json[];
  owners?: Json[];
  entries?: Json[];
  commitChanges?: { [sha: string]: Record };
  layers?: Map<bigint, { review: Record; entry: Record | null }>;
  candidates?: Map<
    string,
    { branch: string; runs: Record[]; checks: Json[]; entries: Json[] }
  >;
}
function groupQueue(options: GroupOptions = {}): Queue {
  const queued = options.queued === undefined ? ENTRY : options.queued;
  const gate = fakeGate({
    queueEnabled: true,
    gh(args) {
      const path = args.at(-1)!;
      if (path.endsWith("/git/ref/heads/dev"))
        return encode({
          ref: "refs/heads/dev",
          object: { sha: options.target ?? BASE },
        });
      if (path.includes("/git/ref/heads/")) {
        const candidate = [...(options.candidates ?? [])].find(([, value]) =>
          path.endsWith(`/heads/${value.branch}`),
        );
        return encode({
          ref: `refs/heads/${candidate?.[1].branch ?? options.branch ?? BRANCH}`,
          object: { sha: candidate?.[0] ?? options.ref ?? GROUP },
        });
      }
      if (path.includes("/git/commits/")) {
        const commit = path.split("/").at(-1)!;
        return encode({
          sha: commit,
          tree: {
            sha:
              commit === GROUP
                ? options.tree === undefined
                  ? TREE
                  : options.tree
                : options.reviewedTree === undefined
                  ? TREE
                  : options.reviewedTree,
          },
          parents: (options.parents ?? [BASE]).map((sha) => ({ sha })),
          ...options.commitChanges?.[commit],
        });
      }
      throw new Error(`unexpected route ${path}`);
    },
  });
  gate.pages = () => options.statuses ?? [];
  const queue = new Queue(gate),
    runs = options.runs ?? RUNS;
  queue.reviewSnapshot = (_repo, number) => {
    options.reviewReads?.push(number);
    return structuredClone(
      options.layers?.get(number)?.review ?? options.review ?? REVIEW,
    );
  };
  queue.entry = (_repo, number) =>
    options.layers?.has(number)
      ? structuredClone(options.layers.get(number)!.entry)
      : structuredClone(queued);
  queue.shaPrs = (_repo, head) => ({
    heads: options.owners ?? [],
    entries:
      options.candidates?.get(string(head))?.entries ??
      options.entries ??
      (queued === null ? [] : [{ number: 42n, id: queued.id }]),
  });
  queue.actions = (path) => {
    const head = new URL(`https://api.github.com/${path}`).searchParams.get(
      "head_sha",
    );
    const current = options.candidates?.get(head ?? "")?.runs ?? runs;
    return [{ total_count: BigInt(current.length), workflow_runs: current }];
  };
  queue.checkRuns = (_repo, head) =>
    options.candidates?.get(string(head))?.checks ??
    options.checks ?? [run("docs"), run("rust"), run("website")];
  return queue;
}
function group(options: GroupOptions = {}): Record {
  return groupQueue(options).groupSnapshot(REPO, options.number ?? 42n);
}

function stackGroup(): GroupOptions {
  const layers = new Map<bigint, { review: Record; entry: Record | null }>([
    [42n, { review: { ...REVIEW, view: LOWER_VIEW }, entry: { ...ENTRY } }],
    [
      43n,
      {
        review: { ...REVIEW, view: UPPER_VIEW },
        entry: {
          ...ENTRY,
          id: "entry-2",
          position: 2n,
          pullRequest: {
            number: 43n,
            headRefOid: UPPER_VIEW.headRefOid,
            baseRefOid: HEAD,
            baseRefName: "codex/lower",
          },
        },
      },
    ],
  ]);
  return {
    layers,
    entries: [
      { number: 42n, id: "entry-1" },
      { number: 43n, id: "entry-2" },
    ],
  };
}

const UPPER_GROUP = "1".repeat(40),
  UPPER_TREE = "2".repeat(40);
function chainedStack(): GroupOptions {
  const options = stackGroup(),
    upper = options.layers!.get(43n)!;
  options.number = 43n;
  upper.entry = {
    ...upper.entry!,
    headCommit: { oid: UPPER_GROUP },
    baseCommit: { oid: GROUP },
  };
  options.commitChanges = {
    [UPPER_GROUP]: { parents: [{ sha: GROUP }], tree: { sha: UPPER_TREE } },
    [string(UPPER_VIEW.headRefOid)]: { tree: { sha: UPPER_TREE } },
  };
  options.candidates = new Map(
    [42n, 43n].map((number) => {
      const sha = number === 42n ? GROUP : UPPER_GROUP;
      const branch = BRANCH.replace("pr-42-", `pr-${number}-`);
      return [
        sha,
        {
          branch,
          runs: RUNS.map((value) => ({
            ...value,
            head_sha: sha,
            head_branch: branch,
          })),
          checks: [run("docs"), run("rust"), run("website")],
          entries: [{ number, id: number === 42n ? "entry-1" : "entry-2" }],
        },
      ];
    }),
  );
  return options;
}

test("upper queue candidate extends the verified lower prefix before either PR merges", () => {
  const options = chainedStack(),
    snapshot = group(options);
  expect(snapshot.conclusion).toBe("success");
  expect(snapshot.head).toBe(UPPER_GROUP);
  expect(field(snapshot.entry, "baseCommit")).toEqual({ oid: GROUP });
  expect(field(snapshot.prefix, "head")).toBe(GROUP);
  expect(field(field(snapshot.prefix, "entry"), "baseCommit")).toEqual({
    oid: BASE,
  });
  expect(field(snapshot.prefix, "conclusion")).toBe("success");
  expect(
    array(snapshot.members).map((member) => field(member, "number")),
  ).toEqual([42n, 43n]);
});

test("lower prefix progress preserves the upper candidate binding", () => {
  const options = chainedStack(),
    before = group(options);
  expect(options.layers!.get(42n)!.entry!.state).toBe("AWAITING_CHECKS");
  options.layers!.get(42n)!.entry!.state = "MERGEABLE";
  const after = group(options);
  expect(before.conclusion).toBe("success");
  expect(after.conclusion).toBe("success");
  expect(after).toEqual(before);
  expect(binding(REPO, 43n, after, true)).toBe(
    binding(REPO, 43n, before, true),
  );
});

test("lower same-SHA reentry invalidates the unchanged upper candidate binding", () => {
  const options = chainedStack(),
    before = group(options),
    lower = options.layers!.get(42n)!.entry!;
  lower.id = "replacement-lower-entry";
  lower.enqueuedAt = "2026-10-19T00:00:01Z";
  options.candidates!.get(GROUP)!.entries = [{ number: 42n, id: lower.id }];
  const after = group(options);
  expect(before.conclusion).toBe("success");
  expect(after.conclusion).toBe("success");
  expect(after.head).toBe(before.head);
  expect(after.entry).toEqual(before.entry);
  expect(field(after.prefix, "head")).toBe(field(before.prefix, "head"));
  expect(field(after.prefix, "entry")).not.toEqual(
    field(before.prefix, "entry"),
  );
  expect(binding(REPO, 43n, after, true)).not.toBe(
    binding(REPO, 43n, before, true),
  );
});

test("upper prefix requires the exact lower commit, parent chain, tree and live dev", () => {
  const wrongBase = chainedStack();
  wrongBase.layers!.get(43n)!.entry!.baseCommit = { oid: BASE };
  expect(() => group(wrongBase)).toThrow("exact supported dev candidate");
  for (const [sha, change] of [
    [GROUP, { parents: [{ sha: OLD }] }],
    [UPPER_GROUP, { parents: [{ sha: BASE }] }],
    [GROUP, { tree: { sha: UPPER_TREE } }],
    [UPPER_GROUP, { tree: { sha: TREE } }],
  ] as [string, Record][]) {
    const options = chainedStack();
    options.commitChanges![sha] = { ...options.commitChanges![sha], ...change };
    expect(() => group(options)).toThrow(
      "does not bind exactly the reviewed head",
    );
  }
  const moved = chainedStack();
  moved.target = OLD;
  expect(() => group(moved)).toThrow("baseline is not current dev");
});

test("upper cannot borrow successful CI while the lower prefix failed, was cancelled or is pending", () => {
  for (const conclusion of ["failure", "cancelled", null]) {
    const options = chainedStack(),
      lower = options.candidates!.get(GROUP)!;
    lower.runs[0] = {
      ...lower.runs[0],
      status: conclusion === null ? "in_progress" : "completed",
      conclusion,
    };
    expect(group(options).conclusion).toBe(
      conclusion === null ? null : "failure",
    );
  }
  const missing = chainedStack();
  missing.candidates!.get(GROUP)!.runs.pop();
  expect(group(missing).conclusion).toBeNull();
  const unapproved = chainedStack();
  unapproved.layers!.get(42n)!.review.reasons = ["lower approval invalidated"];
  expect(group(unapproved).conclusion).toBe("failure");
  const unrelated = chainedStack();
  unrelated
    .candidates!.get(GROUP)!
    .entries.push({ number: 44n, id: "foreign-entry" });
  expect(group(unrelated).conclusion).toBe("failure");
});

test("lower queue evidence invalidates the upper binding and publication", () => {
  const before = group(chainedStack()),
    options = chainedStack();
  options.candidates!.get(GROUP)!.runs[0]!.run_attempt = 2n;
  const after = group(options);
  expect(binding(REPO, 43n, before, true)).not.toBe(
    binding(REPO, 43n, after, true),
  );
  const early = server().queue,
    earlyWrites = capture(early);
  early.groupSnapshot = sequence([after]);
  early.publish(REPO, 43n, before, true);
  expect(earlyWrites.map((write) => write.conclusion)).toEqual(["failure"]);
  const late = server().queue,
    lateWrites = capture(late);
  late.groupSnapshot = sequence([before, after]);
  expect(() => late.publish(REPO, 43n, before, true)).toThrow(
    "evidence changed",
  );
  expect(lateWrites.map((write) => write.conclusion)).toEqual([
    "success",
    "failure",
  ]);
});

test("lower review and entry must stay identical while its prefix is collected", () => {
  const reviewQueue = groupQueue(chainedStack()),
    readReview = reviewQueue.reviewSnapshot.bind(reviewQueue);
  let reviewReads = 0;
  reviewQueue.reviewSnapshot = (repo, number) => {
    const review = readReview(repo, number);
    if (number === 42n && ++reviewReads === 2) review.approval = 99n;
    return review;
  };
  expect(() => reviewQueue.groupSnapshot(REPO, 43n)).toThrow(
    "lower queue prefix changed",
  );
  const entryQueue = groupQueue(chainedStack()),
    readEntry = entryQueue.entry.bind(entryQueue);
  let entryReads = 0;
  entryQueue.entry = (repo, number) => {
    const entry = readEntry(repo, number);
    if (number === 42n && ++entryReads === 2)
      entry!.enqueuedAt = "2026-10-19T00:00:01Z";
    return entry;
  };
  expect(() => entryQueue.groupSnapshot(REPO, 43n)).toThrow(
    "lower queue prefix changed",
  );
  const removed = groupQueue(chainedStack()),
    readBeforeRemoval = removed.entry.bind(removed);
  let reads = 0;
  removed.entry = (repo, number) =>
    number === 42n && ++reads === 2 ? null : readBeforeRemoval(repo, number);
  expect(() => removed.groupSnapshot(REPO, 43n)).toThrow("no longer queued");
});

test("shared stack candidate requires both independent layer admissions", () => {
  const options = stackGroup(),
    snapshot = group(options);
  expect(snapshot.conclusion).toBe("success");
  expect(
    array(snapshot.members).map((member) => field(member, "number")),
  ).toEqual([42n, 43n]);
  options.layers!.get(43n)!.review.reasons = ["security review incomplete"];
  expect(group(options).conclusion).toBe("failure");
});

test("stack group binds ordered members and rejects removed or unrelated entries", () => {
  const options = stackGroup();
  options.layers!.get(43n)!.entry = null;
  expect(group(options).conclusion).toBe("failure");
  const reordered = stackGroup();
  reordered.layers!.get(43n)!.entry!.position = 3n;
  expect(() => group(reordered)).toThrow(QueueWaiting);
  const unrelated = stackGroup();
  unrelated.entries!.push({ number: 44n, id: "other-entry" });
  expect(group(unrelated).conclusion).toBe("failure");
  const failed = stackGroup();
  failed.runs = RUNS.map((run) => ({ ...run, conclusion: "failure" }));
  expect(group(failed).conclusion).toBe("failure");
});

test("serial queue verifies the lower candidate without waiting for or reviewing a future layer", () => {
  const options = stackGroup();
  const upper = options.layers!.get(43n)!;
  upper.entry = { ...upper.entry!, state: "QUEUED", headCommit: null, baseCommit: null };
  upper.review.reasons = ["future layer review pending"];
  options.entries = [{ number: 42n, id: "entry-1" }];
  options.reviewReads = [];
  const prefix = group(options);
  expect(prefix.conclusion).toBe("success");
  expect(options.reviewReads).toEqual([42n]);
  expect(array(prefix.members).map(member => field(member, "number"))).toEqual([42n]);
  const before = binding(REPO, 42n, prefix, true);
  upper.review.approval = 99n;
  expect(binding(REPO, 42n, group(options), true)).toBe(before);
  options.runs = RUNS.map(run => ({ ...run, conclusion: "failure" }));
  expect(group(options).conclusion).toBe("failure");
});

test("upper candidate cannot omit its still-open lower dependency", () => {
  const options = stackGroup();
  options.number = 43n;
  options.layers!.get(42n)!.entry = null;
  expect(() => group(options)).toThrow("left the queue");
});

test("remaining upper layer uses the first queue position after its parent merges", () => {
  const options = stackGroup(), upper = options.layers!.get(43n)!;
  options.number = 43n;
  options.branch = BRANCH.replace("pr-42-", "pr-43-");
  options.runs = RUNS.map(run => ({ ...run, head_branch: options.branch! }));
  options.entries = [{ number: 43n, id: "entry-2" }];
  const view: Record = {
    ...UPPER_VIEW, baseRefName: "dev", baseRefOid: BASE,
    stack: { ...object(UPPER_VIEW.stack), parent: null, members: [array(field(UPPER_VIEW.stack, "members"))[1]] },
  };
  upper.review = { ...REVIEW, view };
  upper.entry = { ...upper.entry!, position: 1n, pullRequest: { number: 43n, headRefOid: view.headRefOid, baseRefName: "dev", baseRefOid: BASE } };
  options.reviewReads = [];
  expect(group(options).conclusion).toBe("success");
  expect(options.reviewReads).toEqual([43n]);
  const grouped = group(options);
  const { queue, gate } = server();
  gate.prView = () => view;
  let evaluations = 0;
  gate.main = () => { expect(++evaluations).toBe(1); return 0; };
  queue.entry = () => upper.entry;
  queue.reviewSnapshot = () => upper.review;
  queue.groupSnapshot = () => grouped;
  expect(queue.reconcile(REPO, 43n)).toBe(0);
  upper.review.reasons = ["review baseline is unrecorded or changed"];
  expect(group(options).conclusion).toBe("failure");
});

test("a shared upper tree cannot be relabeled as a lower-only prefix", () => {
  const options = stackGroup();
  options.layers!.get(43n)!.entry = null;
  options.entries = [{ number: 42n, id: "entry-1" }];
  options.commitChanges = { [HEAD]: { tree: { sha: OLD } } };
  expect(() => group(options)).toThrow("does not bind exactly the reviewed head");
});

test("stack evidence changes invalidate the group binding", () => {
  const before = group(stackGroup());
  const options = stackGroup();
  options.layers!.get(43n)!.review.approval = 11n;
  const after = group(options);
  expect(binding(REPO, 42n, before, true)).not.toBe(
    binding(REPO, 42n, after, true),
  );
});

test("stack members cannot borrow entries from a different queue", () => {
  const options = stackGroup();
  options.layers!.get(43n)!.entry!.mergeQueue = { id: "another-queue" };
  expect(() => group(options)).toThrow(QueueWaiting);
});

test("upper reentry invalidates a previously bound shared candidate", () => {
  const before = group(stackGroup());
  const options = stackGroup();
  options.layers!.get(43n)!.entry!.enqueuedAt = "2026-10-19T00:00:01Z";
  const after = group(options);
  expect(binding(REPO, 42n, before, true)).not.toBe(
    binding(REPO, 42n, after, true),
  );
});

test("stack queue compares the live parent while retaining each pinned diff baseline", () => {
  const options = stackGroup(),
    upper = options.layers!.get(43n)!;
  upper.review = { ...upper.review, view: { ...UPPER_VIEW, baseRefOid: OLD } };
  upper.entry = {
    ...upper.entry!,
    pullRequest: { ...object(upper.entry!.pullRequest), baseRefOid: OLD },
  };
  expect(group(options).conclusion).toBe("success");
  object(upper.review.view).stack = {
    ...object(UPPER_VIEW.stack),
    parent: { number: 42n, head: OLD, ref: "codex/lower" },
  };
  expect(() => group(options)).toThrow("stack members changed");
});
function admission(
  view: Record = VIEW,
  current: Record = object(REVIEW.evidence),
  approvals = [approval()],
) {
  const gate = fakeGate({ queueEnabled: true, gh: () => encode(approvals[0]) });
  gate.prView = () => view;
  gate.assessment = () => [{ ...current, comments: [summary()] }, []];
  gate.pages = () => approvals;
  const queue = new Queue(gate);
  queue.shaPrs = () => ({ heads: [42n], entries: [] });
  return queue;
}
function capture(queue: Queue) {
  const writes: {
    head: Json;
    conclusion: Json;
    snapshot: Record;
    id: bigint | null;
  }[] = [];
  queue.ownChecks = () => new Map();
  queue.setCheck = (_repo, head, _binding, conclusion, snapshot, id = null) => {
    writes.push({ head, conclusion, snapshot, id });
    return id ?? BigInt(writes.length + 7);
  };
  return writes;
}
function sequence<T>(items: (T | Error)[]): () => T {
  let index = 0;
  return () => {
    if (index >= items.length) throw new Error("unexpected extra read");
    const item = items[index++];
    if (item instanceof Error) throw item;
    return structuredClone(item);
  };
}
interface ServerOptions {
  records?: Record[];
  lost?: boolean;
  effect?: boolean;
  corrupt?: Record;
  duplicate?: boolean;
  eventPath?: string;
}
function server(options: ServerOptions = {}) {
  const records = structuredClone(options.records ?? []),
    writes: { args: string[]; payload: Record }[] = [],
    reads: string[] = [];
  const gate = fakeGate({
    queueEnabled: true,
    eventName: options.eventPath ? "workflow_run" : undefined,
    eventPath: options.eventPath,
    gh(args, input) {
      if (input !== undefined) {
        const payload = object(parse(input));
        writes.push({ args, payload: structuredClone(payload) });
        if (options.effect !== false) {
          let target: Record;
          if (args.includes("POST")) {
            const max = records.reduce(
              (max, record) =>
                (record.id as bigint) > max ? (record.id as bigint) : max,
              7n,
            );
            target = {
              id: max + 1n,
              app: { id: CHECK[1], slug: "cladiron" },
              conclusion: null,
            };
            records.push(target);
          } else {
            const id = BigInt(args.at(-3)!.split("/").at(-1)!);
            target = records.find((record) => record.id === id)!;
            expect(Object.hasOwn(payload, "head_sha")).toBe(false);
          }
          Object.assign(target, payload);
          if (target.conclusion !== null) target.status = "completed";
          object(target.output).annotations_count = 0n;
          if (options.duplicate)
            records.push({
              ...structuredClone(target),
              id: (target.id as bigint) + 1n,
            });
        }
        if (options.lost) throw new Error("response lost");
        return "{}";
      }
      reads.push(args.at(-1)!);
      const id = BigInt(args.at(-1)!.split("/").at(-1)!);
      return encode({
        ...structuredClone(records.find((record) => record.id === id)!),
        ...options.corrupt,
      });
    },
  });
  const queue = new Queue(gate);
  queue.checkRuns = (_repo, head) =>
    structuredClone(records.filter((record) => record.head_sha === head));
  return { queue, gate, records, writes, reads };
}
function checkIO(
  options: ServerOptions & { existing?: boolean } = {},
): unknown {
  const records = options.existing
    ? [
        {
          id: 8n,
          app: { id: CHECK[1], slug: "cladiron" },
          name: CHECK[0],
          head_sha: GROUP,
          status: "in_progress",
          conclusion: null,
        },
      ]
    : [];
  const { queue, writes } = server({ ...options, records });
  let error: unknown = null;
  try {
    queue.setCheck(
      REPO,
      GROUP,
      "bound-entry",
      "success",
      { clean: true },
      options.existing ? 8n : null,
    );
  } catch (caught) {
    error = caught;
  }
  expect(writes).toHaveLength(1);
  expect(writes[0].args).toContain(options.existing ? "PATCH" : "POST");
  return error;
}

test("exact single member candidate can pass", () => {
  expect(group().conclusion).toBe("success");
  expect(group()).toEqual(group({ queued: { ...ENTRY, state: "MERGEABLE" } }));
});
for (const queued of [false, true]) {
  for (const phase of ["before", "after"]) {
    test.each(CHECK_CHANGES)(
      `${queued ? "queue" : "admission"} check facts ${phase} publication: $name`,
      ({ before, after, stable }) => {
        const snapshot = (input: typeof before) => {
          const review = {
            ...REVIEW,
            evidence: {
              ...object(REVIEW.evidence),
              checks: checkEvidence(input.runs, input.statuses).checks,
            },
          };
          return queued
            ? group({ review, checks: input.runs, statuses: input.statuses })
            : review;
        };
        const first = snapshot(before),
          next = snapshot(after);
        const queue = admission();
        const writes = capture(queue);
        const read = sequence(
          phase === "before" ? [next, first] : [first, next],
        );
        if (queued) queue.groupSnapshot = read;
        else queue.reviewSnapshot = read;
        const publish = () => queue.publish(REPO, 42n, first, queued);
        if (!stable && phase === "after")
          expect(publish).toThrow(
            "candidate or review evidence changed during check publication",
          );
        else publish();
        expect(writes.map((write) => write.conclusion)).toEqual(
          stable
            ? ["success"]
            : phase === "before"
              ? ["failure"]
              : ["success", "failure"],
        );
        if (!stable && phase === "after") expect(writes[1].id).toBe(8n);
      },
    );
  }
}
test("next entry can use advanced target with unchanged PR review", () => {
  for (const baseRefOid of [BASE, OLD]) {
    const view = { ...VIEW, baseRefOid },
      review = {
        ...REVIEW,
        view,
        evidence: {
          ...object(REVIEW.evidence),
          integration: { base: OLD, merge_base: BASE },
        },
      };
    const queued = {
      ...ENTRY,
      baseCommit: { oid: OLD },
      pullRequest: { ...object(ENTRY.pullRequest), baseRefOid },
    };
    const result = group({
      queued,
      review,
      target: OLD,
      parents: [OLD],
      tree: "f".repeat(40),
    });
    expect(result.conclusion).toBe("success");
    const bound = object(parse(binding(REPO, 42n, result, true)));
    expect(field(bound.reviewed, "baseRefOid")).toBe(BASE);
    expect(field(bound.integration, "base")).toBe(OLD);
  }
});
test("advanced combination still requires exact platform binding", () => {
  const review = {
    ...REVIEW,
    evidence: {
      ...object(REVIEW.evidence),
      integration: { base: OLD, merge_base: BASE },
    },
  };
  const queued: Record = { ...ENTRY, baseCommit: { oid: OLD } },
    options = {
      review,
      queued,
      target: OLD,
      parents: [OLD],
      tree: "f".repeat(40),
    };
  for (const changes of [
    { ref: HEAD },
    { target: BASE },
    { parents: [BASE] },
    { parents: [OLD, HEAD] },
    {
      queued: {
        ...queued,
        pullRequest: { ...object(queued.pullRequest), headRefOid: OLD },
      },
    },
  ] as GroupOptions[])
    expect(() => group({ ...options, ...changes })).toThrow();
  for (const changes of [
    { review: { ...review, reasons: ["new finding"] } },
    { checks: [] },
  ] as GroupOptions[])
    expect(group({ ...options, ...changes }).conclusion).not.toBe("success");
});
test("queue entry drift and non single parent graphs cannot pass", () => {
  for (const queued of [
    null,
    { ...ENTRY, position: true },
    { ...ENTRY, baseCommit: { oid: OLD } },
    {
      ...ENTRY,
      pullRequest: { ...object(ENTRY.pullRequest), headRefOid: OLD },
    },
    { ...ENTRY, headCommit: { oid: HEAD } },
  ] as (Record | null)[])
    expect(() => group({ queued })).toThrow();
  for (const parents of [
    [],
    [OLD],
    [BASE, HEAD],
    [BASE, OLD],
    [BASE, HEAD, OLD],
  ])
    expect(() => group({ parents })).toThrow();
});
test("synchronized squash still requires reviewed tree", () => {
  for (const changes of [
    { tree: OLD },
    { tree: null },
    { reviewedTree: null },
    { commitChanges: { [GROUP]: { sha: OLD } } },
    { commitChanges: { [HEAD]: { sha: OLD } } },
  ] as GroupOptions[])
    expect(() => group(changes)).toThrow();
});
test("squash accepts identical baseline but requires integration evidence", () => {
  for (const integration of [null, { base: OLD, merge_base: BASE }])
    expect(() =>
      group({
        review: {
          ...REVIEW,
          evidence: { ...object(REVIEW.evidence), integration },
        },
      }),
    ).toThrow();
  expect(
    group({
      queued: {
        ...ENTRY,
        pullRequest: { ...object(ENTRY.pullRequest), headRefOid: BASE },
      },
      review: { ...REVIEW, view: { ...VIEW, headRefOid: BASE } },
    }).conclusion,
  ).toBe("success");
});
test("old queue ref or advanced target does not pass", () => {
  for (const changes of [{ ref: OLD }, { target: OLD }])
    expect(() => group(changes)).toThrow();
});
test("CI route requires repository exact SHA native workflow and ref", () => {
  for (const changed of [
    { event: "pull_request" },
    { head_sha: OLD },
    { repository: { full_name: "attacker/repository" } },
    { head_branch: "gh-readonly-queue/dev/pr-43-abcdef" },
  ] as Record[])
    expect(() =>
      group({ runs: [{ ...RUNS[0], ...changed }, RUNS[1]] }),
    ).toThrow();
  expect(
    group({
      runs: [{ ...RUNS[0], path: ".github/workflows/other.yml" }, RUNS[1]],
    }).conclusion,
  ).toBeNull();
});
test("queue event routes by read back run not notice fields", () => {
  const directory = mkdtempSync(join(tmpdir(), "review-event-")),
    path = join(directory, "event.json");
  try {
    writeFileSync(
      path,
      '{"workflow_run":{"event":"merge_group","id":2,"head_branch":"forged","pull_requests":[{"number":99}]}}',
    );
    const queue = new Queue(
      fakeGate({ eventName: "workflow_run", eventPath: path }),
    );
    queue.actions = () => RUNS[1];
    expect(queue.eventTargets()).toEqual([42n]);
    queue.actions = () => ({ ...RUNS[1], id: 9n });
    expect(() => queue.eventTargets()).toThrow();
    queue.actions = () => ({ ...RUNS[1], id: 2 });
    expect(queue.eventTargets()).toEqual([42n]);
    let reads = 0;
    queue.actions = () => {
      reads++;
      return RUNS[1];
    };
    for (const id of [2, true, 0n, -1n, null]) {
      writeFileSync(
        path,
        encode({ workflow_run: { event: "merge_group", id } }),
      );
      expect(() => queue.eventTargets()).toThrow("no exact ID");
    }
    expect(reads).toBe(0);
    writeFileSync(path, Buffer.from([255]));
    expect(() => queue.eventTargets()).toThrow();
    expect(reads).toBe(0);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});
test("incomplete CI holds without success and failed CI fails", () => {
  for (const queued of [
    { ...ENTRY, position: 2n },
    { ...ENTRY, state: "QUEUED" },
  ])
    expect(() => group({ queued })).toThrow(QueueWaiting);
  expect(() => group({ runs: [] })).toThrow(QueueWaiting);
  expect(group({ runs: [RUNS[0]] }).conclusion).toBeNull();
  expect(
    group({
      runs: [{ ...RUNS[0], status: "in_progress", conclusion: null }, RUNS[1]],
    }).conclusion,
  ).toBeNull();
  expect(group({ checks: [run("docs")] }).conclusion).toBeNull();
  expect(
    group({ runs: [{ ...RUNS[0], conclusion: "failure" }, RUNS[1]] })
      .conclusion,
  ).toBe("failure");
  expect(
    group({ review: { ...REVIEW, reasons: ["new finding"] } }).conclusion,
  ).toBe("failure");
});
test("Halo notifications read the exact run before selecting a queue target", () => {
  const directory = mkdtempSync(join(tmpdir(), "halo-notice-")),
    path = join(directory, "event.json");
  try {
    writeFileSync(
      path,
      encode({
        workflow_run: {
          event: "merge_group",
          id: 3n,
          head_branch: "forged",
          pull_requests: [{ number: 99n }],
        },
      }),
    );
    const queue = new Queue(
      fakeGate({ eventName: "workflow_run", eventPath: path }),
    );
    const halo = { ...RUNS[0], id: 3n, path: ".github/workflows/halo.yml" };
    for (const workflow of [halo.path, `${halo.path}@refs/heads/dev`]) {
      queue.actions = (route, paginate) => {
        expect(route).toBe(`repos/${REPO}/actions/runs/3`);
        expect(paginate).toBeUndefined();
        return { ...halo, path: workflow };
      };
      expect(queue.eventTargets()).toEqual([42n]);
    }
    for (const changed of [
      { id: 4n },
      { event: "pull_request" },
      { repository: { full_name: "attacker/repository" } },
      { head_sha: "short" },
      { head_branch: "gh-readonly-queue/main/pr-42-abcdef" },
      { path: ".github/workflows/halo.yml-copy" },
      { path: ".github/workflows/other.yml" },
    ] as Record[]) {
      queue.actions = () => ({ ...halo, ...changed });
      expect(() => queue.eventTargets()).toThrow();
    }
    queue.actions = () => {
      throw new Error("run read failed");
    };
    expect(() => queue.eventTargets()).toThrow("run read failed");
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});
test("queue waits for Halo and rejects its failed or cancelled workflow", () => {
  const withoutHalo = RUNS.slice(0, 2);
  expect(group().conclusion).toBe("success");
  expect(group({ runs: withoutHalo }).conclusion).toBeNull();
  for (const status of ["queued", "in_progress"])
    expect(
      group({
        runs: [...withoutHalo, { ...RUNS[2], status, conclusion: null }],
      }).conclusion,
    ).toBeNull();
  for (const conclusion of ["failure", "cancelled", "timed_out"])
    expect(
      group({ runs: [...withoutHalo, { ...RUNS[2], conclusion }] }).conclusion,
    ).toBe("failure");
});
test("queue requires the exact website check even when Halo reports success", () => {
  const other = [run("docs"), run("rust")];
  for (const website of [
    [],
    [run("website", "in_progress", null)],
    [run("website", "completed", "failure")],
    [run("website", "completed", "cancelled")],
    [run("website", "completed", "success", 999n)],
  ])
    expect(group({ checks: [...other, ...website] }).conclusion).toBeNull();
  expect(
    group({
      checks: other,
      statuses: [{ context: "website", state: "success" }],
    }).conclusion,
  ).toBeNull();
});
test("queue binds Halo to its candidate and selects the latest attempt", () => {
  const other = RUNS.slice(0, 2);
  for (const changed of [
    { repository: { full_name: "attacker/repository" } },
    { event: "pull_request" },
    { head_sha: OLD },
    { head_branch: "gh-readonly-queue/dev/pr-43-abcdef" },
    { head_branch: "gh-readonly-queue/dev/pr-42-fedcba" },
  ] as Record[])
    expect(() =>
      group({ runs: [...other, { ...RUNS[2], ...changed }] }),
    ).toThrow();
  expect(
    group({
      runs: [...other, { ...RUNS[2], path: ".github/workflows/halo.yml-copy" }],
    }).conclusion,
  ).toBeNull();
  for (const newer of [{ id: 4n }, { run_attempt: 2n }] as Record[]) {
    const failed = { ...RUNS[2], ...newer, conclusion: "failure" };
    for (const runs of [
      [...RUNS, failed],
      [failed, ...RUNS],
    ])
      expect(group({ runs }).conclusion).toBe("failure");
  }
});
test("Halo completion refreshes a drifted admission without reusing its failed check", () => {
  const directory = mkdtempSync(join(tmpdir(), "halo-refresh-")),
    path = join(directory, "event.json");
  try {
    writeFileSync(
      path,
      encode({ workflow_run: { event: "merge_group", id: 3n } }),
    );
    const { queue, gate, records } = server({ eventPath: path });
    gate.prView = () => VIEW;
    gate.main = () => 0;
    queue.entry = () => null;
    const snapshot = (complete: boolean): Record => ({
      ...REVIEW,
      evidence: {
        ...object(REVIEW.evidence),
        checks: headChecks(
          [
            run("rust"),
            run("docs"),
            run(
              "website",
              complete ? "completed" : "in_progress",
              complete ? "success" : null,
            ),
          ],
          [],
        ),
      },
    });
    const pending = snapshot(false),
      completed = snapshot(true);
    queue.reviewSnapshot = sequence([pending, completed]);
    expect(queue.reconcile(REPO, 42n)).toBe(0);
    const failed = structuredClone(records[0]);
    expect(failed).toMatchObject({ head_sha: HEAD, conclusion: "failure" });
    queue.actions = () => ({
      ...RUNS[0],
      id: 3n,
      path: ".github/workflows/halo.yml",
    });
    const targets = queue.eventTargets();
    expect(targets).toEqual([42n]);
    queue.reviewSnapshot = () => completed;
    for (const number of targets) expect(queue.reconcile(REPO, number)).toBe(0);
    expect(records).toHaveLength(2);
    expect(records[0]).toEqual(failed);
    expect(records[1]).toMatchObject({ head_sha: HEAD, conclusion: "success" });
    expect(records[1].id).not.toBe(failed.id);
    queue.reviewSnapshot = () => ({
      ...completed,
      reasons: ["new unresolved finding"],
    });
    for (const number of targets) expect(queue.reconcile(REPO, number)).toBe(0);
    expect(records[2]).toMatchObject({ head_sha: HEAD, conclusion: "failure" });
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});
test("neutral and skipped upstream CI preserve required check policy", () => {
  for (const conclusion of ["success", "neutral", "skipped"])
    expect(
      group({
        runs: RUNS.map((item) => ({ ...item, conclusion })),
        checks: [
          run("docs", "completed", conclusion),
          run("rust", "completed", conclusion),
          run("website", "completed", conclusion),
        ],
      }).conclusion,
    ).toBe("success");
});
test("shared PR heads never publish success regardless of processing order", () => {
  for (const order of [
    [42n, 43n],
    [43n, 42n],
  ])
    for (const number of order) {
      const queue = admission();
      if (number === 43n)
        queue.gate.assessment = () => [
          object(REVIEW.evidence),
          ["new finding"],
        ];
      queue.shaPrs = () => ({ heads: [42n, 43n], entries: [] });
      const snapshot = queue.reviewSnapshot(REPO, number),
        writes = capture(queue);
      queue.reviewSnapshot = () => snapshot;
      queue.publish(REPO, number, snapshot);
      expect(writes.map((item) => item.conclusion)).toEqual(["failure"]);
    }
});
test("group SHA also used as PR head cannot publish success", () => {
  expect(group({ owners: [99n] }).conclusion).toBe("failure");
  for (const entries of [
    [],
    [{ number: 42n, id: "old-entry" }],
    [
      { number: 42n, id: ENTRY.id },
      { number: 43n, id: "entry-2" },
    ],
  ])
    expect(group({ entries }).conclusion).toBe("failure");
});
test("PR and queue using same SHA are held in both processing orders", () => {
  const view = { ...VIEW, headRefOid: GROUP };
  const queue = admission(
    view,
    { ...object(REVIEW.evidence), reviewed: view },
    [{ ...approval(), id: 8n, commit_id: GROUP, body: approvalBody(view) }],
  );
  queue.shaPrs = () => ({
    heads: [43n],
    entries: [{ number: 42n, id: ENTRY.id }],
  });
  const snapshot = queue.reviewSnapshot(REPO, 43n),
    grouped = group({ owners: [43n] });
  for (const order of [
    [false, true],
    [true, false],
  ]) {
    const writes = capture(queue);
    queue.reviewSnapshot = () => snapshot;
    queue.groupSnapshot = () => grouped;
    for (const queued of order)
      queue.publish(
        REPO,
        queued ? 42n : 43n,
        queued ? grouped : snapshot,
        queued,
      );
    expect(writes.map((item) => item.head)).toEqual([GROUP, GROUP]);
    expect(writes.map((item) => item.conclusion)).toEqual([
      "failure",
      "failure",
    ]);
  }
});
function pullsResponse(
  nodes: Json[],
  total: Json = 3n,
  more: Json = false,
  cursor: Json = null,
  repo = REPO,
): Record {
  return {
    data: {
      repository: {
        nameWithOwner: repo,
        pullRequests: {
          totalCount: total,
          nodes,
          pageInfo: { hasNextPage: more, endCursor: cursor },
        },
      },
    },
  };
}
const pull = (number: bigint, head = HEAD): Record => ({
  number,
  headRefOid: head,
  baseRefName: "dev",
  state: "OPEN",
  mergeQueueEntry: null,
  stack: null,
});
test("same-SHA isolation includes upper layers and fails closed on missing membership", () => {
  const node = { ...pull(43n), baseRefName: "codex/lower", stack: { baseRefName: "dev" } } as Record;
  const queue = new Queue(fakeGate({ gh: () => encode(pullsResponse([pull(42n), node], 2n)) }));
  expect(queue.shaPrs(REPO, HEAD)).toEqual({ heads: [42n, 43n], entries: [] });
  delete node.stack;
  expect(() => queue.shaPrs(REPO, HEAD)).toThrow("missing stack");
});
test("metadata binds generated candidates and skips only explicit waiting heads", () => {
  const node = {
    ...pull(42n),
    mergeQueueEntry: {
      id: ENTRY.id,
      state: "AWAITING_CHECKS",
      headCommit: { oid: GROUP },
    } as Record,
  };
  const queue = new Queue(
    fakeGate({ gh: () => encode(pullsResponse([node], 1n)) }),
  );
  expect(queue.shaPrs(REPO, GROUP)).toEqual({
    heads: [],
    entries: [{ number: 42n, id: ENTRY.id }],
  });
  node.mergeQueueEntry.headCommit = null;
  expect(() => queue.shaPrs(REPO, GROUP)).toThrow();
  node.mergeQueueEntry.state = "QUEUED";
  expect(queue.shaPrs(REPO, GROUP)).toEqual({ heads: [], entries: [] });
  expect(queue.shaPrs(REPO, HEAD)).toEqual({ heads: [42n], entries: [] });
  delete node.mergeQueueEntry.headCommit;
  expect(() => queue.shaPrs(REPO, GROUP)).toThrow();
});
test("same SHA identity query reads all pages without review content", () => {
  const first = pullsResponse([pull(42n), pull(44n, OLD)], 3n, true, "next"),
    final = pullsResponse([pull(43n)]);
  const values = [first, final],
    calls: string[][] = [];
  const queue = new Queue(
    fakeGate({
      gh(args) {
        calls.push(args);
        return encode(values.shift()!);
      },
    }),
  );
  expect(queue.shaPrs(REPO, HEAD)).toEqual({ heads: [42n, 43n], entries: [] });
  expect(calls.at(-1)).toContain("after=next");
  expect(calls[0].find((arg) => arg.startsWith("query="))).not.toContain(
    "reviewThreads",
  );
  for (const replies of [
    [pullsResponse([pull(42n)], 2n)],
    [first, first],
    [pullsResponse([pull(42n)], 1n, false, null, "attacker/repository")],
    [first, pullsResponse([pull(43n)], 2n)],
    [{ errors: [{ message: "partial response" }] }],
  ])
    expect(() =>
      new Queue(fakeGate({ gh: () => encode(replies.shift()!) })).shaPrs(
        REPO,
        HEAD,
      ),
    ).toThrow();
});
test("second PR appearing during publication invalidates old success", () => {
  const queue = admission(),
    writes = capture(queue);
  queue.shaPrs = sequence([
    { heads: [42n], entries: [] },
    { heads: [42n], entries: [] },
    { heads: [42n, 43n], entries: [] },
  ]);
  const initial = queue.reviewSnapshot(REPO, 42n);
  expect(() => queue.publish(REPO, 42n, initial)).toThrow();
  expect(writes.map((item) => item.conclusion)).toEqual(["success", "failure"]);
});
test("own check does not create dependency or change snapshot", () => {
  expect(group()).toEqual(
    group({
      checks: [
        run("docs"),
        run("rust"),
        run("website"),
        run(CHECK[0], "completed", "success", CHECK[1]),
      ],
    }),
  );
  const gate = fakeGate({ queueEnabled: true });
  expect(gate.prerequisites(QUEUE_RULES, VIEW)).not.toContainEqual(CHECK);
  const changed = structuredClone(QUEUE_RULES);
  array(
    field(field(changed.at(-1), "parameters"), "required_status_checks"),
  ).push(
    { context: CHECK[0], integration_id: 99n },
    { context: "other Cladiron check", integration_id: CHECK[1] },
  );
  expect(gate.prerequisites(changed, VIEW)).toContainEqual([CHECK[0], 99n]);
  expect(gate.prerequisites(changed, VIEW)).toContainEqual([
    "other Cladiron check",
    CHECK[1],
  ]);
  expect(() => contract(gate, changed)).toThrow();
  expect(
    gate.prerequisites(QUEUE_RULES, { ...VIEW, baseRefName: "main" }),
  ).not.toContainEqual(CHECK);
});
test("queue contract rejects missing queue or wrong required producer", () => {
  const gate = fakeGate();
  contract(gate, QUEUE_RULES);
  for (const [type, key, value] of [
    ["merge_queue", "grouping_strategy", "HEADGREEN"],
    ["merge_queue", "max_entries_to_build", 2n],
    ["required_status_checks", "strict_required_status_checks_policy", false],
  ] as [string, string, Json][]) {
    const changed = structuredClone(QUEUE_RULES);
    object(
      field(
        changed.find((rule) => field(rule, "type") === type),
        "parameters",
      ),
    )[key] = value;
    expect(() => contract(gate, changed)).toThrow();
  }
  for (const rules of [
    QUEUE_RULES.filter((rule) => field(rule, "type") !== "merge_queue"),
    QUEUE_RULES.slice(0, -1),
  ])
    expect(() => contract(gate, rules)).toThrow();
  for (const producer of [null, 999n, "missing"] as Json[]) {
    const changed = structuredClone(QUEUE_RULES);
    const parameters = object(field(changed.at(-1), "parameters"));
    parameters.required_status_checks = array(parameters.required_status_checks)
      .filter(
        (check) =>
          producer !== "missing" || field(check, "context") !== "website",
      )
      .map((check) =>
        field(check, "context") === "website"
          ? { ...object(check), integration_id: producer }
          : check,
      );
    expect(() => contract(gate, changed)).toThrow("dev must require");
  }
});
function entryResponse(value: Json): Record {
  return {
    data: {
      repository: {
        nameWithOwner: REPO,
        pullRequest: { number: 42n, mergeQueueEntry: value },
      },
    },
  };
}
test("entry query rejects wrong repo PR and partial evidence", () => {
  expect(
    new Queue(fakeGate({ gh: () => encode(entryResponse(ENTRY)) })).entry(
      REPO,
      42n,
    ),
  ).toEqual(ENTRY);
  const wrong = entryResponse(ENTRY);
  object(field(wrong.data, "repository")).nameWithOwner = "attacker/repository";
  const wrongPR = entryResponse(ENTRY);
  object(field(field(wrongPR.data, "repository"), "pullRequest")).number = 43n;
  for (const value of [{ errors: [{}] }, wrong, wrongPR])
    expect(() =>
      new Queue(fakeGate({ gh: () => encode(value) })).entry(REPO, 42n),
    ).toThrow();
});
test("queued entry without revisions waits without inventing group SHA", () => {
  const waiting = {
    ...ENTRY,
    state: "QUEUED",
    headCommit: null,
    baseCommit: null,
  };
  expect(
    new Queue(fakeGate({ gh: () => encode(entryResponse(waiting)) })).entry(
      REPO,
      42n,
    ),
  ).toEqual(waiting);
  expect(() => group({ queued: waiting })).toThrow(QueueWaiting);
  const queue = new Queue(fakeGate()),
    writes = capture(queue);
  expect(queue.holdEntry(REPO, 42n, VIEW, waiting)).toBeNull();
  expect(writes).toEqual([]);
  for (const changes of [
    { state: "AWAITING_CHECKS" },
    { baseCommit: { oid: BASE } },
    { headCommit: {} },
    { headCommit: { oid: "invalid" } },
  ] as Record[])
    expect(() =>
      new Queue(
        fakeGate({
          gh: () => encode(entryResponse({ ...waiting, ...changes })),
        }),
      ).entry(REPO, 42n),
    ).toThrow();
  for (const key of ["headCommit", "baseCommit"]) {
    const incomplete: Record = { ...waiting };
    delete incomplete[key];
    expect(() =>
      new Queue(
        fakeGate({ gh: () => encode(entryResponse(incomplete)) }),
      ).entry(REPO, 42n),
    ).toThrow();
  }
});
test("review admission requires current reopen binding and fresh approval", () => {
  const queue = admission(),
    confirmations: Json[] = [];
  queue.gate.confirmReview = (...args) => {
    confirmations.push(args[4]!);
  };
  expect(queue.reviewSnapshot(REPO, 42n).reasons).toEqual([]);
  expect(confirmations).toHaveLength(1);
  const reopened = {
    ...VIEW,
    lifecycle: { type: "ReopenedEvent", id: "event-2" },
  };
  const next = admission(reopened, {
    ...object(REVIEW.evidence),
    reviewed: reopened,
  });
  next.gate.confirmReview = () => {
    throw new Error("must not confirm");
  };
  expect(next.reviewSnapshot(REPO, 42n).reasons).not.toEqual([]);
});
test("admission after target advance requires approval for original review inputs", () => {
  const view = { ...VIEW, baseRefOid: OLD },
    current = {
      ...object(REVIEW.evidence),
      integration: { base: OLD, merge_base: BASE },
    };
  for (const [prior, valid] of [
    [approval(), true],
    [{ ...approval(), body: approvalBody(view) }, false],
    [{ ...approval(), state: "DISMISSED" }, false],
  ] as [Record, boolean][]) {
    const queue = admission(view, current, [prior]),
      confirmed: Json[] = [];
    queue.gate.confirmReview = (...args) => {
      confirmed.push(args[4]!);
    };
    expect(array(queue.reviewSnapshot(REPO, 42n).reasons).length === 0).toBe(
      valid,
    );
    expect(confirmed).toEqual(valid ? [VIEW] : []);
  }
});
test("admission requires the current contract approval", () => {
  const old =
    `Review gate: both Codex reviews completed on ${HEAD} against dev at ${BASE}, ` +
    "with no new findings, all threads resolved and required checks passing.\nReopen event: none";
  for (const [body, valid] of [
    [approval().body, true],
    [old, false],
    [
      string(approval().body)
        .replace("nucleus-review-gate:v4", "nucleus-review-gate:v3")
        .replace(
          "Finding handlers record dispositions in the original review threads. " +
            "Routine dev queue merges consume the current admission result.",
          "The merge operator must verify dispositions before merge or enqueue.",
        ),
      false,
    ],
  ] as [Json, boolean][])
    expect(
      array(
        admission(VIEW, object(REVIEW.evidence), [
          { ...approval(), body },
        ]).reviewSnapshot(REPO, 42n).reasons,
      ).length === 0,
    ).toBe(valid);
});
test("bindings distinguish shared head PRs and same second queue reentry", () => {
  const first = group();
  expect(binding(REPO, 42n, first, true)).not.toBe(
    binding(REPO, 43n, first, true),
  );
  let second = structuredClone(first);
  object(second.entry).id = "entry-2";
  expect(binding(REPO, 42n, first, true)).not.toBe(
    binding(REPO, 42n, second, true),
  );
  second = structuredClone(first);
  object(field(second.review, "view")).lifecycle = {
    type: "ReopenedEvent",
    id: "event-2",
  };
  expect(binding(REPO, 42n, first, true)).not.toBe(
    binding(REPO, 42n, second, true),
  );
});
test("changes before or after success hold exact original candidate", () => {
  const first = group(),
    changed = structuredClone(first);
  object(field(changed.review, "view")).lifecycle = {
    type: "ReopenedEvent",
    id: "event-2",
  };
  for (const [reads, expected] of [
    [[changed], ["failure"]],
    [
      [first, changed],
      ["success", "failure"],
    ],
    [[new Error("read unavailable")], ["failure"]],
    [
      [first, new Error("read unavailable")],
      ["success", "failure"],
    ],
  ] as [(Record | Error)[], string[]][]) {
    const queue = new Queue(fakeGate()),
      writes = capture(queue);
    queue.groupSnapshot = sequence(reads);
    queue.gate.prView = () => VIEW;
    try {
      queue.publish(REPO, 42n, first, true);
    } catch {
      /* Assert exact failure writes below. */
    }
    expect(writes.map((item) => item.conclusion)).toEqual(expected);
    expect(writes.every((item) => item.head === GROUP)).toBe(true);
  }
});
test("CI completion during prepublication read is normal progress", () => {
  const pending = group({
      runs: [{ ...RUNS[0], status: "in_progress", conclusion: null }, RUNS[1]],
    }),
    completed = group();
  const queue = new Queue(fakeGate()),
    writes = capture(queue);
  queue.groupSnapshot = sequence([completed, completed]);
  queue.publish(REPO, 42n, pending, true);
  expect(writes.map((item) => item.conclusion)).toEqual(["success"]);
});
test("merge after confirmed check does not rewrite historical success", () => {
  const completed = group(),
    queue = new Queue(fakeGate()),
    writes = capture(queue);
  queue.groupSnapshot = sequence([completed, new Error("entry removed")]);
  queue.gate.prView = () => ({ ...VIEW, state: "MERGED" });
  queue.publish(REPO, 42n, completed, true);
  expect(writes.map((item) => item.conclusion)).toEqual(["success"]);
});
test("new evaluation holds new run and finishes exact run", () => {
  for (const previous of ["success", "failure"]) {
    const old = {
      id: 8n,
      app: { id: CHECK[1], slug: "cladiron" },
      name: CHECK[0],
      head_sha: GROUP,
      status: "completed",
      conclusion: previous,
    };
    const { queue, records, writes } = server({ records: [old] });
    const id = queue.setCheck(REPO, GROUP, "bound-entry", null, {
      evaluating: true,
    });
    expect(records[0]).toEqual(old);
    expect(id).toBe(9n);
    expect(records[1].status).toBe("in_progress");
    expect(records[1].conclusion).toBeNull();
    expect(field(headChecks(records, [])[0], "passed")).toBe(false);
    queue.setCheck(REPO, GROUP, "bound-entry", "success", { clean: true }, id);
    expect(records).toHaveLength(2);
    expect(records[1].conclusion).toBe("success");
    expect(field(headChecks(records, [])[0], "passed")).toBe(true);
    expect(writes[0].args).toContain("POST");
    expect(writes[1].args).toContain("PATCH");
  }
});
test("completed run cannot wait again or supersede newer evaluation", () => {
  const { queue, records, writes } = server(),
    old = queue.setCheck(REPO, GROUP, "entry-1", "success", {});
  expect(() => queue.setCheck(REPO, GROUP, "entry-1", null, {}, old)).toThrow();
  expect(writes).toHaveLength(1);
  const latest = queue.setCheck(REPO, GROUP, "entry-2", null, {});
  for (const conclusion of ["success", "failure"])
    expect(() =>
      queue.setCheck(REPO, GROUP, "entry-1", conclusion, {}, old),
    ).toThrow();
  expect(writes).toHaveLength(2);
  expect(records.at(-1)!.id).toBe(latest);
  expect(records.at(-1)!.conclusion).toBeNull();
});
test("duplicate queue notifications preserve the successful check without writes", () => {
  const grouped = group();
  for (const queued of [null, ENTRY]) {
    const { queue, gate, records, writes } = server();
    let evaluations = 0;
    gate.prView = () => VIEW;
    gate.main = () => {
      evaluations++;
      return 0;
    };
    queue.entry = () => queued;
    queue.reviewSnapshot = () => REVIEW;
    queue.groupSnapshot = () => grouped;
    expect(queue.reconcile(REPO, 42n)).toBe(0);
    const published = structuredClone({ records, writes });
    expect(queue.reconcile(REPO, 42n)).toBe(0);
    expect(evaluations).toBe(queued === null ? 2 : 1);
    if (queued !== null) expect({ records, writes }).toEqual(published);
    expect(records.filter((item) => item.head_sha === HEAD)).toHaveLength(
      queued === null ? 2 : 1,
    );
    expect(records.filter((item) => item.head_sha === GROUP)).toHaveLength(
      queued === null ? 0 : 1,
    );
    expect(records.every((item) => item.conclusion === "success")).toBe(true);
    expect(
      writes
        .filter(({ payload }) => payload.status === "in_progress")
        .map(({ payload }) => payload.head_sha),
    ).toEqual(queued === null ? [HEAD, HEAD] : [GROUP]);
  }
});
test("upper notification reuses the canonical shared stack check", () => {
  const options = stackGroup(), grouped = group(options);
  const { queue, gate, records, writes } = server();
  gate.prView = (_repo, number) => number === 42n ? LOWER_VIEW : UPPER_VIEW;
  gate.main = () => 0;
  queue.entry = (_repo, number) => options.layers!.get(number)!.entry;
  queue.reviewSnapshot = (_repo, number) => options.layers!.get(number)!.review;
  queue.groupSnapshot = (_repo, number) => {
    expect(number).toBe(42n);
    return grouped;
  };
  expect(queue.reconcile(REPO, 42n)).toBe(0);
  const before = structuredClone({ records, writes });
  expect(queue.reconcile(REPO, 43n)).toBe(0);
  expect({ records, writes }).toEqual(before);
});
test("upper clean rereview renews approval before shared group admission", () => {
  const options = stackGroup();
  const upper = options.layers!.get(43n)!.review;
  upper.approval = null;
  upper.reasons = ["no current event-bound Cladiron approval"];
  const { queue, gate, records } = server();
  gate.prView = (_repo, number) => number === 42n ? LOWER_VIEW : UPPER_VIEW;
  const evaluated: bigint[] = [];
  gate.main = number => {
    evaluated.push(number);
    if (number === 43n) {
      upper.approval = 11n;
      upper.reasons = [];
    }
    return 0;
  };
  queue.entry = (_repo, number) => options.layers!.get(number)!.entry;
  queue.reviewSnapshot = (_repo, number) => options.layers!.get(number)!.review;
  queue.groupSnapshot = () => group(options);
  expect(queue.reconcile(REPO, 43n)).toBe(0);
  expect(evaluated).toEqual([43n, 42n]);
  expect(records.filter(record => record.head_sha === GROUP).at(-1)!.conclusion).toBe("success");
});
test("queue reuse verifies current evidence before retaining success", () => {
  const clean = group();
  for (const current of [
    group({ review: { ...REVIEW, approval: null, reasons: ["new finding"] } }),
    group({
      runs: RUNS.map((run) => ({
        ...run,
        status: "in_progress",
        conclusion: null,
      })),
    }),
    new Error("review read unavailable"),
  ]) {
    const { queue, gate, records, writes } = server();
    gate.prView = () => VIEW;
    gate.main = () => 0;
    queue.entry = () => ENTRY;
    queue.reviewSnapshot = () => REVIEW;
    queue.groupSnapshot = () => clean;
    expect(queue.reconcile(REPO, 42n)).toBe(0);
    const original = records.find((record) => record.head_sha === GROUP)!;
    const count = writes.length;
    queue.groupSnapshot = () => {
      if (current instanceof Error) throw current;
      return current;
    };
    gate.main = () => {
      const latest = records
        .filter((record) => record.head_sha === GROUP)
        .at(-1)!;
      expect(latest.id).not.toBe(original.id);
      expect(latest.status).toBe("in_progress");
      return 0;
    };
    expect(queue.reconcile(REPO, 42n)).toBe(current instanceof Error ? 1 : 0);
    expect(writes[count].payload.status).toBe("in_progress");
    const latest = records
      .filter((record) => record.head_sha === GROUP)
      .at(-1)!;
    expect(latest.conclusion).toBe(
      current instanceof Error ? "failure" : current.conclusion,
    );
  }
});
test("same-SHA reentry and changed candidates require a new evaluation", () => {
  for (const change of [
    {
      queued: { ...ENTRY, id: "entry-2", enqueuedAt: "2026-10-19T00:00:01Z" },
      view: VIEW,
    },
    {
      queued: {
        ...ENTRY,
        pullRequest: { ...object(ENTRY.pullRequest), headRefOid: OLD },
      },
      view: { ...VIEW, headRefOid: OLD },
    },
    {
      queued: ENTRY,
      view: { ...VIEW, lifecycle: { id: "reopened-2", type: "ReopenedEvent" } },
    },
    {
      queued: {
        ...ENTRY,
        baseCommit: { oid: OLD },
        pullRequest: { ...object(ENTRY.pullRequest), baseRefOid: OLD },
      },
      view: { ...VIEW, baseRefOid: OLD },
    },
  ]) {
    const { queue, gate, records, writes } = server();
    let queued = ENTRY,
      view = VIEW,
      review = REVIEW,
      snapshot = group();
    gate.prView = () => view;
    gate.main = () => 0;
    queue.entry = () => queued;
    queue.reviewSnapshot = () => review;
    queue.groupSnapshot = () => snapshot;
    expect(queue.reconcile(REPO, 42n)).toBe(0);
    const count = writes.length;
    queued = change.queued;
    view = change.view;
    review = {
      ...REVIEW,
      view,
      evidence: {
        ...object(REVIEW.evidence),
        reviewed: view,
        integration: { base: view.baseRefOid, merge_base: BASE },
      },
    };
    snapshot = group({
      queued,
      review,
      target: view.baseRefOid,
      parents: [view.baseRefOid],
    });
    expect(queue.reconcile(REPO, 42n)).toBe(0);
    expect(writes[count].payload).toMatchObject({
      head_sha: GROUP,
      status: "in_progress",
    });
    const checks = records.filter((record) => record.head_sha === GROUP);
    expect(checks).toHaveLength(2);
    expect(checks[1].external_id).not.toBe(checks[0].external_id);
  }
});
test("queue reuse rejects changes between reads and ignores older success", () => {
  for (const change of [
    "evidence",
    "newer check",
    "output",
    "readback",
  ] as const) {
    const { queue, gate, records, writes } = server();
    const clean = group();
    gate.prView = () => VIEW;
    gate.main = () => 0;
    queue.entry = () => ENTRY;
    queue.reviewSnapshot = () => REVIEW;
    queue.groupSnapshot = () => clean;
    expect(queue.reconcile(REPO, 42n)).toBe(0);
    const count = writes.length;
    const original = records.find((record) => record.head_sha === GROUP)!;
    if (change === "evidence") {
      const changed = group({
        review: { ...REVIEW, approval: null, reasons: ["new finding"] },
      });
      let reads = 0;
      queue.groupSnapshot = () => (++reads === 1 ? clean : changed);
    } else if (change === "newer check") {
      let reads = 0;
      queue.groupSnapshot = () => {
        if (++reads === 2)
          records.push({
            ...structuredClone(original),
            id: 100n,
            status: "completed",
            conclusion: "failure",
          });
        return clean;
      };
    } else if (change === "output") {
      object(original.output).summary = "unbound success";
    } else {
      const gh = gate.gh.bind(gate);
      let first = true;
      gate.gh = (...args) => {
        if (first) {
          first = false;
          throw new Error("check read unavailable");
        }
        return gh(...args);
      };
    }
    gate.main = () => 1;
    expect(queue.reconcile(REPO, 42n)).toBe(1);
    expect(writes[count].payload).toMatchObject({
      head_sha: GROUP,
      status: "in_progress",
    });
    expect(
      records.filter((record) => record.head_sha === GROUP).at(-1)!.conclusion,
    ).toBeNull();
  }
});
test("merge during duplicate notification retains the historical success", () => {
  const { queue, gate, records, writes } = server();
  let view = VIEW;
  gate.prView = () => view;
  gate.main = () => 0;
  queue.entry = () => ENTRY;
  queue.reviewSnapshot = () => REVIEW;
  queue.groupSnapshot = () => group();
  expect(queue.reconcile(REPO, 42n)).toBe(0);
  const published = structuredClone({ records, writes });
  queue.groupSnapshot = () => {
    view = { ...VIEW, state: "MERGED" };
    throw new Error("queue entry no longer exists");
  };
  expect(queue.reconcile(REPO, 42n)).toBe(0);
  expect({ records, writes }).toEqual(published);
});
test("queue readiness change does not start another evaluation", () => {
  const { queue, gate, records } = server();
  gate.prView = () => VIEW;
  gate.main = () => 0;
  queue.entry = sequence([{ ...ENTRY, state: "MERGEABLE" }, ENTRY]);
  queue.reviewSnapshot = () => REVIEW;
  const grouped = group();
  queue.groupSnapshot = () => grouped;
  expect(queue.reconcile(REPO, 42n)).toBe(0);
  expect(records.filter((record) => record.head_sha === GROUP)).toHaveLength(1);
});
for (const queued of [
  null,
  { ...ENTRY, state: "QUEUED", headCommit: null, baseCommit: null },
  ENTRY,
]) {
  for (const existing of [false, true]) {
    test(`head change retires the original ${queued === null ? "admission" : queued.headCommit === null ? "waiting entry" : "queue"} check with new-head history ${existing}`, () => {
      const { queue, gate, records, writes, reads } = server();
      let view = VIEW,
        original: Record | undefined,
        other: Record | undefined;
      gate.prView = () => view;
      queue.entry = () => queued;
      gate.main = () => {
        original = structuredClone(records[0]);
        if (existing) {
          queue.setCheck(REPO, OLD, "another evaluation", null, {});
          other = structuredClone(records[1]);
        }
        view = { ...VIEW, headRefOid: OLD };
        return 0;
      };
      queue.reviewSnapshot = () => ({ ...REVIEW, view });
      expect(queue.reconcile(REPO, 42n)).toBe(0);
      expect(records[0]).toMatchObject({
        id: original!.id,
        head_sha: queued?.headCommit ? GROUP : HEAD,
        external_id: original!.external_id,
        status: "completed",
        conclusion: "failure",
      });
      expect(string(field(records[0].output, "summary")).split("\n")[0]).toBe(
        string(field(original!.output, "summary")).split("\n")[0],
      );
      expect(records).toHaveLength(existing ? 2 : 1);
      if (existing) expect(records[1]).toEqual(other!);
      const patches = writes.filter(({ args }) => args.includes("PATCH"));
      expect(patches).toHaveLength(1);
      expect(patches[0].args).toContain(
        `repos/${REPO}/check-runs/${original!.id}`,
      );
      expect(reads.at(-1)).toBe(`repos/${REPO}/check-runs/${original!.id}`);
    });
  }
}
test("next evaluation gives the new head its own check", () => {
  const { queue, gate, records, writes } = server();
  let view = VIEW;
  gate.prView = () => view;
  queue.entry = () => null;
  gate.main = () => {
    view = { ...VIEW, headRefOid: OLD };
    return 0;
  };
  queue.reviewSnapshot = () => ({ ...REVIEW, view });
  expect(queue.reconcile(REPO, 42n)).toBe(0);
  expect(queue.reconcile(REPO, 42n)).toBe(0);
  expect(
    records.map(({ head_sha, status, conclusion }) => ({
      head_sha,
      status,
      conclusion,
    })),
  ).toEqual([
    { head_sha: HEAD, status: "completed", conclusion: "failure" },
    { head_sha: OLD, status: "completed", conclusion: "success" },
  ]);
  expect(records[0].id).not.toBe(records[1].id);
  expect(
    writes.map(({ args }) => (args.includes("POST") ? "POST" : "PATCH")),
  ).toEqual(["POST", "PATCH", "POST", "PATCH"]);
});
for (const after of [
  { ...VIEW, baseRefName: "main" },
  { ...VIEW, state: "MERGED" },
  { ...VIEW, state: "CLOSED" },
  { ...VIEW, isDraft: true },
  { ...VIEW, lifecycle: { type: "ReopenedEvent", id: "reopened-2" } },
]) {
  test(`candidate change retires the held check: ${encode(after)}`, () => {
    const { queue, gate, records } = server();
    gate.prView = () => VIEW;
    gate.main = () => 0;
    queue.entry = () => null;
    queue.reviewSnapshot = () => ({ ...REVIEW, view: after });
    expect(queue.reconcile(REPO, 42n)).toBe(0);
    expect(records).toHaveLength(1);
    expect(records[0].head_sha).toBe(HEAD);
    expect(records[0].conclusion).toBe("failure");
    expect(field(records[0].output, "summary")).toStartWith(
      binding(REPO, 42n, { view: VIEW }),
    );
  });
}
for (const effect of [true, false]) {
  test(`head-change cleanup reads back a lost PATCH with effect ${effect}`, () => {
    const options: ServerOptions = {},
      { queue, gate, records, writes, reads } = server(options);
    gate.prView = () => VIEW;
    queue.entry = () => null;
    gate.main = () => {
      options.lost = true;
      options.effect = effect;
      return 0;
    };
    queue.reviewSnapshot = () => ({
      ...REVIEW,
      view: { ...VIEW, headRefOid: OLD },
    });
    expect(queue.reconcile(REPO, 42n)).toBe(effect ? 0 : 1);
    expect(records).toHaveLength(1);
    expect(records[0].head_sha).toBe(HEAD);
    expect(records[0].conclusion).toBe(effect ? "failure" : null);
    expect(
      writes.map(({ args }) => (args.includes("POST") ? "POST" : "PATCH")),
    ).toEqual(["POST", "PATCH"]);
    expect(reads.at(-1)).toBe(`repos/${REPO}/check-runs/${records[0].id}`);
  });
}
test("head-change cleanup cannot overwrite a newer run on the original SHA", () => {
  const { queue, gate, records, writes } = server();
  gate.prView = () => VIEW;
  queue.entry = () => null;
  gate.main = () => {
    records.push({ ...structuredClone(records[0]), id: 9n });
    return 0;
  };
  queue.reviewSnapshot = () => ({
    ...REVIEW,
    view: { ...VIEW, headRefOid: OLD },
  });
  expect(queue.reconcile(REPO, 42n)).toBe(1);
  expect(writes).toHaveLength(1);
  expect(
    records.map(({ id, status, conclusion }) => ({
      id,
      status,
      conclusion,
    })),
  ).toEqual([
    { id: 8n, status: "in_progress", conclusion: null },
    { id: 9n, status: "in_progress", conclusion: null },
  ]);
});
for (const failure of ["read", "identity", "newer run"]) {
  test(`head-change cleanup stops after PATCH when readback encounters ${failure}`, () => {
    const { queue, gate, records, writes } = server(),
      read = gate.gh.bind(gate);
    let changed = false;
    gate.prView = () => VIEW;
    queue.entry = () => null;
    gate.main = () => {
      changed = true;
      return 0;
    };
    queue.reviewSnapshot = () => ({
      ...REVIEW,
      view: { ...VIEW, headRefOid: OLD },
    });
    let cleanupReads = 0;
    gate.gh = (...args) => {
      if (!changed) return read(...args);
      cleanupReads++;
      if (failure === "read") throw new Error("check readback unavailable");
      const original = object(parse(read(...args)));
      if (failure === "identity") original.head_sha = OLD;
      else
        records.push({
          ...structuredClone(records[0]),
          id: 9n,
          status: "in_progress",
          conclusion: null,
        });
      return encode(original);
    };
    expect(queue.reconcile(REPO, 42n)).toBe(1);
    expect(cleanupReads).toBe(1);
    expect(
      writes.map(({ args }) => (args.includes("POST") ? "POST" : "PATCH")),
    ).toEqual(["POST", "PATCH"]);
    expect(records[0].conclusion).toBe("failure");
    if (failure === "newer run") {
      expect(records).toHaveLength(2);
      expect(records[1].conclusion).toBeNull();
    } else expect(records).toHaveLength(1);
  });
}
test("publication cannot apply a held check to another SHA", () => {
  const { queue, records, writes } = server(),
    held = queue.holdEntry(REPO, 42n, VIEW, ENTRY)!,
    changed = { ...group(), head: OLD };
  queue.groupSnapshot = () => {
    throw new Error("another candidate must not be evaluated");
  };
  expect(() => queue.publish(REPO, 42n, changed, true, held)).toThrow(
    "candidate changed before check publication",
  );
  expect(records).toHaveLength(1);
  expect(records[0]).toMatchObject({
    id: held.id,
    head_sha: GROUP,
    conclusion: "failure",
  });
  expect(
    writes.map(({ args }) => (args.includes("POST") ? "POST" : "PATCH")),
  ).toEqual(["POST", "PATCH"]);
});
for (const phase of [
  "before admission",
  "after admission",
  "before queue",
  "after queue",
]) {
  test(`head change ${phase} publication finishes both evaluation targets`, () => {
    const { queue, gate, records, writes } = server(),
      changed = { ...REVIEW, view: { ...VIEW, headRefOid: OLD } },
      grouped = group();
    gate.prView = () => VIEW;
    gate.main = () => 0;
    queue.entry = () => ENTRY;
    if (phase.endsWith("queue")) {
      queue.reviewSnapshot = () => REVIEW;
      queue.groupSnapshot = sequence(
        phase === "before queue"
          ? [{ ...grouped, head: OLD, review: changed }]
          : [grouped, grouped, { ...grouped, review: changed }],
      );
    } else {
      queue.reviewSnapshot = sequence(
        phase === "before admission"
          ? [REVIEW, changed]
          : [REVIEW, REVIEW, changed],
      );
      queue.groupSnapshot = () => {
        throw new Error("changed review must stop queue publication");
      };
    }
    expect(queue.reconcile(REPO, 42n)).toBe(1);
    expect(
      records.map(({ head_sha, status, conclusion }) => ({
        head_sha,
        status,
        conclusion,
      })),
    ).toEqual([
      { head_sha: GROUP, status: "completed", conclusion: "failure" },
      { head_sha: HEAD, status: "completed", conclusion: "failure" },
    ]);
    expect(writes.filter(({ args }) => args.includes("POST"))).toHaveLength(2);
    for (const record of records) {
      const final = writes
        .filter(({ args }) =>
          args.includes(`repos/${REPO}/check-runs/${record.id}`),
        )
        .at(-1);
      // Admission can be created directly in its failed state.
      if (final) expect(final.payload.conclusion).toBe("failure");
    }
  });
}
test("unknown admission write stops before attempting queue cleanup", () => {
  const options: ServerOptions = {},
    { queue, gate, records, writes } = server(options);
  gate.prView = () => VIEW;
  queue.entry = () => ENTRY;
  gate.main = () => {
    options.lost = true;
    options.effect = false;
    return 0;
  };
  queue.reviewSnapshot = sequence([
    REVIEW,
    { ...REVIEW, view: { ...VIEW, headRefOid: OLD } },
  ]);
  expect(queue.reconcile(REPO, 42n)).toBe(1);
  expect(
    writes.map(({ args }) => (args.includes("POST") ? "POST" : "PATCH")),
  ).toEqual(["POST", "POST"]);
  expect(records).toHaveLength(1);
  expect(records[0]).toMatchObject({
    head_sha: GROUP,
    status: "in_progress",
    conclusion: null,
  });
});
for (const queued of [ENTRY, null]) {
  test(`real queue evidence retires admission after head drift with ${queued === null ? "removed" : "stale"} entry`, () => {
    const { queue, gate, records } = server();
    gate.prView = () => VIEW;
    gate.main = () => 0;
    queue.entry = sequence([ENTRY, ENTRY, queued]);
    queue.reviewSnapshot = sequence([
      REVIEW,
      REVIEW,
      REVIEW,
      {
        ...REVIEW,
        view: { ...VIEW, headRefOid: OLD },
        evidence: {
          ...object(REVIEW.evidence),
          reviewed: null,
          integration: null,
        },
        approval: null,
        reasons: ["new head has no recorded review baseline"],
      },
    ]);
    expect(queue.reconcile(REPO, 42n)).toBe(1);
    expect(
      records.map(({ head_sha, status, conclusion }) => ({
        head_sha,
        status,
        conclusion,
      })),
    ).toEqual([
      { head_sha: GROUP, status: "completed", conclusion: "failure" },
      { head_sha: HEAD, status: "completed", conclusion: "failure" },
    ]);
  });
}
for (const effect of [true, false]) {
  test(`queue evidence drift stops cleanup when its first write is unconfirmed: ${!effect}`, () => {
    const options: ServerOptions = {},
      { queue, gate, records, writes } = server(options);
    gate.prView = () => VIEW;
    gate.main = () => 0;
    queue.entry = () => ENTRY;
    let reads = 0;
    queue.reviewSnapshot = () => {
      if (++reads <= 3) return REVIEW;
      options.lost = true;
      options.effect = effect;
      return { ...REVIEW, view: { ...VIEW, headRefOid: OLD } };
    };
    expect(queue.reconcile(REPO, 42n)).toBe(1);
    expect(records).toHaveLength(2);
    expect(records[0].conclusion).toBe(effect ? "failure" : null);
    expect(records[1].conclusion).toBe(effect ? "failure" : "success");
    expect(
      writes.map(({ args }) => (args.includes("POST") ? "POST" : "PATCH")),
    ).toEqual(
      effect ? ["POST", "POST", "PATCH", "PATCH"] : ["POST", "POST", "PATCH"],
    );
    expect(writes[2].args).toContain(
      `repos/${REPO}/check-runs/${records[1].id}`,
    );
    if (effect)
      expect(writes[3].args).toContain(
        `repos/${REPO}/check-runs/${records[0].id}`,
      );
  });
}
test("baseline change during publication invalidates same held run", () => {
  const before = group(),
    changed = structuredClone(before);
  object(field(field(changed.review, "evidence"), "integration")).base = OLD;
  for (const [reads, conclusions] of [
    [[changed], ["failure"]],
    [
      [before, changed],
      ["success", "failure"],
    ],
  ] as [Record[], string[]][]) {
    const { queue, records, writes } = server();
    queue.groupSnapshot = sequence(reads);
    const held = queue.holdEntry(REPO, 42n, VIEW, ENTRY)!;
    try {
      queue.publish(REPO, 42n, before, true, held);
    } catch {
      /* Inspect the held run. */
    }
    expect(records).toHaveLength(1);
    expect(records[0].id).toBe(held.id);
    expect(records[0].head_sha).toBe(GROUP);
    expect(records[0].conclusion).toBe("failure");
    expect(writes.slice(1).map(({ payload }) => payload.conclusion)).toEqual(
      conclusions,
    );
  }
});
test("unknown POST and PATCH read back without retry", () => {
  expect(checkIO({ lost: true })).toBeNull();
  expect(checkIO({ existing: true, lost: true })).toBeNull();
  expect(checkIO({ lost: true, effect: false })).toBeInstanceOf(Error);
  expect(checkIO({ duplicate: true })).toBeInstanceOf(Error);
});
test("successful queue check states machine only scope", () => {
  const { queue, records, writes } = server({ lost: true });
  queue.setCheck(REPO, GROUP, "bound-entry", "success", { clean: true });
  expect(writes).toHaveLength(1);
  expect(records[0].external_id).toStartWith("nucleus-review-gate:v4:");
  expect(field(records[0].output, "title")).toBe(
    "Review machine conditions verified",
  );
  expect(field(records[0].output, "summary")).toContain(
    "Substantive finding disposition and original-report completeness are not verified",
  );
  expect(field(records[0].output, "summary")).toContain(
    "Routine dev queue merges consume the current admission result.",
  );
});
test("unknown POST recovers one new run among completed history", () => {
  const { queue, records, writes } = server({ lost: true });
  const ids = [
    queue.setCheck(REPO, GROUP, "entry-1", "success", {}),
    queue.setCheck(REPO, GROUP, "entry-2", "failure", {}),
    queue.setCheck(REPO, GROUP, "entry-3", null, {}),
  ];
  expect(records.map((record) => record.id)).toEqual(ids);
  expect(records.map((record) => record.conclusion)).toEqual([
    "success",
    "failure",
    null,
  ]);
  expect(writes).toHaveLength(3);
  expect(writes.every(({ args }) => args.includes("POST"))).toBe(true);
});
test("newer run during readback stops original evaluation without another write", () => {
  const { queue, gate, records, writes } = server(),
    held = queue.setCheck(REPO, GROUP, "entry-1", null, {});
  gate.gh = () => {
    const original = structuredClone(
      records.find((record) => record.id === held)!,
    );
    records.push({
      ...structuredClone(original),
      id: held + 1n,
      status: "in_progress",
      conclusion: null,
    });
    return encode(original);
  };
  expect(() =>
    queue.setCheck(REPO, GROUP, "entry-1", "success", {}, held),
  ).toThrow();
  expect(writes).toHaveLength(2);
  expect(records.at(-1)!.conclusion).toBeNull();
  expect(field(headChecks(records, [])[0], "passed")).toBe(false);
});
test("exact check readback rejects identity binding result or SHA drift", () => {
  for (const corrupt of [
    { app: { id: 9n, slug: "cladiron" } },
    { app: { id: CHECK[1], slug: "other" } },
    { head_sha: OLD },
    { external_id: "other-entry" },
    { id: 9n },
    { conclusion: "neutral" },
    { status: "in_progress" },
  ] as Record[])
    expect(checkIO({ corrupt })).toBeInstanceOf(Error);
});
test("unknown check write stops before any recovery write", () => {
  const first = group(),
    queue = new Queue(fakeGate());
  let writes = 0;
  queue.groupSnapshot = () => first;
  queue.setCheck = () => {
    writes++;
    throw new Error("unknown write");
  };
  expect(() => queue.publish(REPO, 42n, first, true)).toThrow("unknown write");
  expect(writes).toBe(1);
});
test("active queue is held before evidence failure", () => {
  const queue = new Queue(fakeGate()),
    writes = capture(queue);
  queue.gate.prView = () => VIEW;
  queue.entry = () => ENTRY;
  queue.gate.main = () => 1;
  expect(queue.reconcile(REPO, 42n)).toBe(1);
  expect(writes.map((item) => item.conclusion)).toEqual([null, "failure"]);
  expect(writes.map((item) => item.head)).toEqual([GROUP, HEAD]);
});
test("main publishes bound admission without reading or creating a queue entry", () => {
  const view = { ...VIEW, baseRefName: "main" };
  const result = exercise({
    views: [view],
    mainTarget: BASE,
    records: [{ ...CONTEXT, display_title: `context:42:${HEAD}:${BASE}:main` }],
  });
  const queue = new Queue(result.gate),
    writes = capture(queue);
  queue.shaPrs = () => ({ heads: [42n], entries: [] });
  queue.entry = () => {
    throw new Error("main must not enter the dev queue");
  };
  expect(queue.reconcile(REPO, 42n)).toBe(0);
  expect(writes.map((item) => item.conclusion)).toEqual([null, "success"]);
  expect(writes.every((item) => item.head === HEAD)).toBe(true);
  expect(field(writes.at(-1)!.snapshot.evidence, "reviewed")).toEqual(view);
  expect(field(writes.at(-1)!.snapshot.evidence, "integration")).toEqual({
    base: BASE,
  });
});
test("stacked PRs remain unapproved without failing reconciliation", () => {
  for (const prior of [[], [approval()]]) {
    const result = exercise({
      views: [{ ...VIEW, baseRefName: "codex/stack-base" }],
      queueTarget: BASE,
      approvals: prior,
    });
    expect(result.result).toBe(0);
    expect(result.reviews.some((review) => review.state === "APPROVED")).toBe(
      false,
    );
    const queue = new Queue(result.gate);
    queue.entry = () => {
      throw new Error("stacked PRs must not enter the dev queue");
    };
    expect(queue.reconcile(REPO, 42n)).toBe(0);
  }
});
test("main cannot finish its lifecycle workflow with the check controller disabled", () => {
  for (const isDraft of [false, true]) {
    const result = exercise({
      views: [{ ...VIEW, baseRefName: "main", isDraft }],
    });
    expect(result.result).toBe(1);
    expect(result.reviews).toHaveLength(0);
    expect(result.output).toContain(
      "main admission requires the check controller",
    );
  }
});
test("main does not evaluate or approve before its waiting check is confirmed", () => {
  const queue = new Queue(fakeGate()),
    view = { ...VIEW, baseRefName: "main" };
  let evaluated = false;
  queue.gate.prView = () => view;
  queue.setCheck = () => {
    throw new Error("waiting check write unconfirmed");
  };
  queue.gate.main = () => {
    evaluated = true;
    return 0;
  };
  expect(queue.reconcile(REPO, 42n)).toBe(1);
  expect(evaluated).toBe(false);
});
test("main ignores only its own check while retaining other producers", () => {
  const view = { ...VIEW, baseRefName: "main" },
    passing = [run("docs"), run("rust"), run("website")];
  const before = checkEvidence(passing, [], view);
  expect(
    checkEvidence(
      [...passing, run(CHECK[0], "in_progress", null, CHECK[1])],
      [],
      view,
    ),
  ).toEqual(before);
  expect(
    checkEvidence(
      [...passing, run(CHECK[0], "completed", "failure", CHECK[1])],
      [],
      view,
    ),
  ).toEqual(before);
  expect(
    checkEvidence(
      [...passing, run(CHECK[0], "completed", "success", 99n)],
      [],
      view,
    ),
  ).not.toEqual(before);
});
test("main advancing during final approval readback withdraws that approval", () => {
  const result = exercise({
    views: [{ ...VIEW, baseRefName: "main" }],
    mainTarget: BASE,
    records: [{ ...CONTEXT, display_title: `context:42:${HEAD}:${BASE}:main` }],
    targetReads: [BASE, BASE, BASE, OLD],
  });
  expect(result.methods).toEqual(["POST", "PUT"]);
  expect(result.reviews[0].state).toBe("DISMISSED");
  expect(result.output).toContain("changed during approval");
});
test("main review, checks, baseline and read failures never publish admission success", () => {
  const view = { ...VIEW, baseRefName: "main" };
  for (const changed of [
    { snapshots: [{ comments: [] }] },
    { snapshots: [{ comments: [summary("running")] }] },
    { snapshots: [{ comments: [summary("failed")] }] },
    { snapshots: [{ comments: [summary("completed", "failed")] }] },
    { snapshots: [{ unresolved: 1n }] },
    {
      snapshots: [
        {
          checks: headChecks(
            [run("docs"), run("rust"), run("website", "completed", "failure")],
            [],
          ),
        },
      ],
    },
    { snapshots: [{ rules: QUEUE_RULES }] },
    { snapshots: [{ rules: MAIN_RULES.slice(0, -1) }] },
    { snapshots: [new Error("review evidence unavailable")] },
    { mainTarget: OLD },
    { records: [] },
  ]) {
    const result = exercise({
      views: [view],
      mainTarget: BASE,
      records: [
        { ...CONTEXT, display_title: `context:42:${HEAD}:${BASE}:main` },
      ],
      ...changed,
    });
    const queue = new Queue(result.gate),
      writes = capture(queue);
    queue.shaPrs = () => ({ heads: [42n], entries: [] });
    queue.entry = () => {
      throw new Error("main must not enter the dev queue");
    };
    queue.reconcile(REPO, 42n);
    expect(writes[0].conclusion).toBeNull();
    expect(writes.some((item) => item.conclusion === "success")).toBe(false);
  }
});
test("main configuration requires exact CI producers and strict baseline checks without a queue", () => {
  contract(fakeGate(), MAIN_RULES, "main");
  for (const changed of [
    QUEUE_RULES,
    MAIN_RULES.slice(0, -1),
    MAIN_RULES.map((rule) =>
      field(rule, "type") === "required_status_checks"
        ? {
            ...object(rule),
            parameters: {
              ...object(field(rule, "parameters")),
              strict_required_status_checks_policy: false,
            },
          }
        : rule,
    ),
    ...["docs", "rust", "website", CHECK[0]].map((name) =>
      MAIN_RULES.map((rule) =>
        field(rule, "type") === "required_status_checks"
          ? {
              ...object(rule),
              parameters: {
                ...object(field(rule, "parameters")),
                required_status_checks: array(
                  field(field(rule, "parameters"), "required_status_checks"),
                ).map((check) =>
                  field(check, "context") === name
                    ? { ...object(check), integration_id: 99n }
                    : check,
                ),
              },
            }
          : rule,
      ),
    ),
  ])
    expect(() => contract(fakeGate(), changed, "main")).toThrow();
});
test("main requires its native lifecycle workflow from the trusted dev source", () => {
  const original = MAIN_RULES.find(
    (rule) => field(rule, "type") === "workflows",
  )!;
  const parameters = object(field(original, "parameters"));
  const workflow = object(array(parameters.workflows)[0]);
  const without = MAIN_RULES.filter(
    (rule) => field(rule, "type") !== "workflows",
  );
  const variants: Json[][] = [
    without,
    [
      ...without,
      { ...object(original), parameters: { ...parameters, workflows: [] } },
    ],
    [
      ...without,
      {
        ...object(original),
        parameters: { ...parameters, do_not_enforce_on_create: true },
      },
    ],
    ...(
      [
        { repository_id: 99n },
        { repository_id: 1185290396n },
        { repository_id: "1432619931" },
        { path: ".github/workflows/other.yml" },
        { ref: "refs/heads/main" },
        { sha: OLD },
      ] as Record[]
    ).map((change): Json[] => [
      ...without,
      {
        ...object(original),
        parameters: { ...parameters, workflows: [{ ...workflow, ...change }] },
      },
    ]),
  ];
  for (const rules of variants) {
    expect(() => contract(fakeGate(), rules, "main")).toThrow(
      "main must require the trusted dev lifecycle workflow",
    );
    const result = exercise({
      views: [{ ...VIEW, baseRefName: "main" }],
      mainTarget: BASE,
      snapshots: [{ rules }],
    });
    expect(result.result).toBe(1);
    expect(result.reviews).toHaveLength(0);
  }
});
test("protected admission stops when signed commit protection is removed", () => {
  for (const target of ["dev", "main"]) {
    const rules = target === "dev" ? QUEUE_RULES : MAIN_RULES;
    const changed = rules.filter(
      (rule) => field(rule, "type") !== "required_signatures",
    );
    expect(() => contract(fakeGate(), changed, target)).toThrow(
      "must require signed commits",
    );
    const result = exercise({
      views: [{ ...VIEW, baseRefName: target }],
      ...(target === "dev" ? { queueTarget: BASE } : { mainTarget: BASE }),
      snapshots: [{ rules: changed }],
    });
    expect(result.result).toBe(1);
    expect(result.reviews).toHaveLength(0);
    expect(result.output).toContain(`${target} must require signed commits`);
  }
});
test("protected admission requires a positive integer approval count", () => {
  for (const target of ["dev", "main"]) {
    const rules = target === "dev" ? QUEUE_RULES : MAIN_RULES;
    for (const count of [0n, -1n, null, true, 1, "1", "missing"]) {
      const changed = structuredClone(rules);
      const parameters = object(
        field(
          changed.find((rule) => field(rule, "type") === "pull_request"),
          "parameters",
        ),
      );
      if (count === "missing")
        delete parameters.required_approving_review_count;
      else parameters.required_approving_review_count = count;
      expect(() => contract(fakeGate(), changed, target)).toThrow(
        "must require at least one approval",
      );
    }
    expect(() =>
      contract(
        fakeGate(),
        rules.filter((rule) => field(rule, "type") !== "pull_request"),
        target,
      ),
    ).toThrow("must require at least one approval");
  }
});
test("main and dev cannot consume the same admission SHA", () => {
  const nodes = [
    pull(42n),
    { ...pull(43n), baseRefName: "main" },
    { ...pull(44n), baseRefName: "feature" },
  ];
  const queue = admission();
  queue.gate.gh = (...args) => {
    expect(args.find((arg) => arg.startsWith("query="))).not.toContain(
      'baseRefName:"dev"',
    );
    return encode(pullsResponse(nodes, 3n));
  };
  queue.shaPrs = Queue.prototype.shaPrs;
  const snapshot = queue.reviewSnapshot(REPO, 42n),
    writes = capture(queue);
  expect(snapshot.head_prs).toEqual({ heads: [42n, 43n], entries: [] });
  queue.reviewSnapshot = () => snapshot;
  queue.publish(REPO, 42n, snapshot);
  expect(writes.map((item) => item.conclusion)).toEqual(["failure"]);
});
test("main merge after confirmed admission preserves historical success", () => {
  const view = { ...VIEW, baseRefName: "main" },
    before = { ...REVIEW, view };
  for (const after of [
    { view: { ...view, state: "MERGED" } },
    new Error("PR changed after merge"),
  ]) {
    const queue = new Queue(fakeGate()),
      writes = capture(queue);
    queue.gate.prView = () => ({ ...view, state: "MERGED" });
    queue.reviewSnapshot = sequence([before, after]);
    queue.publish(REPO, 42n, before);
    expect(writes.map((item) => item.conclusion)).toEqual(["success"]);
  }
});
test("a merge into another target invalidates the original admission", () => {
  for (const target of ["main", "dev"]) {
    const view = { ...VIEW, baseRefName: target },
      before = { ...REVIEW, view };
    const merged = {
      ...view,
      state: "MERGED",
      baseRefName: target === "main" ? "dev" : "main",
    };
    for (const after of [{ view: merged }, new Error("changed after merge")]) {
      const queue = new Queue(fakeGate()),
        writes = capture(queue);
      queue.gate.prView = () => merged;
      queue.reviewSnapshot = sequence([before, after]);
      expect(() => queue.publish(REPO, 42n, before)).toThrow();
      expect(writes.map((item) => item.conclusion)).toEqual([
        "success",
        "failure",
      ]);
    }
  }
});
test("unqueued PR cannot keep old admission success after read failure", () => {
  for (const failure of ["main", "snapshot"]) {
    const queue = new Queue(fakeGate()),
      writes = capture(queue);
    queue.gate.prView = () => VIEW;
    queue.entry = () => null;
    queue.gate.main = () => (failure === "main" ? 1 : 0);
    queue.reviewSnapshot = () => {
      throw new Error("review read failed");
    };
    expect(queue.reconcile(REPO, 42n)).toBe(1);
    expect(writes).toHaveLength(1);
    expect(writes[0].head).toBe(HEAD);
    expect(writes[0].conclusion).toBeNull();
  }
});
test("entry created or replaced during evaluation is held before waiting", () => {
  const replacement = { ...ENTRY, id: "entry-2", headCommit: { oid: OLD } };
  for (const [first, second] of [
    [null, ENTRY],
    [ENTRY, replacement],
  ]) {
    const queue = new Queue(fakeGate()),
      writes = capture(queue);
    queue.gate.prView = () => VIEW;
    queue.entry = sequence([first, second]);
    queue.gate.main = () => 0;
    queue.reviewSnapshot = () => REVIEW;
    queue.publish = () => 8n;
    queue.groupSnapshot = () => {
      throw new QueueWaiting("CI not started");
    };
    expect(queue.reconcile(REPO, 42n)).toBe(0);
    expect(writes.at(-1)!.head).toBe(field(second!.headCommit, "oid"));
    expect(writes.at(-1)!.conclusion).toBeNull();
  }
});
for (const phase of ["entry read", "group snapshot"]) {
  test(`queue replacement during ${phase} retires the original check`, () => {
    const { queue, gate, records, writes } = server(),
      replacement = { ...ENTRY, id: "entry-2", headCommit: { oid: OLD } },
      replacementGroup = group({
        queued: replacement,
        runs: RUNS.map((run) => ({ ...run, head_sha: OLD })),
        ref: OLD,
      });
    gate.prView = () => VIEW;
    gate.main = () => 0;
    queue.reviewSnapshot = () => REVIEW;
    queue.entry = sequence([
      ENTRY,
      phase === "entry read" ? replacement : ENTRY,
    ]);
    queue.groupSnapshot = () => replacementGroup;
    expect(queue.reconcile(REPO, 42n)).toBe(0);
    const original = records.filter((record) => record.head_sha === GROUP),
      next = records.filter((record) => record.head_sha === OLD);
    expect(original).toHaveLength(1);
    expect(original[0].conclusion).toBe("failure");
    expect(next).toHaveLength(1);
    expect(next[0].conclusion).toBe("success");
    expect(original[0].id).not.toBe(next[0].id);
    const retired = writes.findIndex(
      ({ args, payload }) =>
        args.includes(`repos/${REPO}/check-runs/${original[0].id}`) &&
        payload.conclusion === "failure",
    );
    const created = writes.findIndex(({ payload }) => payload.head_sha === OLD);
    expect(retired).toBeGreaterThan(0);
    expect(created).toBeGreaterThan(retired);
  });
}
test("removed queue entry retires its held check", () => {
  const { queue, gate, records } = server();
  gate.prView = () => VIEW;
  gate.main = () => 0;
  queue.reviewSnapshot = () => REVIEW;
  queue.entry = sequence([ENTRY, null]);
  expect(queue.reconcile(REPO, 42n)).toBe(0);
  expect(records.filter(({ head_sha }) => head_sha === GROUP)).toMatchObject([
    { head_sha: GROUP, status: "completed", conclusion: "failure" },
  ]);
});
test("waiting entry refreshes admission and later holds generated candidate", () => {
  const waiting = {
      ...ENTRY,
      state: "QUEUED",
      headCommit: null,
      baseCommit: null,
    },
    grouped = group();
  for (const ready of [false, true]) {
    const { queue, gate, records, writes } = server();
    gate.prView = () => VIEW;
    gate.main = () => 0;
    queue.entry = sequence([waiting, waiting]);
    queue.reviewSnapshot = () => REVIEW;
    queue.groupSnapshot = () => {
      if (!ready) throw new QueueWaiting("not generated");
      return grouped;
    };
    expect(queue.reconcile(REPO, 42n)).toBe(0);
    expect(records.map((record) => record.head_sha)).toEqual(
      ready ? [HEAD, GROUP] : [HEAD],
    );
    expect(
      writes
        .filter(({ args }) => args.includes("POST"))
        .map(({ payload }) => payload.head_sha),
    ).toEqual(ready ? [HEAD, GROUP] : [HEAD]);
    expect(records.every((record) => record.conclusion === "success")).toBe(
      true,
    );
  }
});
test("queue numeric equality preserves loose counts and producer IDs but strict identities", () => {
  for (const value of [1, true]) {
    const rules = structuredClone(QUEUE_RULES);
    const parameters = object(
      field(
        rules.find((rule) => field(rule, "type") === "merge_queue"),
        "parameters",
      ),
    );
    for (const key of [
      "max_entries_to_build",
      "max_entries_to_merge",
      "min_entries_to_merge",
    ])
      parameters[key] = value;
    contract(fakeGate(), rules);
  }
  const own: Record = {
    id: 9007199254740993n,
    name: CHECK[0],
    head_sha: GROUP,
    app: { id: Number(CHECK[1]), slug: "cladiron" },
    status: "in_progress",
    conclusion: null,
  };
  const queue = new Queue(
    fakeGate({ gh: () => encode([{ total_count: true, check_runs: [own] }]) }),
  );
  expect([...queue.ownChecks(REPO, GROUP).keys()]).toEqual([9007199254740993n]);
  for (const id of [1, true, 0n, -1n]) {
    queue.checkRuns = () => [{ ...own, id }];
    expect(() => queue.ownChecks(REPO, GROUP)).toThrow();
  }
  for (const value of [1, true]) {
    const node = pull(42n);
    expect(() =>
      new Queue(
        fakeGate({ gh: () => encode(pullsResponse([node], value)) }),
      ).shaPrs(REPO, HEAD),
    ).toThrow();
    expect(() =>
      new Queue(
        fakeGate({
          gh: () => encode(pullsResponse([{ ...node, number: value }], 1n)),
        }),
      ).shaPrs(REPO, HEAD),
    ).toThrow();
  }
});
test("queue distinguishes missing required entry from explicit null and rejects partial waiting pairs", () => {
  const missing = entryResponse(null);
  delete object(field(field(missing.data, "repository"), "pullRequest"))
    .mergeQueueEntry;
  expect(() =>
    new Queue(fakeGate({ gh: () => encode(missing) })).entry(REPO, 42n),
  ).toThrow();
  expect(
    new Queue(fakeGate({ gh: () => encode(entryResponse(null)) })).entry(
      REPO,
      42n,
    ),
  ).toBeNull();
  for (const missingField of ["headCommit", "baseCommit"]) {
    const waiting = {
      ...ENTRY,
      state: "QUEUED",
      headCommit: null,
      baseCommit: null,
    } as Record;
    delete waiting[missingField];
    expect(() =>
      new Queue(fakeGate({ gh: () => encode(entryResponse(waiting)) })).entry(
        REPO,
        42n,
      ),
    ).toThrow();
  }
});
test("publication compares numeric evidence equivalently while hashing the original tree", () => {
  const before = { ...REVIEW, unknown: { value: 1n, zero: 0n } };
  const after = { ...REVIEW, unknown: { value: true, zero: -0 } };
  const { queue, records } = server();
  queue.reviewSnapshot = () => after;
  queue.publish(REPO, 42n, before);
  expect(records[0].conclusion).toBe("success");
  expect(encode(before)).not.toBe(encode(after));
  // Unknown evidence fields remain part of the bytes named in the check.
  const baseline = server();
  baseline.queue.reviewSnapshot = () => REVIEW;
  baseline.queue.publish(REPO, 42n, REVIEW);
  expect(field(records[0].output, "summary")).not.toBe(
    field(baseline.records[0].output, "summary"),
  );
});
test("queue entry and queue IDs use the existing Python whitespace boundary", () => {
  for (const separator of ["\u0085", "\u001c", "\u001d", "\u001e", "\u001f"]) {
    for (const entry of [
      { ...ENTRY, id: `entry${separator}1` },
      {
        ...ENTRY,
        mergeQueue: { ...object(ENTRY.mergeQueue), id: `queue${separator}1` },
      },
    ])
      expect(() =>
        new Queue(fakeGate({ gh: () => encode(entryResponse(entry)) })).entry(
          REPO,
          42n,
        ),
      ).toThrow();
  }
  const entry = { ...ENTRY, id: "entry\ufeff1" };
  expect(
    new Queue(fakeGate({ gh: () => encode(entryResponse(entry)) })).entry(
      REPO,
      42n,
    ),
  ).toEqual(entry);
});
