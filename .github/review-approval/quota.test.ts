import { expect, test } from "bun:test";
import { EvidenceApi } from "./evidence.test.ts";
import { b, providerFixture } from "./fixtures.ts";
import {
  prepareQuota,
  quotaRequest,
  readQuota,
  quotaWithdrawal,
  quotaDeclaration,
} from "./quota.ts";
import { snapshot } from "./evidence.ts";
import { requestBodies } from "./identity.ts";

const now = Date.parse("2026-10-30T14:00:00Z");
async function fixture(currentHead = false, noPeriod = false) {
  const api = new EvidenceApi();
  api.f.comments = api.f.comments.filter((c) => c.id !== 100); // Known depletion requires no new Cubic request.
  api.f.checks = [];
  api.f.reviews = [];
  const source: any = {
    id: 601,
    app: { id: 1082092 },
    name: "cubic · AI code reviewer",
    status: "completed",
    conclusion: "neutral",
    head_sha: currentHead ? b.head : "f".repeat(40),
    started_at: "2026-10-28T10:29:00Z",
    completed_at: "2026-10-28T10:30:00Z",
    output: {
      annotations_count: 0,
      summary:
        "You've reached your plan's monthly review limit. cubic has reviewed 1,200 of the 1,000 allowed lines of code this month." +
        (noPeriod ? "" : " Reviews resume on 1 November 2026 (in 3 days)."),
      text: null,
    },
  };
  const get = api.get.bind(api);
  api.get = async (path) =>
    path.endsWith("/check-runs/601") ? source : get(path);
  const selection = await prepareQuota(api, b, 601, now);
  api.f.comments.find((c) => c.id === 101).body = quotaRequest(b, selection);
  return { api, source, selection };
}
test("trusted cross-PR quota selection uses complete Codex code/security without claiming Cubic passed", async () => {
  const { api } = await fixture();
  const result = await snapshot(api, b.pr, now);
  expect(result.evidence.providers.cubic).toBe("quota-unavailable");
  expect(result.evidence.providers.results).toEqual([201, 203]);
  expect(result.evidence.providers.quota?.selection.check).toBe(601);
  expect(api.f.comments.some((c) => c.body === requestBodies(b)[0])).toBe(
    false,
  );
});
test("unparseable period is limited to the actual current-head refusal", async () => {
  const f = await fixture(true, true);
  expect((await snapshot(f.api, b.pr, now)).evidence.providers.cubic).toBe(
    "quota-unavailable",
  );
  f.source.head_sha = "f".repeat(40);
  await expect(readQuota(f.api, b, f.api.f.comments, [], now)).rejects.toThrow(
    "lacks an explicit period",
  );
});
for (const [name, mutate] of Object.entries({
  "ordinary failure": (f: any) => {
    f.source.conclusion = "failure";
  },
  "generic neutral": (f: any) => {
    f.source.output.summary = "Review unavailable";
  },
  "timeout is not quota": (f: any) => {
    f.source.output.summary = "Request timed out. Try again later.";
  },
  "rate limit is not quota": (f: any) => {
    f.source.output.summary = "HTTP 429 rate limited";
  },
  "different provider": (f: any) => {
    f.source.app.id = 1;
  },
  "source changed": (f: any) => {
    f.source.output.summary = f.source.output.summary.replace("1,200", "1,201");
  },
  "edited selection": (f: any) => {
    f.api.f.comments.find((c: any) => c.id === 101).lastEditedAt =
      "2026-10-30T10:33:50Z";
  },
  "unverified recovery": (f: any) => {
    const c = f.api.f.comments.find((c: any) => c.id === 101);
    c.body = c.body.replace(
      '"recoveryChecked":true',
      '"recoveryChecked":false',
    );
  },
}))
  test(`quota fallback rejects ${name}`, async () => {
    const f = await fixture();
    mutate(f);
    await expect(snapshot(f.api, b.pr, now)).rejects.toThrow();
  });
test("fallback can bind existing complete Codex coverage without issuing another review request", async () => {
  const f = await fixture();
  const existing = f.api.f.comments.find((c) => c.id === 101);
  existing.body = requestBodies(b)[1];
  f.api.f.comments.push({
    id: 503,
    node_id: "quota-503",
    user: { id: 19648989, type: "User" },
    lastEditedAt: null,
    created_at: "2026-10-30T13:10:00Z",
    updated_at: "2026-10-30T13:10:00Z",
    body: quotaDeclaration(b, f.selection, existing.id),
  });
  const result = await snapshot(f.api, b.pr, now);
  expect(result.evidence.providers.cubic).toBe("quota-unavailable");
  expect(result.evidence.providers.quota?.request).toBe(101);
  expect(result.evidence.providers.quota?.statement).toBe(503);
  expect(
    f.api.f.comments.filter((c) => c.body.startsWith("@codex review")),
  ).toHaveLength(1);
});
test("a new selection cannot extend an expired date-only source; expiry does not imply restored coverage", async () => {
  const { api } = await fixture();
  await expect(
    prepareQuota(api, b, 601, Date.parse("2026-11-01T00:00:00Z")),
  ).rejects.toThrow("may have ended");
});
test("already-selected same-input Codex coverage survives later recovery without another Cubic request", async () => {
  const { api } = await fixture();
  const success = providerFixture().checks[0];
  api.f.checks.push(success);
  const result = await snapshot(api, b.pr, Date.parse("2026-11-01T12:00:00Z"));
  expect(result.evidence.providers.cubic).toBe("quota-unavailable");
  expect(api.f.comments.some((c) => c.body === requestBodies(b)[0])).toBe(
    false,
  );
});
test("known current-head Cubic recovery before selection contradicts a new fallback", async () => {
  const f = await fixture();
  const check = providerFixture().checks[0];
  check.started_at = "2026-10-30T10:30:00Z";
  check.completed_at = "2026-10-30T10:32:00Z";
  await expect(
    readQuota(f.api, b, f.api.f.comments, [check], now),
  ).rejects.toThrow("contradicts");
});
test("merge-only SUCCESS does not contradict quota in preparation or approval", async () => {
  const f = await fixture(true);
  const check = providerFixture().checks[0];
  check.started_at = "2026-10-30T10:30:00Z";
  check.completed_at = "2026-10-30T10:32:00Z";
  check.output.summary =
    "This commit only merges another branch into the PR branch, so cubic completed this check without running an AI review. cubic will review your next non-merge commit.";
  f.api.f.checks = [check];
  expect(await prepareQuota(f.api, b, 601, now)).toEqual(f.selection);
  expect((await snapshot(f.api, b.pr, now)).evidence.providers.cubic).toBe(
    "quota-unavailable",
  );
});
test("source-head real completion contradicts a new selection in preparation and approval", async () => {
  const f = await fixture();
  const check = providerFixture({ ...b, head: f.source.head_sha }).checks[0];
  check.completed_at = "2026-10-30T10:32:00Z";
  check.started_at = "2026-10-30T10:30:00Z";
  const pages = f.api.pages.bind(f.api);
  f.api.pages = async (path, key) =>
    path.includes(`/commits/${f.source.head_sha}/check-runs`)
      ? [check]
      : pages(path, key);
  await expect(prepareQuota(f.api, b, 601, now)).rejects.toThrow("contradicts");
  await expect(snapshot(f.api, b.pr, now)).rejects.toThrow("contradicts");
});
test("same-PR prior-head real report contradicts a new-head selection", async () => {
  const f = await fixture();
  const review = providerFixture({ ...b, head: "e".repeat(40) }).reviews[0];
  review.submitted_at = "2026-10-30T10:32:00Z";
  f.api.f.reviews = [review];
  await expect(prepareQuota(f.api, b, 601, now)).rejects.toThrow("contradicts");
  await expect(snapshot(f.api, b.pr, now)).rejects.toThrow("contradicts");
  review.body = review.body.replaceAll("e".repeat(40), b.head);
  expect(
    await readQuota(f.api, b, f.api.f.comments, [], now, f.api.f.reviews),
  ).toBeDefined();
});
test("later source-head completion and prior-head report preserve an existing selection", async () => {
  const f = await fixture();
  const old = providerFixture({ ...b, head: "e".repeat(40) });
  old.reviews[0].submitted_at = "2026-10-30T13:11:00Z";
  f.api.f.reviews = old.reviews;
  const check = providerFixture({ ...b, head: f.source.head_sha }).checks[0];
  check.completed_at = "2026-10-30T13:11:00Z";
  const pages = f.api.pages.bind(f.api);
  f.api.pages = async (path, key) =>
    path.includes(`/commits/${f.source.head_sha}/check-runs`)
      ? [check]
      : pages(path, key);
  expect((await snapshot(f.api, b.pr, now)).evidence.providers.cubic).toBe(
    "quota-unavailable",
  );
  await expect(prepareQuota(f.api, b, 601, now)).rejects.toThrow("contradicts");
});
test("owner can explicitly withdraw quota routing and a later normal request cannot revive it", async () => {
  const f = await fixture();
  f.api.f.comments.push({
    id: 501,
    user: { id: 19648989, type: "User" },
    node_id: "withdraw-501",
    lastEditedAt: null,
    body: quotaWithdrawal(b),
    created_at: "2026-10-30T13:08:00Z",
    updated_at: "2026-10-30T13:08:00Z",
  });
  await expect(snapshot(f.api, b.pr, now)).rejects.toThrow("withdrew");
  f.api.f.comments.push({
    id: 502,
    user: { id: 19648989, type: "User" },
    lastEditedAt: null,
    body: requestBodies(b)[1],
    created_at: "2026-10-30T13:10:00Z",
    updated_at: "2026-10-30T13:10:00Z",
  });
  expect(await readQuota(f.api, b, f.api.f.comments, [], now)).toBeUndefined();
});
test("quota does not hide earlier unresolved threadless Cubic findings or incomplete Codex security", async () => {
  const f = await fixture();
  f.api.f.reviews.push({
    id: 701,
    user: { id: 191113872, type: "Bot" },
    commit_id: b.head,
    state: "COMMENTED",
    submitted_at: "2026-10-30T10:20:00Z",
    body: "P1: unresolved finding",
  });
  await expect(snapshot(f.api, b.pr, now)).rejects.toThrow("cannot erase");
  f.api.f.reviews = [];
  f.api.f.comments = f.api.f.comments.filter((c) => c.id !== 203);
  await expect(snapshot(f.api, b.pr, now)).rejects.toThrow(
    "security no-findings receipt",
  );
});
test("later explicit Cubic routing reuses valid Codex coverage without depending on old quota source", async () => {
  const f = await fixture();
  f.api.f.comments.push({
    id: 504,
    node_id: "cubic-504",
    user: { id: 19648989, type: "User" },
    lastEditedAt: null,
    body: requestBodies(b)[0],
    created_at: "2026-10-30T13:06:00Z",
    updated_at: "2026-10-30T13:06:00Z",
  });
  const prior = providerFixture();
  prior.checks[0].started_at = "2026-10-30T13:06:01Z";
  prior.checks[0].completed_at = "2026-10-30T13:07:00Z";
  prior.reviews[0].submitted_at = "2026-10-30T13:06:30Z";
  f.api.f.checks = prior.checks;
  f.api.f.reviews = prior.reviews;
  const get = f.api.get.bind(f.api);
  f.api.get = async (path) => {
    if (path.endsWith("/check-runs/601"))
      throw new Error("Old source no longer readable");
    return get(path);
  };
  expect((await snapshot(f.api, b.pr, now)).evidence.providers.cubic).toBe(
    "passed",
  );
  expect(
    f.api.f.comments.filter((c) => c.body.startsWith("@codex review")),
  ).toHaveLength(1);
});
