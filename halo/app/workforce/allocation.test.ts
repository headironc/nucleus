import { afterEach, describe, expect, it, vi } from "vitest";
import { AllocationController } from "./allocation";
import { KEY, read } from "./allocation-material";
import type { AllocationResult, AllocationInput } from "../protocol/number";
import type { Account, CurrentResult } from "../protocol/session";
import type { BrowserSession, Selection } from "../authentication/session";
import { sameSelection } from "../authentication/session";

const id = "00000000-0000-4000-8000-000000000001";
const other = "00000000-0000-4000-8000-000000000002";
const operation = "00000000-0000-4000-8000-000000000003";
const fixed = { id: operation, subtype: 902 as const, year: 2026, serial: 5 };
function fixture() {
  let selected: BrowserSession | null = {
    version: 1,
    revision: id,
    session: { id, expiresAt: "2035-01-01T00:00:00Z" },
    attempt: null,
  };
  let accountId = id;
  let now = 10_000;
  let marker: string | null = null;
  let active = true;
  let compatible = true;
  let broken = false;
  const bytes = new Map<string, string>();
  const storage = {
    getItem: (key: string) => bytes.get(key) ?? null,
    setItem: (key: string, value: string) => {
      if (broken) throw Error("denied");
      bytes.set(key, value);
    },
    removeItem: (key: string) => bytes.delete(key),
  };
  const clear = vi.fn(async (expected: Selection) => {
    if (!sameSelection(selected, expected)) return false;
    selected = null;
    return true;
  });
  const allocate = vi
    .fn<
      (
        operation: string,
        session: string,
        input: AllocationInput,
        signal: AbortSignal,
      ) => Promise<AllocationResult>
    >()
    .mockResolvedValue({ kind: "allocated", number: fixed });
  function create(
    overrides: Partial<
      ConstructorParameters<typeof AllocationController>[0]
    > = {},
  ) {
    return new AllocationController({
      storage,
      sessions: { read: async () => selected, clear },
      current: async () => ({
        kind: "account",
        account: {
          id: accountId,
          personId: accountId,
          revisionId: id,
          name: "Person",
        } as Account,
      }),
      workforce: async () => ({
        kind: "status",
        status: active ? "active" : "suspended",
        grant: { id: other, personId: accountId },
      }),
      number: async () => ({ kind: "number", number: null }),
      allocate,
      build: async () => compatible,
      now: () => now,
      uuid: () => operation,
      marker: () => marker,
      mark: (value) => {
        marker = value;
      },
      ...overrides,
    });
  }
  return {
    create,
    allocate,
    clear,
    bytes,
    storage,
    tick: () => {
      now += 60_000;
    },
    broken: () => {
      broken = true;
    },
    incompatible: () => {
      compatible = false;
    },
    suspendGrant: () => {
      active = false;
    },
    switch: (sameAccount = false) => {
      selected = {
        ...selected!,
        session: { id: other, expiresAt: "2035-01-01T00:00:00Z" },
        revision: other,
      };
      if (!sameAccount) accountId = other;
    },
  };
}
async function ready(controller: AllocationController) {
  await controller.verify();
  await vi.waitFor(() => expect(controller.snapshot().kind).toBe("entry"));
}
afterEach(() => vi.useRealTimers());

describe("first self allocation", () => {
  it("rejects damaged stored fixed numbers and impossible completion states", async () => {
    const f = fixture();
    const c = f.create();
    await ready(c);
    await c.submit(902);
    const known = JSON.parse(f.bytes.get(KEY)!);
    for (const damaged of [
      { ...known, number: { ...fixed, serial: 0 } },
      { ...known, number: { ...fixed, subtype: 100 } },
      { ...known, code: "pending" },
      { ...known, outcome: "refused", code: "permission_denied", number: null },
    ]) {
      f.bytes.set(KEY, JSON.stringify(damaged));
      expect(read(f.storage)).toBe("missing");
    }
    c.close();
  });
  it("checks a changed selection before exposing an arriving success", async () => {
    const f = fixture();
    f.allocate.mockImplementationOnce(async () => {
      f.switch();
      return { kind: "allocated", number: fixed };
    });
    const c = f.create();
    await ready(c);
    await c.submit(902);
    await vi.waitFor(() => expect(c.snapshot().kind).toBe("other"));
    expect(read(f.storage)).toMatchObject({ outcome: "complete" });
    c.close();
  });
  it("keeps the known receipt when completion persistence fails", async () => {
    const f = fixture();
    f.allocate.mockImplementationOnce(async () => {
      f.broken();
      return { kind: "allocated", number: fixed };
    });
    const c = f.create();
    await ready(c);
    await c.submit(902);
    expect(c.snapshot()).toMatchObject({
      kind: "result",
      persisted: false,
      allocation: { outcome: "complete", number: fixed },
    });
    await c.verify();
    expect(c.snapshot()).toMatchObject({
      kind: "result",
      persisted: false,
      allocation: { outcome: "complete", number: fixed },
    });
    expect(f.allocate).toHaveBeenCalledTimes(1);
    c.close();
  });
  it("conceals an unsaved receipt after the selected account changes", async () => {
    const f = fixture();
    f.allocate.mockImplementationOnce(async () => {
      f.broken();
      f.switch();
      return { kind: "allocated", number: fixed };
    });
    const c = f.create();
    await ready(c);
    await c.submit(902);
    await vi.waitFor(() => expect(c.snapshot().kind).toBe("other"));
    expect(f.allocate).toHaveBeenCalledTimes(1);
    c.close();
  });
  it("requires a real subtype and saves before the only first POST", async () => {
    const f = fixture();
    const c = f.create();
    await ready(c);
    await c.submit(800);
    expect(f.allocate).not.toHaveBeenCalled();
    await c.submit(902);
    expect(f.allocate).toHaveBeenCalledExactlyOnceWith(
      operation,
      id,
      { subtype: 902 },
      expect.any(AbortSignal),
    );
    expect(read(f.storage)).toMatchObject({
      outcome: "complete",
      number: fixed,
    });
    expect(c.snapshot()).toMatchObject({
      kind: "result",
      allocation: { outcome: "complete" },
    });
    c.close();
  });
  it("does not send when the original input cannot be saved", async () => {
    const f = fixture();
    const c = f.create();
    await ready(c);
    f.broken();
    await c.submit(902);
    expect(f.allocate).not.toHaveBeenCalled();
    expect(c.snapshot().kind).toBe("storage");
    c.close();
  });
  it("keeps unknown input and operation across reload without automatic replay", async () => {
    const f = fixture();
    f.allocate.mockResolvedValueOnce({ kind: "disconnected" });
    const first = f.create();
    await ready(first);
    await first.submit(902);
    first.close();
    const next = f.create();
    await next.verify();
    expect(f.allocate).toHaveBeenCalledTimes(1);
    await next.query();
    expect(f.allocate).toHaveBeenCalledTimes(1);
    f.tick();
    await next.query();
    expect(f.allocate).toHaveBeenCalledTimes(2);
    expect(f.allocate.mock.calls.map((call) => call.slice(0, 3))).toEqual([
      [operation, id, { subtype: 902 }],
      [operation, id, { subtype: 902 }],
    ]);
    expect(next.snapshot()).toMatchObject({
      kind: "result",
      allocation: { number: fixed },
    });
    next.close();
  });
  it("recovers the same person's original with a new session and paused grant", async () => {
    const f = fixture();
    f.allocate.mockResolvedValueOnce({ kind: "disconnected" });
    const c = f.create();
    await ready(c);
    await c.submit(902);
    f.switch(true);
    f.suspendGrant();
    f.tick();
    await c.verify();
    await c.query();
    expect(f.allocate).toHaveBeenLastCalledWith(
      operation,
      other,
      { subtype: 902 },
      expect.any(AbortSignal),
    );
    c.close();
  });
  it("hides another account and cannot replay its original", async () => {
    const f = fixture();
    f.allocate.mockResolvedValueOnce({ kind: "disconnected" });
    const c = f.create();
    await ready(c);
    await c.submit(902);
    f.switch();
    f.tick();
    await c.verify();
    expect(c.snapshot().kind).toBe("other");
    await c.query();
    await c.submit(100);
    expect(f.allocate).toHaveBeenCalledTimes(1);
    c.close();
  });
  it("permanently closes a mismatch across controller recreation", async () => {
    const f = fixture();
    f.allocate.mockResolvedValue({
      kind: "failure",
      status: 409,
      code: "request_mismatch",
      retryAfter: 0,
    });
    const c = f.create();
    await ready(c);
    await c.submit(902);
    c.close();
    f.tick();
    const next = f.create();
    await next.verify();
    await next.query();
    await next.submit(100);
    expect(f.allocate).toHaveBeenCalledTimes(1);
    expect(next.snapshot()).toMatchObject({
      kind: "result",
      allocation: { outcome: "mismatch" },
    });
    next.close();
  });
  it("retains Retry-After across rechecks and reloads", async () => {
    const f = fixture();
    f.allocate.mockResolvedValue({
      kind: "failure",
      status: 429,
      code: "rate_limited",
      retryAfter: 120_000,
    });
    const c = f.create();
    await ready(c);
    await c.submit(902);
    c.close();
    f.tick();
    const next = f.create();
    await next.verify();
    await next.query();
    expect(f.allocate).toHaveBeenCalledTimes(1);
    next.close();
  });
  it("does not turn a missing or malformed original into a new form", async () => {
    const f = fixture();
    f.allocate.mockResolvedValueOnce({ kind: "disconnected" });
    const c = f.create();
    await ready(c);
    await c.submit(902);
    c.close();
    f.bytes.delete(KEY);
    const next = f.create();
    await next.verify();
    expect(next.snapshot().kind).toBe("missing");
    await next.submit(100);
    expect(f.allocate).toHaveBeenCalledTimes(1);
    next.close();
  });
  it("never sends for a paused qualification", async () => {
    const f = fixture();
    f.suspendGrant();
    const c = f.create();
    await c.verify();
    await vi.waitFor(() => expect(c.snapshot().kind).toBe("ineligible"));
    await c.submit(902);
    expect(f.allocate).not.toHaveBeenCalled();
    c.close();
  });
  it("preserves a late success while the current account is hidden", async () => {
    const f = fixture();
    let resolve!: (value: AllocationResult) => void;
    f.allocate.mockImplementationOnce(
      () =>
        new Promise((done) => {
          resolve = done;
        }),
    );
    const c = f.create();
    await ready(c);
    const sent = c.submit(902);
    await vi.waitFor(() => expect(f.allocate).toHaveBeenCalledTimes(1));
    c.suspend();
    f.switch();
    await c.verify();
    resolve({ kind: "allocated", number: fixed });
    await sent;
    expect(c.snapshot().kind).toBe("other");
    expect(read(f.storage)).toMatchObject({
      outcome: "complete",
      number: fixed,
    });
    c.close();
  });
  it("keeps a late success arriving during an explicit result recheck", async () => {
    const f = fixture();
    let finishPost!: (value: AllocationResult) => void;
    f.allocate.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finishPost = resolve;
        }),
    );
    f.allocate.mockResolvedValue({ kind: "disconnected" });
    const account = {
      id,
      personId: id,
      revisionId: id,
      name: "Person",
    } as Account;
    const current = vi.fn(async (): Promise<CurrentResult> => ({
      kind: "account",
      account,
    }));
    const c = f.create({ current });
    await ready(c);
    const sent = c.submit(902);
    await vi.waitFor(() => expect(f.allocate).toHaveBeenCalledTimes(1));
    await c.verify();
    f.tick();
    let finishCheck!: (value: CurrentResult) => void;
    current.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finishCheck = resolve;
        }),
    );
    const checked = c.query();
    await vi.waitFor(() => expect(current).toHaveBeenCalledTimes(3));
    finishPost({ kind: "allocated", number: fixed });
    await sent;
    finishCheck({ kind: "account", account });
    await checked;
    expect(c.snapshot()).toMatchObject({
      kind: "result",
      allocation: { outcome: "complete", number: fixed },
    });
    expect(read(f.storage)).toMatchObject({
      outcome: "complete",
      number: fixed,
    });
    expect(f.allocate).toHaveBeenCalledTimes(1);
    c.close();
  });
  it.each(["before replay", "during replay"] as const)(
    "preserves a late success %s without replacing it with a replay result",
    async (stage) => {
      const f = fixture();
      let finishPost!: (value: AllocationResult) => void;
      let finishReplay!: (value: AllocationResult) => void;
      f.allocate.mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            finishPost = resolve;
          }),
      );
      f.allocate.mockImplementationOnce(() =>
        stage === "before replay"
          ? Promise.resolve({ kind: "disconnected" })
          : new Promise((resolve) => {
              finishReplay = resolve;
            }),
      );
      let held = false;
      let building = false;
      let releaseBuild!: () => void;
      const released = new Promise<void>((resolve) => {
        releaseBuild = resolve;
      });
      const c = f.create({
        build: async () => {
          if (held && c.snapshot().kind === "busy") {
            building = true;
            await released;
          }
          return true;
        },
      });
      await ready(c);
      const sent = c.submit(902);
      await vi.waitFor(() => expect(f.allocate).toHaveBeenCalledTimes(1));
      await c.verify();
      f.tick();
      held = stage === "before replay";
      const checked = c.query();
      if (held) await vi.waitFor(() => expect(building).toBe(true));
      else await vi.waitFor(() => expect(f.allocate).toHaveBeenCalledTimes(2));
      finishPost({ kind: "allocated", number: fixed });
      await sent;
      if (held) releaseBuild();
      else finishReplay({ kind: "disconnected" });
      await checked;
      expect(c.snapshot()).toMatchObject({
        kind: "result",
        allocation: { outcome: "complete", number: fixed },
      });
      expect(read(f.storage)).toMatchObject({
        outcome: "complete",
        number: fixed,
      });
      expect(f.allocate).toHaveBeenCalledTimes(held ? 1 : 2);
      c.close();
    },
    5_000,
  );
  it("clears only the exact invalid selection after a 401", async () => {
    const f = fixture();
    f.allocate.mockResolvedValue({
      kind: "failure",
      status: 401,
      code: "authentication_invalid",
      retryAfter: 0,
    });
    const c = f.create();
    await ready(c);
    await c.submit(902);
    expect(f.clear).toHaveBeenCalledExactlyOnceWith({ id, revision: id });
    expect(c.snapshot()).toMatchObject({
      kind: "current",
      current: { kind: "expired" },
    });
    c.close();
  });
  it("stops on untrusted responses and preserves the original", async () => {
    const f = fixture();
    f.allocate.mockResolvedValue({ kind: "untrusted" });
    const c = f.create();
    await ready(c);
    await c.submit(902);
    expect(c.snapshot().kind).toBe("reload");
    expect(read(f.storage)).toMatchObject({
      outcome: "pending",
      input: { subtype: 902 },
    });
    c.close();
  });
  it("requires reload when verifying the untrusted-response record fails", async () => {
    const f = fixture();
    const getItem = vi.spyOn(f.storage, "getItem");
    f.allocate.mockImplementationOnce(async () => {
      getItem.mockImplementationOnce(() => {
        throw Error("transient read failure");
      });
      return { kind: "untrusted" };
    });
    const c = f.create();
    await ready(c);
    await c.submit(902);
    expect(c.snapshot().kind).toBe("reload");
    f.tick();
    await c.verify();
    await c.query();
    expect(c.snapshot().kind).toBe("reload");
    expect(f.allocate).toHaveBeenCalledTimes(1);
    expect(read(f.storage)).toMatchObject({
      outcome: "pending",
      code: "untrusted",
    });
    c.close();
  });
});
