import { describe, expect, it, vi } from "vitest";
import { Registration } from "./recovery";
import { KEY, maintain, parse, receiptKey } from "./material";
import { LOGIN_KEY, LIFETIME } from "../authentication/material";
import type { BrowserSession, Attempt } from "../authentication/session";
import type { Result } from "../protocol/registration";
import type { Session } from "../protocol/session";

const operation = "9ea8cfed-85c5-4f44-8cf6-093a0f919b4c";
const attempt = {
  operation: "f11342a8-305e-4348-a815-d9747bc0e50b",
  revision: "f171a0f4-bce5-415a-bec6-c58c5db5a69c",
};
const nextRevision = "b8750f17-c9cf-4ee6-b52c-1347cfa8061a";
const session = {
  id: "019354fd-933c-7e6a-8912-345678901234",
  expiresAt: "2035-01-02T00:00:00Z",
};
const registration = {
  id: session.id,
  token: "A".repeat(43),
  expiresAt: "2035-01-01T00:10:00Z",
};
const name = "示例用户";
const idNumber = "11010519491231002X";
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { resolve, promise };
}
function fixture() {
  let now = Date.parse("2035-01-01T00:00:00Z");
  let selected: BrowserSession | null = {
    version: 1,
    revision: attempt.revision,
    attempt: attempt.operation,
    session: null,
  };
  const records = new Map<string, string>();
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
  const next = {
    version: 1,
    kind: "registration",
    attempt,
    registration,
    createdAt: now,
    expiresAt: now + 600_000,
  };
  records.set(LOGIN_KEY, JSON.stringify(next));
  const dependencies = {
    storage,
    now: () => now,
    uuid: vi.fn(() => operation),
    current: vi.fn(async () => true),
    sessions: {
      read: vi.fn(async () => structuredClone(selected)),
      accept: vi.fn(async (expected: Attempt, value: Session) => {
        if (
          selected?.attempt !== expected.operation ||
          selected.revision !== expected.revision
        )
          return null;
        selected = {
          version: 1,
          revision: nextRevision,
          session: value,
          attempt: null,
        };
        return { id: value.id, revision: nextRevision };
      }),
    },
    register: vi.fn(async (): Promise<Result> => ({ kind: "disconnected" })),
  };
  return {
    dependencies,
    storage,
    records,
    next,
    create: () => new Registration(dependencies),
    setNow: (value: number) => {
      now = value;
    },
    advance: (ms: number) => {
      now += ms;
    },
    select: (value: BrowserSession | null) => {
      selected = value;
    },
    selection: () => selected,
  };
}
describe("registration original operation", () => {
  it("does not save unsubmitted names and saves the full original input before POST", async () => {
    const f = fixture();
    const flow = f.create();
    await flow.initialize();
    expect(flow.snapshot().kind).toBe("editing");
    expect(f.records.has(KEY)).toBe(false);
    f.dependencies.register.mockImplementation(async () => {
      const proof = parse(f.records.get(KEY)!);
      expect(proof).toMatchObject({
        operation,
        sent: true,
        attempt,
        input: { name, idNumber },
      });
      expect(f.records.get(LOGIN_KEY)).not.toContain(registration.token);
      return { kind: "disconnected" };
    });
    await flow.submit(` ${name} `, idNumber.toLowerCase());
    expect(f.dependencies.register).toHaveBeenCalledTimes(1);
    expect(operation).not.toBe(attempt.operation);
    expect(flow.snapshot()).toMatchObject({ kind: "pending", sent: true });
  });
  it.each(["throw", "silent", "readback"])(
    "does not submit when original save fails: %s",
    async (mode) => {
      const f = fixture();
      const flow = f.create();
      await flow.initialize();
      const original = f.storage.setItem.getMockImplementation()!;
      f.storage.setItem.mockImplementation((key, value) => {
        if (key === KEY) {
          if (mode === "throw") throw new Error("quota");
          if (mode === "silent") return;
          original(key, value);
          f.storage.getItem.mockImplementationOnce(() => {
            throw new Error("read");
          });
          return;
        }
        original(key, value);
      });
      await flow.submit(name, idNumber);
      expect(f.dependencies.register).not.toHaveBeenCalled();
      expect(flow.snapshot().kind).toBe("storage-before");
      expect(f.records.get(LOGIN_KEY)).toContain(registration.token);
      f.storage.setItem.mockImplementation(original);
      await flow.retryLocal();
      expect(flow.snapshot().kind).toBe("editing");
      expect(f.dependencies.register).not.toHaveBeenCalled();
      expect(f.records.has(KEY)).toBe(false);
    },
  );
  it("reopens without a POST, respects backoff and reuses identical original bytes", async () => {
    const f = fixture();
    const flow = f.create();
    await flow.initialize();
    await flow.submit(name, idNumber);
    const original = f.dependencies.register.mock.calls[0];
    const reopened = f.create();
    await reopened.initialize();
    await reopened.confirm();
    expect(f.dependencies.register).toHaveBeenCalledTimes(1);
    f.advance(5000);
    await reopened.confirm();
    expect(f.dependencies.register).toHaveBeenCalledTimes(2);
    expect(f.dependencies.register.mock.calls[1].slice(0, 2)).toEqual(
      original.slice(0, 2),
    );
  });
  it.each([
    "authentication_invalid",
    "request_mismatch",
    "invalid_input",
    "permission_denied",
  ])(
    "retains unknown original proof and stops recovery after %s",
    async (code) => {
      const f = fixture();
      const flow = f.create();
      await flow.initialize();
      await flow.submit(name, idNumber);
      f.advance(5000);
      f.dependencies.register.mockResolvedValue({
        kind: "failure",
        code,
        status: 409,
        retryAfter: 0,
      });
      await flow.confirm();
      expect(flow.snapshot()).toMatchObject({
        kind: "rejected",
        reason: code,
        uncertain: true,
      });
      expect(f.records.get(KEY)).toContain(idNumber);
      const reopened = f.create();
      await reopened.initialize();
      await reopened.confirm();
      expect(reopened.snapshot()).toMatchObject({
        kind: "rejected",
        uncertain: true,
      });
      expect(f.dependencies.register).toHaveBeenCalledTimes(2);
    },
  );
  it.each([
    "invalid_input",
    "permission_denied",
    "mobile_taken",
    "id_number_taken",
  ])(
    "a known rejection %s clears original names and needs explicit new input",
    async (code) => {
      const f = fixture();
      f.dependencies.register.mockResolvedValue({
        kind: "failure",
        code,
        status: 409,
        retryAfter: 0,
      });
      const flow = f.create();
      await flow.initialize();
      await flow.submit(name, idNumber);
      expect(flow.snapshot()).toMatchObject({
        kind: "rejected",
        uncertain: false,
      });
      expect(f.records.get(KEY)).not.toContain(idNumber);
      await flow.revise();
      expect(flow.snapshot().kind).toBe("editing");
      expect(f.dependencies.register).toHaveBeenCalledTimes(1);
    },
  );
  it("a first rate limit returns to editing only after Retry-After, unknown rate limits keep original POST", async () => {
    const f = fixture();
    const flow = f.create();
    await flow.initialize();
    f.dependencies.register.mockResolvedValue({
      kind: "failure",
      code: "rate_limited",
      status: 429,
      retryAfter: 17000,
    });
    await flow.submit(name, idNumber);
    await flow.revise();
    expect(flow.snapshot()).toMatchObject({ kind: "limited", sent: false });
    f.advance(17000);
    await flow.revise();
    expect(flow.snapshot().kind).toBe("editing");
    f.dependencies.register.mockResolvedValue({ kind: "disconnected" });
    await flow.submit(name, idNumber);
    f.advance(5000);
    f.dependencies.register.mockResolvedValue({
      kind: "failure",
      code: "rate_limited",
      status: 429,
      retryAfter: 17000,
    });
    await flow.confirm();
    expect(flow.snapshot()).toMatchObject({ kind: "limited", sent: true });
    expect(f.records.get(KEY)).toContain(idNumber);
    f.advance(600_000);
    await flow.initialize();
    expect(flow.snapshot()).toMatchObject({ kind: "limited", sent: true });
  });
  it.each(["mobile_taken", "verification_required", "request_mismatch"])(
    "keeps the known %s result when its local save fails",
    async (code) => {
      const f = fixture();
      const write = f.storage.setItem.getMockImplementation()!;
      f.dependencies.register.mockImplementation(async () => {
        f.storage.setItem.mockImplementation(() => {
          throw new Error("quota");
        });
        return { kind: "failure", code, status: 409, retryAfter: 0 };
      });
      const flow = f.create();
      await flow.initialize();
      await flow.submit(name, idNumber);
      expect(flow.snapshot().kind).toBe("storage-write");
      await flow.confirm();
      expect(f.dependencies.register).toHaveBeenCalledTimes(1);
      f.storage.setItem.mockImplementation(write);
      await flow.retryLocal();
      expect(flow.snapshot()).toMatchObject({ kind: "rejected", reason: code });
      await flow.confirm();
      expect(f.dependencies.register).toHaveBeenCalledTimes(1);
      if (code !== "request_mismatch")
        expect(f.records.get(KEY)).not.toContain(idNumber);
    },
  );
  it("keeps a refusal known when storage recovers only after its credential expires", async () => {
    const f = fixture();
    const write = f.storage.setItem.getMockImplementation()!;
    f.dependencies.register.mockImplementation(async () => {
      f.storage.setItem.mockImplementation(() => {
        throw new Error("quota");
      });
      return {
        kind: "failure",
        code: "id_number_taken",
        status: 409,
        retryAfter: 0,
      };
    });
    const flow = f.create();
    await flow.initialize();
    await flow.submit(name, idNumber);
    f.advance(LIFETIME);
    f.storage.setItem.mockImplementation(write);
    await flow.retryLocal();
    expect(flow.snapshot()).toMatchObject({
      kind: "rejected",
      reason: "id_number_taken",
      editable: false,
    });
    expect(f.records.get(KEY)).not.toContain(registration.token);
    expect(f.records.get(KEY)).not.toContain(idNumber);
    expect(f.dependencies.register).toHaveBeenCalledTimes(1);
  });
  it("does not accept a late success over a later browser selection", async () => {
    const f = fixture();
    const answer = deferred<Result>();
    const started = deferred<void>();
    f.dependencies.register.mockImplementation(async () => {
      started.resolve();
      return answer.promise;
    });
    const flow = f.create();
    await flow.initialize();
    const work = flow.submit(name, idNumber);
    await started.promise;
    const newer: BrowserSession = {
      version: 1,
      attempt: null,
      revision: nextRevision,
      session: { ...session, id: "019354fd-933c-7e6a-8912-345678901299" },
    };
    f.select(newer);
    answer.resolve({ kind: "session", session });
    await work;
    expect(f.selection()).toEqual(newer);
    expect(f.dependencies.sessions.accept).not.toHaveBeenCalled();
    expect(flow.snapshot()).toEqual({ kind: "created" });
    expect(f.records.get(KEY)).not.toContain(idNumber);
    expect(f.records.get(receiptKey(operation))).toContain(session.id);
  });
  it("recognizes an already expired refusal after its save readback failed", async () => {
    const f = fixture();
    const write = f.storage.setItem.getMockImplementation()!;
    f.dependencies.register.mockImplementation(async () => {
      f.storage.setItem.mockImplementation((key, value) => {
        write(key, value);
        if (key === KEY && JSON.parse(value).kind === "refused")
          f.storage.getItem.mockImplementationOnce(() => {
            throw new Error("readback");
          });
      });
      return {
        kind: "failure",
        code: "mobile_taken",
        status: 409,
        retryAfter: 0,
      };
    });
    const flow = f.create();
    await flow.initialize();
    await flow.submit(name, idNumber);
    expect(flow.snapshot().kind).toBe("storage-write");
    f.advance(LIFETIME);
    f.storage.setItem.mockImplementation(write);
    expect(maintain(f.storage, f.dependencies.now())).toBe(true);
    await flow.retryLocal();
    expect(flow.snapshot()).toMatchObject({
      kind: "rejected",
      reason: "mobile_taken",
      editable: false,
    });
    expect(f.dependencies.register).toHaveBeenCalledTimes(1);
    expect(f.records.get(KEY)).not.toContain(registration.token);
  });
  it("keeps account creation known when cookie arrived but selection commit fails", async () => {
    const f = fixture();
    f.dependencies.register.mockResolvedValue({ kind: "session", session });
    f.dependencies.sessions.accept.mockRejectedValueOnce(
      new Error("IDB aborted"),
    );
    const flow = f.create();
    await flow.initialize();
    await flow.submit(name, idNumber);
    expect(flow.snapshot().kind).toBe("created-storage");
    expect(f.records.get(KEY)).not.toContain(idNumber);
    await flow.retryLocal();
    expect(flow.snapshot()).toMatchObject({
      kind: "created",
      selection: { id: session.id },
    });
    expect(f.dependencies.register).toHaveBeenCalledTimes(1);
  });
  it("retries local cleanup after success without submitting R2 again", async () => {
    const f = fixture();
    const original = f.storage.setItem.getMockImplementation()!;
    f.dependencies.register.mockImplementation(async () => {
      f.storage.setItem.mockImplementation(() => {
        throw new Error("quota");
      });
      return { kind: "session", session };
    });
    const flow = f.create();
    await flow.initialize();
    await flow.submit(name, idNumber);
    expect(flow.snapshot().kind).toBe("created-cleanup");
    f.storage.setItem.mockImplementation(original);
    await flow.retryLocal();
    expect(f.records.get(KEY)).not.toContain(idNumber);
    expect(flow.snapshot().kind).toBe("created");
    expect(f.dependencies.register).toHaveBeenCalledTimes(1);
  });
  it.each([LIFETIME, -1])(
    "closes secrets at deadline or rollback %i, with no new POST",
    async (elapsed) => {
      const f = fixture();
      const flow = f.create();
      await flow.initialize();
      await flow.submit(name, idNumber);
      f.advance(elapsed);
      flow.expire();
      expect(f.records.get(KEY)).not.toContain(idNumber);
      expect(f.records.get(KEY)).not.toContain(registration.token);
      expect(flow.snapshot().kind).toBe("expired");
      await flow.confirm();
      expect(f.dependencies.register).toHaveBeenCalledTimes(1);
    },
  );
  it("global maintenance retries denied expiry cleanup and preserves unrelated material", async () => {
    const f = fixture();
    const flow = f.create();
    await flow.initialize();
    await flow.submit(name, idNumber);
    f.records.set("unrelated", "preserve");
    f.advance(LIFETIME);
    const original = f.storage.setItem.getMockImplementation()!;
    f.storage.setItem.mockImplementation(() => {
      throw new Error("quota");
    });
    expect(maintain(f.storage, f.dependencies.now())).toBe(false);
    expect(f.records.get(KEY)).toContain(idNumber);
    f.storage.setItem.mockImplementation(original);
    expect(maintain(f.storage, f.dependencies.now())).toBe(true);
    expect(f.records.get(KEY)).not.toContain(idNumber);
    expect(f.records.get("unrelated")).toBe("preserve");
  });
  it("late success after local expiry preserves creation but cannot select a session", async () => {
    const f = fixture();
    const answer = deferred<Result>();
    const started = deferred<void>();
    f.dependencies.register.mockImplementation(async () => {
      started.resolve();
      return answer.promise;
    });
    const flow = f.create();
    await flow.initialize();
    const work = flow.submit(name, idNumber);
    await started.promise;
    f.advance(LIFETIME);
    flow.expire();
    answer.resolve({ kind: "session", session });
    await work;
    expect(f.dependencies.sessions.accept).not.toHaveBeenCalled();
    expect(f.records.get(KEY)).not.toContain(idNumber);
    expect(f.records.get(receiptKey(operation))).toContain(session.id);
    await flow.retryLocal();
    const reopened = f.create();
    await reopened.initialize();
    expect(reopened.snapshot()).toEqual({ kind: "created" });
    expect(f.dependencies.sessions.accept).not.toHaveBeenCalled();
  });
  it("does not replace a later registration form when an expired request finally succeeds", async () => {
    const f = fixture();
    const answer = deferred<Result>();
    const started = deferred<void>();
    f.dependencies.register.mockImplementationOnce(async () => {
      started.resolve();
      return answer.promise;
    });
    const flow = f.create();
    await flow.initialize();
    const work = flow.submit(name, idNumber);
    await started.promise;
    f.advance(LIFETIME);
    flow.expire();
    const later = { operation: nextRevision, revision: operation };
    f.select({
      version: 1,
      revision: later.revision,
      attempt: later.operation,
      session: null,
    });
    f.records.set(
      LOGIN_KEY,
      JSON.stringify({
        ...f.next,
        attempt: later,
        createdAt: f.dependencies.now(),
        expiresAt: f.dependencies.now() + 600_000,
        registration: {
          ...registration,
          expiresAt: new Date(f.dependencies.now() + 600_000).toISOString(),
        },
      }),
    );
    await flow.initialize();
    expect(flow.snapshot().kind).toBe("editing");
    answer.resolve({ kind: "session", session });
    await work;
    await flow.initialize();
    expect(flow.snapshot().kind).toBe("editing");
    expect(f.dependencies.sessions.accept).not.toHaveBeenCalled();
    f.dependencies.uuid.mockReturnValue("e6682805-5b42-437b-a2d7-3b1d7c9a2929");
    await flow.submit(name, idNumber);
    expect(f.dependencies.register).toHaveBeenCalledTimes(2);
    expect(parse(f.records.get(KEY)!)).toMatchObject({ attempt: later });
  });
});
