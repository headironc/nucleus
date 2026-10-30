import { describe, expect, it, vi } from "vitest";
import { Current } from "../authentication/current";
import {
  sameSelection,
  type BrowserSession,
  type Selection,
} from "../authentication/session";
import type { HistoryResult } from "../protocol/correction";
import type { Account, CurrentResult } from "../protocol/session";
import { History } from "./history";

const first = "56ad73b5-d5b9-4cb7-aa22-7c446b97247e";
const second = "0d3d3fb2-e285-4123-b2be-bee3fe77cd02";
const revision = "73a321f3-2e9a-4c41-aeb3-1b24bd5e7264";
const account: Account = {
  id: first,
  personId: first,
  revisionId: first,
  name: "Current Person",
  idNumber: { suffix: "002X" },
  binding: null,
  avatar: {
    revision: first,
    href: "https://unused.invalid",
    expiresAt: "2035-01-01T00:00:00Z",
    mediaType: "image/webp",
    width: 512,
    height: 512,
  },
};
function choice(id = first, rev = revision): BrowserSession {
  return {
    version: 1,
    revision: rev,
    session: { id, expiresAt: "2035-01-01T00:00:00Z" },
    attempt: null,
  };
}
const empty: HistoryResult = { kind: "history", count: 0, corrections: [] };
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}
function setup() {
  let selected: BrowserSession | null = choice();
  let now = 10_000;
  let storage = true;
  let compatible = true;
  const cleared: Selection[] = [];
  const sessions = {
    async read() {
      if (!storage) throw Error("storage");
      return selected;
    },
    async clear(expected: Selection) {
      if (!storage) throw Error("storage");
      cleared.push(expected);
      if (!sameSelection(selected, expected)) return false;
      selected = null;
      return true;
    },
  };
  const readCurrent = vi
    .fn<() => Promise<CurrentResult>>()
    .mockResolvedValue({ kind: "account", account });
  const current = new Current({
    sessions,
    current: readCurrent,
    build: async () => true,
    now: () => now,
  });
  const read = vi
    .fn<
      (
        id: string,
        page: number,
        size: number,
        signal: AbortSignal,
      ) => Promise<HistoryResult>
    >()
    .mockResolvedValue(empty);
  const history = new History({
    current,
    history: read,
    build: async () => compatible,
    now: () => now,
  });
  return {
    current,
    history,
    read,
    readCurrent,
    cleared,
    selected: () => selected,
    select(value: BrowserSession | null) {
      selected = value;
    },
    storage(value: boolean) {
      storage = value;
    },
    build(value: boolean) {
      compatible = value;
    },
    advance(ms: number) {
      now += ms;
    },
    async ready() {
      await current.verify();
      await vi.waitFor(() => expect(history.snapshot().kind).toBe("ready"));
    },
  };
}

describe("visible own-history reads", () => {
  it("starts only after trusted Current and uses default pagination", async () => {
    const f = setup();
    expect(f.history.snapshot()).toEqual({ kind: "hidden" });
    expect(f.read).not.toHaveBeenCalled();
    await f.ready();
    expect(f.read).toHaveBeenCalledWith(first, 1, 20, expect.any(AbortSignal));
    expect(f.history.snapshot()).toEqual({
      kind: "ready",
      page: 1,
      size: 20,
      count: 0,
      corrections: [],
    });
    f.history.close();
  });

  it("invalidates reversed refresh responses and discards the old count immediately", async () => {
    const f = setup();
    await f.ready();
    const older = deferred<HistoryResult>();
    f.read.mockImplementationOnce(() => older.promise);
    const pending = f.history.refresh(2);
    await vi.waitFor(() => expect(f.read).toHaveBeenCalledTimes(2));
    expect(f.history.snapshot()).toEqual({ kind: "loading", page: 1, size: 2 });
    await f.history.refresh(50);
    older.resolve(empty);
    await pending;
    expect(f.history.snapshot()).toEqual({
      kind: "ready",
      page: 1,
      size: 50,
      count: 0,
      corrections: [],
    });
    expect(f.read.mock.calls[1][3].aborted).toBe(true);
    f.history.close();
  });

  it("paginates in server order, recognizes an out-of-range page and refreshes page one", async () => {
    const f = setup();
    await f.ready();
    const records = [first, second].map((id) => ({
      id,
      before: { name: "Before", idNumber: { suffix: "0007" } },
      after: { name: "After", idNumber: { suffix: "002X" } },
      correctedAt: "2026-10-23T01:00:00Z",
    }));
    f.read.mockResolvedValue({
      kind: "history",
      count: 5,
      corrections: records,
    });
    await f.history.refresh(2);
    await f.history.next();
    expect(f.read.mock.calls.at(-1)?.slice(0, 3)).toEqual([first, 2, 2]);
    expect(f.history.snapshot()).toMatchObject({
      kind: "ready",
      page: 2,
      count: 5,
    });
    await f.history.previous();
    expect(f.read.mock.calls.at(-1)?.slice(0, 3)).toEqual([first, 1, 2]);
    await f.history.next();
    f.read.mockResolvedValue({ kind: "history", count: 1, corrections: [] });
    await f.history.next();
    expect(f.history.snapshot()).toMatchObject({
      kind: "ready",
      page: 3,
      count: 1,
    });
    f.read.mockResolvedValue({
      kind: "history",
      count: 1,
      corrections: records.slice(0, 1),
    });
    await f.history.refresh();
    expect(f.read.mock.calls.at(-1)?.slice(0, 3)).toEqual([first, 1, 2]);
    f.history.close();
  });

  it.each(["disconnected", "503", "429"])(
    "keeps Current trusted after ordinary history failure %s and honors the wait",
    async (kind) => {
      const f = setup();
      await f.ready();
      f.read.mockResolvedValue(
        kind === "disconnected"
          ? { kind }
          : {
              kind: "failure",
              status: Number(kind),
              code: kind === "429" ? "rate_limited" : "dependency_unavailable",
              retryAfter: 17_000,
            },
      );
      await f.history.refresh();
      expect(f.current.snapshot().kind).toBe("ready");
      expect(f.history.snapshot()).toEqual({
        kind: kind === "429" ? "limited" : "unavailable",
        page: 1,
        size: 20,
        retryAt: kind === "disconnected" ? 15_000 : 27_000,
      });
      await f.history.refresh();
      expect(f.read).toHaveBeenCalledTimes(2);
      await f.current.verify();
      await vi.waitFor(() =>
        expect(f.history.snapshot().kind).toBe(
          kind === "429" ? "limited" : "unavailable",
        ),
      );
      expect(f.read).toHaveBeenCalledTimes(2);
      f.advance(17_001);
      f.read.mockResolvedValue(empty);
      await f.history.refresh();
      expect(f.history.snapshot().kind).toBe("ready");
      f.history.close();
    },
  );

  it.each([401, 403] as const)(
    "conceals the whole account on %s",
    async (status) => {
      const f = setup();
      await f.ready();
      f.read.mockResolvedValue({
        kind: "failure",
        status,
        code: status === 401 ? "authentication_invalid" : "permission_denied",
        retryAfter: 0,
      });
      await f.history.refresh();
      expect(f.current.snapshot().kind).toBe(
        status === 401 ? "expired" : "denied",
      );
      expect(f.history.snapshot()).toEqual({ kind: "hidden" });
      expect(f.cleared.length).toBe(status === 401 ? 1 : 0);
      expect(f.selected()).toEqual(status === 401 ? null : choice());
      f.history.close();
    },
  );

  it.each(["selection", "revision", "person", "real-name", "suspend", "close"])(
    "discards old data and 401 after %s invalidation",
    async (change) => {
      const f = setup();
      await f.ready();
      const pending = deferred<HistoryResult>();
      f.read.mockImplementationOnce(() => pending.promise);
      const work = f.history.refresh();
      await vi.waitFor(() => expect(f.read).toHaveBeenCalledTimes(2));
      if (change === "selection") f.select(choice(second));
      if (change === "revision") f.select(choice(first, second));
      if (change === "person")
        f.readCurrent.mockResolvedValue({
          kind: "account",
          account: { ...account, id: second, personId: second },
        });
      if (change === "real-name")
        f.readCurrent.mockResolvedValue({
          kind: "account",
          account: { ...account, revisionId: second, name: "Changed" },
        });
      if (change === "close") f.history.close();
      else if (change === "suspend") f.current.suspend();
      else await f.ready();
      pending.resolve({
        kind: "failure",
        status: 401,
        code: "authentication_invalid",
        retryAfter: 0,
      });
      await work;
      expect(f.cleared).toEqual([]);
      expect(f.history.snapshot().kind).toBe(
        ["suspend", "close"].includes(change) ? "hidden" : "ready",
      );
      f.history.close();
    },
  );

  it("rechecks a changed persistent selection even without a notification and never clears its newer revision", async () => {
    const f = setup();
    await f.ready();
    const pending = deferred<HistoryResult>();
    f.read.mockImplementationOnce(() => pending.promise);
    const work = f.history.refresh();
    await vi.waitFor(() => expect(f.read).toHaveBeenCalledTimes(2));
    f.select(choice(first, second));
    pending.resolve({
      kind: "failure",
      status: 401,
      code: "authentication_invalid",
      retryAfter: 0,
    });
    await work;
    await vi.waitFor(() => expect(f.history.snapshot().kind).toBe("ready"));
    expect(f.selected()).toEqual(choice(first, second));
    expect(f.cleared).toEqual([]);
    f.history.close();
  });

  it.each(["storage", "build", "response"])(
    "conceals private state on %s failure",
    async (kind) => {
      const f = setup();
      await f.ready();
      if (kind === "storage") f.storage(false);
      if (kind === "build") f.build(false);
      if (kind === "response") f.read.mockResolvedValue({ kind: "untrusted" });
      await f.history.refresh();
      expect(f.history.snapshot()).toEqual({ kind: "hidden" });
      expect(f.current.snapshot().kind).toBe(
        kind === "storage" ? "storage" : "reload",
      );
      expect(f.cleared).toEqual([]);
      f.history.close();
    },
  );
});
