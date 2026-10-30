import { appId, repository, requireThat } from "./identity.ts";
import { snapshot } from "./evidence.ts";
import type { Api } from "./github.ts";
import type { Record } from "./providers.ts";
import type { Intent } from "./intent.ts";

export type Snapshot = Awaited<ReturnType<typeof snapshot>>;
export type OperationOutcome = {
  // This records only APPROVE POST, not token creation or dismissal PUT.
  attempted: boolean;
  resolved: boolean;
  reviews: number[];
};
export const approvalBody = (s: Snapshot, operation: string) =>
  `Cladiron verified the applicable provider reviews, finding dispositions, resolved threads and CI.\n\n<!-- cladiron-approval:v1 head=${s.b.head} base=${s.b.base} merge=${s.b.merge} evidence=${s.digest} operation=${operation} -->`;
function sameBinding(body: unknown, snapshot: Snapshot) {
  if (typeof body !== "string") return false;
  const operation = body.match(
    / operation=(run-[1-9][0-9]*-attempt-[1-9][0-9]*) -->$/,
  )?.[1];
  return operation !== undefined && body === approvalBody(snapshot, operation);
}
const prefix = `/repos/${repository}`;
async function ownership(api: Api, pr: number) {
  const profile = await api.get("/users/cladiron%5Bbot%5D");
  requireThat(
    Number.isSafeInteger(profile.id) &&
      profile.id > 0 &&
      profile.type === "Bot" &&
      profile.login === "cladiron[bot]",
    "Cannot bind Cladiron bot account",
  );
  const owned = (r: Record) =>
    r.user?.id === profile.id &&
    r.user?.type === "Bot" &&
    r.user?.login === profile.login &&
    (r.performed_via_github_app === undefined ||
      r.performed_via_github_app?.id === appId);
  const read = () => api.pages(`${prefix}/pulls/${pr}/reviews`);
  const dismissalAttempts = new Set<number>();
  const dismiss = async (review: Record) => {
    requireThat(
      owned(review) &&
        Number.isSafeInteger(review.id) &&
        review.body?.includes("<!-- cladiron-approval:v1 "),
      "Refusing to dismiss a foreign review",
    );
    let error: unknown;
    if (!dismissalAttempts.has(review.id)) {
      dismissalAttempts.add(review.id);
      try {
        await api.put(`${prefix}/pulls/${pr}/reviews/${review.id}/dismissals`, {
          message:
            "Cladiron evidence changed or approval outcome was ambiguous.",
          event: "DISMISS",
        });
      } catch (e) {
        error = e;
      }
    } else {
      error = new Error("Previous dismissal attempt requires readback");
    }
    let current: Record;
    try {
      current = await api.get(`${prefix}/pulls/${pr}/reviews/${review.id}`);
    } catch (cause) {
      throw new Error(
        `Cladiron approval ${review.id} withdrawal readback failed; outcome unknown`,
        { cause },
      );
    }
    if (!(
      owned(current) &&
      current.id === review.id &&
      current.commit_id === review.commit_id &&
      current.body === review.body &&
      current.state === "DISMISSED"
    ))
      throw new Error(
        `Cladiron approval ${review.id} withdrawal not confirmed${error ? " after unknown response" : ""}`,
        error ? { cause: error } : undefined,
      );
  };
  const dismissAll = async (reviews: Record[]) => {
    const failures: Error[] = [];
    for (const review of reviews) {
      try {
        await dismiss(review);
      } catch (cause) {
        failures.push(
          cause instanceof Error
            ? cause
            : new Error(`Cladiron approval ${review.id} withdrawal failed`, {
                cause,
              }),
        );
      }
    }
    if (failures.length)
      throw new AggregateError(
        failures,
        failures.map((e) => e.message).join("; "),
      );
  };
  return { owned, read, dismiss, dismissAll };
}
export async function withdraw(api: Api, pr: number) {
  requireThat(Number.isSafeInteger(pr) && pr > 0, "Invalid withdrawal PR");
  const { owned, read, dismissAll } = await ownership(api, pr);
  const prior = (await read()).filter(
    (r) =>
      owned(r) &&
      r.state === "APPROVED" &&
      r.body?.includes("<!-- cladiron-approval:v1 "),
  );
  await dismissAll(prior);
  return prior.map((r) => r.id);
}
export async function approve(
  api: Api,
  pr: number,
  operation: string,
  load: (api: Api, pr: number) => Promise<Snapshot> = snapshot,
  uncertain: Intent[] = [],
  outcome: OperationOutcome = {
    attempted: false,
    resolved: false,
    reviews: [],
  },
) {
  requireThat(
    /^run-[1-9][0-9]*-attempt-[1-9][0-9]*$/.test(operation),
    "Missing unique approval operation identity",
  );
  const { owned, read, dismiss, dismissAll } = await ownership(api, pr);
  let selected: Record | undefined;
  try {
    const first = await load(api, pr);
    const body = approvalBody(first, operation);
    const prior = await read();
    for (const intent of uncertain) {
      const originalBody = approvalBody(
        {
          b: intent.b,
          digest: intent.digest,
        } as Snapshot,
        `run-${intent.run}-attempt-${intent.attempt}`,
      );
      const original = prior.filter(
        (r) =>
          owned(r) &&
          r.commit_id === intent.b.head &&
          r.body === originalBody &&
          ["APPROVED", "DISMISSED"].includes(r.state),
      );
      requireThat(
        original.length === 1,
        `Previous approval run ${intent.run}/${intent.attempt} has unknown outcome; no POST retry`,
      );
    }
    const ownApprovals = prior.filter(
      (r) =>
        owned(r) &&
        r.state === "APPROVED" &&
        r.body?.includes("<!-- cladiron-approval:v1 "),
    );
    const reusable = ownApprovals.filter(
      (r) => r.commit_id === first.b.head && sameBinding(r.body, first),
    );
    requireThat(
      reusable.length <= 1,
      "Multiple existing approvals for one binding",
    );
    const stale = ownApprovals.filter((r) => !reusable.includes(r));
    if (stale.length) {
      outcome.resolved = false;
      outcome.reviews = [
        ...new Set([...outcome.reviews, ...stale.map((r) => r.id)]),
      ];
      await dismissAll(stale);
    }
    const second = await load(api, pr);
    requireThat(
      second.digest === first.digest &&
        JSON.stringify(second.b) === JSON.stringify(first.b),
      "Evidence changed before approval",
    );
    if (reusable.length) selected = reusable[0];
    else {
      const ids = new Set(prior.map((r) => r.id));
      let response: Record | undefined;
      outcome.attempted = true;
      try {
        response = await api.post(`${prefix}/pulls/${pr}/reviews`, {
          event: "APPROVE",
          commit_id: first.b.head,
          body,
        });
      } catch {
        // Unknown write: discover only newly created exact-target reviews, never POST again.
      }
      const after = await read();
      const matching = after.filter(
        (r) =>
          !ids.has(r.id) &&
          owned(r) &&
          r.commit_id === first.b.head &&
          r.body === body &&
          r.state === "APPROVED",
      );
      if (matching.length !== 1) {
        outcome.reviews = matching.map((r) => r.id);
        await dismissAll(matching);
        if (matching.length > 0) {
          outcome.resolved = true;
          outcome.reviews = matching.map((r) => r.id);
        }
        throw new Error(
          `Approval outcome unknown/ambiguous: ${matching.length} exact new reviews; no retry`,
        );
      }
      selected = matching[0];
      requireThat(
        !response || response.id === selected.id,
        "POST response and readback review IDs differ",
      );
    }
    const detail = await api.get(
      `${prefix}/pulls/${pr}/reviews/${selected.id}`,
    );
    requireThat(
      owned(detail) &&
        detail.id === selected.id &&
        detail.commit_id === first.b.head &&
        detail.body === selected.body &&
        sameBinding(detail.body, first) &&
        detail.state === "APPROVED",
      "Approval identity/head/body/state readback failed",
    );
    outcome.resolved = true;
    outcome.reviews = [detail.id];
    const final = await load(api, pr);
    requireThat(
      final.digest === first.digest &&
        JSON.stringify(final.b) === JSON.stringify(first.b),
      "Evidence changed after approval",
    );
    return {
      review: detail.id,
      head: first.b.head,
      base: first.b.base,
      merge: first.b.merge,
      digest: first.digest,
      reused: reusable.length === 1,
      cubic: first.evidence.providers.cubic,
      quota: first.evidence.providers.quota,
    };
  } catch (error) {
    try {
      if (selected) {
        outcome.resolved = false;
        outcome.reviews = [selected.id];
        await dismiss(selected);
        outcome.resolved = true;
      } else {
        // A failed read still invalidates this executor's old approvals; recover exact IDs first.
        const old = (await read()).filter(
          (r) =>
            owned(r) &&
            r.state === "APPROVED" &&
            r.body?.includes("<!-- cladiron-approval:v1 "),
        );
        if (old.length) {
          outcome.resolved = false;
          outcome.reviews = [
            ...new Set([...outcome.reviews, ...old.map((r) => r.id)]),
          ];
        }
        await dismissAll(old);
      }
    } catch (withdrawal) {
      throw new AggregateError(
        [error, withdrawal],
        withdrawal instanceof Error
          ? withdrawal.message
          : "Approval withdrawal failed",
      );
    }
    throw error;
  }
}
