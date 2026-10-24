import { afterEach, describe, expect, it, vi } from "vitest";
import {
  sameSelection,
  type BrowserSession,
  type Selection,
} from "../authentication/session";
import type { CurrentResult } from "../protocol/session";
import type { CancelResult, Input } from "../protocol/cancellation";
import {
  Cancellation,
  hasPendingResult,
  leaveKind,
  type Marker,
  type State,
} from "./controller";
import { KEY, type Material, type Store } from "./material";

const sessionId = "f9bd2899-fb4d-4212-a831-15168b01e783";
const revision = "4a9b6c1d-6f5a-4182-9d2e-3f4a5b6c7d8e";
const accountId = "0b6e3f8a-3c2d-4e5f-8a9b-0c1d2e3f4a5b";
const personId = "1c7f4a9b-4d3e-4f60-9b0c-1d2e3f4a5b6c";
const revisionId = "2d8a5b0c-5e4f-4071-8c1d-2e3f4a5b6c7d";
const result = {
  id: "9e2f1a3b-6c7d-4e8f-9a0b-1c2d3e4f5a6b",
  cancelledAt: "2026-10-25T12:34:56Z",
};
const account = {
  id: accountId,
  personId,
  revisionId,
  name: "Fixture Operator",
  idNumber: { suffix: "002X" },
  avatar: {
    revision: "8e7d6c5b-4a39-4281-8f70-6e5d4c3b2a19",
    href: `https://fixture.invalid/api/v1/avatars/${"a".repeat(64)}.webp?key_id=7&expires_at=2051222400&signature=${"A".repeat(42)}B`,
    expiresAt: "2035-01-01T00:00:00Z",
    mediaType: "image/webp",
    width: 512,
    height: 512,
  },
  binding: null,
};
const ready: CurrentResult = { kind: "account", account };

function browser(id = sessionId, value = revision): BrowserSession {
  return {
    version: 1,
    revision: value,
    session: { id, expiresAt: "2035-01-01T00:00:00Z" },
    attempt: null,
  };
}

class FakeSessions {
  value: BrowserSession | null;
  failRead = false;
  failClear = false;
  clears: Selection[] = [];
  reads = 0;
  constructor(value: BrowserSession | null = browser()) {
    this.value = value;
  }
  async read() {
    this.reads++;
    if (this.failRead) throw new Error("session storage unavailable");
    return this.value ? structuredClone(this.value) : null;
  }
  async clear(expected: Selection) {
    return (await this.release(expected)) !== null;
  }
  async release(expected: Selection) {
    if (this.failClear) throw new Error("session storage unavailable");
    this.clears.push(expected);
    if (!sameSelection(this.value, expected)) return null;
    this.value = {
      version: 1,
      revision: crypto.randomUUID(),
      session: null,
      attempt: null,
    };
    return this.value.revision;
  }
}

function memory(): Store & { values: Map<string, string> } {
  const values = new Map<string, string>();
  return {
    values,
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => {
      values.set(key, value);
    },
    removeItem: (key: string) => {
      values.delete(key);
    },
  };
}

function prepared(operation = crypto.randomUUID(), value = revision): Material {
  return {
    version: 1,
    operation,
    target: { accountId, personId, revisionId },
    selection: { id: sessionId, revision: value },
    phase: "prepared",
    outcome: "pending",
    notice: "pending",
    retryAt: 0,
    result: null,
    hidden: false,
    clearedRevision: null,
  };
}

function possibly(operation = crypto.randomUUID(), value = revision): Material {
  return { ...prepared(operation, value), phase: "possibly-sent" };
}

type Options = {
  build?: () => Promise<boolean>;
  storage?: ReturnType<typeof memory>;
  sessions?: FakeSessions;
  marker?: Marker;
  markThrows?: boolean;
  cancel?: (
    operation: string,
    session: string,
    input: Input,
    signal: AbortSignal,
  ) => Promise<CancelResult>;
};

function setup(options: Options = {}) {
  const storage = options.storage ?? memory();
  const sessions = options.sessions ?? new FakeSessions();
  let currentResult: CurrentResult = ready;
  let marker: Marker = options.marker ?? null;
  const current = vi.fn(async () => currentResult);
  const cancel = vi.fn(
    options.cancel ??
      (async (
        _operation: string,
        _session: string,
        _input: Input,
        _signal: AbortSignal,
      ): Promise<CancelResult> => ({ kind: "cancelled", result })),
  );
  const mark = vi.fn((value?: Marker) => {
    if (options.markThrows) throw new Error("history unavailable");
    marker = value ?? null;
  });
  const clock = { time: 1_000_000 };
  const controller = new Cancellation({
    storage,
    sessions,
    build: options.build ?? (async () => true),
    current,
    cancel,
    now: () => clock.time,
    uuid: () => crypto.randomUUID(),
    marker: () => marker,
    mark,
  });
  return {
    controller,
    storage,
    sessions,
    current,
    cancel,
    mark,
    clock,
    setCurrent(value: CurrentResult) {
      currentResult = value;
    },
    getMarker() {
      return marker;
    },
    setMarker(value: Marker) {
      marker = value;
    },
    saved(): Material | "missing" | null {
      const bytes = storage.values.get(KEY);
      return bytes === undefined ? null : (JSON.parse(bytes) as Material);
    },
  };
}

async function entry(options: Options = {}) {
  const value = setup(options);
  await value.controller.verify();
  expect(value.controller.snapshot()).toMatchObject({ kind: "entry" });
  return value;
}

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("cancellation controller", () => {
  it("shows the trusted current account before any original attempt", async () => {
    const { controller, saved, cancel } = await entry();
    expect(controller.snapshot()).toMatchObject({
      kind: "entry",
      account: { id: accountId },
      selection: { id: sessionId, revision },
    });
    expect(saved()).toBeNull();
    expect(cancel).not.toHaveBeenCalled();
  });

  it("durably stores prepared then possibly-sent and the marker before one POST", async () => {
    let observed: {
      marker: Marker;
      saved: Material | "missing" | null;
    } | null = null;
    const value = await entry({
      cancel: async (_operation, _session, _input, _signal) => {
        observed = {
          marker: value.getMarker(),
          saved: value.saved(),
        };
        return { kind: "cancelled", result };
      },
    });
    await value.controller.submit();
    expect(value.cancel).toHaveBeenCalledOnce();
    const [operation, session, input] = value.cancel.mock.calls[0];
    expect(operation).toMatch(
      /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
    );
    expect(session).toBe(sessionId);
    expect(input).toEqual({ accountId, personId, revisionId });
    expect(observed).not.toBeNull();
    expect(observed!.marker).toBe("submitted");
    expect(observed!.saved).toMatchObject({
      phase: "possibly-sent",
      outcome: "pending",
      operation,
      selection: { id: sessionId, revision },
      target: { accountId, personId, revisionId },
    });
    expect(value.saved()).toMatchObject({
      outcome: "complete",
      result,
      hidden: false,
    });
    expect(value.sessions.value?.session).toBeNull();
    expect(value.sessions.clears).toEqual([{ id: sessionId, revision }]);
    expect(value.controller.snapshot()).toEqual({
      kind: "success",
      result,
      persisted: true,
    });
  });

  it("disables duplicate submission while the first POST is pending", async () => {
    let release!: (value: CancelResult) => void;
    const gate = new Promise<CancelResult>((resolve) => {
      release = resolve;
    });
    const value = await entry({ cancel: () => gate });
    const first = value.controller.submit();
    await vi.waitFor(() => expect(value.cancel).toHaveBeenCalledOnce());
    expect(value.controller.snapshot().kind).toBe("submitting");
    await value.controller.submit();
    expect(value.cancel).toHaveBeenCalledOnce();
    release({ kind: "cancelled", result });
    await first;
    expect(value.controller.snapshot().kind).toBe("success");
  });

  it("never POSTs when the prepared record or its readback fails", async () => {
    const storage = memory();
    let writes = 0;
    const failing: Store = {
      getItem: storage.getItem,
      setItem: (key, value) => {
        writes++;
        if (writes === 1) return;
        storage.setItem(key, value);
      },
      removeItem: storage.removeItem,
    };
    const value = await entry({
      storage: failing as ReturnType<typeof memory>,
    });
    await value.controller.submit();
    expect(value.cancel).not.toHaveBeenCalled();
    expect(value.controller.snapshot()).toEqual({ kind: "unsent" });
    expect(value.getMarker()).toMatchObject({ preparing: expect.any(String) });
  });

  it("never POSTs when the history marker cannot be written or read back", async () => {
    const thrown = await entry({ markThrows: true });
    await thrown.controller.submit();
    expect(thrown.cancel).not.toHaveBeenCalled();
    expect(thrown.controller.snapshot()).toEqual({ kind: "unsent" });
    expect(thrown.saved()).toBeNull();

    const silent = setup();
    // A marker that silently fails to apply is not a verified marker.
    silent.mark.mockImplementation(() => {});
    await silent.controller.verify();
    expect(silent.controller.snapshot().kind).toBe("entry");
    await silent.controller.submit();
    expect(silent.cancel).not.toHaveBeenCalled();
    expect(silent.controller.snapshot()).toEqual({ kind: "unsent" });
  });

  it("reloads an unsent prepared record without POSTing and can return to confirmation", async () => {
    const operation = crypto.randomUUID();
    const storage = memory();
    storage.values.set(KEY, JSON.stringify(prepared(operation)));
    const value = setup({
      storage,
      marker: { preparing: operation },
    });
    await value.controller.verify();
    expect(value.controller.snapshot()).toEqual({ kind: "unsent" });
    expect(value.cancel).not.toHaveBeenCalled();
    await value.controller.start();
    expect(value.controller.snapshot()).toMatchObject({ kind: "entry" });
    expect(storage.values.has(KEY)).toBe(false);
    expect(value.getMarker()).toBeNull();
    expect(value.cancel).not.toHaveBeenCalled();
  });

  it("keeps a possibly-sent record unknown across reloads and checks only GET /current", async () => {
    const storage = memory();
    const operation = crypto.randomUUID();
    storage.values.set(KEY, JSON.stringify(possibly(operation)));
    const value = setup({ storage, marker: "submitted" });
    await value.controller.verify();
    expect(value.controller.snapshot()).toMatchObject({
      kind: "unknown",
      notice: "unknown",
    });
    expect(value.cancel).not.toHaveBeenCalled();
    const currentCalls = value.current.mock.calls.length;
    await value.controller.check();
    expect(value.controller.snapshot()).toMatchObject({
      kind: "unknown",
      notice: "unknown",
    });
    expect(value.current.mock.calls.length).toBe(currentCalls + 1);
    expect(value.cancel).not.toHaveBeenCalled();
    expect(storage.values.get(KEY)).toBe(JSON.stringify(possibly(operation)));
  });

  it.each([
    ["missing with a marker", null, "submitted"],
    ["corrupt with a marker", "{", "submitted"],
    ["missing with a preparation marker", null, "preparation"],
  ] as const)(
    "turns %s into a non-retryable unknown tombstone",
    async (_name, bytes, markerKind) => {
      const operation = crypto.randomUUID();
      const storage = memory();
      if (bytes !== null) storage.values.set(KEY, bytes);
      const marker: Marker =
        markerKind === "submitted" ? "submitted" : { preparing: operation };
      const value = setup({ storage, marker });
      await value.controller.verify();
      expect(value.controller.snapshot()).toMatchObject({
        kind: "unknown",
        notice: "missing",
      });
      expect(JSON.parse(storage.values.get(KEY)!)).toBe("missing");
      expect(value.cancel).not.toHaveBeenCalled();
      await value.controller.start();
      expect(value.controller.snapshot().kind).toBe("unknown");
      expect(value.cancel).not.toHaveBeenCalled();
    },
  );

  it("classifies the first trusted 429 as a definite refusal with a bounded wait", async () => {
    const value = await entry({
      cancel: async () => ({
        kind: "failure",
        status: 429,
        code: "rate_limited",
        retryAfter: 1_000,
      }),
    });
    await value.controller.submit();
    expect(value.controller.snapshot()).toEqual({
      kind: "refused",
      notice: "limited",
      retryAt: 1_005_000,
      canRestart: false,
    });
    expect(value.saved()).toMatchObject({
      outcome: "refused",
      notice: "limited",
      retryAt: 1_005_000,
    });
    await value.controller.start();
    expect(value.controller.snapshot().kind).toBe("refused");
    expect(value.cancel).toHaveBeenCalledOnce();
    value.clock.time = 1_005_000;
    await value.controller.start();
    expect(value.controller.snapshot().kind).toBe("entry");
    expect(value.cancel).toHaveBeenCalledOnce();
  });

  it.each([
    ["target_changed", "target", "target"],
    ["invalid_input", "invalid", "invalid"],
    ["dependency_unavailable", "dependency", "dependency"],
  ] as const)(
    "treats %s as a refusal and requires a fresh confirmation",
    async (code, notice, expectedNotice) => {
      const value = await entry({
        cancel: async () => ({
          kind: "failure",
          status:
            code === "target_changed"
              ? 409
              : code === "invalid_input"
                ? 422
                : 503,
          code,
          retryAfter: 0,
        }),
      });
      await value.controller.submit();
      expect(value.controller.snapshot()).toMatchObject({
        kind: "refused",
        notice: expectedNotice,
        canRestart: true,
      });
      expect(value.saved()).toMatchObject({
        outcome: "refused",
        notice: expectedNotice,
        target: { accountId, personId, revisionId },
      });
      const calls = value.cancel.mock.calls.length;
      await value.controller.start();
      expect(value.controller.snapshot().kind).toBe("entry");
      expect(value.cancel.mock.calls.length).toBe(calls);
    },
  );

  it("clears only the original selection on a trusted 401 and keeps a later login", async () => {
    const value = await entry({
      cancel: async () => ({
        kind: "failure",
        status: 401,
        code: "authentication_invalid",
        retryAfter: 0,
      }),
    });
    await value.controller.submit();
    expect(value.controller.snapshot()).toMatchObject({
      kind: "refused",
      notice: "authentication",
      canRestart: false,
    });
    expect(value.sessions.clears).toEqual([{ id: sessionId, revision }]);
    expect(value.sessions.value?.session).toBeNull();
    // A later login is outside this original selection and remains intact.
    value.sessions.value = browser(
      "6b1c2d3e-4f50-4612-9a7b-8c9d0e1f2a3b",
      "7c2d3e4f-5061-4723-8b8c-9d0e1f2a3b4c",
    );
    await value.controller.verify();
    expect(value.controller.snapshot()).toMatchObject({
      kind: "hidden",
    });
    expect(value.sessions.value?.session?.id).toBe(
      "6b1c2d3e-4f50-4612-9a7b-8c9d0e1f2a3b",
    );
  });

  it("does not clear a session on 403", async () => {
    const value = await entry({
      cancel: async () => ({
        kind: "failure",
        status: 403,
        code: "permission_denied",
        retryAfter: 0,
      }),
    });
    await value.controller.submit();
    expect(value.controller.snapshot()).toMatchObject({
      kind: "refused",
      notice: "denied",
      canRestart: false,
    });
    expect(value.sessions.clears).toEqual([]);
    expect(value.sessions.value?.session?.id).toBe(sessionId);
  });

  it.each([
    [
      { kind: "failure", status: 500, code: "internal_error", retryAfter: 0 },
      "unknown",
    ],
    [
      { kind: "failure", status: 503, code: "outcome_unknown", retryAfter: 0 },
      "unknown",
    ],
    [
      { kind: "failure", status: 409, code: "request_mismatch", retryAfter: 0 },
      "mismatch",
    ],
    [{ kind: "untrusted" }, "unknown"],
    [{ kind: "disconnected" }, "unknown"],
  ] as const)(
    "treats %j as unknown without resubmission or session clearing",
    async (outcome, notice) => {
      const value = await entry({ cancel: async () => outcome });
      await value.controller.submit();
      expect(value.controller.snapshot()).toMatchObject({
        kind: "unknown",
        notice,
        persisted: true,
      });
      expect(value.sessions.clears).toEqual([]);
      expect(value.sessions.value?.session?.id).toBe(sessionId);
      await value.controller.start();
      await value.controller.submit();
      expect(value.cancel).toHaveBeenCalledOnce();
      expect(value.sessions.clears).toEqual([]);
      expect(value.controller.snapshot().kind).toBe("unknown");
    },
  );

  it("drops a late trusted success after the local wait was suspended", async () => {
    let release!: (value: CancelResult) => void;
    const gate = new Promise<CancelResult>((resolve) => {
      release = resolve;
    });
    const value = await entry({ cancel: () => gate });
    const pending = value.controller.submit();
    await vi.waitFor(() => expect(value.cancel).toHaveBeenCalledOnce());
    value.controller.suspend();
    release({ kind: "cancelled", result });
    await pending;
    expect(value.sessions.clears).toEqual([]);
    expect(value.sessions.value?.session?.id).toBe(sessionId);
    await value.controller.verify();
    expect(value.controller.snapshot()).toMatchObject({
      kind: "unknown",
      notice: "unknown",
    });
    expect(value.cancel).toHaveBeenCalledOnce();
  });

  it("hides a trusted success from a different selection and never resurrects it", async () => {
    let release!: (value: CancelResult) => void;
    const gate = new Promise<CancelResult>((resolve) => {
      release = resolve;
    });
    const value = await entry({ cancel: () => gate });
    const pending = value.controller.submit();
    await vi.waitFor(() => expect(value.cancel).toHaveBeenCalledOnce());
    const later = browser(
      "6b1c2d3e-4f50-4612-9a7b-8c9d0e1f2a3b",
      "7c2d3e4f-5061-4723-8b8c-9d0e1f2a3b4c",
    );
    value.sessions.value = later;
    release({ kind: "cancelled", result });
    await pending;
    expect(value.controller.snapshot()).toMatchObject({
      kind: "hidden",
      current: { kind: "changed" },
    });
    expect(value.sessions.clears).toEqual([]);
    expect(value.sessions.value?.session?.id).toBe(later.session!.id);
    expect(value.saved()).toMatchObject({
      outcome: "complete",
      hidden: true,
      result,
    });
    // The original selection is gone; an empty later selection must not
    // resurrect the old result either.
    value.sessions.value = null;
    await value.controller.verify();
    expect(value.controller.snapshot().kind).toBe("hidden");
  });

  it("preserves but conceals a trusted success when the selection read fails", async () => {
    let release!: (value: CancelResult) => void;
    const gate = new Promise<CancelResult>((resolve) => {
      release = resolve;
    });
    const value = await entry({ cancel: () => gate });
    const pending = value.controller.submit();
    await vi.waitFor(() => expect(value.cancel).toHaveBeenCalledOnce());
    value.sessions.failRead = true;
    release({ kind: "cancelled", result });
    await pending;
    expect(value.controller.snapshot()).toMatchObject({ kind: "hidden" });
    expect(value.saved()).toMatchObject({ outcome: "complete", hidden: false });
  });

  it.each(["success", "unknown", "refused"] as const)(
    "conceals %s synchronously and does not revive it after an intervening login attempt",
    async (kind) => {
      const value = await entry({
        cancel: async () =>
          kind === "success"
            ? { kind: "cancelled", result }
            : kind === "unknown"
              ? { kind: "disconnected" }
              : {
                  kind: "failure",
                  status: 403,
                  code: "permission_denied",
                  retryAfter: 0,
                },
      });
      await value.controller.submit();
      expect(value.controller.snapshot().kind).toBe(kind);
      value.controller.suspend();
      expect(value.controller.snapshot().kind).toBe("hidden");
      expect(leaveKind(value.controller.snapshot())).toBeNull();
      value.sessions.value = {
        version: 1,
        revision: crypto.randomUUID(),
        session: null,
        attempt: crypto.randomUUID(),
      };
      await value.controller.verify();
      expect(value.controller.snapshot().kind).toBe("hidden");
      expect(value.saved()).toMatchObject({ hidden: true });
      value.sessions.value = browser();
      await value.controller.verify();
      expect(value.controller.snapshot().kind).toBe("hidden");
      expect(value.cancel).toHaveBeenCalledOnce();
    },
  );

  it("restores known success only through the exact revision created by its own clear", async () => {
    const value = await entry();
    await value.controller.submit();
    value.controller.close();
    const restored = setup({
      storage: value.storage,
      sessions: value.sessions,
      marker: "submitted",
    });
    await restored.controller.verify();
    expect(restored.controller.snapshot().kind).toBe("success");
    restored.sessions.value = {
      ...restored.sessions.value!,
      revision: crypto.randomUUID(),
    };
    await restored.controller.verify();
    expect(restored.controller.snapshot().kind).toBe("hidden");
    expect(restored.cancel).not.toHaveBeenCalled();
  });

  it("keeps an unknown result unknown after a separate current 401 clears its original selection", async () => {
    const value = await entry({
      cancel: async () => ({ kind: "disconnected" }),
    });
    await value.controller.submit();
    value.setCurrent({
      kind: "failure",
      status: 401,
      code: "authentication_invalid",
      retryAfter: 0,
    });
    await value.controller.check();
    expect(value.controller.snapshot().kind).toBe("unknown");
    expect(value.controller.checkInfo()).toMatchObject({
      visible: true,
      current: { kind: "expired" },
    });
    expect(value.saved()).toMatchObject({
      outcome: "pending",
      clearedRevision: value.sessions.value!.revision,
    });
    await value.controller.verify();
    expect(value.controller.snapshot().kind).toBe("unknown");
    expect(value.cancel).toHaveBeenCalledOnce();
  });

  it("conceals an unknown result while a separate current check awaits a response", async () => {
    const value = await entry({
      cancel: async () => ({ kind: "disconnected" }),
    });
    await value.controller.submit();
    let resolve!: (value: CurrentResult) => void;
    value.current.mockImplementationOnce(
      () =>
        new Promise((done) => {
          resolve = done;
        }),
    );
    const checking = value.controller.check();
    expect(value.controller.snapshot().kind).toBe("hidden");
    await vi.waitFor(() => expect(resolve).toBeDefined());
    value.sessions.value = browser(crypto.randomUUID(), crypto.randomUUID());
    resolve(ready);
    await checking;
    expect(value.controller.snapshot().kind).toBe("hidden");
    expect(value.controller.checkInfo().visible).toBe(false);
    expect(value.cancel).toHaveBeenCalledOnce();
  });

  it("shows definitely unsent when the last durable write fails before any POST", async () => {
    const storage = memory();
    const setItem = storage.setItem;
    let writes = 0;
    storage.setItem = (key, bytes) => {
      if (++writes === 2) throw new Error("quota");
      setItem(key, bytes);
    };
    const value = await entry({ storage });
    await value.controller.submit();
    expect(value.cancel).not.toHaveBeenCalled();
    expect(value.controller.snapshot().kind).toBe("unsent");
  });

  it("reports a persistence limit without downgrading a trusted success", async () => {
    let writes = 0;
    const storage = memory();
    const flaky: Store = {
      getItem: storage.getItem,
      setItem: (key, value) => {
        writes++;
        // Prepared and possibly-sent succeed; the accepted result cannot persist.
        if (writes >= 3) throw new Error("quota");
        storage.setItem(key, value);
      },
      removeItem: storage.removeItem,
    };
    const value = await entry({
      storage: flaky as ReturnType<typeof memory>,
      cancel: async () => ({ kind: "cancelled", result }),
    });
    await value.controller.submit();
    expect(value.controller.snapshot()).toEqual({
      kind: "success",
      result,
      persisted: false,
    });
    expect(value.sessions.value?.session).toBeNull();
  });

  it("turns a bounded 35-second wait into unknown", async () => {
    vi.useFakeTimers();
    const value = await entry({
      cancel: (_operation, _session, _input, signal) =>
        new Promise((resolve) => {
          signal.addEventListener("abort", () =>
            resolve({ kind: "disconnected" }),
          );
        }),
    });
    const pending = value.controller.submit();
    for (
      let index = 0;
      index < 20 && value.cancel.mock.calls.length === 0;
      index++
    )
      await Promise.resolve();
    expect(value.cancel).toHaveBeenCalledOnce();
    await vi.advanceTimersByTimeAsync(35_000);
    await pending;
    expect(value.controller.snapshot()).toMatchObject({
      kind: "unknown",
      notice: "unknown",
    });
    expect(value.cancel).toHaveBeenCalledOnce();
  });

  it("shows controlled current-reading states without submitting", async () => {
    const limited = setup();
    limited.setCurrent({
      kind: "failure",
      status: 429,
      code: "rate_limited",
      retryAfter: 2_000,
    });
    await limited.controller.verify();
    expect(limited.controller.snapshot()).toMatchObject({
      kind: "current",
      current: { kind: "limited", retryAt: 1_005_000 },
    });
    expect(limited.cancel).not.toHaveBeenCalled();

    const expired = setup();
    expired.setCurrent({
      kind: "failure",
      status: 401,
      code: "authentication_invalid",
      retryAfter: 0,
    });
    await expired.controller.verify();
    expect(expired.controller.snapshot()).toMatchObject({
      kind: "current",
      current: { kind: "expired" },
    });
    expect(expired.sessions.clears).toEqual([{ id: sessionId, revision }]);
    expect(expired.cancel).not.toHaveBeenCalled();

    const denied = setup();
    denied.setCurrent({
      kind: "failure",
      status: 403,
      code: "permission_denied",
      retryAfter: 0,
    });
    await denied.controller.verify();
    expect(denied.controller.snapshot()).toMatchObject({
      kind: "current",
      current: { kind: "denied" },
    });
    expect(denied.cancel).not.toHaveBeenCalled();

    const storage = setup();
    storage.sessions.failRead = true;
    await storage.controller.verify();
    expect(storage.controller.snapshot()).toMatchObject({
      kind: "current",
      current: { kind: "storage" },
    });
    expect(storage.cancel).not.toHaveBeenCalled();
  });

  it("hides private callbacks and the check panel before an asynchronous check", async () => {
    const storage = memory();
    storage.values.set(KEY, JSON.stringify(possibly(crypto.randomUUID())));
    const value = setup({ storage, marker: "submitted" });
    const hide = vi.fn();
    value.controller.onHide(hide);
    await value.controller.verify();
    expect(hide).toHaveBeenCalled();
    value.current.mockClear();
    await value.controller.check();
    expect(value.current).toHaveBeenCalledOnce();
    expect(value.controller.checkInfo().visible).toBe(true);
    value.controller.suspend();
    expect(hide).toHaveBeenCalledTimes(3);
    expect(value.controller.checkInfo().visible).toBe(false);
  });
});

describe("restored presentation", () => {
  it("requires the current build before revealing a durable success", async () => {
    const storage = memory();
    storage.values.set(
      KEY,
      JSON.stringify({ ...possibly(), outcome: "complete", result }),
    );
    const value = setup({ storage, build: async () => false });
    await value.controller.verify();
    expect(value.controller.snapshot().kind).toBe("reload");
    expect(value.saved()).toMatchObject({ outcome: "complete", result });
    expect(value.current).not.toHaveBeenCalled();
    expect(value.cancel).not.toHaveBeenCalled();
  });

  it("finishes concealment after a different selection without an endless check", async () => {
    const storage = memory();
    storage.values.set(KEY, JSON.stringify(possibly()));
    const value = setup({
      storage,
      sessions: new FakeSessions(browser(sessionId, crypto.randomUUID())),
    });
    await value.controller.verify();
    expect(value.controller.snapshot()).toMatchObject({
      kind: "hidden",
      current: { kind: "changed" },
    });
    expect(value.controller.checkInfo().checking).toBe(false);
    expect(value.current).not.toHaveBeenCalled();
  });

  it("discards only definitively unsent prepared material before confirming a new selection", async () => {
    const storage = memory();
    storage.values.set(KEY, JSON.stringify(prepared()));
    const value = setup({
      storage,
      marker: "submitted",
      sessions: new FakeSessions(browser(sessionId, crypto.randomUUID())),
    });
    await value.controller.verify();
    expect(value.controller.snapshot().kind).toBe("entry");
    expect(value.saved()).toBeNull();
    expect(value.getMarker()).toBeNull();
    expect(value.cancel).not.toHaveBeenCalled();
  });
});

describe("leave responsibility", () => {
  const state = (value: Partial<State> & { kind: State["kind"] }) =>
    value as State;

  it("warns pending only for a possible send or unknown outcome", () => {
    expect(hasPendingResult(state({ kind: "preparing" }))).toBe(false);
    expect(hasPendingResult(state({ kind: "submitting" }))).toBe(true);
    expect(
      hasPendingResult(
        state({ kind: "unknown", notice: "unknown", persisted: true }),
      ),
    ).toBe(true);
    expect(hasPendingResult(state({ kind: "unsent" }))).toBe(false);
    expect(
      hasPendingResult(state({ kind: "success", result, persisted: true })),
    ).toBe(false);
    expect(
      hasPendingResult(
        state({
          kind: "refused",
          notice: "target",
          retryAt: 0,
          canRestart: true,
        }),
      ),
    ).toBe(false);
    expect(
      hasPendingResult(
        state({ kind: "hidden", current: { kind: "checking" } }),
      ),
    ).toBe(false);
  });

  it("warns unsubmitted for a visible confirmation and nothing for a concealed result", () => {
    expect(
      leaveKind(
        state({
          kind: "entry",
          account,
          selection: { id: sessionId, revision },
        }),
      ),
    ).toBe("unsubmitted");
    expect(leaveKind(state({ kind: "unsent" }))).toBeNull();
    expect(
      leaveKind(state({ kind: "hidden", current: { kind: "checking" } })),
    ).toBeNull();
    expect(
      leaveKind(
        state({ kind: "unknown", notice: "missing", persisted: false }),
      ),
    ).toBe("pending");
  });
});
