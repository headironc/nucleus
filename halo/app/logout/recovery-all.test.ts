import * as vitest from "vitest";
import * as recovery from "./recovery";
import * as material from "./material";
import * as session from "../authentication/session";
import * as protocol from "../protocol/session";

const operation = `b4e3bb16-fec5-4d86-9d2b-d172b8ea5000`;
const first = `a4e3bb16-fec5-4d86-9d2b-d172b8ea5000`;
const second = `c4e3bb16-fec5-4d86-9d2b-d172b8ea5000`;
const third = `d4e3bb16-fec5-4d86-9d2b-d172b8ea5000`;
const revision = `d4e3bb16-fec5-4d86-9d2b-d172b8ea5000`;
const accountId = `e4e3bb16-fec5-4d86-9d2b-d172b8ea5000`;
const time = `2026-10-23T00:00:00Z`;

type Marker = recovery.Marker;
type Revoke =
  | {
      kind: `complete`;
      completion: { id: string; count: number; revokedAt: string };
    }
  | {
      kind: `failure`;
      status: number;
      code: string;
      retryAfter: number;
    }
  | { kind: `disconnected` | `untrusted` };
type Read =
  | {
      kind: `collection`;
      collection: {
        code: string;
        original: { scope: "current" | "all"; initiatorId: string } | null;
        count: number;
        revocations: { id: string; revokedAt: string }[];
      };
    }
  | {
      kind: `failure`;
      status: number;
      code: string;
      retryAfter: number;
    }
  | { kind: `disconnected` | `untrusted` };

function selection(id = first, rev = revision): session.BrowserSession {
  return {
    version: 1,
    session: { id, expiresAt: `2035-01-01T00:00:00Z` },
    revision: rev,
    attempt: null,
  };
}
function account(id = accountId): protocol.Account {
  return {
    id,
    personId: first,
    revisionId: revision,
    name: `虚构姓名`,
    idNumber: { suffix: `002X` },
    binding: null,
    avatar: {
      revision,
      href: `https://fixture.invalid/avatar`,
      expiresAt: time,
      mediaType: `image/webp`,
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
  let selected: session.BrowserSession | null = selection();
  let now = 10_000;
  let marker: Marker = null;
  let owner = accountId;
  const sessions = {
    read: vitest.vi.fn(async () => selected),
    clear: vitest.vi.fn(async (expected: session.Selection) => {
      if (!session.sameSelection(selected, expected)) return false;
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
    mark: (value: Marker = `submitted`) => {
      marker = value;
    },
    build: vitest.vi.fn(async () => true),
    current: vitest.vi.fn(async () => ({
      kind: `account` as const,
      account: account(owner),
    })),
    revoke: vitest.vi.fn(async (): Promise<Revoke> => ({
      kind: `disconnected`,
    })),
    query: vitest.vi.fn(async (): Promise<Read> => ({
      kind: `collection`,
      collection: { code: `ok`, original: null, count: 0, revocations: [] },
    })),
  };
  const logout = new recovery.Logout(dependencies, `all`);
  return {
    logout,
    dependencies,
    values,
    selected: () => selected,
    select: (value: session.BrowserSession | null) => {
      selected = value;
    },
    owner: (id: string) => {
      owner = id;
    },
    advance: (ms = 5000) => {
      now += ms;
    },
    reopen: () => new recovery.Logout(dependencies, `all`),
    material: () => material.readAll(storage) as material.AllMaterial,
  };
}

vitest.describe(`the all-session logout controller`, () => {
  vitest.it.each([1, 3])(
    `persists explicit all scope before the only DELETE and reports %s sessions`,
    async (count) => {
      const f = fixture();
      await f.logout.verify();
      f.dependencies.revoke.mockImplementation(async () => {
        vitest.expect(f.material()).toMatchObject({
          scope: `all`,
          operation,
          account: accountId,
          selection: { id: first, revision },
          outcome: `pending`,
          binding: "pending",
          count: null,
          record: null,
        });
        vitest
          .expect(JSON.stringify(f.material()))
          .not.toMatch(/虚构姓名|002X|revocations/);
        return {
          kind: `complete`,
          completion: { id: operation, count, revokedAt: time },
        };
      });
      await f.logout.submit();
      vitest
        .expect(f.dependencies.revoke)
        .toHaveBeenCalledExactlyOnceWith(
          operation,
          first,
          vitest.expect.any(AbortSignal),
        );
      vitest.expect(f.selected()).toBeNull();
      vitest.expect(f.logout.snapshot()).toMatchObject({
        kind: `result`,
        outcome: `complete`,
        canStart: false,
      });
      vitest.expect(f.logout.detail()).toEqual({
        count,
        binding: "matched",
        record: null,
        revokedAt: time,
      });
      vitest.expect(f.material()).toMatchObject({
        outcome: `complete`,
        count,
        revokedAt: time,
        binding: "matched",
        record: null,
      });
      vitest.expect(JSON.stringify(f.material())).not.toContain(`revocations`);
    },
  );
  vitest.it(
    `never sends when the explicit all record cannot be persisted first`,
    async () => {
      const f = fixture();
      await f.logout.verify();
      const set = f.dependencies.storage.setItem;
      f.dependencies.storage.setItem = () => {
        throw new Error(`refused`);
      };
      await f.logout.submit();
      vitest.expect(f.dependencies.revoke).not.toHaveBeenCalled();
      vitest.expect(f.logout.snapshot()).toEqual({
        kind: `storage`,
        unsubmitted: true,
        pending: false,
      });
      f.dependencies.storage.setItem = set;
      const reopened = f.reopen();
      await reopened.verify();
      vitest.expect(reopened.snapshot().kind).toBe(`ready`);
      vitest.expect(f.material()).toBeNull();
      vitest.expect(f.dependencies.revoke).not.toHaveBeenCalled();
    },
  );
  vitest.it(
    `keeps an unknown all logout pending and queries it with verified membership`,
    async () => {
      const f = fixture();
      await f.logout.verify();
      await f.logout.submit();
      f.advance();
      const collection = {
        code: `ok`,
        original: { scope: "all" as const, initiatorId: first },
        count: 2,
        revocations: [
          { id: first, revokedAt: time },
          { id: second, revokedAt: time },
        ],
      };
      f.dependencies.query.mockResolvedValueOnce({
        kind: `collection`,
        collection,
      });
      await f.logout.query();
      vitest
        .expect(f.dependencies.query)
        .toHaveBeenCalledExactlyOnceWith(
          operation,
          first,
          first,
          vitest.expect.any(AbortSignal),
        );
      vitest.expect(f.material()).toMatchObject({
        outcome: `complete`,
        count: 2,
        revokedAt: time,
        binding: "matched",
      });
      vitest.expect(f.logout.detail()).toEqual({
        count: 2,
        binding: "matched",
        record: null,
        revokedAt: time,
      });
      vitest.expect(f.logout.snapshot()).toMatchObject({
        kind: `result`,
        outcome: `complete`,
      });
      vitest.expect(f.dependencies.revoke).toHaveBeenCalledTimes(1);
      vitest.expect(f.dependencies.sessions.clear).not.toHaveBeenCalled();
      vitest.expect(f.selected()?.session?.id).toBe(first);
    },
  );
  vitest.it(
    `keeps an empty all collection pending without a binding proof`,
    async () => {
      const f = fixture();
      await f.logout.verify();
      await f.logout.submit();
      f.advance();
      await f.logout.query();
      vitest.expect(f.material()).toMatchObject({
        outcome: `pending`,
        notice: `empty`,
        binding: "pending",
        count: null,
      });
      vitest.expect(f.logout.snapshot()).toMatchObject({
        kind: `result`,
        outcome: `pending`,
        notice: `empty`,
        canStart: false,
      });
      vitest.expect(f.dependencies.revoke).toHaveBeenCalledTimes(1);
    },
  );
  vitest.it.each([
    { scope: "current" as const, initiatorId: first },
    { scope: "all" as const, initiatorId: second },
  ])(`does not accept a different original binding %j`, async (original) => {
    const f = fixture();
    await f.logout.verify();
    await f.logout.submit();
    f.advance();
    f.dependencies.query.mockResolvedValueOnce({
      kind: "collection",
      collection: {
        code: "ok",
        original,
        count: 1,
        revocations: [{ id: original.initiatorId, revokedAt: time }],
      },
    });
    await f.logout.query();
    vitest
      .expect(f.material())
      .toMatchObject({
        outcome: "pending",
        binding: "mismatch",
        record: { count: 1, revokedAt: time },
      });
    vitest.expect(f.dependencies.sessions.clear).not.toHaveBeenCalled();
    vitest.expect(f.dependencies.revoke).toHaveBeenCalledTimes(1);
  });
  vitest.it(
    `never upgrades a known mismatch even if a later response claims a matching binding`,
    async () => {
      const f = fixture();
      await f.logout.verify();
      f.dependencies.revoke.mockResolvedValueOnce({
        kind: "failure",
        status: 409,
        code: "request_mismatch",
        retryAfter: 0,
      });
      await f.logout.submit();
      f.advance();
      f.dependencies.query.mockResolvedValueOnce({
        kind: "collection",
        collection: {
          code: "ok",
          original: { scope: "all", initiatorId: first },
          count: 1,
          revocations: [{ id: first, revokedAt: time }],
        },
      });
      await f.logout.query();
      vitest
        .expect(f.material())
        .toMatchObject({
          outcome: "pending",
          binding: "mismatch",
          record: { count: 1, revokedAt: time },
        });
    },
  );
  vitest.it(
    `permanently verifies the scope failure across later notices, reloads and logins`,
    async () => {
      const f = fixture();
      await f.logout.verify();
      f.dependencies.revoke.mockResolvedValueOnce({
        kind: `failure`,
        status: 409,
        code: `request_mismatch`,
        retryAfter: 0,
      });
      await f.logout.submit();
      vitest.expect(f.material()).toMatchObject({
        outcome: `pending`,
        notice: `mismatch`,
        binding: "mismatch",
        record: null,
      });
      f.advance();
      f.dependencies.query.mockResolvedValueOnce({
        kind: `collection`,
        collection: {
          code: `ok`,
          original: { scope: "all" as const, initiatorId: second },
          count: 2,
          revocations: [
            { id: second, revokedAt: time },
            { id: third, revokedAt: time },
          ],
        },
      });
      await f.logout.query();
      vitest
        .expect(f.dependencies.query)
        .toHaveBeenCalledWith(
          operation,
          first,
          first,
          vitest.expect.any(AbortSignal),
        );
      vitest.expect(f.material()).toMatchObject({
        outcome: `pending`,
        binding: "mismatch",
        record: { count: 2, revokedAt: time },
      });
      vitest.expect(f.logout.detail()).toEqual({
        count: null,
        binding: "mismatch",
        record: { count: 2, revokedAt: time },
        revokedAt: null,
      });
      f.advance();
      f.dependencies.query.mockResolvedValueOnce({
        kind: `failure`,
        status: 429,
        code: `rate_limited`,
        retryAfter: 0,
      });
      await f.logout.query();
      vitest.expect(f.material()).toMatchObject({
        binding: "mismatch",
        notice: `limited`,
        record: { count: 2, revokedAt: time },
      });
      f.advance();
      f.dependencies.query.mockResolvedValueOnce({
        kind: `collection`,
        collection: { code: `ok`, original: null, count: 0, revocations: [] },
      });
      await f.logout.query();
      vitest.expect(f.material()).toMatchObject({
        binding: "mismatch",
        notice: `empty`,
        record: { count: 2, revokedAt: time },
      });
      const reopened = f.reopen();
      await reopened.verify();
      vitest.expect(reopened.snapshot()).toMatchObject({
        kind: `result`,
        outcome: `pending`,
        canRead: true,
        canStart: false,
      });
      vitest.expect(reopened.detail()).toEqual({
        count: null,
        binding: "mismatch",
        record: { count: 2, revokedAt: time },
        revokedAt: null,
      });
      f.select(selection(second));
      await reopened.verify();
      vitest.expect(reopened.snapshot()).toMatchObject({
        kind: `result`,
        outcome: `pending`,
        newLogin: true,
      });
      vitest.expect(reopened.detail()?.binding).toBe("mismatch");
      await reopened.submit();
      await reopened.start();
      vitest.expect(f.dependencies.revoke).toHaveBeenCalledTimes(1);
    },
  );
  vitest.it(
    `conceals a late all completion after identity changes`,
    async () => {
      const f = fixture();
      const result = deferred<Revoke>();
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
        kind: `complete`,
        completion: { id: operation, count: 3, revokedAt: time },
      });
      await work;
      vitest.expect(f.logout.snapshot().kind).toBe(`other`);
      vitest.expect(f.material().outcome).toBe(`pending`);
      vitest.expect(f.material().binding).toBe("pending");
      vitest.expect(f.selected()?.session?.id).toBe(second);
      vitest.expect(f.dependencies.sessions.clear).not.toHaveBeenCalled();
    },
  );
  vitest.it(
    `a page interrupted before classifying DELETE recovers from the original binding`,
    async () => {
      const f = fixture();
      const response = deferred<Revoke>();
      const started = deferred<void>();
      f.dependencies.revoke.mockImplementationOnce(() => {
        started.resolve();
        return response.promise;
      });
      await f.logout.verify();
      const work = f.logout.submit();
      await started.promise;
      f.logout.suspend();
      f.dependencies.mark(null);
      const restored = f.reopen();
      await restored.verify();
      f.advance();
      f.dependencies.query.mockResolvedValueOnce({
        kind: "collection",
        collection: {
          code: "ok",
          original: { scope: "all" as const, initiatorId: first },
          count: 1,
          revocations: [{ id: first, revokedAt: time }],
        },
      });
      await restored.query();
      vitest
        .expect(restored.snapshot())
        .toMatchObject({ kind: "result", outcome: "complete" });
      vitest.expect(restored.detail()).toMatchObject({
        binding: "matched",
        count: 1,
        revokedAt: time,
        record: null,
      });
      response.resolve({
        kind: "complete",
        completion: { id: operation, count: 2, revokedAt: time },
      });
      await work;
      vitest.expect(f.material().binding).toBe("matched");
      vitest.expect(f.dependencies.revoke).toHaveBeenCalledTimes(1);
    },
  );
  vitest.it(
    `retains a known completion when session cleanup fails`,
    async () => {
      const f = fixture();
      await f.logout.verify();
      f.dependencies.revoke.mockResolvedValueOnce({
        kind: `complete`,
        completion: { id: operation, count: 3, revokedAt: time },
      });
      f.dependencies.sessions.clear.mockRejectedValueOnce(
        new Error(`IndexedDB unavailable`),
      );
      await f.logout.submit();
      vitest.expect(f.material()).toMatchObject({
        outcome: `complete`,
        count: 3,
      });
      vitest.expect(f.logout.snapshot()).toEqual({
        kind: `storage`,
        unsubmitted: false,
        pending: false,
      });
      await f.logout.verify();
      vitest.expect(f.logout.snapshot()).toMatchObject({
        kind: `result`,
        outcome: `complete`,
      });
      vitest.expect(f.logout.detail()?.count).toBe(3);
      vitest.expect(f.dependencies.revoke).toHaveBeenCalledTimes(1);
    },
  );
  vitest.it(
    `retains a known all completion when its terminal save fails`,
    async () => {
      const f = fixture();
      await f.logout.verify();
      const set = f.dependencies.storage.setItem;
      f.dependencies.revoke.mockImplementationOnce(async () => {
        f.dependencies.storage.setItem = () => {
          throw new Error(`terminal save unavailable`);
        };
        return {
          kind: `complete`,
          completion: { id: operation, count: 3, revokedAt: time },
        };
      });
      await f.logout.submit();
      f.dependencies.storage.setItem = set;
      await f.logout.verify();
      vitest.expect(f.material()).toMatchObject({
        outcome: `complete`,
        count: 3,
      });
      vitest.expect(f.logout.detail()?.count).toBe(3);
      vitest.expect(f.dependencies.revoke).toHaveBeenCalledTimes(1);
    },
  );
  vitest.it(
    `keeps a known all query completion when its terminal save fails`,
    async () => {
      const f = fixture();
      await f.logout.verify();
      await f.logout.submit();
      f.advance();
      const set = f.dependencies.storage.setItem;
      f.dependencies.query.mockImplementationOnce(async () => {
        f.dependencies.storage.setItem = () => {
          throw new Error(`terminal save unavailable`);
        };
        return {
          kind: `collection`,
          collection: {
            code: `ok`,
            original: { scope: "all" as const, initiatorId: first },
            count: 2,
            revocations: [
              { id: first, revokedAt: time },
              { id: second, revokedAt: time },
            ],
          },
        };
      });
      await f.logout.query();
      f.dependencies.storage.setItem = set;
      await f.logout.verify();
      vitest.expect(f.material()).toMatchObject({
        outcome: `complete`,
        count: 2,
      });
      vitest.expect(f.logout.detail()?.count).toBe(2);
      vitest.expect(f.dependencies.revoke).toHaveBeenCalledTimes(1);
    },
  );
  vitest.it(
    `closes damaged all material without sending or querying`,
    async () => {
      const f = fixture();
      f.values.set(material.ALL_KEY, `broken`);
      await f.logout.verify();
      vitest.expect(f.logout.snapshot()).toEqual({ kind: `missing` });
      await f.logout.submit();
      await f.logout.query();
      vitest.expect(f.dependencies.revoke).not.toHaveBeenCalled();
      vitest.expect(f.dependencies.query).not.toHaveBeenCalled();
      vitest
        .expect(f.dependencies.storage.getItem(material.ALL_KEY))
        .toBe(JSON.stringify(`missing`));
    },
  );
  vitest.it.each([false, true])(
    `retains mismatch after failed storage and a fresh entry (reopen: %s)`,
    async (reopen) => {
      const f = fixture();
      await f.logout.verify();
      const set = f.dependencies.storage.setItem;
      f.dependencies.revoke.mockImplementationOnce(async () => {
        f.dependencies.storage.setItem = () => {
          throw new Error("storage unavailable");
        };
        return {
          kind: "failure",
          status: 409,
          code: "request_mismatch",
          retryAfter: 0,
        };
      });
      await f.logout.submit();
      vitest.expect(f.material().binding).toBe("pending");
      f.dependencies.mark(null);
      f.dependencies.storage.setItem = set;
      const controller = reopen ? f.reopen() : f.logout;
      await controller.verify();
      vitest.expect(f.material().binding).toBe(reopen ? "pending" : "mismatch");
      f.advance();
      f.dependencies.query.mockResolvedValueOnce({
        kind: "collection",
        collection: {
          code: "ok",
          original: { scope: "current" as const, initiatorId: second },
          count: 1,
          revocations: [{ id: second, revokedAt: time }],
        },
      });
      await controller.query();
      vitest
        .expect(controller.snapshot())
        .toMatchObject({ kind: "result", outcome: "pending" });
      vitest.expect(controller.detail()).toMatchObject({
        binding: "mismatch",
        record: { count: 1, revokedAt: time },
      });
      vitest.expect(f.dependencies.revoke).toHaveBeenCalledTimes(1);
    },
  );
});
