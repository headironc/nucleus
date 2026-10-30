import { describe, expect, it, vi } from "vitest";
import { Current } from "../authentication/current";
import {
  sameSelection,
  type BrowserSession,
  type Selection,
} from "../authentication/session";
import type { Account } from "../protocol/session";
import type { WorkforceResult } from "../protocol/workforce";
import { Workforce } from "./status";

const id = "00000000-0000-4000-8000-000000000001";
const other = "00000000-0000-4000-8000-000000000002";
const account = {
  id,
  personId: id,
  revisionId: id,
  name: "Current Person",
  idNumber: { suffix: "002X" },
  binding: null,
} as Account;
const grant = { id: other, personId: id };
function choice(selected = id): BrowserSession {
  return {
    version: 1,
    revision: selected,
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
  let selected: BrowserSession | null = choice();
  let now = 10_000;
  let compatible = true;
  const clear = vi.fn(async (expected: Selection) => {
    if (!sameSelection(selected, expected)) return false;
    selected = null;
    return true;
  });
  const current = new Current({
    sessions: { read: async () => selected, clear },
    current: async () => ({ kind: "account", account }),
    build: async () => true,
    now: () => now,
  });
  const read = vi
    .fn<(id: string, signal: AbortSignal) => Promise<WorkforceResult>>()
    .mockResolvedValue({ kind: "status", status: "active", grant });
  const controller = new Workforce({
    current,
    workforce: read,
    build: async () => compatible,
    now: () => now,
  });
  return {
    current,
    controller,
    read,
    clear,
    selected: () => selected,
    select: (next = other) => {
      selected = choice(next);
    },
    advance: (ms: number) => {
      now += ms;
    },
    incompatible: () => {
      compatible = false;
    },
    async ready() {
      await current.verify();
      await vi.waitFor(() => expect(controller.snapshot().kind).toBe("ready"));
    },
  };
}
describe("visible self Workforce status", () => {
  it("waits for trusted Current and stores only status", async () => {
    const f = fixture();
    expect(f.read).not.toHaveBeenCalled();
    await f.ready();
    expect(f.controller.snapshot()).toEqual({
      kind: "ready",
      status: "active",
      grant,
    });
    expect(f.read).toHaveBeenCalledWith(id, expect.any(AbortSignal));
    f.controller.close();
  });
  it("clears immediately and rejects reversed refresh responses", async () => {
    const f = fixture();
    await f.ready();
    const old = deferred<WorkforceResult>();
    f.read.mockImplementationOnce(() => old.promise);
    const pending = f.controller.refresh();
    await vi.waitFor(() => expect(f.read).toHaveBeenCalledTimes(2));
    expect(f.controller.snapshot()).toEqual({ kind: "loading" });
    f.read.mockResolvedValueOnce({
      kind: "status",
      status: "terminated",
      grant,
    });
    await f.controller.refresh();
    old.resolve({ kind: "status", status: "active", grant });
    await pending;
    expect(f.controller.snapshot()).toEqual({
      kind: "ready",
      status: "terminated",
      grant,
    });
    expect(f.read.mock.calls[1][1].aborted).toBe(true);
    f.controller.close();
  });
  it("hides synchronously on lifecycle/identity changes and discards late responses", async () => {
    const f = fixture();
    await f.ready();
    const old = deferred<WorkforceResult>();
    f.read.mockImplementationOnce(() => old.promise);
    const pending = f.controller.refresh();
    await vi.waitFor(() => expect(f.read).toHaveBeenCalledTimes(2));
    f.current.suspend();
    expect(f.controller.snapshot()).toEqual({ kind: "hidden" });
    f.select();
    await f.ready();
    old.resolve({ kind: "status", status: "suspended", grant });
    await pending;
    expect(f.controller.snapshot()).toEqual({
      kind: "ready",
      status: "active",
      grant,
    });
    f.controller.close();
  });
  it("rechecks selection before accepting even without a notification", async () => {
    const f = fixture();
    await f.ready();
    const old = deferred<WorkforceResult>();
    f.read.mockImplementationOnce(() => old.promise);
    const pending = f.controller.refresh();
    await vi.waitFor(() => expect(f.read).toHaveBeenCalledTimes(2));
    f.select();
    old.resolve({ kind: "status", status: "terminated", grant });
    await pending;
    await vi.waitFor(() =>
      expect(f.read).toHaveBeenCalledWith(other, expect.any(AbortSignal)),
    );
    expect(f.controller.snapshot()).not.toEqual({
      kind: "ready",
      status: "terminated",
      grant,
    });
    f.controller.close();
  });
  it("keeps trusted account during failure and retains Retry-After through reverification", async () => {
    const f = fixture();
    await f.ready();
    f.read.mockResolvedValueOnce({
      kind: "failure",
      status: 429,
      code: "rate_limited",
      retryAfter: 60_000,
    });
    await f.controller.refresh();
    expect(f.current.snapshot().kind).toBe("ready");
    expect(f.controller.snapshot()).toEqual({
      kind: "limited",
      retryAt: 70_000,
    });
    await f.current.verify();
    await vi.waitFor(() =>
      expect(f.controller.snapshot().kind).toBe("limited"),
    );
    expect(f.read).toHaveBeenCalledTimes(2);
    f.advance(60_000);
    await f.controller.refresh();
    expect(f.controller.snapshot().kind).toBe("ready");
    f.controller.close();
  });
  it.each([401, 403] as const)(
    "conceals restricted content on %i with conditional selection clearing",
    async (status) => {
      const f = fixture();
      await f.ready();
      f.read.mockResolvedValueOnce({
        kind: "failure",
        status,
        code: status === 401 ? "authentication_invalid" : "permission_denied",
        retryAfter: 0,
      });
      await f.controller.refresh();
      expect(f.controller.snapshot()).toEqual({ kind: "hidden" });
      expect(f.current.snapshot().kind).toBe(
        status === 401 ? "expired" : "denied",
      );
      expect(f.clear).toHaveBeenCalledTimes(status === 401 ? 1 : 0);
      f.controller.close();
    },
  );
  it("requires reload on incompatible build or untrusted response", async () => {
    for (const mismatch of [false, true]) {
      const f = fixture();
      await f.ready();
      if (mismatch) f.incompatible();
      else f.read.mockResolvedValueOnce({ kind: "untrusted" });
      await f.controller.refresh();
      expect(f.current.snapshot().kind).toBe("reload");
      expect(f.controller.snapshot().kind).toBe("hidden");
      f.controller.close();
    }
  });
});
