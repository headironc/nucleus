import { describe, it, expect, vi } from "vitest";
import { Logout, hasPendingResult, type Marker } from "./recovery";
import { KEY, read, save, type Material } from "./material";
import {
  sameSelection,
  type BrowserSession,
  type Selection,
} from "../authentication/session";
import type { Account, CurrentResult } from "../protocol/session";
import type { RevokeResult, ReadResult } from "../protocol/revocation";
import { loginPath, logoutReturn } from "./return";

const operation = "b4e3bb16-fec5-4d86-9d2b-d172b8ea5000";
const first = "a4e3bb16-fec5-4d86-9d2b-d172b8ea5000";
const second = "c4e3bb16-fec5-4d86-9d2b-d172b8ea5000";
const revision = "d4e3bb16-fec5-4d86-9d2b-d172b8ea5000";
const accountId = "e4e3bb16-fec5-4d86-9d2b-d172b8ea5000";
const time = "2026-10-23T00:00:00Z";
function selection(id = first, rev = revision): BrowserSession {
  return {
    version: 1,
    session: { id, expiresAt: "2035-01-01T00:00:00Z" },
    revision: rev,
    attempt: null,
  };
}
function account(id = accountId): Account {
  return {
    id,
    personId: first,
    revisionId: revision,
    name: "虚构姓名",
    idNumber: { suffix: "002X" },
    binding: null,
    avatar: {
      revision,
      href: "https://fixture.invalid/avatar",
      expiresAt: time,
      mediaType: "image/webp",
      width: 512,
      height: 512,
    },
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
  const storage = {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => {
      values.set(key, value);
    },
    removeItem: (key: string) => {
      values.delete(key);
    },
  };
  let selected: BrowserSession | null = selection();
  let now = 10_000;
  let marker: Marker = null;
  let owner = accountId;
  const sessions = {
    read: vi.fn(async () => selected),
    clear: vi.fn(async (expected: Selection) => {
      if (!sameSelection(selected, expected)) return false;
      selected = selected?.attempt ? { ...selected, session: null } : null;
      return true;
    }),
  };
  const dependencies = {
    storage,
    sessions,
    now: () => now,
    uuid: () => operation,
    marker: () => marker,
    mark: (value: Marker = "submitted") => {
      marker = value;
    },
    build: vi.fn(async () => true),
    current: vi.fn(async (): Promise<CurrentResult> => ({
      kind: "account",
      account: account(owner),
    })),
    revoke: vi.fn(async (): Promise<RevokeResult> => ({
      kind: "disconnected",
    })),
    query: vi.fn(async (): Promise<ReadResult> => ({
      kind: "collection",
      collection: { code: "ok", original: null, count: 0, revocations: [] },
    })),
  };
  const logout = new Logout(dependencies);
  return {
    logout,
    dependencies,
    values,
    selected: () => selected,
    select: (value: BrowserSession | null) => {
      selected = value;
    },
    owner: (id: string) => {
      owner = id;
    },
    advance: (ms = 5000) => {
      now += ms;
    },
    reopen: () => new Logout(dependencies),
    material: () => read(storage) as Material,
  };
}

describe("one original current-session logout", () => {
  it.each([false, true])(
    "does not let an older unsent history entry erase a later pending logout (same controller: %s)",
    async (sameController) => {
      const f = fixture();
      await f.logout.verify();
      const set = f.dependencies.storage.setItem;
      f.dependencies.storage.setItem = () => {
        throw new Error("first save refused");
      };
      await f.logout.submit();
      const oldEntry = f.dependencies.marker();
      f.dependencies.storage.setItem = set;
      // A new entry can confirm a logout because the first one was never sent.
      f.dependencies.mark(null);
      f.dependencies.uuid = () => second;
      const later = f.reopen();
      await later.verify();
      await later.submit();
      expect(f.material().operation).toBe(second);
      // Back navigation restores the earlier entry, while sessionStorage is shared.
      f.dependencies.mark(oldEntry);
      const earlier = sameController ? f.logout : f.reopen();
      await earlier.verify();
      expect(earlier.snapshot()).toMatchObject({
        kind: "result",
        outcome: "pending",
      });
      expect(f.material().operation).toBe(second);
      await earlier.submit();
      expect(f.dependencies.revoke).toHaveBeenCalledTimes(1);
    },
  );
  it("retains a terminal result when clearing its history marker fails", async () => {
    const f = fixture();
    await f.logout.verify();
    f.dependencies.revoke.mockResolvedValue({
      kind: "failure",
      status: 400,
      code: "invalid_input",
      retryAfter: 0,
    });
    await f.logout.submit();
    f.advance();
    const mark = f.dependencies.mark;
    f.dependencies.mark = () => {
      throw new Error("history refused");
    };
    await f.logout.start();
    f.dependencies.mark = mark;
    await f.logout.verify();
    expect(f.logout.snapshot()).toMatchObject({
      kind: "result",
      outcome: "rejected",
      canStart: true,
    });
    await f.logout.start();
    expect(f.logout.snapshot().kind).toBe("ready");
    expect(f.dependencies.revoke).toHaveBeenCalledTimes(1);
  });
  it("keeps preflight work unsubmitted and stops it when suspended", async () => {
    const f = fixture();
    await f.logout.verify();
    const build = deferred<boolean>();
    f.dependencies.build.mockImplementationOnce(() => build.promise);
    const submitting = f.logout.submit();
    expect(f.logout.snapshot().kind).toBe("preparing");
    f.logout.suspend();
    build.resolve(true);
    await submitting;
    expect(f.dependencies.revoke).not.toHaveBeenCalled();
    expect(f.material()).toBeNull();
  });
  it.each(["throw", "readback"])(
    "reopens a known-unsent preparation after %s failure",
    async (mode) => {
      const f = fixture();
      await f.logout.verify();
      const set = f.dependencies.storage.setItem;
      const get = f.dependencies.storage.getItem;
      if (mode === "throw")
        f.dependencies.storage.setItem = () => {
          throw new Error("refused");
        };
      else
        f.dependencies.storage.getItem = (key) => {
          if (f.values.has(key)) throw new Error("readback refused");
          return get(key);
        };
      await f.logout.submit();
      expect(f.dependencies.revoke).not.toHaveBeenCalled();
      f.dependencies.storage.setItem = set;
      f.dependencies.storage.getItem = get;
      const reopened = f.reopen();
      await reopened.verify();
      expect(reopened.snapshot().kind).toBe("ready");
      expect(f.material()).toBeNull();
      expect(f.dependencies.revoke).not.toHaveBeenCalled();
      await reopened.submit();
      expect(f.dependencies.revoke).toHaveBeenCalledTimes(1);
    },
  );
  it("retains the original recovery link when its query confirms authentication expiry", async () => {
    const f = fixture();
    await f.logout.verify();
    await f.logout.submit();
    f.advance();
    f.dependencies.query.mockResolvedValue({
      kind: "failure",
      status: 401,
      code: "authentication_invalid",
      retryAfter: 0,
    });
    await f.logout.query();
    expect(f.selected()).toBeNull();
    expect(f.logout.snapshot()).toMatchObject({
      kind: "current",
      current: { kind: "expired", selection: { id: first, revision } },
      pending: true,
    });
    expect(f.material().outcome).toBe("pending");
    expect(f.dependencies.revoke).toHaveBeenCalledTimes(1);
  });
  it.each(["disconnected", "internal_error", "dependency_unavailable"])(
    "keeps a visible query failure after %s and reopening",
    async (code) => {
      const f = fixture();
      await f.logout.verify();
      await f.logout.submit();
      f.advance();
      f.dependencies.query.mockResolvedValue(
        code === "disconnected"
          ? { kind: "disconnected" }
          : { kind: "failure", status: 503, code, retryAfter: 0 },
      );
      await f.logout.query();
      expect(f.logout.snapshot()).toMatchObject({
        kind: "result",
        outcome: "pending",
        notice: "unavailable",
        canRead: true,
        canStart: false,
      });
      const reopened = f.reopen();
      await reopened.verify();
      expect(reopened.snapshot()).toMatchObject({
        kind: "result",
        notice: "unavailable",
      });
      expect(f.dependencies.query).toHaveBeenCalledTimes(1);
      expect(f.dependencies.revoke).toHaveBeenCalledTimes(1);
    },
  );
  it("does not infer original expiry from a newer login's query refusal", async () => {
    const f = fixture();
    await f.logout.verify();
    await f.logout.submit();
    f.select(selection(second));
    f.advance();
    await f.logout.verify();
    f.dependencies.query.mockResolvedValue({
      kind: "failure",
      status: 401,
      code: "authentication_invalid",
      retryAfter: 0,
    });
    await f.logout.query();
    expect(f.logout.snapshot()).toMatchObject({
      kind: "current",
      current: { kind: "expired" },
      pending: false,
    });
    expect(f.material().outcome).toBe("pending");
  });
  it.each([false, true])(
    "conceals a late query refusal when the selection changes during clearing (suspended: %s)",
    async (suspended) => {
      const f = fixture();
      await f.logout.verify();
      await f.logout.submit();
      f.advance();
      f.dependencies.query.mockResolvedValue({
        kind: "failure",
        status: 401,
        code: "authentication_invalid",
        retryAfter: 0,
      });
      const clearing = deferred<boolean>();
      const entered = deferred<void>();
      f.dependencies.sessions.clear.mockImplementationOnce(() => {
        entered.resolve();
        return clearing.promise;
      });
      const pending = f.logout.query();
      await entered.promise;
      f.select(selection(second));
      f.owner(second);
      if (suspended) await f.logout.verify();
      clearing.resolve(false);
      await pending;
      expect(f.logout.snapshot()).toEqual({ kind: "other" });
      expect(f.selected()?.session?.id).toBe(second);
      expect(f.dependencies.revoke).toHaveBeenCalledTimes(1);
    },
  );
  it("retains pending responsibility when post-send persistence fails", async () => {
    const f = fixture();
    await f.logout.verify();
    f.dependencies.revoke.mockImplementation(async () => {
      f.dependencies.storage.setItem = () => {
        throw new Error("refused after send");
      };
      return { kind: "disconnected" };
    });
    await f.logout.submit();
    expect(f.logout.snapshot()).toMatchObject({
      kind: "storage",
      unsubmitted: false,
      pending: true,
    });
    expect(f.dependencies.revoke).toHaveBeenCalledTimes(1);
  });
  it.each(["complete", "rejected"] as const)(
    "a selection cleanup failure does not make a persisted %s result unknown",
    async (outcome) => {
      const f = fixture();
      await f.logout.verify();
      f.dependencies.revoke.mockResolvedValue(
        outcome === "complete"
          ? {
              kind: "complete",
              completion: { id: operation, count: 1, revokedAt: time },
            }
          : {
              kind: "failure",
              code: "authentication_invalid",
              status: 401,
              retryAfter: 0,
            },
      );
      f.dependencies.sessions.clear.mockRejectedValue(
        new Error("IndexedDB unavailable"),
      );
      await f.logout.submit();
      expect(f.material().outcome).toBe(outcome);
      expect(f.logout.snapshot()).toEqual({
        kind: "storage",
        unsubmitted: false,
        pending: false,
      });
      await f.logout.submit();
      await f.logout.query();
      expect(f.dependencies.revoke).toHaveBeenCalledTimes(1);
      expect(f.dependencies.query).not.toHaveBeenCalled();
    },
  );
  it.each(["complete", "rejected"] as const)(
    "reload during selection cleanup keeps the saved %s outcome determined",
    async (outcome) => {
      const f = fixture();
      await f.logout.verify();
      const cleanup = deferred<boolean>();
      f.dependencies.sessions.clear.mockReturnValue(cleanup.promise);
      f.dependencies.revoke.mockResolvedValue(
        outcome === "complete"
          ? {
              kind: "complete",
              completion: { id: operation, count: 1, revokedAt: time },
            }
          : {
              kind: "failure",
              code: "authentication_invalid",
              status: 401,
              retryAfter: 0,
            },
      );
      const submitting = f.logout.submit();
      await vi.waitFor(() =>
        expect(f.dependencies.sessions.clear).toHaveBeenCalledTimes(1),
      );
      expect(f.material().outcome).toBe(outcome);
      expect(hasPendingResult(f.logout.snapshot())).toBe(false);
      f.logout.reload();
      expect(f.logout.snapshot()).toEqual({ kind: "reload", pending: false });
      cleanup.resolve(true);
      await submitting;
      expect(f.logout.snapshot()).toEqual({ kind: "reload", pending: false });
      expect(f.dependencies.revoke).toHaveBeenCalledTimes(1);
    },
  );
  it.each(["complete", "rejected", "query"] as const)(
    "a known %s result survives failed terminal storage and a same-controller check",
    async (result) => {
      const f = fixture();
      await f.logout.verify();
      const set = f.dependencies.storage.setItem;
      const refuseSave = () => {
        f.dependencies.storage.setItem = () => {
          throw new Error("terminal save unavailable");
        };
      };
      if (result === "query") {
        await f.logout.submit();
        f.advance();
        f.dependencies.query.mockImplementation(async () => {
          refuseSave();
          return {
            kind: "collection",
            collection: {
              code: "ok",
              original: { scope: "current" as const, initiatorId: first },
              count: 1,
              revocations: [{ id: first, revokedAt: time }],
            },
          };
        });
        await f.logout.query();
      } else {
        f.dependencies.revoke.mockImplementation(async () => {
          refuseSave();
          return result === "complete"
            ? {
                kind: "complete",
                completion: { id: operation, count: 1, revokedAt: time },
              }
            : {
                kind: "failure",
                code: "authentication_invalid",
                status: 401,
                retryAfter: 0,
              };
        });
        await f.logout.submit();
      }
      expect(f.material().outcome).toBe("pending");
      expect(hasPendingResult(f.logout.snapshot())).toBe(false);
      f.dependencies.storage.setItem = set;
      await f.logout.verify();
      expect(f.logout.snapshot()).toMatchObject({
        kind: "result",
        outcome: result === "rejected" ? "rejected" : "complete",
      });
      expect(f.material().outcome).toBe(
        result === "rejected" ? "rejected" : "complete",
      );
      expect(f.dependencies.revoke).toHaveBeenCalledTimes(1);
      expect(f.dependencies.query).toHaveBeenCalledTimes(
        result === "query" ? 1 : 0,
      );
    },
  );
  it("a cached terminal result never overwrites a later operation during storage recovery", async () => {
    const f = fixture();
    await f.logout.verify();
    const set = f.dependencies.storage.setItem;
    f.dependencies.revoke.mockImplementation(async () => {
      f.dependencies.storage.setItem = () => {
        throw new Error("terminal save unavailable");
      };
      return {
        kind: "complete",
        completion: { id: operation, count: 1, revokedAt: time },
      };
    });
    await f.logout.submit();
    f.dependencies.storage.setItem = set;
    const later = { ...f.material(), operation: second };
    save(f.dependencies.storage, later);
    await f.logout.verify();
    expect(f.material()).toEqual(later);
    expect(f.logout.snapshot()).toMatchObject({
      kind: "result",
      outcome: "pending",
    });
    expect(f.dependencies.revoke).toHaveBeenCalledTimes(1);
  });
  it.each([
    { field: "outcome", value: ["pending"] },
    { field: "outcome", value: ["rejected"] },
    { field: "notice", value: ["denied"] },
  ])(
    "closes malformed $field without querying or restarting the unknown logout",
    async ({ field, value }) => {
      const f = fixture();
      await f.logout.verify();
      await f.logout.submit();
      f.dependencies.storage.setItem(
        KEY,
        JSON.stringify({ ...f.material(), [field]: value, retryAt: 0 }),
      );
      f.advance();
      const reopened = f.reopen();
      await reopened.verify();
      await reopened.query();
      await reopened.start();
      await reopened.submit();
      expect(f.dependencies.query).not.toHaveBeenCalled();
      expect(f.dependencies.revoke).toHaveBeenCalledTimes(1);
      expect(f.dependencies.sessions.clear).not.toHaveBeenCalled();
      expect(f.dependencies.storage.getItem(KEY)).toBe('"missing"');
      expect(f.dependencies.marker()).toBe("submitted");
      expect(reopened.snapshot()).toEqual({ kind: "missing" });
      const again = f.reopen();
      await again.verify();
      await again.start();
      await again.submit();
      expect(again.snapshot()).toEqual({ kind: "missing" });
      expect(f.dependencies.storage.getItem(KEY)).toBe('"missing"');
      expect(f.dependencies.revoke).toHaveBeenCalledTimes(1);
    },
  );
  it("requires trusted current account and explicit confirmation, persisting before the only DELETE", async () => {
    const f = fixture();
    await f.logout.verify();
    expect(f.logout.snapshot().kind).toBe("ready");
    expect(f.dependencies.revoke).not.toHaveBeenCalled();
    f.dependencies.revoke.mockImplementation(async () => {
      expect(f.material()).toMatchObject({
        operation,
        account: accountId,
        selection: { id: first, revision },
        outcome: "pending",
      });
      expect(JSON.stringify(f.material())).not.toMatch(
        /虚构姓名|002X|token|secret/,
      );
      return { kind: "disconnected" };
    });
    await f.logout.submit();
    expect(f.dependencies.revoke).toHaveBeenCalledExactlyOnceWith(
      operation,
      first,
      expect.any(AbortSignal),
    );
    await f.logout.verify();
    await f.logout.submit();
    const reopened = f.reopen();
    await reopened.verify();
    expect(f.dependencies.revoke).toHaveBeenCalledTimes(1);
    expect(reopened.snapshot()).toMatchObject({
      kind: "result",
      outcome: "pending",
    });
  });
  it.each(["throw", "silent", "readback"])(
    "never sends when first storage persistence fails: %s",
    async (mode) => {
      const f = fixture();
      await f.logout.verify();
      const get = f.dependencies.storage.getItem;
      if (mode === "readback")
        f.dependencies.storage.getItem = (key) => {
          if (f.values.has(key)) throw new Error("refused");
          return get(key);
        };
      else
        f.dependencies.storage.setItem = () => {
          if (mode === "throw") throw new Error("refused");
        };
      await f.logout.submit();
      expect(f.dependencies.revoke).not.toHaveBeenCalled();
      expect(f.logout.snapshot()).toEqual({
        kind: "storage",
        unsubmitted: true,
        pending: false,
      });
    },
  );
  it("stops before persistence when the selected session changes during build verification", async () => {
    const f = fixture();
    await f.logout.verify();
    f.dependencies.build.mockImplementationOnce(async () => {
      f.select(selection(second));
      return true;
    });
    await f.logout.submit();
    expect(f.dependencies.revoke).not.toHaveBeenCalled();
    expect(f.material()).toBeNull();
  });
  it("rechecks identity after a failed first save and requires another explicit confirmation", async () => {
    const f = fixture();
    await f.logout.verify();
    const saveItem = f.dependencies.storage.setItem;
    f.dependencies.storage.setItem = () => {
      throw new Error("unavailable");
    };
    await f.logout.submit();
    f.dependencies.storage.setItem = saveItem;
    f.select(selection(second));
    f.owner(second);
    await f.logout.verify();
    expect(f.logout.snapshot()).toMatchObject({
      kind: "ready",
      account: { id: second },
      selection: { id: second },
    });
    expect(f.dependencies.revoke).not.toHaveBeenCalled();
    expect(f.material()).toBeNull();
    await f.logout.submit();
    expect(f.dependencies.revoke).toHaveBeenCalledExactlyOnceWith(
      operation,
      second,
      expect.any(AbortSignal),
    );
  });
  it.each(["disconnected", "untrusted", "unknown"])(
    "retains the exact original after %s",
    async (kind) => {
      const f = fixture();
      await f.logout.verify();
      f.dependencies.revoke.mockResolvedValue(
        kind === "unknown"
          ? {
              kind: "failure",
              status: 503,
              code: "outcome_unknown",
              retryAfter: 0,
            }
          : { kind: kind as "disconnected" | "untrusted" },
      );
      await f.logout.submit();
      expect(f.material()).toMatchObject({
        operation,
        account: accountId,
        selection: { id: first, revision },
        outcome: "pending",
      });
      expect(f.selected()?.session?.id).toBe(first);
      if (kind === "untrusted") expect(f.logout.snapshot().kind).toBe("reload");
    },
  );
  it("ending the wait leaves the original pending, and a late success cannot replace later identity", async () => {
    const f = fixture();
    const result = deferred<RevokeResult>();
    const started = deferred<void>();
    f.dependencies.revoke.mockImplementation(() => {
      started.resolve();
      return result.promise;
    });
    await f.logout.verify();
    const work = f.logout.submit();
    await started.promise;
    f.logout.suspend();
    f.select(selection(second));
    f.owner(second);
    await f.logout.verify();
    result.resolve({
      kind: "complete",
      completion: { id: operation, count: 1, revokedAt: time },
    });
    await work;
    expect(f.logout.snapshot().kind).toBe("other");
    expect(f.material().outcome).toBe("pending");
    expect(f.selected()?.session?.id).toBe(second);
    expect(f.dependencies.sessions.clear).not.toHaveBeenCalled();
  });
  it("a success clears only the original selection while a concurrent login attempt is preserved", async () => {
    const f = fixture();
    await f.logout.verify();
    f.dependencies.revoke.mockImplementation(async () => {
      f.select({ ...selection(), attempt: second });
      return {
        kind: "complete",
        completion: { id: operation, count: 1, revokedAt: time },
      };
    });
    await f.logout.submit();
    expect(f.selected()).toMatchObject({
      session: null,
      attempt: second,
      revision,
    });
    expect(f.material().outcome).toBe("complete");
  });
  it("a success received after a newer choice does not clear that choice", async () => {
    const f = fixture();
    await f.logout.verify();
    f.dependencies.revoke.mockImplementation(async () => {
      f.select(selection(second, operation));
      return {
        kind: "complete",
        completion: { id: operation, count: 1, revokedAt: time },
      };
    });
    await f.logout.submit();
    expect(f.selected()?.session?.id).toBe(second);
    expect(f.dependencies.sessions.clear).toHaveBeenCalledWith({
      id: first,
      revision,
    });
  });
  it.each([
    "invalid_input",
    "permission_denied",
    "authentication_invalid",
    "rate_limited",
  ])("keeps an initial known rejection distinct: %s", async (code) => {
    const f = fixture();
    await f.logout.verify();
    f.dependencies.revoke.mockResolvedValue({
      kind: "failure",
      status:
        code === "rate_limited"
          ? 429
          : code === "authentication_invalid"
            ? 401
            : code === "permission_denied"
              ? 403
              : 400,
      code,
      retryAfter: 19_000,
    });
    await f.logout.submit();
    expect(f.material().outcome).toBe("rejected");
    expect(f.material().retryAt).toBe(29_000);
    if (code === "authentication_invalid") {
      expect(f.logout.snapshot()).toMatchObject({
        kind: "current",
        pending: false,
        rejected: true,
      });
      await f.logout.verify();
      expect(f.logout.snapshot()).toMatchObject({
        kind: "current",
        current: { kind: "anonymous" },
        pending: false,
        rejected: false,
      });
    }
  });
  it("uses GET with the original session while still valid, and keeps empty results pending", async () => {
    const f = fixture();
    await f.logout.verify();
    await f.logout.submit();
    await f.logout.query();
    expect(f.dependencies.query).not.toHaveBeenCalled();
    f.advance();
    await f.logout.query();
    expect(f.dependencies.query).toHaveBeenCalledExactlyOnceWith(
      operation,
      first,
      first,
      expect.any(AbortSignal),
    );
    expect(f.material()).toMatchObject({ outcome: "pending", notice: "empty" });
    expect(f.dependencies.revoke).toHaveBeenCalledTimes(1);
  });
  it("same-account reauthentication only reads the original collection and does not clear the new session", async () => {
    const f = fixture();
    await f.logout.verify();
    await f.logout.submit();
    f.select(selection(second));
    f.advance();
    await f.logout.verify();
    f.dependencies.query.mockResolvedValue({
      kind: "collection",
      collection: {
        code: "ok",
        original: { scope: "current" as const, initiatorId: first },
        count: 1,
        revocations: [{ id: first, revokedAt: time }],
      },
    });
    await f.logout.query();
    expect(f.dependencies.query).toHaveBeenCalledWith(
      operation,
      first,
      second,
      expect.any(AbortSignal),
    );
    expect(f.dependencies.sessions.clear).not.toHaveBeenCalled();
    expect(f.selected()?.session?.id).toBe(second);
    expect(f.material()).toMatchObject({
      outcome: "complete",
      revokedAt: time,
    });
  });
  it.each([
    "permission_denied",
    "authentication_invalid",
    "rate_limited",
    "invalid_input",
  ])(
    "a query refusal never changes unknown logout into rejected: %s",
    async (code) => {
      const f = fixture();
      await f.logout.verify();
      await f.logout.submit();
      f.advance();
      f.dependencies.query.mockResolvedValue({
        kind: "failure",
        code,
        status:
          code === "authentication_invalid"
            ? 401
            : code === "rate_limited"
              ? 429
              : code === "permission_denied"
                ? 403
                : 400,
        retryAfter: 29_000,
      });
      await f.logout.query();
      expect(f.material().outcome).toBe("pending");
      expect(f.material().retryAt).toBe(44_000);
      const reopened = f.reopen();
      await reopened.verify();
      expect(f.material().outcome).toBe("pending");
      expect(f.dependencies.revoke).toHaveBeenCalledTimes(1);
    },
  );
  it("request_mismatch leads to original GET, never a second DELETE", async () => {
    const f = fixture();
    await f.logout.verify();
    f.dependencies.revoke.mockResolvedValue({
      kind: "failure",
      status: 409,
      code: "request_mismatch",
      retryAfter: 0,
    });
    await f.logout.submit();
    expect(f.logout.snapshot()).toMatchObject({
      kind: "result",
      notice: "mismatch",
      newLogin: false,
    });
    f.advance();
    await f.logout.query();
    expect(f.dependencies.query).toHaveBeenCalledTimes(1);
    expect(f.dependencies.revoke).toHaveBeenCalledTimes(1);
  });
  it("conceals old material and sends no GET across accounts or failed current checks", async () => {
    const f = fixture();
    await f.logout.verify();
    await f.logout.submit();
    f.advance();
    f.owner(second);
    f.select(selection(second));
    await f.logout.verify();
    await f.logout.query();
    expect(f.logout.snapshot()).toEqual({ kind: "other" });
    f.dependencies.current.mockResolvedValue({ kind: "disconnected" });
    await f.logout.verify();
    await f.logout.query();
    expect(f.logout.snapshot()).toMatchObject({
      kind: "current",
      current: { kind: "unavailable" },
      pending: false,
    });
    expect(f.dependencies.query).not.toHaveBeenCalled();
    expect(JSON.stringify(f.logout.snapshot())).not.toContain(accountId);
  });
  it("conceals a late GET after a different selection without accepting its result", async () => {
    const f = fixture();
    await f.logout.verify();
    await f.logout.submit();
    f.advance();
    const response = deferred<ReadResult>();
    const started = deferred<void>();
    f.dependencies.query.mockImplementation(() => {
      started.resolve();
      return response.promise;
    });
    const work = f.logout.query();
    await started.promise;
    f.select(selection(second));
    f.owner(second);
    response.resolve({
      kind: "collection",
      collection: {
        code: "ok",
        original: { scope: "current" as const, initiatorId: first },
        count: 1,
        revocations: [{ id: first, revokedAt: time }],
      },
    });
    await work;
    expect(f.logout.snapshot()).toEqual({ kind: "other" });
    expect(f.material().outcome).toBe("pending");
  });
  it("damaged or missing material cannot become a fresh logout after refresh", async () => {
    const f = fixture();
    f.values.set(KEY, "broken");
    await f.logout.verify();
    expect(f.logout.snapshot()).toEqual({ kind: "missing" });
    f.values.delete(KEY);
    await f.logout.verify();
    await f.logout.submit();
    expect(f.material()).toBe("missing");
    expect(f.dependencies.revoke).not.toHaveBeenCalled();
  });
  it("keeps missing material distinct from an original result that can be queried after login", async () => {
    const f = fixture();
    f.values.set(KEY, "broken");
    f.select(null);
    await f.logout.verify();
    expect(f.logout.snapshot()).toEqual({ kind: "missing" });
    await f.logout.query();
    await f.logout.submit();
    expect(f.dependencies.query).not.toHaveBeenCalled();
    expect(f.dependencies.revoke).not.toHaveBeenCalled();
  });
  it.each(["permission_denied", "invalid_input"])(
    "does not reopen a denied result query after refresh: %s",
    async (code) => {
      const f = fixture();
      await f.logout.verify();
      await f.logout.submit();
      f.advance();
      f.dependencies.query.mockResolvedValue({
        kind: "failure",
        code,
        status: code === "permission_denied" ? 403 : 400,
        retryAfter: 0,
      });
      await f.logout.query();
      if (code === "permission_denied")
        expect(f.logout.snapshot()).toMatchObject({
          kind: "result",
          outcome: "pending",
          notice: "denied",
          canRead: false,
          canStart: false,
        });
      f.advance();
      const reopened = f.reopen();
      await reopened.verify();
      await reopened.query();
      expect(reopened.snapshot()).toMatchObject({
        kind: "result",
        outcome: "pending",
        notice: "denied",
        canRead: false,
        canStart: false,
      });
      expect(f.dependencies.query).toHaveBeenCalledTimes(1);
      expect(f.dependencies.revoke).toHaveBeenCalledTimes(1);
    },
  );
  it.each(["submit", "query"] as const)(
    "keeps the pending leave responsibility after an untrusted %s response",
    async (action) => {
      const f = fixture();
      await f.logout.verify();
      if (action === "submit")
        f.dependencies.revoke.mockResolvedValue({ kind: "untrusted" });
      await f.logout.submit();
      if (action === "query") {
        f.advance();
        f.dependencies.query.mockResolvedValue({ kind: "untrusted" });
        await f.logout.query();
      }
      expect(f.logout.snapshot()).toEqual({ kind: "reload", pending: true });
      expect(f.material().outcome).toBe("pending");
      await f.logout.submit();
      await f.logout.query();
      expect(f.dependencies.revoke).toHaveBeenCalledTimes(1);
      expect(f.dependencies.query).toHaveBeenCalledTimes(
        action === "query" ? 1 : 0,
      );
      f.logout.suspend();
      expect(f.logout.snapshot()).toEqual({ kind: "reload", pending: false });
    },
  );
  it("a pre-submit build reload has no pending leave responsibility", async () => {
    const f = fixture();
    await f.logout.verify();
    f.dependencies.build.mockResolvedValue(false);
    await f.logout.submit();
    expect(f.logout.snapshot()).toEqual({ kind: "reload", pending: false });
    expect(f.dependencies.revoke).not.toHaveBeenCalled();
    expect(f.material()).toBeNull();
  });
  it.each(["submit", "query"] as const)(
    "an untrusted %s response cannot disclose pending responsibility after an unnoticed account change",
    async (action) => {
      const f = fixture();
      await f.logout.verify();
      const response = deferred<{ kind: "untrusted" }>();
      if (action === "submit")
        f.dependencies.revoke.mockReturnValue(response.promise);
      else {
        await f.logout.submit();
        f.advance();
        f.dependencies.query.mockReturnValue(response.promise);
      }
      const waiting = f.logout[action]();
      await vi.waitFor(() =>
        expect(f.logout.snapshot().kind).toBe(
          action === "submit" ? "submitting" : "querying",
        ),
      );
      f.select(selection(second));
      f.owner(second);
      response.resolve({ kind: "untrusted" });
      await waiting;
      expect(f.logout.snapshot()).toEqual({ kind: "other" });
      expect(f.material().outcome).toBe("pending");
      expect(f.dependencies.revoke).toHaveBeenCalledTimes(1);
    },
  );
  it("shows an immediate successful-clear receipt but does not replay it on anonymous reentry", async () => {
    const f = fixture();
    await f.logout.verify();
    f.dependencies.revoke.mockResolvedValue({
      kind: "complete",
      completion: { id: operation, count: 1, revokedAt: time },
    });
    await f.logout.submit();
    expect(f.selected()).toBeNull();
    expect(f.logout.snapshot()).toMatchObject({
      kind: "result",
      outcome: "complete",
      canStart: false,
    });
    await f.logout.verify();
    expect(f.logout.snapshot()).toMatchObject({
      kind: "current",
      current: { kind: "anonymous" },
      pending: false,
    });
    const reopened = f.reopen();
    await reopened.verify();
    expect(reopened.snapshot()).toMatchObject({
      kind: "current",
      current: { kind: "anonymous" },
      pending: false,
    });
    expect(f.material().outcome).toBe("complete");
    f.select(selection(second));
    await reopened.verify();
    expect(reopened.snapshot()).toMatchObject({
      kind: "result",
      outcome: "complete",
      newLogin: true,
    });
    expect(f.dependencies.sessions.clear).toHaveBeenCalledTimes(1);
  });
  it("does not disclose an old completion after another account is selected and later cleared", async () => {
    const f = fixture();
    await f.logout.verify();
    await f.logout.submit();
    f.select(selection(second));
    f.advance();
    await f.logout.verify();
    f.dependencies.query.mockResolvedValue({
      kind: "collection",
      collection: {
        code: "ok",
        original: { scope: "current" as const, initiatorId: first },
        count: 1,
        revocations: [{ id: first, revokedAt: time }],
      },
    });
    await f.logout.query();
    expect(f.logout.snapshot()).toMatchObject({
      kind: "result",
      outcome: "complete",
      newLogin: true,
    });
    expect(f.dependencies.sessions.clear).not.toHaveBeenCalled();
    f.owner(second);
    f.select(selection(operation));
    await f.logout.verify();
    expect(f.logout.snapshot()).toEqual({ kind: "other" });
    f.select(null);
    await f.logout.verify();
    expect(f.logout.snapshot()).toMatchObject({
      kind: "current",
      current: { kind: "anonymous" },
      pending: false,
    });
    f.owner(accountId);
    f.select(selection(second));
    await f.logout.verify();
    expect(f.logout.snapshot()).toMatchObject({
      kind: "result",
      outcome: "complete",
      newLogin: true,
    });
    expect(f.dependencies.revoke).toHaveBeenCalledTimes(1);
  });
  it.each([
    { outcome: "pending", terminal: "anonymous" },
    { outcome: "pending", terminal: "expired" },
    { outcome: "rejected", terminal: "anonymous" },
    { outcome: "rejected", terminal: "expired" },
  ] as const)(
    "conceals the old $outcome after another account becomes $terminal",
    async ({ outcome, terminal }) => {
      const f = fixture();
      await f.logout.verify();
      await f.logout.submit();
      save(f.dependencies.storage, { ...f.material(), outcome });
      f.owner(second);
      f.select(selection(second));
      await f.logout.verify();
      expect(f.logout.snapshot()).toEqual({ kind: "other" });
      if (terminal === "anonymous") f.select(null);
      else
        f.dependencies.current.mockResolvedValueOnce({
          kind: "failure",
          status: 401,
          code: "authentication_invalid",
          retryAfter: 0,
        });
      await f.logout.verify();
      expect(f.logout.snapshot()).toMatchObject({
        kind: "current",
        current: { kind: terminal },
        pending: false,
        rejected: false,
      });
      const reopened = f.reopen();
      await reopened.verify();
      expect(reopened.snapshot()).toMatchObject({
        kind: "current",
        pending: false,
        rejected: false,
      });
      f.owner(accountId);
      f.select(selection(first));
      await reopened.verify();
      expect(reopened.snapshot()).toMatchObject({ kind: "result", outcome });
      expect(f.dependencies.revoke).toHaveBeenCalledTimes(1);
      expect(f.dependencies.query).not.toHaveBeenCalled();
    },
  );
  it("shows the recovery reminder only for the original selection's verified expiration", async () => {
    const f = fixture();
    await f.logout.verify();
    await f.logout.submit();
    f.dependencies.current.mockResolvedValueOnce({
      kind: "failure",
      status: 401,
      code: "authentication_invalid",
      retryAfter: 0,
    });
    await f.logout.verify();
    expect(f.logout.snapshot()).toMatchObject({
      kind: "current",
      current: { kind: "expired" },
      pending: true,
      rejected: false,
    });
    await f.logout.verify();
    expect(f.logout.snapshot()).toMatchObject({
      kind: "current",
      current: { kind: "anonymous" },
      pending: false,
      rejected: false,
    });
  });
  it("accepts retained server identities independently of their UUID version", async () => {
    const f = fixture();
    const serverAccount = "00000000-0000-0000-0000-000000000000";
    const serverSession = "019354fd-933c-7e6a-8912-345678901234";
    f.owner(serverAccount);
    f.select(selection(serverSession));
    await f.logout.verify();
    await f.logout.submit();
    const reopened = f.reopen();
    await reopened.verify();
    f.advance();
    await reopened.query();
    expect(f.dependencies.query).toHaveBeenCalledExactlyOnceWith(
      operation,
      serverSession,
      serverSession,
      expect.any(AbortSignal),
    );
    expect(f.material()).toMatchObject({
      account: serverAccount,
      selection: { id: serverSession },
      outcome: "pending",
    });
  });
  it("permits a new confirmation only after a known outcome and explicit user action", async () => {
    const f = fixture();
    await f.logout.verify();
    await f.logout.submit();
    await f.logout.start();
    expect(f.material().outcome).toBe("pending");
    save(f.dependencies.storage, { ...f.material(), outcome: "rejected" });
    f.advance();
    await f.logout.verify();
    await f.logout.start();
    expect(f.material()).toBeNull();
    await f.logout.verify();
    expect(f.logout.snapshot().kind).toBe("ready");
    expect(f.dependencies.revoke).toHaveBeenCalledTimes(1);
  });
});

describe("fixed login return", () => {
  it("accepts only the fixed intent, without using arbitrary URLs or identifiers", () => {
    expect(loginPath("/authentication/login", "?return=logout-result")).toBe(
      "/authentication/login?return=logout-result",
    );
    for (const search of [
      "?return=https://evil.invalid",
      "?return=/sessions/logout",
      "?return=logout-result&return=logout-result",
      "?operation_id=" + operation,
    ]) {
      expect(logoutReturn(search)).toBe(false);
      expect(loginPath("/authentication/login", search)).toBe(
        "/authentication/login",
      );
    }
  });
});
