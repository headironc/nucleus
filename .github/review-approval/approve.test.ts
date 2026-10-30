import { expect, test } from "bun:test";
import {
  approve as approveOperation,
  approvalBody as operationBody,
  type Snapshot,
  type OperationOutcome,
} from "./approve.ts";
import { b } from "./fixtures.ts";
import type { Api } from "./github.ts";
import type { Record } from "./providers.ts";
import type { Intent } from "./intent.ts";

const s = {
  b,
  digest: "d".repeat(64),
  evidence: { providers: { cubic: "passed" } },
  reviews: [],
} as unknown as Snapshot;
const approvalBody = (s: Snapshot) => operationBody(s, "run-1-attempt-1");
const approve = (
  api: Api,
  pr: number,
  load: () => Promise<Snapshot>,
  uncertain: Intent[] = [],
  outcome?: OperationOutcome,
  operation = "run-1-attempt-1",
) => approveOperation(api, pr, operation, load, uncertain, outcome);
const user = { id: 300000001, type: "Bot", login: "cladiron[bot]" }; // Synthetic bot identity.
class ApprovalApi implements Api {
  reviews: Record[] = [];
  posts: Record[] = [];
  dismissals: number[] = [];
  dismissalAttempts: number[] = [];
  outcome = "success";
  failDismiss = false;
  failDismissRead = false;
  async get(path: string) {
    if (path.startsWith("/users/")) return user;
    if (this.failDismissRead && this.dismissalAttempts.length)
      throw new Error("Synthetic withdrawal readback failure");
    return this.reviews.find((r) => path.endsWith(`/${r.id}`));
  }
  async pages() {
    return structuredClone(this.reviews);
  }
  async graphql() {
    throw new Error("Unexpected GraphQL");
  }
  async receipt() {
    throw new Error("Unexpected artifact");
  }
  async post(_path: string, body: Record) {
    this.posts.push(body);
    const r = {
      id: 1000 + this.reviews.length,
      user,
      performed_via_github_app: { id: 3104501 },
      body: body.body,
      state: "APPROVED",
      commit_id: body.commit_id,
    };
    if (this.outcome !== "unknown-none") this.reviews.push(r);
    if (this.outcome === "unknown-multiple")
      this.reviews.push({ ...r, id: r.id + 1 });
    if (this.outcome.startsWith("unknown"))
      throw new Error("Synthetic POST timeout");
    if (this.outcome === "wrong-response") return { ...r, id: -1 };
    return r;
  }
  async put(path: string) {
    const id = Number(path.split("/").at(-2));
    this.dismissalAttempts.push(id);
    if (this.failDismiss) throw new Error("Synthetic dismissal failure");
    this.dismissals.push(id);
    this.reviews.find((r) => r.id === id)!.state = "DISMISSED";
    return {};
  }
}
test("exact head approve and repeated event reuse the same review", async () => {
  const api = new ApprovalApi();
  expect((await approve(api, b.pr, async () => s)).reused).toBe(false);
  expect((await approve(api, b.pr, async () => s)).reused).toBe(true);
  expect(api.posts).toEqual([
    { event: "APPROVE", commit_id: b.head, body: approvalBody(s) },
  ]);
});
test("unknown POST with one exact newly created review is read back without retry", async () => {
  const api = new ApprovalApi();
  api.outcome = "unknown-one";
  expect((await approve(api, b.pr, async () => s)).review).toBe(1000);
  expect(api.posts).toHaveLength(1);
});
for (const outcome of ["unknown-none", "unknown-multiple", "wrong-response"])
  test(outcome, async () => {
    const api = new ApprovalApi();
    api.outcome = outcome;
    await expect(approve(api, b.pr, async () => s)).rejects.toThrow();
    expect(api.posts).toHaveLength(1);
    expect(api.reviews.every((r) => r.state === "DISMISSED")).toBe(true);
  });
test("persisted unknown intent prevents a subsequent dispatch from blindly retrying", async () => {
  const api = new ApprovalApi();
  const intent = {
    version: 1 as const,
    repositoryId: 1432619931,
    run: 8,
    attempt: 1,
    b,
    digest: s.digest,
  };
  await expect(approve(api, b.pr, async () => s, [intent])).rejects.toThrow(
    "unknown outcome",
  );
  expect(api.posts).toHaveLength(0);
});
test("outcome distinguishes no POST from an unknown POST for later runs", async () => {
  const before: OperationOutcome = {
    attempted: false,
    resolved: false,
    reviews: [],
  };
  let count = 0;
  const api = new ApprovalApi();
  await expect(
    approve(
      api,
      b.pr,
      async () => (++count === 2 ? { ...s, digest: "e".repeat(64) } : s),
      [],
      before,
    ),
  ).rejects.toThrow();
  expect(before.attempted).toBe(false);
  const unknown: OperationOutcome = {
    attempted: false,
    resolved: false,
    reviews: [],
  };
  api.outcome = "unknown-none";
  await expect(
    approve(api, b.pr, async () => s, [], unknown),
  ).rejects.toThrow();
  expect(unknown).toEqual({ attempted: true, resolved: false, reviews: [] });
});
test("known original review resolves a prior unknown intent and is reused", async () => {
  const api = new ApprovalApi();
  await approve(api, b.pr, async () => s, [], undefined, "run-8-attempt-1");
  const intent = {
    version: 1 as const,
    repositoryId: 1432619931,
    run: 8,
    attempt: 1,
    b,
    digest: s.digest,
  };
  expect((await approve(api, b.pr, async () => s, [intent])).reused).toBe(true);
  expect(api.posts).toHaveLength(1);
});
test("old dismissed same-binding review cannot resolve a later operation's unknown POST", async () => {
  const api = new ApprovalApi();
  api.reviews.push({
    id: 7,
    user,
    body: operationBody(s, "run-7-attempt-1"),
    state: "DISMISSED",
    commit_id: b.head,
  });
  const intent = {
    version: 1 as const,
    repositoryId: 1432619931,
    run: 8,
    attempt: 1,
    b,
    digest: s.digest,
  };
  await expect(
    approve(api, b.pr, async () => s, [intent], undefined, "run-9-attempt-1"),
  ).rejects.toThrow("unknown outcome");
  expect(api.posts).toHaveLength(0);
});
for (const stage of [2, 3])
  test(`drift at snapshot ${stage} stops or withdraws exact own approval`, async () => {
    const api = new ApprovalApi();
    let count = 0;
    await expect(
      approve(api, b.pr, async () =>
        ++count === stage ? { ...s, b: { ...b, merge: "e".repeat(40) } } : s,
      ),
    ).rejects.toThrow("changed");
    expect(api.posts).toHaveLength(stage === 2 ? 0 : 1);
    expect(api.reviews.every((r) => r.state === "DISMISSED")).toBe(true);
  });
test("failed withdrawal stays a failure and preserves the review ID", async () => {
  const api = new ApprovalApi();
  api.failDismiss = true;
  let count = 0;
  await expect(
    approve(api, b.pr, async () =>
      ++count === 3 ? { ...s, digest: "f".repeat(64) } : s,
    ),
  ).rejects.toThrow("1000 withdrawal not confirmed");
  expect(api.posts).toHaveLength(1);
  expect(api.reviews[0].state).toBe("APPROVED");
  expect(api.dismissalAttempts).toEqual([1000]);
});
test("unknown stale dismissal is only read back during generic cleanup", async () => {
  const api = new ApprovalApi();
  await approve(api, b.pr, async () => s);
  api.failDismiss = true;
  await expect(
    approve(api, b.pr, async () => ({ ...s, digest: "f".repeat(64) })),
  ).rejects.toThrow("1000 withdrawal not confirmed");
  expect(api.dismissalAttempts).toEqual([1000]);
  expect(api.posts).toHaveLength(1);
  expect(api.reviews[0].state).toBe("APPROVED");
});
test("unknown ambiguous-approval rollback does not repeat the dismissal PUT", async () => {
  const api = new ApprovalApi();
  api.outcome = "unknown-multiple";
  api.failDismiss = true;
  await expect(approve(api, b.pr, async () => s)).rejects.toThrow(
    "1000 withdrawal not confirmed",
  );
  expect(api.dismissalAttempts).toEqual([1000]);
  expect(api.posts).toHaveLength(1);
  expect(api.reviews.every((r) => r.state === "APPROVED")).toBe(true);
});
for (const stale of [false, true])
  test(`withdrawal readback failure preserves unknown and review ID, stale=${stale}`, async () => {
    const api = new ApprovalApi();
    const outcome: OperationOutcome = {
      attempted: false,
      resolved: false,
      reviews: [],
    };
    if (stale) await approve(api, b.pr, async () => s);
    api.failDismissRead = true;
    let count = 0;
    await expect(
      approve(
        api,
        b.pr,
        async () =>
          stale || ++count === 3 ? { ...s, digest: "f".repeat(64) } : s,
        [],
        outcome,
      ),
    ).rejects.toThrow("1000 withdrawal readback failed");
    expect(api.dismissalAttempts).toEqual([1000]);
    expect(outcome.resolved).toBe(false);
    expect(outcome.reviews).toEqual([1000]);
    expect(api.posts).toHaveLength(1);
  });
test("identity mismatch cannot be reused or dismissed as this App", async () => {
  const api = new ApprovalApi();
  api.reviews.push({
    id: 9,
    user: { ...user, id: 1 },
    body: approvalBody(s),
    state: "APPROVED",
    commit_id: b.head,
  });
  await approve(api, b.pr, async () => s);
  expect(api.dismissals).not.toContain(9);
  expect(api.posts).toHaveLength(1);
});
test("evidence read failure withdraws the exact old self approval", async () => {
  const api = new ApprovalApi();
  await approve(api, b.pr, async () => s);
  await expect(
    approve(api, b.pr, async () => {
      throw new Error("Incomplete page");
    }),
  ).rejects.toThrow("Incomplete page");
  expect(api.dismissals).toEqual([1000]);
});
