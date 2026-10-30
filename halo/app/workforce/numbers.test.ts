import { afterEach, describe, expect, it, vi } from "vitest";
import { Current } from "../authentication/current";
import {
  sameSelection,
  type BrowserSession,
  type Selection,
} from "../authentication/session";
import type { Account } from "../protocol/session";
import type { NumberResult } from "../protocol/number";
import type { WorkforceResult } from "../protocol/workforce";
import { Workforce } from "./status";
import { Numbers } from "./numbers";

const id = "00000000-0000-4000-8000-000000000001";
const other = "00000000-0000-4000-8000-000000000002";
const account = {
  id,
  personId: id,
  revisionId: id,
  name: "Person",
  idNumber: { suffix: "002X" },
  binding: null,
} as Account;
const known: NumberResult = {
  kind: "number",
  number: { display: "92605", subtype: "实习" },
};
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}
function fixture() {
  let selected: BrowserSession | null = {
    version: 1,
    revision: id,
    session: { id, expiresAt: "2035-01-01T00:00:00Z" },
    attempt: null,
  };
  let now = 10_000;
  let compatible = true;
  const clear = vi.fn(async (expected: Selection) => {
    if (!sameSelection(selected, expected)) return false;
    selected = null;
    return true;
  });
  const identity = vi.fn(async () => ({ kind: "account" as const, account }));
  const current = new Current({
    sessions: { read: async () => selected, clear },
    current: identity,
    build: async () => true,
    now: () => now,
  });
  const serviceRead = vi
    .fn<() => Promise<WorkforceResult>>()
    .mockResolvedValue({
      kind: "status",
      status: "active",
      grant: { id: other, personId: id },
    });
  const workforce = new Workforce({
    current,
    workforce: serviceRead,
    build: async () => true,
    now: () => now,
  });
  const read = vi
    .fn<(id: string, signal: AbortSignal) => Promise<NumberResult>>()
    .mockResolvedValue(known);
  const numbers = new Numbers({
    current,
    workforce,
    number: read,
    build: async () => compatible,
    now: () => now,
  });
  return {
    current,
    workforce,
    numbers,
    read,
    serviceRead,
    identity,
    clear,
    selected: () => selected,
    select: (session = other, revision = other) => {
      selected = {
        ...selected!,
        revision,
        session: { id: session, expiresAt: "2035-01-01T00:00:00Z" },
      };
    },
    advance: (ms: number) => {
      now += ms;
    },
    incompatible: () => {
      compatible = false;
    },
    ready: async () => {
      await current.verify();
      await vi.waitFor(() => expect(numbers.snapshot().kind).toBe("ready"));
    },
    close: () => {
      numbers.close();
      workforce.close();
      current.suspend();
    },
  };
}
afterEach(() => vi.useRealTimers());
describe("visible current number", () => {
  it("waits for trusted active service and refreshes independently", async () => {
    const f = fixture();
    expect(f.read).not.toHaveBeenCalled();
    await f.ready();
    expect(f.numbers.snapshot()).toEqual({
      kind: "ready",
      number: known.number,
    });
    await f.numbers.refresh();
    expect(f.read).toHaveBeenCalledTimes(2);
    expect(f.serviceRead).toHaveBeenCalledTimes(1);
    expect(f.identity).toHaveBeenCalledTimes(1);
    f.close();
  });
  it("preserves independent identity/service during null, failure and Retry-After", async () => {
    const f = fixture();
    await f.ready();
    f.read.mockResolvedValueOnce({ kind: "number", number: null });
    await f.numbers.refresh();
    expect(f.numbers.snapshot()).toEqual({ kind: "ready", number: null });
    f.read.mockResolvedValueOnce({
      kind: "failure",
      status: 429,
      code: "rate_limited",
      retryAfter: 61_000,
    });
    await f.numbers.refresh();
    expect(f.numbers.snapshot()).toEqual({ kind: "limited", retryAt: 71_000 });
    await f.current.verify();
    await vi.waitFor(() => expect(f.numbers.snapshot().kind).toBe("limited"));
    expect(f.read).toHaveBeenCalledTimes(3);
    f.advance(61_000);
    f.read.mockResolvedValueOnce({
      kind: "failure",
      status: 503,
      code: "dependency_unavailable",
      retryAfter: 0,
    });
    await f.numbers.refresh();
    expect(f.numbers.snapshot()).toEqual({
      kind: "unavailable",
      retryAt: 76_000,
    });
    expect(f.current.snapshot().kind).toBe("ready");
    expect(f.workforce.snapshot()).toEqual({
      kind: "ready",
      status: "active",
      grant: { id: other, personId: id },
    });
    f.close();
  });
  it.each(["absent", "suspended", "terminated"] as const)(
    "conceals immediately on service recheck to %s and rejects old numbers",
    async (status) => {
      const f = fixture();
      await f.ready();
      const old = deferred<NumberResult>();
      f.read.mockImplementationOnce(() => old.promise);
      const pending = f.numbers.refresh();
      await vi.waitFor(() => expect(f.read).toHaveBeenCalledTimes(2));
      const service = deferred<WorkforceResult>();
      f.serviceRead.mockImplementationOnce(() => service.promise);
      const refresh = f.workforce.refresh();
      expect(f.numbers.snapshot()).toEqual({ kind: "hidden" });
      expect(f.read.mock.calls[1][1].aborted).toBe(true);
      service.resolve({
        kind: "status",
        status,
        grant: status === "absent" ? null : { id: other, personId: id },
      });
      await refresh;
      old.resolve(known);
      await pending;
      expect(f.numbers.snapshot()).toEqual({ kind: "hidden" });
      f.close();
    },
  );
  it("discards reversed refreshes and lifecycle responses", async () => {
    const f = fixture();
    await f.ready();
    const old = deferred<NumberResult>();
    f.read.mockImplementationOnce(() => old.promise);
    const pending = f.numbers.refresh();
    await vi.waitFor(() => expect(f.read).toHaveBeenCalledTimes(2));
    f.read.mockResolvedValueOnce({ kind: "number", number: null });
    await f.numbers.refresh();
    old.resolve(known);
    await pending;
    expect(f.numbers.snapshot()).toEqual({ kind: "ready", number: null });
    f.current.suspend();
    expect(f.numbers.snapshot()).toEqual({ kind: "hidden" });
    f.close();
  });
  it.each([false, true])(
    "rejects late responses after selection/account revision change (%s)",
    async (sameId) => {
      const f = fixture();
      await f.ready();
      const old = deferred<NumberResult>();
      f.read.mockImplementationOnce(() => old.promise);
      const pending = f.numbers.refresh();
      await vi.waitFor(() => expect(f.read).toHaveBeenCalledTimes(2));
      f.select(sameId ? id : other);
      old.resolve({
        kind: "failure",
        status: 401,
        code: "authentication_invalid",
        retryAfter: 0,
      });
      await pending;
      await vi.waitFor(() => expect(f.read).toHaveBeenCalledTimes(3));
      expect(f.clear).not.toHaveBeenCalled();
      expect(f.selected()?.revision).toBe(other);
      f.identity.mockResolvedValue({
        kind: "account",
        account: { ...account, revisionId: other },
      });
      const verifying = f.current.verify();
      expect(f.numbers.snapshot()).toEqual({ kind: "hidden" });
      await verifying;
      await vi.waitFor(() => expect(f.numbers.snapshot().kind).toBe("ready"));
      f.close();
    },
  );
  it.each([401, 403] as const)(
    "conceals on %i and clears only original 401 selection",
    async (status) => {
      const f = fixture();
      await f.ready();
      f.read.mockResolvedValueOnce({
        kind: "failure",
        status,
        code: status === 401 ? "authentication_invalid" : "permission_denied",
        retryAfter: 0,
      });
      await f.numbers.refresh();
      expect(f.current.snapshot().kind).toBe(
        status === 401 ? "expired" : "denied",
      );
      expect(f.numbers.snapshot().kind).toBe("hidden");
      expect(f.clear).toHaveBeenCalledTimes(status === 401 ? 1 : 0);
      expect(f.selected() === null).toBe(status === 401);
      f.close();
    },
  );
  it("times out even when a transport ignores abort and rejects its late success", async () => {
    const f = fixture();
    await f.ready();
    vi.useFakeTimers();
    const old = deferred<NumberResult>();
    f.read.mockImplementationOnce(() => old.promise);
    const pending = f.numbers.refresh();
    await vi.advanceTimersByTimeAsync(0);
    expect(f.read).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(35_000);
    expect(f.numbers.snapshot().kind).toBe("unavailable");
    old.resolve(known);
    await pending;
    expect(f.numbers.snapshot().kind).toBe("unavailable");
    f.close();
  });
  it.each([false, true])(
    "requires reload for untrusted data or build mismatch (%s)",
    async (mismatch) => {
      const f = fixture();
      await f.ready();
      if (mismatch) f.incompatible();
      else f.read.mockResolvedValueOnce({ kind: "untrusted" });
      await f.numbers.refresh();
      expect(f.current.snapshot().kind).toBe("reload");
      expect(f.numbers.snapshot().kind).toBe("hidden");
      f.close();
    },
  );
});
