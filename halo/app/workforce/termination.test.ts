import { afterEach, describe, expect, it, vi } from "vitest";
import { sameSelection, type BrowserSession } from "../authentication/session";
import type { Account, CurrentResult } from "../protocol/session";
import type {
  TerminationResult,
  WorkforceResult,
} from "../protocol/workforce";
import { KEY, type Material } from "./termination-material";
import { Termination, type Marker } from "./termination";

const id = "00000000-0000-4000-8000-000000000001";
const other = "00000000-0000-4000-8000-000000000002";
const accountId = "019354fd-933c-7e6a-8912-345678901234";
const personId = "018f3c1a-0000-7000-8000-000000000001";
const grantId = "018f3c1a-0000-7000-8000-000000000002";
const replacementGrant = "018f3c1a-0000-7000-8000-000000000003";
const operation = "4ef1d157-e547-4d18-bf84-45a159709a4b";
const nextOperation = "6ee1d157-e547-4d18-bf84-45a159709a4c";
const terminatedAt = "2026-10-24T00:00:00Z";
const account = {
  id: accountId,
  personId,
  revisionId: personId,
  name: "Current Person",
  idNumber: { suffix: "002X" },
  binding: null,
} as Account;
const otherAccount = {
  ...account,
  id: other,
  personId: other,
  revisionId: other,
  name: "Other Person",
} as Account;

function choice(selected = id, revision = id): BrowserSession {
  return {
    version: 1,
    revision,
    session: { id: selected, expiresAt: "2035-01-01T00:00:00Z" },
    attempt: null,
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

function fixture() {
  const values = new Map<string, string>();
  let selected: BrowserSession | null = choice();
  let now = 10_000;
  let identity = account;
  let service: WorkforceResult = {
    kind: "status",
    status: "active",
    grant: { id: grantId, personId },
  };
  let marker: Marker = null;
  let markFailure = false;
  let operations = 0;
  let refuse: ((key: string, bytes: string) => boolean) | undefined;
  const store = {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, bytes: string) => {
      if (refuse?.(key, bytes)) throw new Error("fixture refusal");
      values.set(key, bytes);
    },
    removeItem: (key: string) => {
      values.delete(key);
    },
  };
  const clear = vi.fn(async (expected: { id: string; revision: string }) => {
    if (!sameSelection(selected, expected)) return false;
    selected = null;
    return true;
  });
  const current = vi.fn<() => Promise<CurrentResult>>(async () => ({
    kind: "account" as const,
    account: identity,
  }));
  const workforce = vi.fn(async () => service);
  const terminate = vi.fn<
    (
      operation: string,
      grant: string,
      person: string,
      session: string,
      signal: AbortSignal,
    ) => Promise<TerminationResult>
  >(async () => ({ kind: "termination", terminatedAt }));
  const controller = new Termination({
    storage: store,
    sessions: { read: async () => selected, clear },
    current,
    workforce,
    terminate,
    build: async () => true,
    now: () => now,
    uuid: () => (operations++ === 0 ? operation : nextOperation),
    marker: () => marker,
    mark: (value = "submitted") => {
      if (markFailure) throw new Error("fixture history refusal");
      marker = value;
    },
  });
  return {
    controller,
    terminate,
    workforce,
    current,
    clear,
    values,
    marker: () => marker,
    setMarker: (value: Marker) => {
      marker = value;
    },
    setMarkFailure: (value: boolean) => {
      markFailure = value;
    },
    material(): Material | "missing" | null {
      const bytes = values.get(KEY);
      if (bytes === undefined) return null;
      try {
        return JSON.parse(bytes) as Material;
      } catch {
        return "missing";
      }
    },
    select: (value: BrowserSession | null) => {
      selected = value;
    },
    selection: () => selected,
    identity: () => identity,
    setIdentity: (value: Account) => {
      identity = value;
    },
    setService: (value: WorkforceResult) => {
      service = value;
    },
    refuse: (value?: (key: string, bytes: string) => boolean) => {
      refuse = value;
    },
    advance: (ms: number) => {
      now += ms;
    },
    now: () => now,
    async ready() {
      await controller.verify();
      await vi.waitFor(() =>
        expect(controller.snapshot().kind).not.toBe("current"),
      );
      await vi.waitFor(() =>
        expect(
          ["entry", "result", "missing", "other", "mismatch"].includes(
            controller.snapshot().kind,
          ),
        ).toBe(true),
      );
    },
    close() {
      controller.close();
    },
  };
}

afterEach(() => vi.useRealTimers());

describe("self termination page", () => {
  it.each(["current", "workforce"] as const)(
    "requires a page reload after an untrusted %s read and preserves the pending warning",
    async (source) => {
      const f = fixture();
      await f.ready();
      f.terminate.mockResolvedValueOnce({ kind: "disconnected" });
      await f.controller.submit();
      f[source].mockResolvedValue({ kind: "untrusted" });
      await f.controller.verify();
      await vi.waitFor(() =>
        expect(f.controller.snapshot()).toEqual({ kind: "reload", pending: true }),
      );
      await f.controller.verify();
      await f.controller.submit();
      await f.controller.query();
      expect(f.controller.snapshot()).toEqual({ kind: "reload", pending: true });
      expect(f.terminate).toHaveBeenCalledTimes(1);
      f.close();
    },
  );

  it("retains each account's completed result without blocking another account's own Grant", async () => {
    const f = fixture();
    await f.ready();
    await f.controller.submit();
    f.select(choice(other, other));
    f.setIdentity(otherAccount);
    f.setService({ kind: "status", status: "suspended", grant: { id: replacementGrant, personId: other } });
    await f.ready();
    expect(f.controller.snapshot()).toMatchObject({ kind: "entry", account: otherAccount, known: null });
    await f.controller.submit();
    expect(f.terminate.mock.calls[1].slice(0, 4)).toEqual([nextOperation, replacementGrant, other, other]);
    expect(f.material()).toMatchObject({ known: [
      { account: accountId, operation, grant: grantId },
      { account: other, operation: nextOperation, grant: replacementGrant },
    ], active: null });
    f.select(choice());
    f.setIdentity(account);
    f.setService({ kind: "status", status: "terminated", grant: { id: grantId, personId } });
    await f.ready();
    expect(f.controller.snapshot()).toMatchObject({ kind: "result", outcome: "complete", account, grant: grantId, terminatedAt });
    expect(f.terminate).toHaveBeenCalledTimes(2);
    f.close();
  });

  it.each([429, 400, 404, 409])(
    "lets another account explicitly replace a first-attempt rejection (%s)",
    async (status) => {
      const f = fixture();
      await f.ready();
      f.terminate.mockResolvedValueOnce({
        kind: "failure", status, code: "invalid_input", retryAfter: 60,
      });
      await f.controller.submit();
      expect(f.controller.snapshot()).toMatchObject({ kind: "result", outcome: "rejected" });
      const rejected = f.values.get(KEY);
      f.select(choice(other, other));
      f.setIdentity(otherAccount);
      f.setService({ kind: "status", status: "active", grant: { id: replacementGrant, personId: other } });
      await f.ready();
      expect(f.controller.snapshot()).toMatchObject({ kind: "entry", account: otherAccount });
      expect(f.values.get(KEY)).toBe(rejected);
      expect(f.terminate).toHaveBeenCalledTimes(1);
      f.select(choice());
      f.setIdentity(account);
      f.setService({ kind: "status", status: "active", grant: { id: grantId, personId } });
      await f.ready();
      expect(f.controller.snapshot()).toMatchObject({ kind: "result", outcome: "rejected" });
      f.select(choice(other, other));
      f.setIdentity(otherAccount);
      f.setService({ kind: "status", status: "active", grant: { id: replacementGrant, personId: other } });
      await f.ready();
      await f.controller.submit();
      expect(f.terminate.mock.calls[1].slice(0, 4)).toEqual([nextOperation, replacementGrant, other, other]);
      expect(f.controller.snapshot()).toMatchObject({ kind: "result", outcome: "complete", account: otherAccount });
      f.close();
    },
  );

  it.each(["active", "suspended"] as const)(
    "requires explicit confirmation of a new %s Grant after a definite conflict",
    async (status) => {
      const f = fixture();
      await f.ready();
      f.terminate.mockResolvedValueOnce({ kind: "failure", status: 409, code: "state_conflict", retryAfter: 0 });
      await f.controller.submit();
      const rejected = f.values.get(KEY);
      expect(f.controller.snapshot()).toMatchObject({ kind: "result", outcome: "rejected", canStart: false });
      await f.controller.start();
      expect(f.values.get(KEY)).toBe(rejected);
      f.setService({ kind: "disconnected" });
      await f.controller.verify();
      await vi.waitFor(() => expect(f.controller.snapshot()).toMatchObject({ kind: "result", canStart: false, service: { kind: "unavailable" } }));
      f.setService({ kind: "status", status, grant: { id: replacementGrant, personId } });
      f.advance(5_000);
      await f.controller.verify();
      await vi.waitFor(() => expect(f.controller.snapshot()).toMatchObject({ kind: "result", outcome: "rejected", canStart: true }));
      await f.controller.submit();
      expect(f.terminate).toHaveBeenCalledTimes(1);
      expect(f.values.get(KEY)).toBe(rejected);
      await f.controller.start();
      await vi.waitFor(() => expect(f.controller.snapshot()).toMatchObject({ kind: "entry", grant: { id: replacementGrant } }));
      expect(f.terminate).toHaveBeenCalledTimes(1);
      await f.controller.submit();
      expect(f.terminate.mock.calls[1].slice(0, 4)).toEqual([nextOperation, replacementGrant, personId, id]);
      expect(f.controller.snapshot()).toMatchObject({ kind: "result", outcome: "complete", grant: replacementGrant });
      f.close();
    },
  );

  it.each([false, true])("keeps the original unknown when missing-material persistence fails: %s", async (refused) => {
    const f = fixture();
    await f.ready();
    f.terminate.mockResolvedValueOnce({ kind: "disconnected" });
    await f.controller.submit();
    f.values.delete(KEY);
    if (refused) f.refuse((_key, bytes) => bytes === JSON.stringify("missing"));
    f.advance(5_000);
    await f.controller.query();
    expect(f.controller.snapshot()).toMatchObject(refused
      ? { kind: "storage", pending: true, unsubmitted: false }
      : { kind: "missing" });
    await f.controller.submit();
    expect(f.terminate).toHaveBeenCalledTimes(1);
    f.refuse();
    await f.ready();
    expect(f.controller.snapshot().kind).toBe("missing");
    expect(f.material()).toBe("missing");
    await f.controller.submit();
    expect(f.terminate).toHaveBeenCalledTimes(1);
    f.close();
  });

  it("keeps the submitted marker pending when a fresh page cannot save its missing placeholder", async () => {
    const f = fixture();
    f.setMarker("submitted");
    f.refuse(() => true);
    await f.controller.verify();
    expect(f.controller.snapshot()).toMatchObject({ kind: "storage", pending: true, unsubmitted: false });
    await f.controller.submit();
    expect(f.terminate).not.toHaveBeenCalled();
    f.close();
  });

  it.each(["account", "person", "grant"] as const)("does not adopt a changed %s for the remembered original", async (field) => {
    const f = fixture();
    await f.ready();
    f.terminate.mockResolvedValueOnce({ kind: "disconnected" });
    await f.controller.submit();
    const saved = JSON.parse(f.values.get(KEY)!);
    saved.active[field] = other;
    f.values.set(KEY, JSON.stringify(saved));
    f.advance(5_000);
    await f.controller.query();
    expect(f.controller.snapshot().kind).toBe("missing");
    expect(f.material()).toBe("missing");
    await f.controller.submit();
    expect(f.terminate).toHaveBeenCalledTimes(1);
    f.close();
  });

  it("binds the visible Grant, person and selection and saves before sending", async () => {
    const f = fixture();
    await f.ready();
    expect(f.controller.snapshot()).toMatchObject({
      kind: "entry",
      status: "active",
      grant: { id: grantId, personId },
    });
    expect(f.terminate).not.toHaveBeenCalled();
    await f.controller.submit();
    expect(f.terminate).toHaveBeenCalledTimes(1);
    expect(f.terminate.mock.calls[0].slice(0, 4)).toEqual([
      operation,
      grantId,
      personId,
      id,
    ]);
    expect(f.material()).toMatchObject({
      known: [{
        operation,
        account: accountId,
        person: personId,
        grant: grantId,
        terminatedAt,
      }],
      active: null,
    });
    expect(f.controller.snapshot()).toMatchObject({
      kind: "result",
      outcome: "complete",
      persisted: true,
      allowNew: false,
    });
    expect(f.marker()).toBeNull();
    f.close();
  });

  it("ends a suspended Grant for an available account", async () => {
    const f = fixture();
    f.setService({
      kind: "status",
      status: "suspended",
      grant: { id: grantId, personId },
    });
    await f.ready();
    expect(f.controller.snapshot()).toMatchObject({
      kind: "entry",
      status: "suspended",
    });
    await f.controller.submit();
    expect(f.terminate).toHaveBeenCalledTimes(1);
    expect(f.terminate.mock.calls[0][1]).toBe(grantId);
    f.close();
  });

  it.each(["throw", "silent"])(
    "sends nothing when the pre-send save fails (%s)",
    async (mode) => {
      const f = fixture();
      await f.ready();
      let writes = 0;
      if (mode === "throw")
        f.refuse((key) => {
          if (key !== KEY) return false;
          writes++;
          return writes === 1;
        });
      else f.refuse(() => true);
      await f.controller.submit();
      expect(f.terminate).not.toHaveBeenCalled();
      expect(f.controller.snapshot()).toMatchObject({
        kind: "storage",
        unsubmitted: true,
      });
      f.refuse();
      f.close();
    },
  );

  it("sends nothing when the history marker cannot be confirmed", async () => {
    const f = fixture();
    await f.ready();
    f.setMarkFailure(true);
    await f.controller.submit();
    expect(f.terminate).not.toHaveBeenCalled();
    expect(f.controller.snapshot()).toMatchObject({
      kind: "storage",
      unsubmitted: true,
    });
    f.close();
  });

  it("re-entering after a failed pre-send save shows the entry, not an unknown request", async () => {
    const f = fixture();
    await f.ready();
    f.values.set(
      KEY,
      JSON.stringify({
        version: 1,
        known: [],
        active: {
          operation,
          account: accountId,
          person: personId,
          grant: grantId,
          selection: { id, revision: id },
          outcome: "pending",
          notice: "pending",
          sent: false,
          retryAt: 10_000,
        },
      }),
    );
    await f.ready();
    expect(f.controller.snapshot().kind).toBe("entry");
    expect(f.values.get(KEY)).toBeUndefined();
    expect(f.terminate).not.toHaveBeenCalled();
    f.close();
  });

  it("treats a submitted history marker without material as an unavailable result", async () => {
    const f = fixture();
    f.setMarker("submitted");
    await f.ready();
    expect(f.controller.snapshot().kind).toBe("missing");
    expect(f.values.get(KEY)).toBe(JSON.stringify("missing"));
    expect(f.terminate).not.toHaveBeenCalled();
    f.close();
  });

  it("returns the first trusted 429 to the confirmation after the wait", async () => {
    const f = fixture();
    await f.ready();
    f.terminate.mockResolvedValueOnce({
      kind: "failure",
      status: 429,
      code: "rate_limited",
      retryAfter: 60_000,
    });
    await f.controller.submit();
    expect(f.controller.snapshot()).toMatchObject({
      kind: "result",
      outcome: "rejected",
      notice: "limited",
      retryAt: 70_000,
    });
    await f.controller.start();
    expect(f.controller.snapshot().kind).toBe("result");
    f.advance(60_000);
    await f.controller.start();
    await vi.waitFor(() =>
      expect(f.controller.snapshot().kind).toBe("entry"),
    );
    expect(f.terminate).toHaveBeenCalledTimes(1);
    f.close();
  });

  it("keeps an unknown original through a replay 429 but blocks a permanent mismatch", async () => {
    const f = fixture();
    await f.ready();
    f.terminate.mockResolvedValueOnce({ kind: "disconnected" });
    await f.controller.submit();
    expect(f.controller.snapshot()).toMatchObject({
      kind: "result",
      outcome: "pending",
    });
    f.advance(5_000);
    f.terminate.mockResolvedValueOnce({
      kind: "failure",
      status: 429,
      code: "rate_limited",
      retryAfter: 30_000,
    });
    await f.controller.query();
    expect(f.controller.snapshot()).toMatchObject({
      kind: "result",
      outcome: "pending",
      notice: "limited",
    });
    f.advance(30_000);
    f.terminate.mockResolvedValueOnce({
      kind: "failure",
      status: 409,
      code: "request_mismatch",
      retryAfter: 0,
    });
    await f.controller.query();
    expect(f.controller.snapshot().kind).toBe("mismatch");
    expect(f.material()).toMatchObject({
      active: { outcome: "pending", notice: "mismatch" },
    });
    expect(f.terminate.mock.calls.map((call) => call[0])).toEqual([
      operation,
      operation,
      operation,
    ]);
    f.close();
    const again = fixture();
    again.values.set(KEY, f.values.get(KEY)!);
    await again.ready();
    expect(again.controller.snapshot().kind).toBe("mismatch");
    await again.controller.query();
    expect(again.terminate).not.toHaveBeenCalled();
    again.close();
  });

  it("replays the exact original through a same-account new session", async () => {
    const f = fixture();
    await f.ready();
    f.terminate.mockResolvedValueOnce({ kind: "disconnected" });
    await f.controller.submit();
    f.advance(5_000);
    f.select(choice(other, other));
    await f.ready();
    expect(f.controller.snapshot()).toMatchObject({
      kind: "result",
      outcome: "pending",
    });
    f.terminate.mockResolvedValueOnce({ kind: "termination", terminatedAt });
    await f.controller.query();
    expect(f.terminate.mock.calls[1].slice(0, 4)).toEqual([
      operation,
      grantId,
      personId,
      other,
    ]);
    expect(f.controller.snapshot()).toMatchObject({
      kind: "result",
      outcome: "complete",
    });
    f.close();
  });

  it("conceals another account's record and never sends its material", async () => {
    const f = fixture();
    await f.ready();
    f.terminate.mockResolvedValueOnce({ kind: "disconnected" });
    await f.controller.submit();
    f.setIdentity(otherAccount);
    await f.ready();
    expect(f.controller.snapshot().kind).toBe("other");
    await f.controller.query();
    await f.controller.submit();
    expect(f.terminate).toHaveBeenCalledTimes(1);
    f.close();
  });

  it("drops a late success after the selection and identity generation changed", async () => {
    const f = fixture();
    await f.ready();
    const held = deferred<TerminationResult>();
    f.terminate.mockImplementationOnce(() => held.promise);
    const submitting = f.controller.submit();
    await vi.waitFor(() => expect(f.terminate).toHaveBeenCalledTimes(1));
    f.select(choice(other, other));
    f.controller.suspend();
    await f.ready();
    held.resolve({ kind: "termination", terminatedAt });
    await submitting;
    expect(f.controller.snapshot()).toMatchObject({
      kind: "result",
      outcome: "pending",
    });
    expect(f.material()).toMatchObject({
      known: [],
      active: { outcome: "pending", sent: true },
    });
    f.close();
  });

  it("keeps an accepted result when its terminal save fails and on current-read failure", async () => {
    const f = fixture();
    await f.ready();
    f.refuse((key, bytes) => key === KEY && bytes.includes("terminatedAt"));
    f.setService({
      kind: "failure",
      status: 503,
      code: "dependency_unavailable",
      retryAfter: 0,
    });
    await f.controller.submit();
    await vi.waitFor(() =>
      expect(f.controller.snapshot()).toMatchObject({
        service: { kind: "unavailable" },
      }),
    );
    expect(f.controller.snapshot()).toMatchObject({
      kind: "result",
      outcome: "complete",
      persisted: false,
      service: { kind: "unavailable" },
    });
    expect(f.material()).toMatchObject({
      active: { outcome: "pending", sent: true },
    });
    await f.ready();
    expect(f.controller.snapshot()).toMatchObject({
      kind: "result",
      outcome: "complete",
      persisted: false,
    });
    await f.controller.query();
    expect(f.terminate).toHaveBeenCalledTimes(1);
    f.close();
  });

  it("keeps a trusted mismatch blocked when storage fails and identity is rechecked", async () => {
    const f = fixture();
    await f.ready();
    f.terminate.mockResolvedValueOnce({ kind: "disconnected" });
    await f.controller.submit();
    f.advance(5_000);
    f.refuse((key, bytes) => key === KEY && bytes.includes("mismatch"));
    f.terminate.mockResolvedValueOnce({
      kind: "failure",
      status: 409,
      code: "request_mismatch",
      retryAfter: 0,
    });
    await f.controller.query();
    expect(f.controller.snapshot().kind).toBe("mismatch");
    await f.ready();
    expect(f.controller.snapshot().kind).toBe("mismatch");
    f.advance(5_000);
    await f.controller.query();
    expect(f.terminate).toHaveBeenCalledTimes(2);
    f.close();
  });

  it("does not invent a sent request when the final preparation save fails", async () => {
    const f = fixture();
    await f.ready();
    f.refuse((key, bytes) => key === KEY && bytes.includes('"sent":true'));
    await f.controller.submit();
    expect(f.terminate).not.toHaveBeenCalled();
    expect(f.controller.snapshot()).toMatchObject({
      kind: "storage",
      unsubmitted: true,
    });
    f.refuse();
    await f.ready();
    expect(f.controller.snapshot().kind).toBe("entry");
    expect(f.terminate).not.toHaveBeenCalled();
    f.close();
  });

  it("retains a new pending original after an earlier unsaved success", async () => {
    const f = fixture();
    await f.ready();
    f.refuse((key, bytes) => key === KEY && bytes.includes("terminatedAt"));
    await f.controller.submit();
    f.refuse();
    f.setService({
      kind: "status",
      status: "active",
      grant: { id: replacementGrant, personId },
    });
    await f.ready();
    await vi.waitFor(() =>
      expect(f.controller.snapshot()).toMatchObject({ allowNew: true }),
    );
    f.terminate.mockResolvedValueOnce({ kind: "disconnected" });
    await f.controller.submit();
    await f.ready();
    expect(f.controller.snapshot()).toMatchObject({
      kind: "result",
      outcome: "pending",
      grant: replacementGrant,
      previous: terminatedAt,
    });
    expect(f.terminate).toHaveBeenCalledTimes(2);
    f.close();
  });

  it("keeps the known result while a different current Grant can be ended explicitly", async () => {
    const f = fixture();
    await f.ready();
    await f.controller.submit();
    f.setService({
      kind: "status",
      status: "active",
      grant: { id: replacementGrant, personId },
    });
    await f.ready();
    await vi.waitFor(() =>
      expect(f.controller.snapshot()).toMatchObject({
        service: { kind: "ready", grant: { id: replacementGrant } },
      }),
    );
    expect(f.controller.snapshot()).toMatchObject({
      kind: "result",
      outcome: "complete",
      allowNew: true,
      service: { kind: "ready", grant: { id: replacementGrant } },
    });
    expect(f.terminate).toHaveBeenCalledTimes(1);
    await f.controller.submit();
    expect(f.terminate).toHaveBeenCalledTimes(2);
    expect(f.terminate.mock.calls[1][1]).toBe(replacementGrant);
    expect(f.terminate.mock.calls[1][0]).toBe(nextOperation);
    expect(f.controller.snapshot()).toMatchObject({
      kind: "result",
      outcome: "complete",
      grant: replacementGrant,
    });
    f.close();
  });

  it.each([401, 403] as const)(
    "handles a trusted %i without losing the original",
    async (status) => {
      const f = fixture();
      await f.ready();
      f.terminate.mockResolvedValueOnce({ kind: "disconnected" });
      await f.controller.submit();
      f.advance(5_000);
      f.terminate.mockResolvedValueOnce({
        kind: "failure",
        status,
        code: status === 401 ? "authentication_invalid" : "permission_denied",
        retryAfter: 0,
      });
      await f.controller.query();
      expect(f.controller.snapshot()).toMatchObject({
        kind: "current",
        current: { kind: status === 401 ? "expired" : "denied" },
        pending: true,
      });
      expect(f.material()).toMatchObject({
        active: {
          outcome: "pending",
          notice: status === 401 ? "authentication" : "denied",
        },
      });
      expect(f.clear).toHaveBeenCalledTimes(status === 401 ? 1 : 0);
      f.close();
    },
  );

  it("aborts a 35 second wait and keeps the original unknown", async () => {
    const f = fixture();
    await f.ready();
    vi.useFakeTimers();
    f.terminate.mockImplementationOnce(
      (_operation, _grant, _person, _session, signal) =>
        new Promise<TerminationResult>((resolve) => {
          signal.addEventListener("abort", () =>
            resolve({ kind: "disconnected" }),
          );
        }),
    );
    const submitting = f.controller.submit();
    await vi.advanceTimersByTimeAsync(0);
    expect(f.terminate).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(35_001);
    await submitting;
    expect(f.controller.snapshot()).toMatchObject({
      kind: "result",
      outcome: "pending",
    });
    f.close();
  });
});
