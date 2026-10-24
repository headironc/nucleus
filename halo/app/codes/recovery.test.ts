import { describe, it, expect, vi } from "vitest";
import { Recovery, STORAGE_KEY } from "./recovery";
import { expireMaterial } from "./material";
import type { Input, Result } from "../protocol/codes";

const original = "fdb0e5e6-2cd3-46c2-8ae3-3b4783beadbe";
const next = "560b94be-ee54-48b2-a84c-c7283802dd93";
const input = { mobile: "13900000000", token: "original-proof" };
const message = {
  id: "fde21b5b-9cc3-47f4-a324-0cd9ff1edee6",
  delivery: "sent" as const,
  expiresAt: "2035-01-01T00:05:00Z",
};

function fixture() {
  let now = Date.parse("2035-01-01T00:00:00Z");
  let marked = false;
  let blocked = false;
  const records = new Map<string, string>();
  const storage = {
    getItem: vi.fn((key: string) => records.get(key) ?? null),
    setItem: vi.fn((key: string, value: string) => {
      records.set(key, value);
    }),
    removeItem: vi.fn((key: string) => {
      records.delete(key);
    }),
  };
  const dependencies = {
    storage,
    now: () => now,
    uuid: vi.fn(() => original),
    marked: () => marked,
    blocked: () => blocked,
    mark: (value = false) => {
      marked = true;
      blocked = value;
    },
    current: vi.fn(async () => true),
    received: vi.fn(
      (_operation: string, _mobile: string, _message: unknown) => {},
    ),
    send: vi.fn(
      async (
        _operation: string,
        _input: Input,
        _signal: AbortSignal,
      ): Promise<Result> => ({ kind: "disconnected" }),
    ),
  };
  const recovery = new Recovery(dependencies);
  recovery.initialize();
  return {
    dependencies,
    recovery,
    records,
    advance: (milliseconds: number) => {
      now += milliseconds;
    },
  };
}

describe("C1 original operation recovery", () => {
  it("retains the original proof until the login handoff is saved", async () => {
    const { recovery, dependencies, records, advance } = fixture();
    dependencies.send.mockResolvedValue({ kind: "message", message });
    dependencies.received.mockImplementationOnce(() => {
      throw new Error("storage_full");
    });
    await recovery.submit(input);
    expect(recovery.state.kind).toBe("pending");
    expect(JSON.parse(records.get(STORAGE_KEY)!).input).toEqual(input);
    advance(5000);
    await recovery.confirm();
    expect(dependencies.received).toHaveBeenLastCalledWith(
      original,
      input.mobile,
      message,
    );
    expect(records.get(STORAGE_KEY)).not.toContain(input.token);
    expect(dependencies.send.mock.calls[1].slice(0, 2)).toEqual([
      original,
      input,
    ]);
  });

  it("marks a reentered operation before material can disappear", async () => {
    const { recovery, dependencies, records } = fixture();
    await recovery.submit(input);
    let marked = false;
    const navigation = {
      ...dependencies,
      marked: () => marked,
      mark: () => {
        marked = true;
      },
    };
    const reentered = new Recovery(navigation);
    reentered.initialize();
    expect(marked).toBe(true);
    records.clear();
    const refreshed = new Recovery(navigation);
    refreshed.initialize();
    expect(refreshed.state.kind).toBe("missing");
    expect(dependencies.send).toHaveBeenCalledTimes(1);
  });

  it.each(["{", '{"version":1,"input":{"token":"original-proof"}}'])(
    "keeps a non-secret missing-material marker after restoring %s",
    (bytes) => {
      const { dependencies, records } = fixture();
      records.set(STORAGE_KEY, bytes);
      const reentered = new Recovery(dependencies);
      reentered.initialize();
      expect(reentered.state.kind).toBe("missing");
      expect(JSON.parse(records.get(STORAGE_KEY)!)).toEqual({
        version: 1,
        closed: true,
      });
      expect(dependencies.marked()).toBe(true);
      const refreshed = new Recovery(dependencies);
      refreshed.initialize();
      expect(refreshed.state.kind).toBe("missing");
      records.clear();
      const returned = new Recovery(dependencies);
      returned.initialize();
      expect(returned.state.kind).toBe("missing");
      expect(dependencies.send).not.toHaveBeenCalled();
    },
  );

  it("retains the closed record when a corrupt restore cannot mark history", () => {
    const { dependencies, records } = fixture();
    records.set(STORAGE_KEY, "{");
    const reentered = new Recovery({
      ...dependencies,
      mark: () => {
        throw new Error("history unavailable");
      },
    });
    reentered.initialize();
    expect(reentered.state.kind).toBe("missing");
    expect(JSON.parse(records.get(STORAGE_KEY)!)).toEqual({
      version: 1,
      closed: true,
    });
    expect(dependencies.send).not.toHaveBeenCalled();
  });

  it("retains a valid restored proof when the history marker cannot be saved", async () => {
    const { recovery, dependencies, records, advance } = fixture();
    await recovery.submit(input);
    const saved = records.get(STORAGE_KEY);
    const reentered = new Recovery({
      ...dependencies,
      mark: () => {
        throw new Error("history unavailable");
      },
    });
    reentered.initialize();
    expect(reentered.state.kind).toBe("missing");
    expect(records.get(STORAGE_KEY)).toBe(saved);
    advance(30 * 60 * 1000);
    reentered.expire();
    expect(records.get(STORAGE_KEY)).not.toContain("original-proof");
    expect(dependencies.send).toHaveBeenCalledTimes(1);
  });

  it("saves and reads back exact material before calling the backend", async () => {
    const { recovery, dependencies, records } = fixture();
    dependencies.send.mockImplementation(async (operation, body) => {
      const saved = JSON.parse(records.get(STORAGE_KEY)!);
      expect(saved.operation).toBe(operation);
      expect(saved.input).toEqual(body);
      expect(dependencies.marked()).toBe(true);
      return { kind: "disconnected" };
    });
    await recovery.submit(input);
    expect(recovery.state.kind).toBe("pending");
  });

  it.each(["throws", "readback"])(
    "does not send after storage %s failure",
    async (failure) => {
      const { recovery, dependencies } = fixture();
      if (failure === "throws")
        dependencies.storage.setItem.mockImplementation(() => {
          throw new Error("full");
        });
      else dependencies.storage.getItem.mockReturnValue(null);
      await recovery.submit(input);
      expect(dependencies.send).not.toHaveBeenCalled();
      expect(recovery.state.kind).toBe("storage");
    },
  );

  it("refresh does not auto-send and manual recovery preserves proof", async () => {
    const { recovery, dependencies, advance } = fixture();
    await recovery.submit(input);
    const reopened = new Recovery(dependencies);
    reopened.initialize();
    expect(dependencies.send).toHaveBeenCalledTimes(1);
    expect(reopened.state.kind).toBe("pending");
    advance(5000);
    await reopened.confirm();
    expect(dependencies.send).toHaveBeenCalledTimes(2);
    for (const call of dependencies.send.mock.calls)
      expect(call.slice(0, 2)).toEqual([original, input]);
    expect(dependencies.uuid).toHaveBeenCalledTimes(1);
  });

  it("prevents replacement while pending, including changed inputs", async () => {
    const { recovery, dependencies } = fixture();
    await recovery.submit(input);
    dependencies.uuid.mockReturnValue(next);
    recovery.begin();
    await recovery.submit({ ...input, mobile: "13800000000" });
    expect(dependencies.send).toHaveBeenCalledTimes(1);
    expect(recovery.state.kind).toBe("pending");
  });

  it("does not accept a late reply after local cancellation", async () => {
    const { recovery, dependencies } = fixture();
    let finish!: (result: Result) => void;
    dependencies.send.mockImplementation(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    const waiting = recovery.submit(input);
    await vi.waitFor(() => expect(dependencies.send).toHaveBeenCalledTimes(1));
    recovery.suspend();
    finish({ kind: "message", message });
    await waiting;
    expect(recovery.state.kind).toBe("pending");
  });

  it("persists a new delay when the document becomes unusable after waiting", async () => {
    const { recovery, dependencies, records, advance } = fixture();
    await recovery.submit(input);
    advance(5000);
    recovery.reload();
    expect(recovery.state.kind).toBe("reload");
    expect(JSON.parse(records.get(STORAGE_KEY)!).retryAt).toBe(
      dependencies.now() + 5000,
    );
    const reopened = new Recovery(dependencies);
    reopened.initialize();
    advance(4999);
    await reopened.confirm();
    expect(dependencies.send).toHaveBeenCalledTimes(1);
    advance(1);
    await reopened.confirm();
    expect(dependencies.send).toHaveBeenCalledTimes(2);
    expect(dependencies.send.mock.calls[1].slice(0, 2)).toEqual([
      original,
      input,
    ]);
  });

  it("keeps the original proof when a late result follows a required reload", async () => {
    const { recovery, dependencies, records } = fixture();
    let finish!: (result: Result) => void;
    dependencies.send.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    const waiting = recovery.submit(input);
    await vi.waitFor(() => expect(dependencies.send).toHaveBeenCalledTimes(1));
    recovery.reload();
    finish({ kind: "message", message });
    await waiting;
    expect(recovery.state.kind).toBe("reload");
    expect(JSON.parse(records.get(STORAGE_KEY)!).input).toEqual(input);
  });

  it("persists cancellation backoff before manual or reopened confirmation", async () => {
    const { recovery, dependencies, records, advance } = fixture();
    let finish!: (result: Result) => void;
    dependencies.send.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    const waiting = recovery.submit(input);
    await vi.waitFor(() => expect(dependencies.send).toHaveBeenCalledTimes(1));
    recovery.suspend();
    finish({ kind: "disconnected" });
    await waiting;
    expect(JSON.parse(records.get(STORAGE_KEY)!).retryAt).toBe(
      dependencies.now() + 5000,
    );
    await recovery.confirm();
    expect(dependencies.send).toHaveBeenCalledTimes(1);
    const reopened = new Recovery(dependencies);
    reopened.initialize();
    advance(4999);
    await reopened.confirm();
    expect(dependencies.send).toHaveBeenCalledTimes(1);
    advance(1);
    await reopened.confirm();
    expect(dependencies.send).toHaveBeenCalledTimes(2);
    expect(dependencies.send.mock.calls[1].slice(0, 2)).toEqual([
      original,
      input,
    ]);
  });

  it.each(["reject", "outdated"])(
    "persists backoff after a %s build check",
    async (outcome) => {
      const { recovery, dependencies, records, advance } = fixture();
      if (outcome === "reject")
        dependencies.current.mockRejectedValueOnce(new Error("disconnected"));
      else dependencies.current.mockResolvedValueOnce(false);
      await recovery.submit(input);
      expect(recovery.state.kind).toBe(
        outcome === "reject" ? "pending" : "reload",
      );
      expect(JSON.parse(records.get(STORAGE_KEY)!).retryAt).toBe(
        dependencies.now() + 5000,
      );
      await recovery.confirm();
      expect(dependencies.current).toHaveBeenCalledTimes(1);
      const reopened = new Recovery(dependencies);
      reopened.initialize();
      advance(4999);
      await reopened.confirm();
      expect(dependencies.send).not.toHaveBeenCalled();
      advance(1);
      await reopened.confirm();
      expect(dependencies.send.mock.calls[0].slice(0, 2)).toEqual([
        original,
        input,
      ]);
    },
  );

  it.each(["reject", "outdated", "current"] as const)(
    "does not revive expired proof after a late %s build check",
    async (outcome) => {
      const { recovery, dependencies, records, advance } = fixture();
      let resolve!: (current: boolean) => void;
      let reject!: (error: Error) => void;
      dependencies.current.mockImplementationOnce(
        () =>
          new Promise((yes, no) => {
            resolve = yes;
            reject = no;
          }),
      );
      const waiting = recovery.submit(input);
      advance(30 * 60 * 1000);
      recovery.expire();
      expect(recovery.state.kind).toBe("expired");
      if (outcome === "reject") reject(new Error("disconnected"));
      else resolve(outcome === "current");
      await waiting;
      expect(recovery.state.kind).toBe("expired");
      expect(records.get(STORAGE_KEY)).not.toContain("original-proof");
      await recovery.confirm();
      expect(dependencies.send).not.toHaveBeenCalled();
    },
  );

  it("keeps a longer wait when the page is suspended again", async () => {
    const { recovery, dependencies, records, advance } = fixture();
    dependencies.send.mockResolvedValue({
      kind: "failure",
      code: "rate_limited",
      retryAfter: 20_000,
    });
    await recovery.submit(input);
    const deadline = JSON.parse(records.get(STORAGE_KEY)!).retryAt;
    advance(1000);
    recovery.suspend();
    expect(JSON.parse(records.get(STORAGE_KEY)!).retryAt).toBe(deadline);
    advance(18_999);
    await recovery.confirm();
    expect(dependencies.send).toHaveBeenCalledTimes(1);
  });

  it.each(["request_mismatch", "authentication_invalid"] as const)(
    "retains original material after %s without offering a new operation",
    async (code) => {
      const { recovery, dependencies, records, advance } = fixture();
      dependencies.send.mockResolvedValue({
        kind: "failure",
        code,
        retryAfter: 0,
      });
      await recovery.submit(input);
      const proof = JSON.parse(records.get(STORAGE_KEY)!);
      expect(proof.operation).toBe(original);
      expect(proof.input).toEqual(input);
      const reopened = new Recovery(dependencies);
      reopened.initialize();
      expect(reopened.state).toEqual({
        kind: "rejected",
        reason: code,
        uncertain: true,
      });
      reopened.begin();
      await reopened.confirm();
      expect(dependencies.send).toHaveBeenCalledTimes(1);
      expect(dependencies.uuid).toHaveBeenCalledTimes(1);
      advance(30 * 60 * 1000);
      reopened.expire();
      expect(records.get(STORAGE_KEY)).not.toContain("original-proof");
    },
  );

  it("ends a timed-out wait without retrying or replacing the original operation", async () => {
    vi.useFakeTimers();
    try {
      const { recovery, dependencies, records, advance } = fixture();
      dependencies.send.mockImplementation(
        (_operation, _input, signal) =>
          new Promise((resolve) => {
            signal.addEventListener(
              "abort",
              () => resolve({ kind: "disconnected" }),
              { once: true },
            );
          }),
      );
      const waiting = recovery.submit(input);
      await vi.advanceTimersByTimeAsync(34_999);
      expect(recovery.state.kind).toBe("submitting");
      expect(dependencies.send).toHaveBeenCalledTimes(1);
      advance(35_000);
      await vi.advanceTimersByTimeAsync(1);
      await waiting;
      expect(recovery.state.kind).toBe("pending");
      expect(dependencies.send).toHaveBeenCalledTimes(1);
      expect(JSON.parse(records.get(STORAGE_KEY)!).input).toEqual(input);
      advance(5000);
      dependencies.send.mockResolvedValue({ kind: "message", message });
      await recovery.confirm();
      expect(dependencies.send).toHaveBeenCalledTimes(2);
      for (const call of dependencies.send.mock.calls)
        expect(call.slice(0, 2)).toEqual([original, input]);
    } finally {
      vi.useRealTimers();
    }
  });

  it("expires at the deadline, removes proof and never guesses failure", async () => {
    const { recovery, dependencies, records, advance } = fixture();
    await recovery.submit(input);
    advance(30 * 60 * 1000);
    await recovery.confirm();
    expect(recovery.state.kind).toBe("expired");
    expect(records.get(STORAGE_KEY)).not.toContain("original-proof");
    expect(dependencies.send).toHaveBeenCalledTimes(1);
  });

  it("closes in-memory recovery after expiry even when storage cleanup must retry", async () => {
    const { recovery, dependencies, records, advance } = fixture();
    await recovery.submit(input);
    const before = records.get(STORAGE_KEY);
    dependencies.storage.setItem.mockImplementationOnce(() => {
      throw new Error("temporarily denied");
    });
    advance(30 * 60 * 1000);
    recovery.expire();
    expect(recovery.state.kind).toBe("expired");
    expect(records.get(STORAGE_KEY)).toBe(before);
    expect(dependencies.storage.removeItem).not.toHaveBeenCalled();
    await recovery.confirm();
    expect(dependencies.send).toHaveBeenCalledTimes(1);
    expireMaterial(dependencies.storage, dependencies.now());
    expect(records.get(STORAGE_KEY)).not.toContain("original-proof");
  });

  it("cannot close a later operation using an earlier controller's deadline", async () => {
    const { recovery, dependencies, records, advance } = fixture();
    await recovery.submit(input);
    advance(30 * 60 * 1000);
    const later = {
      ...JSON.parse(records.get(STORAGE_KEY)!),
      operation: next,
      createdAt: dependencies.now(),
      expiresAt: dependencies.now() + 30 * 60 * 1000,
    };
    records.set(STORAGE_KEY, JSON.stringify(later));
    recovery.expire();
    expect(recovery.state.kind).toBe("expired");
    expect(records.get(STORAGE_KEY)).toBe(JSON.stringify(later));
    expect(dependencies.send).toHaveBeenCalledTimes(1);
  });

  it("clears secrets for confirmed terminal results", async () => {
    const { recovery, dependencies, records } = fixture();
    dependencies.send.mockResolvedValue({ kind: "message", message });
    await recovery.submit(input);
    expect(recovery.state).toEqual({ kind: "message", message });
    expect(records.get(STORAGE_KEY)).not.toContain("original-proof");
    const reopened = new Recovery(dependencies);
    reopened.initialize();
    expect(reopened.state).toEqual({
      kind: "message",
      message: { delivery: "sent" },
    });
  });

  it("keeps sending status recoverable and narrows the deadline", async () => {
    const { recovery, dependencies, records, advance } = fixture();
    dependencies.send.mockResolvedValue({
      kind: "message",
      message: { ...message, delivery: "sending" },
    });
    await recovery.submit(input);
    expect(recovery.state.kind).toBe("sending");
    expect(JSON.parse(records.get(STORAGE_KEY)!).expiresAt).toBe(
      Date.parse(message.expiresAt),
    );
    advance(5 * 60 * 1000);
    await recovery.confirm();
    expect(recovery.state.kind).toBe("expired");
    expect(dependencies.send).toHaveBeenCalledTimes(1);
  });

  it("respects Retry-After across refresh using the same operation", async () => {
    const { recovery, dependencies, advance } = fixture();
    dependencies.send.mockResolvedValue({
      kind: "failure",
      code: "rate_limited",
      retryAfter: 10_000,
    });
    await recovery.submit(input);
    const reopened = new Recovery(dependencies);
    reopened.initialize();
    advance(9999);
    await reopened.confirm();
    expect(dependencies.send).toHaveBeenCalledTimes(1);
    advance(1);
    await reopened.confirm();
    expect(dependencies.send).toHaveBeenCalledTimes(2);
  });

  it("preserves untrusted material for reload and stops new actions", async () => {
    const { recovery, dependencies, records } = fixture();
    dependencies.send.mockResolvedValue({ kind: "untrusted" });
    await recovery.submit(input);
    expect(recovery.state.kind).toBe("reload");
    expect(records.get(STORAGE_KEY)).toContain("original-proof");
    recovery.begin();
    expect(dependencies.uuid).toHaveBeenCalledTimes(1);
  });

  it("creates new widget context only after an explicit new verification", async () => {
    const { recovery, dependencies } = fixture();
    dependencies.send.mockResolvedValue({
      kind: "failure",
      code: "verification_invalid",
      retryAfter: 0,
    });
    await recovery.submit(input);
    expect(recovery.state.kind).toBe("rejected");
    dependencies.uuid.mockReturnValue(next);
    recovery.begin();
    expect(recovery.state).toEqual({ kind: "editing", operation: next });
  });

  it.each(["invalid_input", "permission_denied"] as const)(
    "allows an explicit new operation after a first-attempt %s rejection",
    async (code) => {
      const { recovery, dependencies, records } = fixture();
      dependencies.send.mockResolvedValue({
        kind: "failure",
        code,
        retryAfter: 0,
      });
      await recovery.submit(input);
      expect(recovery.state).toEqual({ kind: "rejected", reason: code });
      expect(records.get(STORAGE_KEY)).not.toContain("original-proof");
      const reopened = new Recovery(dependencies);
      reopened.initialize();
      expect(reopened.state).toEqual({ kind: "rejected", reason: code });
      expect(dependencies.send).toHaveBeenCalledTimes(1);
      dependencies.uuid.mockReturnValue(next);
      reopened.begin();
      expect(reopened.state).toEqual({ kind: "editing", operation: next });
    },
  );

  it.each(["invalid_input", "permission_denied"] as const)(
    "preserves a never-sent request through reload before a first %s rejection",
    async (code) => {
      const { recovery, dependencies, advance } = fixture();
      dependencies.current.mockResolvedValueOnce(false);
      await recovery.submit(input);
      expect(dependencies.send).not.toHaveBeenCalled();
      const reopened = new Recovery(dependencies);
      reopened.initialize();
      advance(5000);
      dependencies.send.mockResolvedValue({
        kind: "failure",
        code,
        retryAfter: 0,
      });
      await reopened.confirm();
      expect(reopened.state).toEqual({ kind: "rejected", reason: code });
      dependencies.uuid.mockReturnValue(next);
      reopened.begin();
      expect(reopened.state).toEqual({ kind: "editing", operation: next });
    },
  );

  it("does not send if the attempt marker cannot be saved", async () => {
    const { recovery, dependencies } = fixture();
    dependencies.current.mockImplementation(async () => {
      dependencies.storage.setItem.mockImplementation(() => {
        throw new Error("storage_unavailable");
      });
      return true;
    });
    await recovery.submit(input);
    expect(dependencies.send).not.toHaveBeenCalled();
    expect(recovery.state.kind).toBe("storage");
  });

  it("treats a stored request without an attempt marker as possibly sent", async () => {
    const { recovery, dependencies, records, advance } = fixture();
    await recovery.submit(input);
    const proof = JSON.parse(records.get(STORAGE_KEY)!);
    delete proof.attempted;
    records.set(STORAGE_KEY, JSON.stringify(proof));
    const reopened = new Recovery(dependencies);
    reopened.initialize();
    advance(5000);
    dependencies.send.mockResolvedValue({
      kind: "failure",
      code: "permission_denied",
      retryAfter: 0,
    });
    await reopened.confirm();
    expect(reopened.state).toEqual({
      kind: "rejected",
      reason: "permission_denied",
      uncertain: true,
    });
    reopened.begin();
    expect(dependencies.uuid).toHaveBeenCalledTimes(1);
  });

  it.each(["invalid_input", "permission_denied"] as const)(
    "does not replace an uncertain original after a replay %s rejection",
    async (code) => {
      const { recovery, dependencies, records, advance } = fixture();
      await recovery.submit(input);
      dependencies.send.mockResolvedValue({
        kind: "failure",
        code,
        retryAfter: 0,
      });
      advance(5000);
      await recovery.confirm();
      expect(recovery.state).toEqual({
        kind: "rejected",
        reason: code,
        uncertain: true,
      });
      expect(JSON.parse(records.get(STORAGE_KEY)!).input).toEqual(input);
      const reopened = new Recovery(dependencies);
      reopened.initialize();
      expect(reopened.state).toEqual(recovery.state);
      reopened.begin();
      await reopened.confirm();
      expect(dependencies.uuid).toHaveBeenCalledTimes(1);
      expect(dependencies.send).toHaveBeenCalledTimes(2);
    },
  );

  it("stops on unavailable secure randomness or a version change", async () => {
    const { recovery, dependencies } = fixture();
    dependencies.current.mockResolvedValue(false);
    await recovery.submit(input);
    expect(dependencies.send).not.toHaveBeenCalled();
    expect(recovery.state.kind).toBe("reload");
    const separate = fixture();
    separate.dependencies.uuid.mockImplementation(() => {
      throw new Error("unavailable");
    });
    separate.recovery.begin();
    expect(separate.recovery.state.kind).toBe("random");
  });
});
