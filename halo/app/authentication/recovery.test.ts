import { describe, expect, it, vi } from "vitest";
import { Login, LOGIN_KEY } from "./recovery";
import {
  MESSAGE_KEY,
  receiveCode,
  maintainMaterial,
  blocksSend,
  receiptKey,
} from "./material";
import type { Attempt, BrowserSession, Selection } from "./session";
import { SessionStorageError } from "./session";
import type { Input, LoginResult, Session } from "../protocol/session";

const operation = "e8a537ec-584b-40af-8026-d3cc3292b762";
const revision = "b8750f17-c9cf-4ee6-b52c-1347cfa8061a";
const second = "7cb34797-43cf-4b7a-8c53-b631788d492f";
const session = {
  id: "019354fd-933c-7e6a-8912-345678901234",
  expiresAt: "2035-01-02T00:00:00Z",
};
const registration = {
  id: session.id,
  token: "A".repeat(43),
  expiresAt: "2035-01-01T00:10:00Z",
};

function fixture() {
  let now = Date.parse("2035-01-01T00:00:00Z");
  const records = new Map<string, string>();
  let selected: BrowserSession | null = null;
  let marked = false;
  const storage = {
    get length() {
      return records.size;
    },
    key: (index: number) => [...records.keys()][index] ?? null,
    getItem: vi.fn((key: string) => records.get(key) ?? null),
    setItem: vi.fn((key: string, value: string) => {
      records.set(key, value);
    }),
    removeItem: vi.fn((key: string) => {
      records.delete(key);
    }),
  };
  receiveCode(
    storage,
    operation,
    "13900000000",
    {
      id: session.id,
      delivery: "sent",
      expiresAt: "2035-01-01T00:05:00Z",
    },
    now,
  );
  const sessions = {
    read: vi.fn(async () => structuredClone(selected)),
    begin: vi.fn(async (id: string): Promise<Attempt> => {
      selected = {
        version: 1,
        session: selected?.session ?? null,
        attempt: id,
        revision,
      };
      return { operation: id, revision };
    }),
    accept: vi.fn(
      async (attempt: Attempt, value: Session): Promise<Selection | null> => {
        if (
          selected?.attempt !== attempt.operation ||
          selected.revision !== attempt.revision
        )
          return null;
        selected = {
          version: 1,
          session: value,
          attempt: null,
          revision: second,
        };
        return { id: value.id, revision: second };
      },
    ),
  };
  const dependencies = {
    storage,
    sessions,
    now: () => now,
    uuid: vi.fn(() => operation),
    secret: vi.fn(() => "A".repeat(43)),
    current: vi.fn(async () => true),
    marked: () => marked,
    mark: vi.fn(() => {
      marked = true;
    }),
    login: vi.fn(
      async (
        _id: string,
        _input: Input,
        _signal: AbortSignal,
      ): Promise<LoginResult> => ({ kind: "disconnected" }),
    ),
  };
  const login = new Login(dependencies);
  return {
    login,
    dependencies,
    records,
    advance: (ms: number) => {
      now += ms;
    },
    select: (value: BrowserSession | null) => {
      selected = value;
    },
    selected: () => selected,
    newEntry: () => {
      marked = false;
    },
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

describe("original login recovery", () => {
  it("opens a saved new code after retiring a damaged receipt without automatic login", async () => {
    const f = fixture();
    f.records.set(receiptKey(second), "truncated");
    const code = f.records.get(MESSAGE_KEY);
    await f.login.initialize();
    expect(f.login.snapshot().kind).toBe("editing");
    expect(f.records.has(receiptKey(second))).toBe(false);
    expect(f.records.get(MESSAGE_KEY)).toBe(code);
    expect(f.dependencies.sessions.begin).not.toHaveBeenCalled();
    expect(f.dependencies.sessions.accept).not.toHaveBeenCalled();
    expect(f.dependencies.login).not.toHaveBeenCalled();
  });

  it("recovers a valid original success beside a damaged receipt without repeating R1", async () => {
    const f = fixture();
    f.records.set(receiptKey(second), "truncated");
    const completed = {
      version: 1,
      kind: "session",
      attempt: { operation, revision },
      session,
    };
    f.records.set(receiptKey(operation), JSON.stringify(completed));
    f.select({ version: 1, revision, attempt: operation, session: null });
    await f.login.initialize();
    expect(f.login.snapshot().kind).toBe("session");
    expect(f.selected()?.session).toEqual(session);
    expect(f.records.has(receiptKey(second))).toBe(false);
    expect(f.records.get(receiptKey(operation))).toBe(
      JSON.stringify(completed),
    );
    expect(f.dependencies.sessions.begin).not.toHaveBeenCalled();
    expect(f.dependencies.login).not.toHaveBeenCalled();
  });

  it("keeps an unresolved known Session reachable when another C1 hands off a code", async () => {
    const f = fixture();
    f.dependencies.login.mockResolvedValue({ kind: "session", session });
    f.dependencies.sessions.accept.mockRejectedValueOnce(
      new SessionStorageError(),
    );
    await f.login.initialize();
    await f.login.submit("000001");
    expect(f.login.snapshot().kind).toBe("session-storage");
    const completed = f.records.get(LOGIN_KEY);
    expect(blocksSend(f.dependencies.storage)).toBe(true);
    receiveCode(
      f.dependencies.storage,
      second,
      "13900000001",
      {
        id: "019354fd-933c-7e6a-8912-345678901235",
        delivery: "sent",
        expiresAt: "2035-01-01T00:05:00Z",
      },
      f.dependencies.now(),
    );
    expect(f.records.get(LOGIN_KEY)).toBe(completed);
    expect(f.records.has(receiptKey(operation))).toBe(true);
    const restored = new Login(f.dependencies);
    await restored.initialize();
    expect(restored.snapshot().kind).toBe("session");
    expect(f.selected()?.session).toEqual(session);
    expect(f.dependencies.login).toHaveBeenCalledOnce();
  });

  it("discovers a receipt after the fixed record disappears without repeating login", async () => {
    const f = fixture();
    f.dependencies.login.mockResolvedValue({ kind: "session", session });
    f.dependencies.sessions.accept.mockRejectedValueOnce(
      new SessionStorageError(),
    );
    await f.login.initialize();
    await f.login.submit("000001");
    f.records.delete(LOGIN_KEY);
    const restored = new Login(f.dependencies);
    await restored.initialize();
    expect(restored.snapshot().kind).toBe("session");
    expect(f.selected()?.session).toEqual(session);
    expect(f.dependencies.login).toHaveBeenCalledOnce();
    expect(blocksSend(f.dependencies.storage)).toBe(false);
  });

  it("does not choose a mismatched receipt just because it is enumerated first", async () => {
    const f = fixture();
    const stale = {
      version: 1,
      kind: "session",
      attempt: { operation: second, revision: second },
      session: { ...session, id: "019354fd-933c-7e6a-8912-345678901235" },
    };
    f.records.set(receiptKey(second), JSON.stringify(stale));
    const matching = {
      version: 1,
      kind: "session",
      attempt: { operation, revision },
      session,
    };
    f.records.set(receiptKey(operation), JSON.stringify(matching));
    f.records.delete(MESSAGE_KEY);
    f.select({ version: 1, revision, attempt: operation, session: null });
    await f.login.initialize();
    expect(f.selected()?.session).toEqual(session);
    expect(f.records.get(receiptKey(second))).toBe(JSON.stringify(stale));
    expect(f.dependencies.sessions.accept).toHaveBeenCalledWith(
      { operation, revision },
      session,
    );
    expect(f.dependencies.login).not.toHaveBeenCalled();
  });

  it("does not mistake an uncleared old handoff for a new C1 after the fixed result disappears", async () => {
    const f = fixture();
    f.records.set(
      receiptKey(operation),
      JSON.stringify({
        version: 1,
        kind: "session",
        attempt: { operation, revision },
        session,
        cleanup: { message: session.id, send: operation },
      }),
    );
    f.select({ version: 1, revision: second, attempt: null, session });
    await f.login.initialize();
    expect(f.login.snapshot().kind).toBe("session");
    expect(f.records.has(MESSAGE_KEY)).toBe(false);
    expect(f.dependencies.sessions.begin).not.toHaveBeenCalled();
    expect(f.dependencies.login).not.toHaveBeenCalled();
  });

  it.each([null, second])(
    "offers an explicit new handoff with another active attempt %s",
    async (attempt) => {
      const f = fixture();
      f.dependencies.login.mockResolvedValue({ kind: "session", session });
      await f.login.initialize();
      await f.login.submit("000001");
      receiveCode(
        f.dependencies.storage,
        second,
        "13900000001",
        {
          id: "019354fd-933c-7e6a-8912-345678901235",
          delivery: "sent",
          expiresAt: "2035-01-01T00:05:00Z",
        },
        f.dependencies.now(),
      );
      f.select({ version: 1, revision: second, attempt, session });
      f.newEntry();
      const begins = f.dependencies.sessions.begin.mock.calls.length;
      const restored = new Login(f.dependencies);
      await restored.initialize();
      expect(restored.snapshot().kind).toBe("editing");
      expect(f.records.has(receiptKey(operation))).toBe(true);
      expect(f.dependencies.sessions.begin).toHaveBeenCalledTimes(begins);
      expect(f.dependencies.login).toHaveBeenCalledOnce();
    },
  );

  it("retries registration handoff cleanup after refresh before opening the next stage", async () => {
    const f = fixture();
    const remove = f.dependencies.storage.removeItem.getMockImplementation()!;
    f.dependencies.login.mockImplementation(async () => {
      f.dependencies.storage.removeItem.mockImplementation((key) => {
        if (key === MESSAGE_KEY) throw new Error("temporarily denied");
        remove(key);
      });
      return { kind: "registration", registration };
    });
    await f.login.initialize();
    await f.login.submit("000001");
    expect(JSON.parse(f.records.get(LOGIN_KEY)!)).toMatchObject({
      kind: "registration",
      cleanup: { message: session.id, send: operation },
    });
    expect(f.records.has(MESSAGE_KEY)).toBe(true);
    f.dependencies.storage.removeItem.mockImplementation(remove);
    const restored = new Login(f.dependencies);
    await restored.initialize();
    expect(restored.snapshot().kind).toBe("registration");
    expect(f.records.has(MESSAGE_KEY)).toBe(false);
    expect(JSON.parse(f.records.get(LOGIN_KEY)!)).not.toHaveProperty("cleanup");
    expect(f.dependencies.login).toHaveBeenCalledOnce();
  });

  it("retains a registration cleanup locator after expiry until removal is confirmed", async () => {
    const f = fixture();
    const remove = f.dependencies.storage.removeItem.getMockImplementation()!;
    f.dependencies.login.mockImplementation(async () => {
      f.dependencies.storage.removeItem.mockImplementation((key) => {
        if (key === MESSAGE_KEY) return;
        remove(key);
      });
      return { kind: "registration", registration };
    });
    await f.login.initialize();
    await f.login.submit("000001");
    f.advance(30 * 60_000);
    expect(maintainMaterial(f.dependencies.storage, f.dependencies.now())).toBe(
      false,
    );
    expect(JSON.parse(f.records.get(LOGIN_KEY)!)).toEqual({
      version: 1,
      kind: "closed",
      reason: "expired",
      cleanup: { message: session.id, send: operation },
    });
    expect(f.records.get(LOGIN_KEY)).not.toContain(registration.token);
    expect(f.records.has(MESSAGE_KEY)).toBe(true);
    f.dependencies.storage.removeItem.mockImplementation(remove);
    expect(maintainMaterial(f.dependencies.storage, f.dependencies.now())).toBe(
      true,
    );
    expect(f.records.has(MESSAGE_KEY)).toBe(false);
    expect(JSON.parse(f.records.get(LOGIN_KEY)!)).not.toHaveProperty("cleanup");
    expect(f.dependencies.login).toHaveBeenCalledOnce();
  });

  it("restores the exact receipt after a transient fixed-record replacement failure", async () => {
    const f = fixture();
    const write = f.dependencies.storage.setItem.getMockImplementation()!;
    const remove = f.dependencies.storage.removeItem.getMockImplementation()!;
    let replacements = 0;
    f.dependencies.login.mockImplementation(async () => {
      f.dependencies.storage.setItem.mockImplementation((key, bytes) => {
        if (key === LOGIN_KEY && ++replacements === 1)
          throw new Error("fixture transient replacement refusal");
        write(key, bytes);
      });
      f.dependencies.storage.removeItem.mockImplementation((key) => {
        if (key === MESSAGE_KEY) throw new Error("fixture cleanup refusal");
        remove(key);
      });
      return { kind: "session", session };
    });
    await f.login.initialize();
    await f.login.submit("000001");
    expect(f.login.snapshot().kind).toBe("session-storage");
    const receipt = {
      version: 1,
      kind: "session",
      attempt: { operation, revision },
      session,
      cleanup: { message: session.id, send: operation },
    };
    expect(JSON.parse(f.records.get(LOGIN_KEY)!)).toEqual(receipt);
    expect(JSON.parse(f.records.get(`halo.r1.session.${operation}`)!)).toEqual(
      receipt,
    );
    expect(replacements).toBe(2);
    f.dependencies.storage.removeItem.mockImplementation(remove);
    const restored = new Login(f.dependencies);
    await restored.initialize();
    expect(restored.snapshot().kind).toBe("session");
    expect(f.selected()?.session).toEqual(session);
    expect(f.records.has(MESSAGE_KEY)).toBe(false);
    await restored.confirm();
    expect(f.dependencies.login).toHaveBeenCalledOnce();
  });

  it("preserves a newer flow installed during a failed success replacement", async () => {
    const f = fixture();
    const write = f.dependencies.storage.setItem.getMockImplementation()!;
    let newerProof = "";
    let newerCode = "";
    f.dependencies.login.mockImplementation(async () => {
      const proof = JSON.parse(f.records.get(LOGIN_KEY)!);
      proof.attempt = { operation: second, revision: second };
      newerProof = JSON.stringify(proof);
      const code = JSON.parse(f.records.get(MESSAGE_KEY)!);
      code.message.id = "019354fd-933c-7e6a-8912-345678901235";
      newerCode = JSON.stringify(code);
      f.dependencies.storage.setItem.mockImplementation((key, bytes) => {
        if (key === LOGIN_KEY) {
          f.records.set(LOGIN_KEY, newerProof);
          f.records.set(MESSAGE_KEY, newerCode);
          f.select({
            version: 1,
            attempt: second,
            revision: second,
            session: null,
          });
          throw new Error("fixture replaced by newer flow");
        }
        write(key, bytes);
      });
      return { kind: "session", session };
    });
    await f.login.initialize();
    await f.login.submit("000001");
    expect(f.login.snapshot().kind).toBe("session-storage");
    expect(f.records.get(LOGIN_KEY)).toBe(newerProof);
    expect(f.records.get(MESSAGE_KEY)).toBe(newerCode);
    expect(f.selected()?.attempt).toBe(second);
    expect(f.dependencies.sessions.accept).not.toHaveBeenCalled();
    expect(f.dependencies.login).toHaveBeenCalledOnce();
  });

  it("keeps a known receipt in memory while the fixed record remains unwritable", async () => {
    const f = fixture();
    const write = f.dependencies.storage.setItem.getMockImplementation()!;
    f.dependencies.login.mockImplementation(async () => {
      f.dependencies.storage.setItem.mockImplementation((key, bytes) => {
        if (key === LOGIN_KEY) throw new Error("fixture fixed-key refusal");
        write(key, bytes);
      });
      return { kind: "session", session };
    });
    await f.login.initialize();
    await f.login.submit("000001");
    expect(f.records.has(LOGIN_KEY)).toBe(false);
    expect(
      JSON.parse(f.records.get(`halo.r1.session.${operation}`)!),
    ).toMatchObject({
      attempt: { operation, revision },
      session,
    });
    await f.login.initialize();
    expect(f.login.snapshot().kind).toBe("session-storage");
    expect(f.dependencies.sessions.accept).not.toHaveBeenCalled();
    f.dependencies.storage.setItem.mockImplementation(write);
    await f.login.initialize();
    expect(f.login.snapshot().kind).toBe("session");
    expect(f.selected()?.session).toEqual(session);
    expect(f.dependencies.login).toHaveBeenCalledOnce();
  });

  it("does not replace a proof when removal did not actually remove it", async () => {
    const f = fixture();
    const write = f.dependencies.storage.setItem.getMockImplementation()!;
    const remove = f.dependencies.storage.removeItem.getMockImplementation()!;
    let replacements = 0;
    f.dependencies.login.mockImplementation(async () => {
      f.dependencies.storage.setItem.mockImplementation((key, bytes) => {
        if (key === LOGIN_KEY) {
          replacements++;
          throw new Error("fixture fixed-key refusal");
        }
        write(key, bytes);
      });
      f.dependencies.storage.removeItem.mockImplementation((key) => {
        if (key === MESSAGE_KEY) throw new Error("fixture cleanup refusal");
        if (key !== LOGIN_KEY) remove(key);
      });
      return { kind: "session", session };
    });
    await f.login.initialize();
    await f.login.submit("000001");
    expect(f.login.snapshot().kind).toBe("session-storage");
    expect(JSON.parse(f.records.get(LOGIN_KEY)!).kind).toBe("login");
    expect(replacements).toBe(1);
    expect(f.dependencies.login).toHaveBeenCalledOnce();
  });

  it.each([
    "json",
    "empty",
    "null",
    "array",
    "primitive",
    "missing",
    "version",
    "mobile",
    "operation",
    "deadline",
    "message",
  ])(
    "retries deleting an unidentifiable %s handoff after refresh",
    async (shape) => {
      const f = fixture();
      const remove = f.dependencies.storage.removeItem.getMockImplementation()!;
      const code = JSON.parse(f.records.get(MESSAGE_KEY)!);
      code.message.id = "019354fd-933c-7e6a-8912-345678901235";
      let bytes = "{";
      if (shape === "empty") bytes = "";
      else if (shape === "null") bytes = "null";
      else if (shape === "array") bytes = "[]";
      else if (shape === "primitive") bytes = "true";
      else if (shape === "missing")
        bytes = JSON.stringify({ mobile: code.mobile });
      else if (shape !== "json") {
        if (shape === "version") code.version = 99;
        if (shape === "mobile") code.mobile = "invalid";
        if (shape === "operation") code.operation = "invalid";
        if (shape === "deadline") code.expiresAt = code.createdAt;
        if (shape === "message") code.message.delivery = "invalid";
        bytes = JSON.stringify(code);
      }
      f.dependencies.login.mockImplementation(async () => {
        f.records.set(MESSAGE_KEY, bytes);
        return { kind: "session", session };
      });
      await f.login.initialize();
      f.dependencies.storage.removeItem.mockImplementation((key) => {
        if (key === MESSAGE_KEY) throw new Error("fixture removal refusal");
        remove(key);
      });
      await f.login.submit("000001");
      expect(f.login.snapshot().kind).toBe("session-storage");
      expect(f.records.get(MESSAGE_KEY)).toBe(bytes);
      expect(JSON.parse(f.records.get(LOGIN_KEY)!)).toHaveProperty("cleanup", {
        message: session.id,
        send: operation,
      });
      const restored = new Login(f.dependencies);
      await restored.initialize();
      expect(restored.snapshot().kind).toBe("session-storage");
      f.dependencies.storage.removeItem.mockImplementation(remove);
      await restored.initialize();
      expect(restored.snapshot().kind).toBe("session");
      expect(f.records.has(MESSAGE_KEY)).toBe(false);
      expect(JSON.parse(f.records.get(LOGIN_KEY)!)).not.toHaveProperty(
        "cleanup",
      );
      expect(f.dependencies.login).toHaveBeenCalledOnce();
    },
  );

  it.each(["same-instance", "refresh", "expiry"] as const)(
    "retries saved success handoff cleanup after %s without another POST",
    async (boundary) => {
      const f = fixture();
      const remove = f.dependencies.storage.removeItem.getMockImplementation()!;
      f.dependencies.login.mockResolvedValue({ kind: "session", session });
      await f.login.initialize();
      f.dependencies.storage.removeItem.mockImplementation((key) => {
        if (key === MESSAGE_KEY)
          throw new Error("fixture transient removal refusal");
        remove(key);
      });
      await f.login.submit("000001");
      expect(f.login.snapshot().kind).toBe("session-storage");
      const saved = JSON.parse(f.records.get(LOGIN_KEY)!);
      expect(saved).toMatchObject({
        version: 1,
        kind: "session",
        attempt: { operation, revision },
        session,
      });
      expect(JSON.stringify(saved)).not.toMatch(
        /secret|credential|mobile|token/,
      );
      const restored =
        boundary === "refresh" ? new Login(f.dependencies) : f.login;
      await restored.initialize();
      expect(restored.snapshot().kind).toBe("session-storage");
      expect(f.records.has(MESSAGE_KEY)).toBe(true);
      f.dependencies.storage.removeItem.mockImplementation(remove);
      if (boundary === "expiry") {
        f.advance(24 * 60 * 60_000);
        restored.expire();
      } else await restored.initialize();
      expect(restored.snapshot().kind).toBe(
        boundary === "expiry" ? "expired" : "session",
      );
      expect(f.records.has(MESSAGE_KEY)).toBe(false);
      expect(JSON.parse(f.records.get(LOGIN_KEY)!)).not.toHaveProperty(
        "cleanup",
      );
      expect(
        JSON.parse(f.records.get(`halo.r1.session.${operation}`)!),
      ).not.toHaveProperty("cleanup");
      await restored.confirm();
      expect(f.dependencies.login).toHaveBeenCalledOnce();
    },
  );

  it.each(["active", "expired"] as const)(
    "does not clean a later %s handoff or select an old result while retrying cleanup",
    async (deadline) => {
      const f = fixture();
      const remove = f.dependencies.storage.removeItem.getMockImplementation()!;
      f.dependencies.login.mockResolvedValue({ kind: "session", session });
      await f.login.initialize();
      f.dependencies.storage.removeItem.mockImplementation(() => {
        throw new Error("fixture removal refusal");
      });
      await f.login.submit("000001");
      const newer = JSON.parse(f.records.get(MESSAGE_KEY)!);
      newer.operation = second;
      newer.message.id = "019354fd-933c-7e6a-8912-345678901235";
      const bytes = JSON.stringify(newer);
      f.records.set(MESSAGE_KEY, bytes);
      f.records.set("halo.c1", JSON.stringify({ operation: second }));
      f.select({
        version: 1,
        revision: second,
        attempt: second,
        session: null,
      });
      if (deadline === "expired") f.advance(5 * 60_000);
      f.dependencies.storage.removeItem.mockImplementation(remove);
      const restored = new Login(f.dependencies);
      await restored.initialize();
      expect(restored.snapshot().kind).toBe("changed");
      expect(f.records.get(MESSAGE_KEY)).toBe(bytes);
      expect(f.records.get("halo.c1")).toBe(
        JSON.stringify({ operation: second }),
      );
      expect(f.dependencies.sessions.accept).not.toHaveBeenCalled();
      expect(f.dependencies.login).toHaveBeenCalledOnce();
    },
  );

  it.each([null, 123, "not-an-id", {}, session.id + "?secret=bad"])(
    "rejects an invalid saved cleanup locator: %s",
    async (message) => {
      const f = fixture();
      f.records.set(
        LOGIN_KEY,
        JSON.stringify({
          version: 1,
          kind: "session",
          attempt: { operation, revision },
          session,
          message,
        }),
      );
      await f.login.initialize();
      expect(f.login.snapshot().kind).toBe("missing");
      expect(f.dependencies.sessions.accept).not.toHaveBeenCalled();
      expect(f.dependencies.login).not.toHaveBeenCalled();
    },
  );

  it.each(["editing", "login", "registration"] as const)(
    "preserves a newer C1 operation while restoring %s",
    async (stage) => {
      const f = fixture();
      if (stage !== "editing") {
        if (stage === "registration")
          f.dependencies.login.mockResolvedValue({ kind: stage, registration });
        await f.login.initialize();
        await f.login.submit("000001");
      }
      const newer = JSON.stringify({
        version: 1,
        operation: second,
        input: { mobile: "13900000001", token: "new-turnstile-proof" },
        createdAt: f.dependencies.now(),
        expiresAt: f.dependencies.now() + 30 * 60_000,
        retryAt: 0,
        attempted: true,
      });
      f.records.set("halo.c1", newer);
      const restored = new Login(f.dependencies);
      await restored.initialize();
      expect(f.records.get("halo.c1")).toBe(newer);
    },
  );

  it.each(["login", "registration"] as const)(
    "stops using expired %s secrets and retries a refused closed-record write",
    async (kind) => {
      const f = fixture();
      if (kind === "registration")
        f.dependencies.login.mockResolvedValue({ kind, registration });
      await f.login.initialize();
      await f.login.submit("000001");
      const write = f.dependencies.storage.setItem.getMockImplementation()!;
      f.dependencies.storage.setItem.mockImplementation(() => {
        throw new Error("fixture quota refusal");
      });
      f.advance(30 * 60_000);
      f.login.expire();
      expect(f.records.has(LOGIN_KEY)).toBe(true);
      expect(f.records.has(MESSAGE_KEY)).toBe(false);
      await f.login.confirm();
      expect(f.dependencies.login).toHaveBeenCalledOnce();
      f.dependencies.storage.setItem.mockImplementation(write);
      expect(
        maintainMaterial(f.dependencies.storage, f.dependencies.now()),
      ).toBe(true);
      expect(JSON.parse(f.records.get(LOGIN_KEY)!)).toEqual({
        version: 1,
        kind: "closed",
        reason: "expired",
      });
    },
  );

  it("rejects a confirmed failed SMS when login is opened directly", async () => {
    const f = fixture();
    const code = JSON.parse(f.records.get(MESSAGE_KEY)!);
    code.message.delivery = "failed";
    f.records.set(MESSAGE_KEY, JSON.stringify(code));
    await f.login.initialize();
    await f.login.submit("000001");
    expect(f.login.snapshot().kind).toBe("delivery-failed");
    expect(f.dependencies.sessions.begin).not.toHaveBeenCalled();
    expect(f.dependencies.login).not.toHaveBeenCalled();
  });

  it.each(["changed", "suspended", "accept-race", "accept-error"] as const)(
    "retains a nonsecret successful session after %s",
    async (boundary) => {
      const f = fixture();
      const response = deferred<LoginResult>();
      f.dependencies.login.mockReturnValue(response.promise);
      await f.login.initialize();
      const work = f.login.submit("000001");
      await vi.waitFor(() =>
        expect(f.dependencies.login).toHaveBeenCalledOnce(),
      );
      if (boundary === "changed")
        f.select({
          version: 1,
          revision: second,
          attempt: second,
          session: null,
        });
      if (boundary === "suspended") f.login.suspend();
      if (boundary === "accept-race")
        f.dependencies.sessions.accept.mockResolvedValue(null);
      if (boundary === "accept-error")
        f.dependencies.sessions.accept.mockRejectedValue(
          new SessionStorageError("unavailable"),
        );
      response.resolve({ kind: "session", session });
      await work;
      expect(
        JSON.parse(f.records.get(`halo.r1.session.${operation}`)!),
      ).toEqual({
        version: 1,
        kind: "session",
        attempt: { operation, revision },
        session,
      });
      expect(f.records.get(LOGIN_KEY)).not.toContain('"secret"');
      expect(f.records.has(MESSAGE_KEY)).toBe(false);
      f.advance(6000);
      await f.login.confirm();
      expect(f.dependencies.login).toHaveBeenCalledOnce();
      if (boundary === "changed" || boundary === "suspended")
        expect(f.dependencies.sessions.accept).not.toHaveBeenCalled();
      if (boundary === "changed")
        expect(f.selected()).toEqual({
          version: 1,
          revision: second,
          attempt: second,
          session: null,
        });
    },
  );

  it("retains late success without replacing newer tab material", async () => {
    const f = fixture();
    const response = deferred<LoginResult>();
    f.dependencies.login.mockReturnValue(response.promise);
    await f.login.initialize();
    const work = f.login.submit("000001");
    await vi.waitFor(() => expect(f.dependencies.login).toHaveBeenCalledOnce());
    const newer = JSON.parse(f.records.get(LOGIN_KEY)!);
    newer.attempt.operation = second;
    newer.input.credential.id = "019354fd-933c-7e6a-8912-345678901235";
    const code = JSON.parse(f.records.get(MESSAGE_KEY)!);
    code.operation = second;
    code.message.id = newer.input.credential.id;
    const nextCode = JSON.stringify(code);
    const nextLogin = JSON.stringify(newer);
    // This pending send is newer than both available message handoffs.
    const nextSend = JSON.stringify({
      operation: revision,
      input: { token: "new" },
    });
    f.records.set(LOGIN_KEY, nextLogin);
    f.records.set(MESSAGE_KEY, nextCode);
    f.records.set("halo.c1", nextSend);
    f.select({ version: 1, revision: second, attempt: second, session: null });
    await f.login.selectionChanged();
    response.resolve({ kind: "session", session });
    await work;
    expect(
      JSON.parse(f.records.get(`halo.r1.session.${operation}`)!),
    ).toMatchObject({
      attempt: { operation, revision },
      session,
    });
    expect(f.records.get(LOGIN_KEY)).toBe(nextLogin);
    expect(f.records.get(MESSAGE_KEY)).toBe(nextCode);
    expect(f.records.get("halo.c1")).toBe(nextSend);
    expect(f.dependencies.sessions.accept).not.toHaveBeenCalled();
  });

  it("remembers known success in memory while its storage is unavailable", async () => {
    const f = fixture();
    const write = f.dependencies.storage.setItem.getMockImplementation()!;
    f.dependencies.login.mockImplementation(async () => {
      f.dependencies.storage.setItem.mockImplementation(() => {
        throw new Error("fixture quota refusal");
      });
      return { kind: "session", session };
    });
    await f.login.initialize();
    await f.login.submit("000001");
    expect(f.login.snapshot().kind).toBe("session-storage");
    f.advance(6000);
    await f.login.confirm();
    await f.login.initialize();
    expect(f.login.snapshot().kind).toBe("session-storage");
    expect(f.dependencies.login).toHaveBeenCalledOnce();
    f.dependencies.storage.setItem.mockImplementation(write);
    await f.login.initialize();
    expect(f.dependencies.login).toHaveBeenCalledOnce();
    expect(
      JSON.parse(f.records.get(`halo.r1.session.${operation}`)!),
    ).toMatchObject({
      session,
    });
    expect(f.records.get(LOGIN_KEY)).not.toContain('"secret"');
  });

  it("restores a saved session without reposting a leftover login proof", async () => {
    const f = fixture();
    const write = f.dependencies.storage.setItem.getMockImplementation()!;
    const remove = f.dependencies.storage.removeItem.getMockImplementation()!;
    f.dependencies.login.mockImplementation(async () => {
      f.dependencies.storage.setItem.mockImplementation((key, value) => {
        if (key === LOGIN_KEY) throw new Error("fixture replacement refusal");
        write(key, value);
      });
      f.dependencies.storage.removeItem.mockImplementation((key) => {
        if (key === LOGIN_KEY) throw new Error("fixture removal refusal");
        remove(key);
      });
      return { kind: "session", session };
    });
    await f.login.initialize();
    await f.login.submit("000001");
    expect(f.login.snapshot().kind).toBe("session-storage");
    expect(JSON.parse(f.records.get(LOGIN_KEY)!).kind).toBe("login");
    f.dependencies.storage.setItem.mockImplementation(write);
    f.dependencies.storage.removeItem.mockImplementation(remove);
    const restored = new Login(f.dependencies);
    await restored.initialize();
    expect(restored.snapshot().kind).toBe("session");
    expect(f.selected()?.session).toEqual(session);
    expect(f.records.get(LOGIN_KEY)).not.toContain('"secret"');
    await restored.confirm();
    expect(f.dependencies.login).toHaveBeenCalledOnce();
  });

  it("keeps the locator of a success received after secret expiry", async () => {
    const f = fixture();
    const response = deferred<LoginResult>();
    f.dependencies.login.mockReturnValue(response.promise);
    await f.login.initialize();
    const work = f.login.submit("000001");
    await vi.waitFor(() => expect(f.dependencies.login).toHaveBeenCalledOnce());
    f.advance(30 * 60_000);
    f.login.expire();
    response.resolve({ kind: "session", session });
    await work;
    expect(f.login.snapshot().kind).toBe("expired");
    expect(
      JSON.parse(f.records.get(`halo.r1.session.${operation}`)!),
    ).toMatchObject({
      session,
    });
    expect(f.records.get(LOGIN_KEY)).not.toContain('"secret"');
    expect(f.dependencies.sessions.accept).not.toHaveBeenCalled();
  });

  it.each(["null", "[]", "true", "3", "{"])(
    "removes malformed message %s and permits a later real C1 handoff",
    async (bytes) => {
      const f = fixture();
      const original = JSON.parse(f.records.get(MESSAGE_KEY)!);
      f.records.set(MESSAGE_KEY, bytes);
      await f.login.initialize();
      expect(f.login.snapshot().kind).toBe("missing");
      expect(f.records.has(MESSAGE_KEY)).toBe(false);
      receiveCode(
        f.dependencies.storage,
        original.operation,
        original.mobile,
        original.message,
        f.dependencies.now(),
      );
      const fresh = new Login(f.dependencies);
      await fresh.initialize();
      expect(fresh.snapshot().kind).toBe("editing");
      expect(f.dependencies.login).not.toHaveBeenCalled();
    },
  );

  it("discards the previous C1 proof only after a valid message is available", async () => {
    const f = fixture();
    const proof = JSON.stringify({
      operation,
      input: { token: "old-turnstile-proof" },
    });
    f.records.set("halo.c1", proof);
    const message = f.records.get(MESSAGE_KEY)!;
    f.records.delete(MESSAGE_KEY);
    await f.login.initialize();
    expect(f.records.get("halo.c1")).toBe(proof);
    f.records.set(MESSAGE_KEY, message);
    await f.login.initialize();
    expect(f.login.snapshot().kind).toBe("editing");
    expect(f.records.has("halo.c1")).toBe(false);
    expect(f.records.get(MESSAGE_KEY)).toBe(message);
    expect(f.dependencies.login).not.toHaveBeenCalled();
  });

  it("stops login if the previous-stage proof cannot be removed", async () => {
    const f = fixture();
    const proof = JSON.stringify({
      operation,
      input: { token: "old-turnstile-proof" },
    });
    f.records.set("halo.c1", proof);
    f.dependencies.storage.removeItem.mockImplementation((key) => {
      if (key === "halo.c1") throw new Error("fixture cleanup refusal");
      f.records.delete(key);
    });
    await f.login.initialize();
    await f.login.submit("000001");
    expect(f.login.snapshot().kind).toBe("storage");
    expect(f.records.get("halo.c1")).toBe(proof);
    expect(f.records.has(MESSAGE_KEY)).toBe(true);
    expect(f.dependencies.login).not.toHaveBeenCalled();
  });

  it.each(["login", "registration"] as const)(
    "marks restored %s material before a later loss can permit editing",
    async (kind) => {
      const { login, dependencies, records, newEntry } = fixture();
      const message = records.get(MESSAGE_KEY)!;
      if (kind === "registration")
        dependencies.login.mockResolvedValue({ kind, registration });
      await login.initialize();
      await login.submit("000001");
      const original = records.get(LOGIN_KEY);
      records.set(MESSAGE_KEY, message);
      newEntry();
      const restored = new Login(dependencies);
      await restored.initialize();
      expect(dependencies.marked()).toBe(true);
      expect(records.get(LOGIN_KEY)).toBe(original);
      expect(restored.snapshot().kind).toBe(
        kind === "login" ? "pending" : "registration",
      );
      records.delete(LOGIN_KEY);
      const missing = new Login(dependencies);
      await missing.initialize();
      await missing.submit("000002");
      expect(missing.snapshot().kind).toBe("missing");
      expect(dependencies.sessions.begin).toHaveBeenCalledTimes(1);
      expect(dependencies.login).toHaveBeenCalledTimes(1);
    },
  );

  it.each([
    "{",
    "null",
    '{"version":1,"kind":"login"}',
    '{"version":1,"kind":"closed"}',
    '{"version":1,"kind":"closed","reason":["session"]}',
    JSON.stringify({
      version: 1,
      kind: "closed",
      reason: "session",
      input: { secret: "stale-secret", credential: { code: "000001" } },
      registration: { token: "stale-token" },
    }),
  ])(
    "closes damaged material %s and marks its history entry",
    async (bytes) => {
      const { login, dependencies, records, newEntry } = fixture();
      await login.initialize();
      await login.submit("000001");
      newEntry();
      records.set(LOGIN_KEY, bytes);
      const damaged = new Login(dependencies);
      await damaged.initialize();
      expect(damaged.snapshot().kind).toBe("missing");
      expect(dependencies.marked()).toBe(true);
      expect(JSON.parse(records.get(LOGIN_KEY)!)).toEqual({
        version: 1,
        kind: "closed",
        reason: "missing",
      });
      newEntry();
      const closed = new Login(dependencies);
      await closed.initialize();
      expect(dependencies.marked()).toBe(true);
      records.delete(LOGIN_KEY);
      const missing = new Login(dependencies);
      await missing.initialize();
      await missing.submit("000002");
      expect(missing.snapshot().kind).toBe("missing");
      expect(dependencies.sessions.begin).toHaveBeenCalledTimes(1);
      expect(dependencies.login).toHaveBeenCalledTimes(1);
    },
  );

  it.each(["login", "registration"] as const)(
    "retains restored %s until expiry when its history marker fails",
    async (kind) => {
      const { login, dependencies, records, advance, newEntry } = fixture();
      if (kind === "registration")
        dependencies.login.mockResolvedValue({ kind, registration });
      await login.initialize();
      await login.submit("000001");
      const original = records.get(LOGIN_KEY);
      newEntry();
      dependencies.mark.mockImplementation(() => {
        throw new Error("history unavailable");
      });
      const restored = new Login(dependencies);
      await restored.initialize();
      expect(restored.snapshot().kind).toBe("storage");
      expect(records.get(LOGIN_KEY)).toBe(original);
      advance(5000);
      await restored.confirm();
      expect(dependencies.login).toHaveBeenCalledTimes(1);
      advance(30 * 60_000);
      restored.expire();
      expect(restored.snapshot().kind).toBe("expired");
      expect(JSON.parse(records.get(LOGIN_KEY)!)).toEqual({
        version: 1,
        kind: "closed",
        reason: "expired",
      });
    },
  );

  it("keeps an unsent proof until expiry when the first history marker fails", async () => {
    const { login, dependencies, records, advance } = fixture();
    await login.initialize();
    dependencies.mark.mockImplementation(() => {
      throw new Error("history unavailable");
    });
    await login.submit("000001");
    expect(login.snapshot().kind).toBe("storage");
    expect(JSON.parse(records.get(LOGIN_KEY)!)).toMatchObject({
      kind: "login",
      sent: false,
    });
    await login.confirm();
    expect(dependencies.login).not.toHaveBeenCalled();
    advance(30 * 60_000);
    login.expire();
    expect(login.snapshot().kind).toBe("expired");
    expect(records.get(LOGIN_KEY)).not.toContain("secret");
  });

  it("keeps the marker when persisting a damaged-material closure fails", async () => {
    const { login, dependencies, records, newEntry } = fixture();
    await login.initialize();
    await login.submit("000001");
    newEntry();
    records.set(LOGIN_KEY, '{"version":1,"kind":"login"}');
    dependencies.storage.setItem.mockImplementationOnce(() => {
      throw new Error("full");
    });
    const restored = new Login(dependencies);
    await restored.initialize();
    expect(restored.snapshot().kind).toBe("storage");
    expect(dependencies.marked()).toBe(true);
    records.delete(LOGIN_KEY);
    const missing = new Login(dependencies);
    await missing.initialize();
    expect(missing.snapshot().kind).toBe("missing");
    expect(dependencies.login).toHaveBeenCalledTimes(1);
  });

  it.each([
    { version: 99, input: { secret: "damaged-secret" } },
    {
      version: 1,
      kind: "closed",
      reason: "session",
      input: { secret: "stale-secret" },
    },
  ])(
    "closes damaged secret material when its history marker fails: %j",
    async (value) => {
      const { login, dependencies, records } = fixture();
      records.set(LOGIN_KEY, JSON.stringify(value));
      dependencies.mark.mockImplementation(() => {
        throw new Error("history unavailable");
      });
      await login.initialize();
      expect(login.snapshot().kind).toBe("storage");
      expect(JSON.parse(records.get(LOGIN_KEY)!)).toEqual({
        version: 1,
        kind: "closed",
        reason: "missing",
      });
      await login.confirm();
      expect(dependencies.login).not.toHaveBeenCalled();
    },
  );

  it("expires an unsent login at the SMS deadline after a failed preflight", async () => {
    const { login, dependencies, records, advance } = fixture();
    await login.initialize();
    advance(299_000);
    dependencies.current.mockRejectedValueOnce(new Error("offline"));
    await login.submit("000001");
    expect(login.snapshot()).toMatchObject({ kind: "pending", sent: false });
    advance(5000);
    const reopened = new Login(dependencies);
    await reopened.initialize();
    expect(reopened.snapshot().kind).toBe("expired");
    await reopened.confirm();
    expect(dependencies.login).not.toHaveBeenCalled();
    expect(records.get(LOGIN_KEY)).not.toContain("secret");
    expect(records.has(MESSAGE_KEY)).toBe(false);
  });

  it("expires restored login secrets even when the browser selection cannot be read", async () => {
    const { login, dependencies, records, advance } = fixture();
    await login.initialize();
    await login.submit("000001");
    dependencies.sessions.read.mockRejectedValue(
      new SessionStorageError("unavailable"),
    );
    const restored = new Login(dependencies);
    await restored.initialize();
    expect(restored.snapshot().kind).toBe("storage");
    advance(30 * 60 * 1000);
    restored.expire();
    expect(JSON.parse(records.get(LOGIN_KEY)!)).toEqual({
      version: 1,
      kind: "closed",
      reason: "expired",
    });
    expect(records.has(MESSAGE_KEY)).toBe(false);
    await restored.confirm();
    expect(dependencies.login).toHaveBeenCalledTimes(1);
  });

  it("keeps a sent login recoverable after the SMS deadline", async () => {
    const { login, dependencies, records, advance } = fixture();
    await login.initialize();
    advance(299_000);
    await login.submit("000001");
    const original = dependencies.login.mock.calls[0].slice(0, 2);
    advance(5000);
    const reopened = new Login(dependencies);
    await reopened.initialize();
    expect(reopened.snapshot()).toMatchObject({ kind: "pending", sent: true });
    await reopened.confirm();
    expect(dependencies.login.mock.calls[1].slice(0, 2)).toEqual(original);
    advance(30 * 60_000);
    reopened.expire();
    expect(reopened.snapshot().kind).toBe("expired");
    expect(records.get(LOGIN_KEY)).not.toContain("secret");
  });

  it.each(["untrusted", "build"] as const)(
    "keeps the %s reload requirement across foreground checks",
    async (failure) => {
      const { login, dependencies, records, advance } = fixture();
      if (failure === "untrusted")
        dependencies.login.mockResolvedValueOnce({ kind: "untrusted" });
      else dependencies.current.mockResolvedValueOnce(false);
      await login.initialize();
      await login.submit("000001");
      const original = records.get(LOGIN_KEY);
      expect(login.snapshot().kind).toBe("reload");
      advance(6000);
      await login.selectionChanged();
      await login.confirm();
      expect(login.snapshot().kind).toBe("reload");
      expect(records.get(LOGIN_KEY)).toBe(original);
      expect(dependencies.login).toHaveBeenCalledTimes(
        failure === "untrusted" ? 1 : 0,
      );
      const reopened = new Login(dependencies);
      await reopened.initialize();
      expect(reopened.snapshot().kind).toBe("pending");
    },
  );

  it("allows an explicit corrected code after restoring a known wrong-code result", async () => {
    const { login, dependencies, newEntry } = fixture();
    dependencies.login.mockResolvedValue({
      kind: "failure",
      code: "verification_invalid",
      status: 403,
      retryAfter: 0,
    });
    await login.initialize();
    await login.submit("000001");
    newEntry();
    const restored = new Login(dependencies);
    await restored.initialize();
    expect(dependencies.marked()).toBe(true);
    expect(restored.snapshot()).toMatchObject({
      kind: "editing",
      invalid: true,
    });
    expect(dependencies.login).toHaveBeenCalledTimes(1);
    dependencies.uuid.mockReturnValue(second);
    await restored.submit("000002");
    expect(dependencies.login.mock.calls[1][0]).toBe(second);
    expect(dependencies.login.mock.calls[1][1].credential.code).toBe("000002");
  });

  it("keeps the reload barrier while retaining a later known session", async () => {
    const { login, dependencies, records, advance } = fixture();
    const result = deferred<LoginResult>();
    const started = deferred<void>();
    dependencies.login.mockImplementation(async () => {
      started.resolve();
      return result.promise;
    });
    await login.initialize();
    const work = login.submit("000001");
    await started.promise;
    const original = JSON.parse(records.get(LOGIN_KEY)!);
    login.reload();
    const saved = JSON.parse(records.get(LOGIN_KEY)!);
    expect(saved).toEqual({
      ...original,
      retryAt: dependencies.now() + 5000,
    });
    result.resolve({ kind: "session", session });
    await work;
    expect(dependencies.sessions.accept).not.toHaveBeenCalled();
    expect(login.snapshot().kind).toBe("reload");
    advance(6000);
    await login.selectionChanged();
    await login.confirm();
    expect(login.snapshot().kind).toBe("reload");
    expect(dependencies.login).toHaveBeenCalledOnce();
    expect(JSON.parse(records.get(LOGIN_KEY)!)).toEqual({
      version: 1,
      kind: "session",
      attempt: { operation, revision },
      session,
    });
    expect(records.has(MESSAGE_KEY)).toBe(false);
  });

  it("lets only fresh SMS material reach explicit authentication after a damaged selection", async () => {
    const { login, dependencies } = fixture();
    dependencies.sessions.read.mockRejectedValueOnce(
      new SessionStorageError("invalid"),
    );
    await login.initialize();
    expect(login.snapshot().kind).toBe("editing");
    expect(dependencies.sessions.begin).not.toHaveBeenCalled();
    expect(dependencies.login).not.toHaveBeenCalled();
    await login.submit("000001");
    expect(dependencies.sessions.begin).toHaveBeenCalledOnce();
    expect(dependencies.login).toHaveBeenCalledOnce();
  });

  it("does not replace an unknown original login when the selection is damaged", async () => {
    const { login, dependencies, records } = fixture();
    await login.initialize();
    await login.submit("000001");
    const proof = records.get(LOGIN_KEY);
    dependencies.sessions.read.mockRejectedValueOnce(
      new SessionStorageError("invalid"),
    );
    await login.initialize();
    expect(login.snapshot().kind).toBe("storage");
    expect(records.get(LOGIN_KEY)).toBe(proof);
    expect(dependencies.sessions.begin).toHaveBeenCalledTimes(1);
    expect(dependencies.login).toHaveBeenCalledTimes(1);
  });

  it("saves the exact credential, secret and browser attempt before the write", async () => {
    const { login, dependencies, records } = fixture();
    dependencies.login.mockImplementation(async (id, input) => {
      const proof = JSON.parse(records.get(LOGIN_KEY)!);
      expect(proof.attempt).toEqual({ operation: id, revision });
      expect(proof.input).toEqual(input);
      expect(input.credential.code).toBe("000001");
      expect(dependencies.marked()).toBe(true);
      expect(await dependencies.sessions.read()).toMatchObject({
        attempt: id,
        revision,
      });
      return { kind: "disconnected" };
    });
    await login.initialize();
    await login.submit("000001");
    expect(dependencies.login).toHaveBeenCalledOnce();
    expect(login.snapshot().kind).toBe("pending");
  });

  it("refresh and backoff preserve the original operation and never auto-submit", async () => {
    const { login, dependencies, advance } = fixture();
    await login.initialize();
    await login.submit("000001");
    const reopened = new Login(dependencies);
    await reopened.initialize();
    expect(reopened.snapshot()).toMatchObject({ kind: "pending", sent: true });
    await reopened.confirm();
    expect(dependencies.login).toHaveBeenCalledTimes(1);
    advance(5000);
    await reopened.confirm();
    expect(dependencies.login).toHaveBeenCalledTimes(2);
    expect(dependencies.login.mock.calls[1].slice(0, 2)).toEqual(
      dependencies.login.mock.calls[0].slice(0, 2),
    );
    expect(dependencies.uuid).toHaveBeenCalledTimes(1);
    expect(dependencies.secret).toHaveBeenCalledTimes(1);
  });

  it("does not send when durable material cannot be saved", async () => {
    const { login, dependencies } = fixture();
    await login.initialize();
    dependencies.storage.setItem.mockImplementation(() => {
      throw new Error("full");
    });
    await login.submit("000001");
    expect(dependencies.login).not.toHaveBeenCalled();
    expect(login.snapshot().kind).toBe("storage");
  });

  it("rechecks an attempt after delayed build verification before writing", async () => {
    const { login, dependencies, select } = fixture();
    const build = deferred<boolean>();
    dependencies.current.mockReturnValue(build.promise);
    await login.initialize();
    const work = login.submit("000001");
    await vi.waitFor(() => expect(dependencies.current).toHaveBeenCalled());
    select({ version: 1, revision: second, attempt: second, session: null });
    build.resolve(true);
    await work;
    expect(dependencies.login).not.toHaveBeenCalled();
    expect(login.snapshot().kind).toBe("changed");
  });

  it.each(["session", "registration", "failure"] as const)(
    "does not let a delayed %s override a later attempt",
    async (kind) => {
      const { login, dependencies, select, records } = fixture();
      const response = deferred<LoginResult>();
      dependencies.login.mockReturnValue(response.promise);
      await login.initialize();
      const work = login.submit("000001");
      await vi.waitFor(() => expect(dependencies.login).toHaveBeenCalled());
      select({ version: 1, revision: second, attempt: second, session: null });
      response.resolve(
        kind === "session"
          ? { kind, session }
          : kind === "registration"
            ? { kind, registration }
            : {
                kind,
                code: "verification_invalid",
                status: 403,
                retryAfter: 0,
              },
      );
      await work;
      expect(login.snapshot().kind).toBe("changed");
      expect(dependencies.sessions.accept).not.toHaveBeenCalled();
      expect(JSON.parse(records.get(LOGIN_KEY)!).kind).toBe(
        kind === "session" ? "session" : "login",
      );
    },
  );

  it("selects the exact session without treating it as a verified account", async () => {
    const { login, dependencies, records, selected } = fixture();
    records.set(
      "halo.c1",
      JSON.stringify({ operation, input: { token: "old" } }),
    );
    dependencies.login.mockResolvedValue({ kind: "session", session });
    await login.initialize();
    await login.submit("000001");
    expect(login.snapshot()).toEqual({
      kind: "session",
      selection: { id: session.id, revision: second },
    });
    expect(selected()?.session).toEqual(session);
    expect(JSON.parse(records.get(LOGIN_KEY)!)).toEqual({
      version: 1,
      kind: "closed",
      reason: "session",
    });
    expect(records.has(MESSAGE_KEY)).toBe(false);
    expect(records.has("halo.c1")).toBe(false);
  });

  it("saves bounded registration material before deleting the login secret", async () => {
    const { login, dependencies, records, advance } = fixture();
    dependencies.login.mockResolvedValue({
      kind: "registration",
      registration,
    });
    await login.initialize();
    await login.submit("000001");
    const saved = JSON.parse(records.get(LOGIN_KEY)!);
    expect(saved.kind).toBe("registration");
    expect(saved.registration).toEqual(registration);
    expect(saved).not.toHaveProperty("input");
    expect(login.snapshot()).toEqual({
      kind: "registration",
      expiresAt: Date.parse(registration.expiresAt),
    });
    advance(600_000);
    login.expire();
    expect(login.snapshot().kind).toBe("expired");
    expect(records.get(LOGIN_KEY)).not.toContain(registration.token);
  });

  it("retains original material if saving the registration handoff fails", async () => {
    const { login, dependencies, records } = fixture();
    dependencies.login.mockImplementation(async () => {
      dependencies.storage.setItem.mockImplementation((key, value) => {
        if (JSON.parse(value).kind === "registration") throw new Error("full");
        records.set(key, value);
      });
      return { kind: "registration", registration };
    });
    await login.initialize();
    await login.submit("000001");
    expect(JSON.parse(records.get(LOGIN_KEY)!).kind).toBe("login");
    expect(login.snapshot().kind).toBe("pending");
  });

  it.each([false, true])(
    "preserves rate-limit context after an earlier unknown result: %s",
    async (unknown) => {
      const f = fixture();
      await f.login.initialize();
      if (unknown) {
        await f.login.submit("000001");
        f.advance(5000);
      }
      f.dependencies.login.mockResolvedValue({
        kind: "failure",
        code: "rate_limited",
        status: 429,
        retryAfter: 60_000,
      });
      if (unknown) await f.login.confirm();
      else await f.login.submit("000001");
      const proof = JSON.parse(f.records.get(LOGIN_KEY)!);
      expect(f.login.snapshot()).toEqual({
        kind: "limited",
        retryAt: proof.retryAt,
        sent: unknown,
      });
      expect(proof.sent).toBe(unknown);
      expect(proof.expiresAt).toBe(
        Date.parse(unknown ? "2035-01-01T00:30:00Z" : "2035-01-01T00:05:00Z"),
      );
      const calls = f.dependencies.login.mock.calls.length;
      const reopened = new Login(f.dependencies);
      await reopened.initialize();
      expect(reopened.snapshot()).toEqual(f.login.snapshot());
      await reopened.confirm();
      expect(f.dependencies.login).toHaveBeenCalledTimes(calls);
      f.advance(60_000);
      await reopened.confirm();
      expect(f.dependencies.login).toHaveBeenCalledTimes(calls + 1);
      expect(f.dependencies.login.mock.calls.at(-1)!.slice(0, 2)).toEqual(
        f.dependencies.login.mock.calls[0].slice(0, 2),
      );
      expect(JSON.parse(f.records.get(LOGIN_KEY)!).expiresAt).toBe(
        proof.expiresAt,
      );
      f.advance(proof.expiresAt - f.dependencies.now());
      const expired = new Login(f.dependencies);
      await expired.initialize();
      await expired.confirm();
      expect(expired.snapshot().kind).toBe("expired");
      expect(f.dependencies.login).toHaveBeenCalledTimes(calls + 1);
      expect(f.records.get(LOGIN_KEY)).not.toContain("000001");
    },
  );

  it("keeps a long server backoff across refresh", async () => {
    const { login, dependencies, advance } = fixture();
    dependencies.login.mockResolvedValue({
      kind: "failure",
      code: "rate_limited",
      status: 429,
      retryAfter: 60_000,
    });
    await login.initialize();
    await login.submit("000001");
    advance(5000);
    const reopened = new Login(dependencies);
    await reopened.initialize();
    await reopened.confirm();
    expect(dependencies.login).toHaveBeenCalledTimes(1);
    advance(55_000);
    await reopened.confirm();
    expect(dependencies.login).toHaveBeenCalledTimes(2);
  });

  it.each(["request_mismatch", "authentication_invalid"])(
    "retains proof and stops replay for %s",
    async (code) => {
      const { login, dependencies, records, advance } = fixture();
      dependencies.login.mockResolvedValue({
        kind: "failure",
        code,
        status: code === "request_mismatch" ? 409 : 401,
        retryAfter: 0,
      });
      await login.initialize();
      await login.submit("000001");
      const original = records.get(LOGIN_KEY);
      advance(6000);
      const reopened = new Login(dependencies);
      await reopened.initialize();
      await reopened.confirm();
      expect(reopened.snapshot()).toMatchObject({
        kind: "rejected",
        reason: code,
      });
      expect(dependencies.login).toHaveBeenCalledTimes(1);
      expect(records.get(LOGIN_KEY)).toBe(original);
    },
  );

  it("starts a distinct, explicit attempt for a corrected code only after a known rejection", async () => {
    const { login, dependencies } = fixture();
    dependencies.login.mockResolvedValue({
      kind: "failure",
      code: "verification_invalid",
      status: 403,
      retryAfter: 0,
    });
    await login.initialize();
    await login.submit("000001");
    expect(login.snapshot()).toMatchObject({ kind: "editing", invalid: true });
    expect(dependencies.login).toHaveBeenCalledTimes(1);
    dependencies.uuid.mockReturnValue(second);
    await login.submit("000002");
    expect(dependencies.login.mock.calls[1][0]).toBe(second);
    expect(dependencies.login.mock.calls[1][1].credential.code).toBe("000002");
  });

  it.each(["permission_denied", "invalid_input"])(
    "a preflight %s on replay does not erase an unknown original outcome",
    async (code) => {
      const { login, dependencies, records, advance } = fixture();
      await login.initialize();
      await login.submit("000001");
      advance(5000);
      dependencies.login.mockResolvedValue({
        kind: "failure",
        code,
        status: code === "permission_denied" ? 403 : 422,
        retryAfter: 0,
      });
      await login.confirm();
      expect(JSON.parse(records.get(LOGIN_KEY)!)).toMatchObject({
        kind: "login",
        reason: code,
        input: { credential: { code: "000001" } },
      });
      const reopened = new Login(dependencies);
      await reopened.initialize();
      await reopened.confirm();
      expect(dependencies.login).toHaveBeenCalledTimes(2);
    },
  );

  it("finishes accepted-session cleanup after the page stops waiting", async () => {
    const { login, dependencies, records } = fixture();
    const accepted = deferred<void>();
    const accept = dependencies.sessions.accept.getMockImplementation()!;
    dependencies.sessions.accept.mockImplementation(async (attempt, value) => {
      const selection = await accept(attempt, value);
      await accepted.promise;
      return selection;
    });
    dependencies.login.mockResolvedValue({ kind: "session", session });
    await login.initialize();
    const work = login.submit("000001");
    await vi.waitFor(() =>
      expect(dependencies.sessions.accept).toHaveBeenCalledOnce(),
    );
    login.suspend();
    accepted.resolve();
    await work;
    expect(JSON.parse(records.get(LOGIN_KEY)!)).toEqual({
      version: 1,
      kind: "session",
      attempt: { operation, revision },
      session,
    });
    expect(records.has(MESSAGE_KEY)).toBe(false);
    await login.selectionChanged();
    expect(login.snapshot().kind).toBe("session");
    const reopened = new Login(dependencies);
    await reopened.initialize();
    expect(reopened.snapshot().kind).toBe("session");
  });

  it("an accepted old session does not remove a later tab operation", async () => {
    const { login, dependencies, records } = fixture();
    const message = JSON.parse(records.get(MESSAGE_KEY)!);
    message.operation = second;
    message.message.id = "019354fd-933c-7e6a-8912-345678901235";
    const nextMessage = JSON.stringify(message);
    const accepted = deferred<Selection>();
    dependencies.sessions.accept.mockReturnValue(accepted.promise);
    dependencies.login.mockResolvedValue({ kind: "session", session });
    await login.initialize();
    const work = login.submit("000001");
    await vi.waitFor(() =>
      expect(dependencies.sessions.accept).toHaveBeenCalledOnce(),
    );
    login.suspend();
    const next = JSON.parse(records.get(LOGIN_KEY)!);
    next.attempt.operation = second;
    const saved = JSON.stringify(next);
    records.set(LOGIN_KEY, saved);
    records.set(MESSAGE_KEY, nextMessage);
    accepted.resolve({ id: session.id, revision: second });
    await work;
    expect(records.get(LOGIN_KEY)).toBe(saved);
    expect(records.get(MESSAGE_KEY)).toBe(nextMessage);
  });

  it("returning during local attempt registration restores editing without a write", async () => {
    const { login, dependencies, records } = fixture();
    const begun = deferred<Attempt>();
    dependencies.sessions.begin.mockReturnValueOnce(begun.promise);
    await login.initialize();
    const work = login.submit("000001");
    expect(dependencies.sessions.begin).toHaveBeenCalledOnce();
    login.suspend();
    await login.selectionChanged();
    expect(login.snapshot().kind).toBe("editing");
    begun.resolve({ operation, revision });
    await work;
    expect(login.snapshot().kind).toBe("editing");
    expect(dependencies.login).not.toHaveBeenCalled();
    expect(records.has(LOGIN_KEY)).toBe(false);
  });

  it("cancellation ends the local wait while preserving a five second backoff", async () => {
    const { login, dependencies, advance, records } = fixture();
    const build = deferred<boolean>();
    dependencies.current.mockReturnValueOnce(build.promise);
    await login.initialize();
    const work = login.submit("000001");
    await vi.waitFor(() => expect(dependencies.current).toHaveBeenCalled());
    login.suspend();
    build.reject(new Error("offline"));
    await work;
    expect(login.snapshot()).toMatchObject({ kind: "pending", sent: false });
    const original = JSON.parse(records.get(LOGIN_KEY)!);
    const reopened = new Login(dependencies);
    await reopened.initialize();
    expect(reopened.snapshot()).toMatchObject({ kind: "pending", sent: false });
    await reopened.confirm();
    expect(dependencies.login).not.toHaveBeenCalled();
    advance(5000);
    await reopened.confirm();
    expect(dependencies.login).toHaveBeenCalledOnce();
    expect(dependencies.login.mock.calls[0].slice(0, 2)).toEqual([
      original.attempt.operation,
      original.input,
    ]);
    expect(reopened.snapshot()).toMatchObject({ kind: "pending", sent: true });
  });

  it("a delayed preflight cannot revive expired material", async () => {
    const { login, dependencies, advance, records } = fixture();
    const build = deferred<boolean>();
    dependencies.current.mockReturnValue(build.promise);
    await login.initialize();
    const work = login.submit("000001");
    await vi.waitFor(() => expect(dependencies.current).toHaveBeenCalled());
    advance(30 * 60_000);
    login.expire();
    build.reject(new Error("offline"));
    await work;
    expect(login.snapshot().kind).toBe("expired");
    expect(dependencies.login).not.toHaveBeenCalled();
    expect(records.get(LOGIN_KEY)).not.toContain("secret");
  });
});
