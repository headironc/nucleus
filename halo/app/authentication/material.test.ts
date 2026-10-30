import { describe, expect, it, vi } from "vitest";
import { Recovery } from "../codes/recovery";
import {
  availableCode,
  LOGIN_KEY,
  MESSAGE_KEY,
  maintainMaterial,
  parseMaterial,
  readCode,
  receipts,
  receiptKey,
  receiveCode,
  type Proof,
} from "./material";

const now = Date.parse("2035-01-01T00:00:00Z");
const send = "fdb0e5e6-2cd3-46c2-8ae3-3b4783beadbe";
const operation = "e8a537ec-584b-40af-8026-d3cc3292b762";
const revision = "b8750f17-c9cf-4ee6-b52c-1347cfa8061a";
const id = "019354fd-933c-7e6a-8912-345678901234";
const cleanup = { message: id, send };
const code = {
  version: 1,
  operation: send,
  mobile: "13900000000",
  message: { id, delivery: "sent", expiresAt: "2035-01-01T00:00:05Z" },
  createdAt: now,
  expiresAt: now + 5000,
};
const proof: Proof = {
  version: 1,
  kind: "login",
  attempt: { operation, revision },
  send,
  input: {
    secret: "A".repeat(43),
    credential: { type: "message", id, mobile: code.mobile, code: "000001" },
  },
  createdAt: now,
  expiresAt: now + 30_000,
  retryAt: 0,
  sent: true,
};
const next = {
  version: 1,
  kind: "registration",
  attempt: { operation, revision },
  registration: {
    id,
    token: Buffer.alloc(32, 1).toString("base64url"),
    expiresAt: "2035-01-01T00:01:00Z",
  },
  createdAt: now,
  expiresAt: now + 30_000,
  cleanup,
};

function fixture(value: unknown = proof) {
  const c1 = {
    version: 1,
    operation: send,
    input: { mobile: code.mobile, token: "turnstile-proof" },
    createdAt: now,
    expiresAt: now + 30 * 60_000,
    retryAt: 0,
    attempted: true,
  };
  const records = new Map([
    [LOGIN_KEY, JSON.stringify(value)],
    [MESSAGE_KEY, JSON.stringify(code)],
    ["halo.c1", JSON.stringify(c1)],
    ["unrelated", "preserve"],
  ]);
  const storage = {
    get length() {
      return records.size;
    },
    key: (index: number) => [...records.keys()][index] ?? null,
    getItem: vi.fn((key: string) => records.get(key) ?? null),
    setItem: vi.fn((key: string, bytes: string) => {
      records.set(key, bytes);
    }),
    removeItem: vi.fn((key: string) => {
      records.delete(key);
    }),
  };
  return { storage, records };
}

describe("anonymous authentication material maintenance", () => {
  it.each([
    ["expired", JSON.stringify(code), now + 5000],
    ["clock rollback", JSON.stringify(code), now - 1],
    ["invalid clock", JSON.stringify(code), NaN],
    ["invalid JSON", "truncated", now],
    ["invalid shape", JSON.stringify({ ...code, mobile: "invalid" }), now],
  ] as const)(
    "preserves an orphan handoff after %s as a non-secret C1 placeholder",
    (_name, bytes, time) => {
      const { storage, records } = fixture();
      records.delete(LOGIN_KEY);
      records.delete("halo.c1");
      records.set(MESSAGE_KEY, bytes);
      expect(maintainMaterial(storage, time)).toBe(true);
      expect(records.has(MESSAGE_KEY)).toBe(false);
      expect(records.get("halo.c1")).toBe(
        JSON.stringify({ version: 1, closed: true }),
      );
      expect(records.has(LOGIN_KEY)).toBe(false);
      expect(availableCode(storage, now)).toBeUndefined();
      const uuid = vi.fn(() => operation);
      const sendRequest = vi.fn();
      const recovery = new Recovery({
        storage,
        now: () => now,
        marked: () => false,
        blocked: () => false,
        mark: vi.fn(),
        uuid,
        current: async () => true,
        send: sendRequest,
      });
      recovery.initialize();
      expect(recovery.snapshot()).toEqual({ kind: "missing" });
      expect(uuid).not.toHaveBeenCalled();
      expect(sendRequest).not.toHaveBeenCalled();
      expect(records.get("unrelated")).toBe("preserve");
    },
  );

  it.each(["throw", "no-op", "readback"])(
    "keeps an orphan handoff until its placeholder is verified after %s",
    (failure) => {
      const { storage, records } = fixture();
      records.delete(LOGIN_KEY);
      records.delete("halo.c1");
      const write = storage.setItem.getMockImplementation()!;
      const read = storage.getItem.getMockImplementation()!;
      storage.setItem.mockImplementation((key, bytes) => {
        if (key !== "halo.c1") return write(key, bytes);
        if (failure === "throw") throw new Error("write denied");
        if (failure === "readback") {
          write(key, bytes);
          storage.getItem.mockImplementation((target) => {
            if (target === "halo.c1") throw new Error("readback denied");
            return read(target);
          });
        }
      });
      expect(maintainMaterial(storage, now + 5000)).toBe(false);
      expect(records.get(MESSAGE_KEY)).toBe(JSON.stringify(code));
      expect(storage.removeItem).not.toHaveBeenCalled();
      storage.setItem.mockImplementation(write);
      storage.getItem.mockImplementation(read);
      expect(maintainMaterial(storage, now + 5001)).toBe(true);
      expect(records.has(MESSAGE_KEY)).toBe(false);
      expect(records.get("halo.c1")).toBe(
        JSON.stringify({ version: 1, closed: true }),
      );
    },
  );

  it("keeps the orphan placeholder across failed handoff removal and retries", () => {
    const { storage, records } = fixture();
    records.delete(LOGIN_KEY);
    records.delete("halo.c1");
    const remove = storage.removeItem.getMockImplementation()!;
    storage.removeItem.mockImplementation(() => {});
    expect(maintainMaterial(storage, now + 5000)).toBe(false);
    expect(records.get("halo.c1")).toBe(
      JSON.stringify({ version: 1, closed: true }),
    );
    expect(records.get(MESSAGE_KEY)).toBe(JSON.stringify(code));
    storage.removeItem.mockImplementation(remove);
    expect(maintainMaterial(storage, now + 5001)).toBe(true);
    expect(records.has(MESSAGE_KEY)).toBe(false);
  });

  it("preserves a newer C1 while retiring an expired handoff", () => {
    const { storage, records } = fixture();
    records.delete(LOGIN_KEY);
    const newer = JSON.stringify({
      ...JSON.parse(records.get("halo.c1")!),
      operation,
    });
    records.set("halo.c1", newer);
    expect(readCode(storage, now + 5000)).toBeUndefined();
    expect(records.get("halo.c1")).toBe(newer);
    expect(records.has(LOGIN_KEY)).toBe(false);
  });

  it.each([proof, next])(
    "does not create a C1 placeholder for a surviving $kind stage",
    (value) => {
      const { storage, records } = fixture(value);
      records.delete("halo.c1");
      expect(readCode(storage, now + 5000)).toBeUndefined();
      expect(records.has("halo.c1")).toBe(false);
      expect(records.get(LOGIN_KEY)).toBe(JSON.stringify(value));
    },
  );

  it.each(["throw", "no-op", "readback"])(
    "keeps the previous stage when saving an SMS handoff fails with %s",
    (failure) => {
      const closed = {
        version: 1,
        kind: "closed",
        reason: "verification_required",
      };
      const { storage, records } = fixture(closed);
      records.delete(MESSAGE_KEY);
      const originalSend = records.get("halo.c1");
      const write = storage.setItem.getMockImplementation()!;
      storage.setItem.mockImplementation((key, bytes) => {
        if (key !== MESSAGE_KEY) return write(key, bytes);
        if (failure === "throw") throw new Error("write denied");
        if (failure === "readback") {
          write(key, bytes);
          storage.getItem.mockImplementation(() => {
            throw new Error("readback denied");
          });
        }
      });
      expect(() =>
        receiveCode(
          storage,
          send,
          code.mobile,
          { ...code.message, delivery: "sent" },
          now,
        ),
      ).toThrow();
      expect(records.get(LOGIN_KEY)).toBe(JSON.stringify(closed));
      expect(records.get("halo.c1")).toBe(originalSend);
      expect(storage.removeItem).not.toHaveBeenCalled();
    },
  );

  it.each([
    [5, 5],
    [60, 30],
  ])(
    "bounds an SMS deadline of %i minutes to %i minutes without extending it on receipt",
    (protocolMinutes, expectedMinutes) => {
      const { storage, records } = fixture();
      records.delete(LOGIN_KEY);
      records.delete(MESSAGE_KEY);
      const message = {
        ...code.message,
        delivery: "sent" as const,
        expiresAt: new Date(now + protocolMinutes * 60_000).toISOString(),
      };
      receiveCode(storage, send, code.mobile, message, now);
      const initial = readCode(storage, now);
      expect(initial?.createdAt).toBe(now);
      expect(initial?.expiresAt).toBe(now + expectedMinutes * 60_000);
      receiveCode(storage, send, code.mobile, message, now + 60_000);
      expect(readCode(storage, now + 60_000)).toEqual(initial);
      expect(readCode(storage, now + expectedMinutes * 60_000)).toBeUndefined();
      expect(records.has(MESSAGE_KEY)).toBe(false);
      expect(records.has("halo.c1")).toBe(true);
    },
  );

  it.each(["throw", "no-op"])(
    "retains the cleanup locator when saving the closed state fails with %s",
    (failure) => {
      const { storage, records } = fixture();
      const originalSend = records.get("halo.c1");
      const write = storage.setItem.getMockImplementation()!;
      storage.setItem.mockImplementation((key, bytes) => {
        if (key !== LOGIN_KEY) return write(key, bytes);
        if (failure === "throw") throw new Error("write denied");
      });
      expect(maintainMaterial(storage, now + 30_000)).toBe(false);
      expect(records.get(LOGIN_KEY)).toBe(JSON.stringify(proof));
      expect(records.get("halo.c1")).toBe(originalSend);
      storage.setItem.mockImplementation(write);
      const remove = storage.removeItem.getMockImplementation()!;
      storage.removeItem.mockImplementation((key) => {
        if (key === "halo.c1") {
          expect(JSON.parse(records.get(LOGIN_KEY)!)).toEqual({
            version: 1,
            kind: "closed",
            reason: "expired",
            cleanup,
          });
          expect(records.get(LOGIN_KEY)).not.toContain(proof.input.secret);
        }
        remove(key);
      });
      expect(maintainMaterial(storage, now + 30_001)).toBe(true);
      expect(records.has("halo.c1")).toBe(false);
      expect(JSON.parse(records.get(LOGIN_KEY)!)).toEqual({
        version: 1,
        kind: "closed",
        reason: "expired",
      });
    },
  );

  it("maintains valid receipts even when a damaged receipt cannot be removed", () => {
    const { storage, records } = fixture();
    records.delete(LOGIN_KEY);
    const invalidKey = receiptKey(operation);
    const validKey = receiptKey(send);
    records.set(invalidKey, "truncated");
    const valid = {
      version: 1,
      kind: "session",
      attempt: { operation: send, revision },
      session: { id, expiresAt: "2035-01-02T00:00:00Z" },
    };
    records.set(validKey, JSON.stringify({ ...valid, cleanup }));
    const remove = storage.removeItem.getMockImplementation()!;
    storage.removeItem.mockImplementation((key) => {
      if (key !== invalidKey) remove(key);
    });
    expect(maintainMaterial(storage, now)).toBe(false);
    expect(records.get(invalidKey)).toBe("truncated");
    expect(records.get(validKey)).toBe(JSON.stringify(valid));
    expect(records.has(MESSAGE_KEY)).toBe(false);
    expect(records.has("halo.c1")).toBe(false);
    expect(records.get("unrelated")).toBe("preserve");
    storage.removeItem.mockImplementation(remove);
    expect(maintainMaterial(storage, now)).toBe(true);
    expect(receipts(storage)).toEqual([valid]);
  });

  it.each([
    ["truncated JSON", '{"version":1'],
    ["old shape", JSON.stringify({ version: 1, session: { id } })],
    ["wrong kind", JSON.stringify(next)],
    [
      "mismatched operation",
      JSON.stringify({
        version: 1,
        kind: "session",
        attempt: { operation: send, revision },
        session: { id, expiresAt: "2035-01-02T00:00:00Z" },
      }),
    ],
  ])("retires only the invalid receipt with %s", (_name, bytes) => {
    const { storage, records } = fixture();
    const original = new Map(records);
    const valid = {
      version: 1,
      kind: "session",
      attempt: { operation: send, revision },
      session: { id, expiresAt: "2035-01-02T00:00:00Z" },
    };
    records.set(receiptKey(operation), bytes);
    records.set(receiptKey(send), JSON.stringify(valid));
    expect(receipts(storage)).toEqual([valid]);
    expect(records.has(receiptKey(operation))).toBe(false);
    expect(records.get(receiptKey(send))).toBe(JSON.stringify(valid));
    for (const [key, value] of original) expect(records.get(key)).toBe(value);
  });

  it.each(["throw", "no-op"])(
    "retries invalid receipt removal after a %s without reporting success",
    (failure) => {
      const { storage, records } = fixture();
      const key = receiptKey(operation);
      records.set(key, "truncated");
      const remove = storage.removeItem.getMockImplementation()!;
      storage.removeItem.mockImplementation((target) => {
        if (target !== key) return remove(target);
        if (failure === "throw") throw new Error("temporarily denied");
      });
      expect(() => receipts(storage)).toThrow();
      expect(maintainMaterial(storage, now)).toBe(false);
      expect(records.get(key)).toBe("truncated");
      storage.removeItem.mockImplementation(remove);
      expect(maintainMaterial(storage, now)).toBe(true);
      expect(records.has(key)).toBe(false);
      expect(receipts(storage)).toEqual([]);
      expect(records.get(LOGIN_KEY)).toBe(JSON.stringify(proof));
    },
  );

  it.each([proof, next])(
    "removes expired secrets for $kind independently of the login route",
    (value) => {
      const { storage, records } = fixture(value);
      expect(maintainMaterial(storage, now + 30_000)).toBe(true);
      expect(JSON.parse(records.get(LOGIN_KEY)!)).toEqual({
        version: 1,
        kind: "closed",
        reason: "expired",
      });
      expect(records.has(MESSAGE_KEY)).toBe(false);
      expect(records.has("halo.c1")).toBe(false);
      expect(records.get("unrelated")).toBe("preserve");
    },
  );

  it.each([now - 1, NaN, Infinity])(
    "fails closed at unreliable time %s",
    (time) => {
      const { storage, records } = fixture();
      expect(maintainMaterial(storage, time)).toBe(true);
      expect(records.get(LOGIN_KEY)).not.toContain(proof.input.secret);
    },
  );

  it("does not shorten C1 recovery just because an unused SMS handoff expires", () => {
    const { storage, records } = fixture();
    records.delete(LOGIN_KEY);
    const original = records.get("halo.c1");
    maintainMaterial(storage, now + 5000);
    expect(records.has(MESSAGE_KEY)).toBe(false);
    expect(records.get("halo.c1")).toBe(original);
  });

  it("retries the original C1 cleanup after its Code has expired and disappeared", () => {
    const { storage, records } = fixture(next);
    const remove = storage.removeItem.getMockImplementation()!;
    storage.removeItem.mockImplementation((key) => {
      if (key === "halo.c1") throw new Error("temporarily denied");
      remove(key);
    });
    expect(maintainMaterial(storage, now + 5000)).toBe(false);
    expect(records.has(MESSAGE_KEY)).toBe(false);
    expect(JSON.parse(records.get(LOGIN_KEY)!).cleanup).toEqual(cleanup);
    expect(records.get("halo.c1")).toContain("turnstile-proof");
    expect(maintainMaterial(storage, now + 30_000)).toBe(false);
    expect(JSON.parse(records.get(LOGIN_KEY)!)).toEqual({
      version: 1,
      kind: "closed",
      reason: "expired",
      cleanup,
    });
    expect(records.get(LOGIN_KEY)).not.toContain(next.registration.token);
    storage.removeItem.mockImplementation(remove);
    expect(maintainMaterial(storage, now + 30_001)).toBe(true);
    expect(records.has("halo.c1")).toBe(false);
    expect(JSON.parse(records.get(LOGIN_KEY)!)).not.toHaveProperty("cleanup");
  });

  it("preserves a newer C1 and a different handoff when retiring an old result", () => {
    const { storage, records } = fixture(next);
    const replacement = JSON.stringify({
      ...code,
      operation,
      message: { ...code.message, id: "019354fd-933c-7e6a-8912-345678901235" },
    });
    records.set(MESSAGE_KEY, replacement);
    const newer = JSON.stringify({
      ...JSON.parse(records.get("halo.c1")!),
      operation,
    });
    records.set("halo.c1", newer);
    expect(maintainMaterial(storage, now)).toBe(true);
    expect(records.get(MESSAGE_KEY)).toBe(replacement);
    expect(records.get("halo.c1")).toBe(newer);
    expect(JSON.parse(records.get(LOGIN_KEY)!)).not.toHaveProperty("cleanup");
  });

  it("retries cleanup after a no-op removal without discarding the locator", () => {
    const { storage, records } = fixture(next);
    const remove = storage.removeItem.getMockImplementation()!;
    storage.removeItem.mockImplementation(() => {});
    expect(maintainMaterial(storage, now)).toBe(false);
    expect(JSON.parse(records.get(LOGIN_KEY)!).cleanup).toEqual(cleanup);
    storage.removeItem.mockImplementation(remove);
    expect(maintainMaterial(storage, now)).toBe(true);
    expect(records.has("halo.c1")).toBe(false);
    expect(records.has(MESSAGE_KEY)).toBe(false);
  });

  it.each([
    null,
    { message: id },
    { send },
    { message: id, send: "invalid" },
    { message: id, send, secret: "leak" },
  ])("rejects an incomplete cleanup locator %j", (value) => {
    expect(
      parseMaterial(JSON.stringify({ ...next, cleanup: value })),
    ).toBeUndefined();
    expect(
      parseMaterial(
        JSON.stringify({
          version: 1,
          kind: "closed",
          reason: "expired",
          cleanup: value,
        }),
      ),
    ).toBeUndefined();
  });

  it("exposes saved SMS input after C1 bookkeeping fails but rejects a later operation", () => {
    const { storage, records } = fixture();
    records.delete(LOGIN_KEY);
    records.delete("halo.c1");
    expect(availableCode(storage, now)).toEqual(code);
    records.set("halo.c1", JSON.stringify({ version: 1, closed: true }));
    expect(availableCode(storage, now)).toEqual(code);
    records.set("halo.c1", JSON.stringify({ operation }));
    expect(availableCode(storage, now)).toBeUndefined();
  });

  it("retires a determined R1 rejection only after an explicitly new SMS is saved", () => {
    const { storage, records } = fixture({
      version: 1,
      kind: "closed",
      reason: "verification_required",
    });
    records.delete("halo.c1");
    expect(availableCode(storage, now)).toBeUndefined();
    expect(readCode(storage, now)).toEqual(code);
    expect(records.has(LOGIN_KEY)).toBe(true);
    receiveCode(
      storage,
      operation,
      code.mobile,
      {
        ...code.message,
        delivery: "sent",
        id: "019354fd-933c-7e6a-8912-345678901235",
      },
      now,
    );
    expect(records.has(LOGIN_KEY)).toBe(false);
    expect(availableCode(storage, now)?.operation).toBe(operation);
  });
});
