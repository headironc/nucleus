import { describe, expect, it, vi } from "vitest";
import type { Account, CurrentResult } from "../protocol/session";
import type {
  CompleteInput,
  EstablishInput,
  Result,
  SendInput,
} from "../protocol/mobile";
import { MobileChange, hasPendingResult, hasTransfer } from "./recovery";
import { KEY, LIFETIME, maintain, type Material } from "./material";

const accountId = "019354fd-933c-7e6a-8912-345678901234";
const personId = "018f3c1a-0000-7000-8000-000000000001";
const sessionId = "ffffffff-ffff-ffff-ffff-ffffffffffff";
const session2 = "ffffffff-ffff-ffff-ffff-fffffffffffe";
const messageId = "018f3c1a-0000-7000-8000-000000000002";
const transferId = "018f3c1a-0000-7000-8000-000000000003";
const expires = "2035-01-01T00:00:00Z";

const account = (id = accountId, person = personId): Account => ({
  id,
  personId: person,
  revisionId: "018f3c1a-0000-7000-8000-000000000010",
  name: "示例用户",
  idNumber: { suffix: "012X" },
  avatar: {
    revision: "018f3c1a-0000-7000-8000-000000000011",
    href: "https://media.invalid/api/v1/avatars/x.webp",
    expiresAt: expires,
    mediaType: "image/webp",
    width: 512,
    height: 512,
  },
  binding: null,
});

const message = {
  id: messageId,
  delivery: "sent" as const,
  expiresAt: expires,
};
const transfer = { id: transferId, expiresAt: expires };
const completion = { id: transferId, completedAt: "2026-10-24T12:00:00Z" };

function memory() {
  const values = new Map<string, string>();
  return {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => {
      values.set(key, value);
    },
    removeItem: (key: string) => {
      values.delete(key);
    },
    values,
  };
}

function setup(options: { readonly?: boolean } = {}) {
  let clock = 1_800_000_000_000;
  let revision = "4ef1d157-e547-4d18-bf84-45a159709a4b";
  let selectedId: string | null = sessionId;
  let currentResult: CurrentResult = { kind: "account", account: account() };
  let counter = 0;
  let marker = false;
  const storage = memory();
  const build = vi.fn(async () => true);
  const mark = vi.fn(() => {
    marker = true;
  });
  const sessions = {
    read: vi.fn(async () =>
      selectedId
        ? {
            version: 1 as const,
            revision,
            session: { id: selectedId, expiresAt: expires },
            attempt: null,
          }
        : null,
    ),
    clear: vi.fn(async (expected: { id: string; revision: string }) => {
      if (selectedId !== expected.id || revision !== expected.revision)
        return false;
      selectedId = null;
      revision = "4ef1d157-e547-4d18-bf84-45a159709a4c";
      return true;
    }),
  };
  const results = {
    sendCurrent: vi.fn<
      (
        operation: string,
        session: string,
        input: SendInput,
      ) => Promise<Result<typeof message>>
    >(async () => ({ kind: "ok", value: message })),
    sendNew: vi.fn<
      (
        operation: string,
        session: string,
        transfer: string,
        input: SendInput,
      ) => Promise<Result<typeof message>>
    >(async () => ({ kind: "ok", value: message })),
    establish: vi.fn<
      (
        operation: string,
        session: string,
        input: EstablishInput,
      ) => Promise<Result<typeof transfer>>
    >(async () => ({ kind: "ok", value: transfer })),
    complete: vi.fn<
      (
        operation: string,
        session: string,
        transfer: string,
        input: CompleteInput,
      ) => Promise<Result<typeof completion>>
    >(async () => ({ kind: "ok", value: completion })),
    read: vi.fn<
      (
        session: string,
        transfer: string,
      ) => Promise<
        Result<
          | { status: "pending"; expiresAt: string }
          | { status: "completed"; completedAt: string }
          | { status: "expired" }
        >
      >
    >(async () => ({
      kind: "ok",
      value: { status: "pending", expiresAt: expires },
    })),
  };
  const dependencies: ConstructorParameters<typeof MobileChange>[0] = {
    storage,
    sessions,
    build,
    current: async () => currentResult,
    sendCurrent: results.sendCurrent,
    sendNew: results.sendNew,
    establish: results.establish,
    complete: results.complete,
    read: results.read,
    now: () => clock,
    uuid: () => {
      counter += 1;
      return `00000000-0000-4000-8000-${String(counter).padStart(12, "0")}`;
    },
    marker: () => marker,
    mark,
    unmark: () => {
      marker = false;
    },
    readonly: options.readonly ?? false,
  };
  const controller = new MobileChange(dependencies);
  return {
    controller,
    reopen: () => new MobileChange(dependencies),
    storage,
    sessions,
    results,
    build,
    mark,
    unmark: () => {
      marker = false;
    },
    advance: (milliseconds: number) => {
      clock += milliseconds;
    },
    setCurrent(result: CurrentResult) {
      currentResult = result;
    },
    setAccount(value: Account) {
      currentResult = { kind: "account", account: value };
    },
    newSession() {
      selectedId = session2;
      revision = "4ef1d157-e547-4d18-bf84-45a159709a4d";
    },
    markState: () => marker,
    get clock() {
      return clock;
    },
    material(): Material | null {
      const bytes = storage.values.get(KEY);
      if (!bytes) return null;
      try {
        return JSON.parse(bytes) as Material;
      } catch {
        return null;
      }
    },
  };
}

async function ready(options: { readonly?: boolean } = {}) {
  const harness = setup(options);
  await harness.controller.verify();
  return harness;
}

describe("self mobile change flow", () => {
  for (const action of [
    "sendCurrent",
    "establish",
    "sendNew",
    "complete",
  ] as const) {
    it.each([false, true])(
      `requires a reload after an untrusted ${action} response (replay: %s)`,
      async (replay) => {
        const h = await ready();
        if (action !== "sendCurrent")
          await h.controller.submitCurrent("13900000000", "original-current");
        if (action === "sendNew" || action === "complete")
          await h.controller.submitCurrentCode("012345");
        if (action === "complete") {
          await h.controller.submitNew("13800000000", "original-new");
          await h.controller.submitNewCode("654321");
        }
        const write = h.results[action];
        write.mockResolvedValueOnce({
          kind: replay ? "disconnected" : "untrusted",
        });
        if (action === "sendCurrent")
          await h.controller.submitCurrent("13900000000", "original-current");
        else if (action === "establish")
          await h.controller.submitCurrentCode("012345");
        else if (action === "sendNew")
          await h.controller.submitNew("13800000000", "original-new");
        else await h.controller.confirm();
        const original = write.mock.calls[0].slice(0, -1);
        if (replay) {
          h.advance(5000);
          write.mockResolvedValueOnce({ kind: "untrusted" });
          await h.controller.retry();
        }
        const attempts = replay ? 2 : 1;
        expect(h.controller.snapshot()).toEqual({
          kind: "reload",
          pending: true,
        });
        const saved = h.material()!;
        expect(saved.active).toMatchObject({
          operation: original[0],
          attempted: true,
          retryAt: h.clock + 5000,
        });
        expect(saved.known).toEqual([]);
        h.advance(10000);
        await h.controller.verify();
        await h.controller.retry();
        expect(h.controller.snapshot()).toEqual({
          kind: "reload",
          pending: true,
        });
        expect(write).toHaveBeenCalledTimes(attempts);
        const resumed = h.reopen();
        await resumed.verify();
        expect(write).toHaveBeenCalledTimes(attempts);
        expect(h.material()).toEqual(saved);
        await resumed.retry();
        expect(write).toHaveBeenCalledTimes(attempts + 1);
        expect(write.mock.calls.at(-1)!.slice(0, -1)).toEqual(original);
      },
    );
  }

  it.each(["longer", "storage", "later"] as const)(
    "preserves %s recovery state while abandoning a send",
    async (condition) => {
      const h = await ready();
      let release!: (value: Result<typeof message>) => void;
      h.results.sendCurrent.mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            release = resolve;
          }),
      );
      const pending = h.controller.submitCurrent("13900000000", "original");
      await vi.waitFor(() =>
        expect(h.results.sendCurrent).toHaveBeenCalledOnce(),
      );
      let record = h.material()!;
      const setItem = h.storage.setItem;
      if (condition === "longer") {
        record = {
          ...record,
          active: { ...record.active!, retryAt: h.clock + 30_000 },
        };
        setItem(KEY, JSON.stringify(record));
      } else if (condition === "later") {
        record = {
          ...record,
          active: {
            ...record.active!,
            operation: "00000000-0000-4000-8000-000000000099",
          },
        };
        setItem(KEY, JSON.stringify(record));
      } else
        h.storage.setItem = () => {
          throw Error("storage unavailable");
        };
      h.controller.suspend();
      release({ kind: "disconnected" });
      await pending;
      if (condition === "storage") {
        expect(h.controller.snapshot().kind).toBe("storage");
        h.controller.close();
        h.storage.setItem = setItem;
        await h.controller.verify();
        expect(h.material()!.active!.retryAt).toBe(h.clock + 5000);
      } else expect(h.material()).toEqual(record);
      expect(h.results.sendCurrent).toHaveBeenCalledOnce();
    },
  );

  for (const stage of ["current", "new"] as const) {
    it.each(["failure", "suspend"] as const)(
      `keeps the limited ${stage} proof across a build %s`,
      async (interruption) => {
        const h = await ready();
        if (stage === "new") {
          await h.controller.submitCurrent("13900000000", "initial");
          await h.controller.submitCurrentCode("012345");
        }
        const send =
          stage === "current" ? h.results.sendCurrent : h.results.sendNew;
        send.mockResolvedValueOnce({
          kind: "failure",
          status: 429,
          code: "rate_limited",
          retryAfter: 5000,
        });
        if (stage === "current")
          await h.controller.submitCurrent("13900000000", "original");
        else await h.controller.submitNew("13800000000", "original");
        const original = send.mock.calls[0];
        h.advance(5000);
        let release!: (compatible: boolean) => void;
        h.build.mockImplementationOnce(
          () =>
            new Promise<boolean>((resolve) => {
              release = resolve;
            }),
        );
        const retrying = h.controller.retry();
        await vi.waitFor(() => expect(release).toBeDefined());
        if (interruption === "suspend") h.controller.suspend();
        release(interruption === "suspend");
        await retrying;
        expect(send).toHaveBeenCalledOnce();
        const resumed = h.reopen();
        await resumed.verify();
        expect(resumed.snapshot()).toMatchObject({ kind: "limited", stage });
        expect(h.material()!.active!.retryAt).toBeGreaterThanOrEqual(
          h.clock + 5000,
        );
        await resumed.retry();
        expect(send).toHaveBeenCalledOnce();
        h.advance(5000);
        await resumed.retry();
        expect(send).toHaveBeenCalledTimes(2);
        expect(send.mock.calls[1].slice(0, -1)).toEqual(original.slice(0, -1));
      },
    );

    it.each(["suspend", "close", "reload", "verify"] as const)(
      `saves a cooldown before ${stage} SMS is abandoned by %s`,
      async (interruption) => {
        const h = await ready();
        if (stage === "new") {
          await h.controller.submitCurrent("13900000000", "initial");
          await h.controller.submitCurrentCode("012345");
        }
        const send =
          stage === "current" ? h.results.sendCurrent : h.results.sendNew;
        let release!: (value: Result<typeof message>) => void;
        send.mockImplementationOnce(
          () =>
            new Promise((resolve) => {
              release = resolve;
            }),
        );
        const pending =
          stage === "current"
            ? h.controller.submitCurrent("13900000000", "original")
            : h.controller.submitNew("13800000000", "original");
        await vi.waitFor(() => expect(send).toHaveBeenCalledOnce());
        const original = h.material()!.active!;
        const signal = send.mock.calls[0].at(-1) as unknown as AbortSignal;
        let savedBeforeAbort = false;
        signal.addEventListener("abort", () => {
          savedBeforeAbort = h.material()!.active!.retryAt >= h.clock + 5000;
        });
        await h.controller[interruption]();
        expect(savedBeforeAbort).toBe(true);
        expect(h.material()!.active).toEqual({
          ...original,
          retryAt: h.clock + 5000,
        });
        release({ kind: "disconnected" });
        await pending;
        const resumed = h.reopen();
        await resumed.verify();
        await resumed.retry();
        expect(send).toHaveBeenCalledOnce();
        h.advance(5000);
        await resumed.retry();
        expect(send).toHaveBeenCalledTimes(2);
        expect(send.mock.calls[1].slice(0, -1)).toEqual(
          send.mock.calls[0].slice(0, -1),
        );
      },
    );
  }

  it.each(["controller", "lifecycle"] as const)(
    "preserves the transfer when only the new code expires through %s",
    async (path) => {
      const h = await ready();
      await h.controller.submitCurrent("13900000000", "challenge");
      await h.controller.submitCurrentCode("012345");
      const deadline = h.material()!.active!.transferExpiresAt;
      h.results.sendNew.mockResolvedValueOnce({
        kind: "ok",
        value: {
          ...message,
          expiresAt: new Date(h.clock + 60_000).toISOString(),
        },
      });
      await h.controller.submitNew("13800000000", "challenge2");
      h.advance(61_000);
      if (path === "controller") h.controller.expire();
      else {
        expect(maintain(h.storage, h.clock)).toBe(true);
        await h.controller.verify();
      }
      expect(h.controller.snapshot()).toMatchObject({
        kind: "editing",
        stage: "new",
        mobile: "",
      });
      expect(h.material()!.active!.expiresAt).toBe(deadline);
      for (const key of ["newMobile", "code", "token", "messageId"])
        expect(h.material()!.active).not.toHaveProperty(key);
      expect(h.results.sendNew).toHaveBeenCalledOnce();
      await h.controller.submitNew("13800000000", "fresh-challenge");
      expect(h.results.sendNew).toHaveBeenCalledTimes(2);
      expect(h.material()!.active!.transferExpiresAt).toBe(deadline);
    },
  );

  it("restores the history marker before pending material can be lost", async () => {
    const h = await ready();
    h.results.sendCurrent.mockResolvedValueOnce({ kind: "disconnected" });
    await h.controller.submitCurrent("13900000000", "challenge");
    h.unmark();
    await h.controller.verify();
    expect(h.markState()).toBe(true);
    h.storage.removeItem(KEY);
    await h.controller.verify();
    expect(h.controller.snapshot()).toMatchObject({
      kind: "closed",
      why: "unknown",
      canRestart: false,
    });
    expect(h.results.sendCurrent).toHaveBeenCalledOnce();
  });

  it.each(["throws", "silent"] as const)(
    "retains pending material when restoring the marker %s",
    async (failure) => {
      const h = await ready();
      h.results.sendCurrent.mockResolvedValueOnce({ kind: "disconnected" });
      await h.controller.submitCurrent("13900000000", "challenge");
      const original = h.material();
      h.unmark();
      h.mark.mockImplementationOnce(() => {
        if (failure === "throws") throw Error("history unavailable");
      });
      await h.controller.verify();
      expect(h.controller.snapshot()).toMatchObject({
        kind: "storage",
        pending: true,
      });
      expect(h.material()).toEqual(original);
      await h.controller.verify();
      expect(h.markState()).toBe(true);
      expect(h.controller.snapshot().kind).toBe("unknown");
    },
  );

  it.each(["current", "new"] as const)(
    "retries a limited %s send with only its saved proof",
    async (stage) => {
      const h = await ready();
      if (stage === "new") {
        await h.controller.submitCurrent("13900000000", "challenge");
        await h.controller.submitCurrentCode("012345");
      }
      const send =
        stage === "current" ? h.results.sendCurrent : h.results.sendNew;
      send.mockResolvedValueOnce({
        kind: "failure",
        status: 429,
        code: "rate_limited",
        retryAfter: 7_000,
      });
      if (stage === "current")
        await h.controller.submitCurrent("13900000000", "saved-token");
      else await h.controller.submitNew("13800000000", "saved-token");
      const original = send.mock.calls[0];
      expect(h.controller.snapshot()).toMatchObject({ kind: "limited", stage });
      h.advance(7_001);
      if (stage === "current")
        await h.controller.submitCurrent("13900000000", "replacement-token");
      else await h.controller.submitNew("13800000000", "replacement-token");
      expect(send).toHaveBeenCalledOnce();
      await h.controller.verify();
      await h.controller.retry();
      expect(send).toHaveBeenCalledTimes(2);
      expect(send.mock.calls[1].slice(0, -1)).toEqual(original.slice(0, -1));
      expect(h.controller.snapshot()).toMatchObject({ kind: "coding", stage });
      expect(h.sessions.clear).not.toHaveBeenCalled();
    },
  );

  it.each(["establish", "transfer"] as const)(
    "does not label a later %s failure with an older completion",
    async (stage) => {
      const h = await ready();
      await h.controller.submitCurrent("13900000000", "challenge");
      await h.controller.submitCurrentCode("012345");
      await h.controller.submitNew("13800000000", "challenge2");
      await h.controller.submitNewCode("654321");
      await h.controller.confirm();
      h.newSession();
      await h.controller.verify();
      h.controller.restart();
      await h.controller.submitCurrent("13800000000", "next-challenge");
      if (stage === "establish")
        h.results.establish.mockResolvedValueOnce({
          kind: "failure",
          status: 410,
          code: "verification_required",
          retryAfter: 0,
        });
      else
        h.results.establish.mockResolvedValueOnce({
          kind: "ok",
          value: { ...transfer, id: "018f3c1a-0000-7000-8000-000000000004" },
        });
      await h.controller.submitCurrentCode("012345");
      if (stage === "transfer") {
        h.advance(LIFETIME + 1);
        h.controller.expire();
      }
      expect(h.controller.snapshot()).toMatchObject({
        kind: "closed",
        known: null,
      });
      expect(h.material()!.known[0].transfer).toBe(transferId);
    },
  );

  it("replaces the consumed original proof with the transfer's own deadline", async () => {
    const h = await ready();
    h.results.sendCurrent.mockResolvedValueOnce({
      kind: "ok",
      value: {
        ...message,
        expiresAt: new Date(h.clock + 300_000).toISOString(),
      },
    });
    await h.controller.submitCurrent("13900000000", "challenge");
    h.advance(240_000);
    const established = h.clock;
    h.results.establish.mockResolvedValueOnce({
      kind: "ok",
      value: {
        ...transfer,
        expiresAt: new Date(established + 600_000).toISOString(),
      },
    });
    await h.controller.submitCurrentCode("012345");
    expect(h.material()!.active).toMatchObject({
      createdAt: established,
      expiresAt: established + 600_000,
      transferExpiresAt: established + 600_000,
    });
    for (const key of ["currentMobile", "code", "token", "messageId"])
      expect(h.material()!.active).not.toHaveProperty(key);
    h.advance(61_000);
    await h.controller.verify();
    await h.controller.submitNew("13800000000", "challenge2");
    await h.controller.submitNewCode("654321");
    await h.controller.confirm();
    expect(h.results.complete).toHaveBeenCalledOnce();
    expect(h.controller.snapshot().kind).toBe("known");
  });

  it("uses the remaining transfer window when explicitly requesting another new code", async () => {
    const h = await ready();
    await h.controller.submitCurrent("13900000000", "challenge");
    await h.controller.submitCurrentCode("012345");
    const deadline = h.material()!.active!.transferExpiresAt;
    h.results.sendNew.mockResolvedValueOnce({
      kind: "ok",
      value: {
        ...message,
        expiresAt: new Date(h.clock + 60_000).toISOString(),
      },
    });
    await h.controller.submitNew("13800000000", "challenge2");
    h.controller.resend();
    expect(h.material()!.active!.expiresAt).toBe(deadline);
    h.advance(61_000);
    await h.controller.verify();
    await h.controller.submitNew("13800000000", "challenge3");
    expect(h.results.sendNew).toHaveBeenCalledTimes(2);
    expect(h.material()!.active!.transferExpiresAt).toBe(deadline);
  });

  it("starts an explicit new change while retaining the old completion across refresh", async () => {
    const h = await ready();
    await h.controller.submitCurrent("13900000000", "challenge");
    await h.controller.submitCurrentCode("012345");
    await h.controller.submitNew("13800000000", "challenge2");
    await h.controller.submitNewCode("654321");
    await h.controller.confirm();
    h.newSession();
    await h.controller.verify();
    h.controller.restart();
    await h.controller.verify();
    expect(h.controller.snapshot()).toMatchObject({
      kind: "editing",
      stage: "current",
    });
    expect(h.material()!.known[0].transfer).toBe(transferId);
    expect(h.results.sendCurrent).toHaveBeenCalledTimes(1);
    const nextTransfer = "018f3c1a-0000-7000-8000-000000000004";
    h.results.establish.mockResolvedValueOnce({
      kind: "ok",
      value: { ...transfer, id: nextTransfer },
    });
    await h.controller.submitCurrent("13800000000", "next-challenge");
    await h.controller.submitCurrentCode("012345");
    await h.controller.query();
    expect(h.controller.snapshot()).toMatchObject({
      kind: "query",
      status: "pending",
      transfer: nextTransfer,
    });
    expect(h.results.read).toHaveBeenLastCalledWith(
      session2,
      nextTransfer,
      expect.any(AbortSignal),
    );
  });

  it.each(["current", "new"] as const)(
    "checks the build before the first %s send",
    async (stage) => {
      const h = await ready();
      if (stage === "new") {
        await h.controller.submitCurrent("13900000000", "challenge");
        await h.controller.submitCurrentCode("012345");
      }
      h.build.mockResolvedValue(false);
      if (stage === "current")
        await h.controller.submitCurrent("13900000000", "challenge");
      else await h.controller.submitNew("13800000000", "challenge2");
      expect(h.controller.snapshot().kind).toBe("reload");
      expect(
        stage === "current" ? h.results.sendCurrent : h.results.sendNew,
      ).not.toHaveBeenCalled();
      expect(h.material()!.active).toMatchObject({
        attempted: false,
        token: stage === "current" ? "challenge" : "challenge2",
      });
      expect(h.material()!.active!.retryAt).toBeGreaterThan(h.clock);
    },
  );

  it.each(["current", "new"] as const)(
    "preserves the original %s send when the replay build check fails",
    async (stage) => {
      const h = await ready();
      const send =
        stage === "current" ? h.results.sendCurrent : h.results.sendNew;
      if (stage === "new") {
        await h.controller.submitCurrent("13900000000", "challenge");
        await h.controller.submitCurrentCode("012345");
      }
      send.mockResolvedValueOnce({ kind: "disconnected" });
      if (stage === "current")
        await h.controller.submitCurrent("13900000000", "challenge");
      else await h.controller.submitNew("13800000000", "challenge2");
      const original = h.material()!.active!;
      h.advance(5_001);
      h.build.mockRejectedValueOnce(new Error("build unavailable"));
      await h.controller.retry();
      expect(h.controller.snapshot().kind).toBe("reload");
      expect(send).toHaveBeenCalledOnce();
      expect(h.material()!.active).toMatchObject({
        operation: original.operation,
        token: original.token,
        attempted: true,
        expiresAt: original.expiresAt,
      });
    },
  );

  it("keeps a fixed login return read-only when its original locator is missing", async () => {
    const h = await ready({ readonly: true });
    expect(h.controller.snapshot()).toMatchObject({
      kind: "closed",
      why: "missing",
      canRestart: false,
    });
    await h.controller.submitCurrent("13900000000", "challenge");
    expect(h.results.sendCurrent).not.toHaveBeenCalled();
    h.controller.restart();
    expect(h.controller.snapshot().kind).toBe("closed");
  });

  it.each(["suspend", "close", "selection"] as const)(
    "does not send after %s changes during the build check",
    async (change) => {
      const h = await ready();
      let resolve!: (compatible: boolean) => void;
      h.build.mockImplementationOnce(
        () =>
          new Promise((done) => {
            resolve = done;
          }),
      );
      const sending = h.controller.submitCurrent("13900000000", "challenge");
      await vi.waitFor(() => expect(h.build).toHaveBeenCalledTimes(2));
      if (change === "selection") {
        h.newSession();
        h.setAccount(account("019354fd-933c-7e6a-8912-345678901235", personId));
      } else h.controller[change]();
      resolve(true);
      await sending;
      expect(h.results.sendCurrent).not.toHaveBeenCalled();
      expect(h.material()!.active!.attempted).toBe(false);
    },
  );

  it("merges an unsaved completed fact into later storage after recovery", async () => {
    const h = await ready();
    await h.controller.submitCurrent("13900000000", "challenge");
    await h.controller.submitCurrentCode("012345");
    const original = h.material()!;
    const get = h.storage.getItem;
    h.results.read.mockImplementationOnce(async () => {
      h.storage.getItem = () => {
        throw Error("storage unavailable");
      };
      return {
        kind: "ok",
        value: { status: "completed", completedAt: completion.completedAt },
      };
    });
    await h.controller.query();
    const other = {
      ...original,
      active: {
        ...original.active!,
        transfer: "018f3c1a-0000-7000-8000-000000000099",
      },
    };
    h.storage.values.set(KEY, JSON.stringify(other));
    h.storage.getItem = get;
    await h.controller.verify();
    expect(h.material()!.active).toEqual(other.active);
    expect(h.material()!.known).toEqual([
      {
        account: accountId,
        person: personId,
        transfer: transferId,
        completedAt: completion.completedAt,
      },
    ]);
  });

  it("keeps a rejected original code editable after reload without reusing its operation", async () => {
    const h = await ready();
    await h.controller.submitCurrent("13900000000", "challenge");
    h.results.establish.mockResolvedValueOnce({
      kind: "failure",
      status: 403,
      code: "verification_invalid",
      retryAfter: 0,
    });
    await h.controller.submitCurrentCode("012345");
    const original = h.results.establish.mock.calls[0][0];
    h.controller.suspend();
    await h.controller.verify();
    expect(h.controller.snapshot()).toMatchObject({
      kind: "coding",
      stage: "current",
    });
    await h.controller.submitCurrentCode("654321");
    expect(h.results.establish).toHaveBeenCalledTimes(2);
    expect(h.results.establish.mock.calls[1][0]).not.toBe(original);
  });

  it("rejects stale form actions while completion is unknown or belongs to another selection", async () => {
    const h = await ready();
    await h.controller.submitCurrent("13900000000", "challenge");
    await h.controller.submitCurrentCode("012345");
    await h.controller.submitNew("13800000000", "challenge2");
    await h.controller.submitNewCode("654321");
    h.results.complete.mockResolvedValueOnce({ kind: "disconnected" });
    await h.controller.confirm();
    const original = h.material();
    await h.controller.submitNew("13700000000", "different challenge");
    await h.controller.submitNewCode("111111");
    await h.controller.submitCurrent("13900000000", "different challenge");
    expect(h.material()).toEqual(original);
    expect(h.results.sendCurrent).toHaveBeenCalledOnce();
    expect(h.results.sendNew).toHaveBeenCalledOnce();
    h.newSession();
    await h.controller.verify();
    await h.controller.confirm();
    expect(h.results.complete).toHaveBeenCalledOnce();
  });

  it("starts from a trusted current account without any request", async () => {
    const h = await ready();
    expect(h.controller.snapshot()).toMatchObject({
      kind: "editing",
      stage: "current",
    });
    expect(h.results.sendCurrent).not.toHaveBeenCalled();
    expect(h.markState()).toBe(false);
  });

  it("completes all four stages with independent operations and no retained code", async () => {
    const h = await ready();
    await h.controller.submitCurrent("13900000000", "challenge");
    expect(h.results.sendCurrent).toHaveBeenCalledOnce();
    const send = h.results.sendCurrent.mock.calls[0];
    expect(send[1]).toBe(sessionId);
    expect(send[2]).toEqual({ mobile: "13900000000", token: "challenge" });
    expect(h.controller.snapshot()).toMatchObject({
      kind: "coding",
      stage: "current",
      delivery: "sent",
    });
    expect(h.material()!.active).toMatchObject({
      step: "current-code",
      messageId,
      attempted: false,
    });
    expect(h.material()!.active?.token).toBeUndefined();

    await h.controller.submitCurrentCode("012345");
    expect(h.results.establish).toHaveBeenCalledOnce();
    const establish = h.results.establish.mock.calls[0];
    expect(establish[0]).not.toBe(send[0]);
    expect(establish[2]).toEqual({ messageId, code: "012345" });
    expect(h.controller.snapshot()).toMatchObject({
      kind: "editing",
      stage: "new",
    });
    expect(h.material()!.active).toMatchObject({
      step: "transfer",
      transfer: transferId,
    });
    expect(h.material()!.active?.messageId).toBeUndefined();
    expect(h.material()!.active?.code).toBeUndefined();

    await h.controller.submitNew("13800000000", "challenge2");
    expect(h.results.sendNew).toHaveBeenCalledOnce();
    expect(h.results.sendNew.mock.calls[0][2]).toBe(transferId);
    expect(h.results.sendNew.mock.calls[0][0]).not.toBe(establish[0]);
    expect(h.controller.snapshot()).toMatchObject({
      kind: "coding",
      stage: "new",
    });

    await h.controller.submitNewCode("654321");
    expect(h.controller.snapshot()).toMatchObject({ kind: "confirm" });
    await h.controller.confirm();
    expect(h.results.complete).toHaveBeenCalledOnce();
    const complete = h.results.complete.mock.calls[0];
    expect(complete[2]).toBe(transferId);
    expect(complete[3]).toEqual({
      mobile: "13800000000",
      messageId,
      code: "654321",
    });
    expect(
      new Set([
        send[0],
        establish[0],
        h.results.sendNew.mock.calls[0][0],
        complete[0],
      ]).size,
    ).toBe(4);
    expect(h.controller.snapshot()).toMatchObject({ kind: "known" });
    expect(await h.sessions.read()).toBeNull();
    expect(h.material()).toEqual({
      version: 1,
      known: [
        {
          account: accountId,
          person: personId,
          transfer: transferId,
          completedAt: completion.completedAt,
        },
      ],
      active: null,
      closed: null,
    });
    expect(h.material()!.active).toBeNull();
  });

  it.each(["throw", "silent", "readback"] as const)(
    "sends zero requests when saving fails (%s)",
    async (mode) => {
      const h = await ready();
      const original = h.storage.setItem;
      if (mode === "throw")
        h.storage.setItem = () => {
          throw new DOMException("denied", "QuotaExceededError");
        };
      if (mode === "silent") h.storage.setItem = () => {};
      if (mode === "readback")
        h.storage.setItem = (key: string, value: string) => {
          original(key, `${value} `);
        };
      await h.controller.submitCurrent("13900000000", "challenge");
      expect(h.results.sendCurrent).not.toHaveBeenCalled();
      expect(h.controller.snapshot().kind).toBe("storage");
      h.storage.setItem = original;
    },
  );

  it("keeps the current message across a wrong establish code and uses a new operation", async () => {
    const h = await ready();
    await h.controller.submitCurrent("13900000000", "challenge");
    const sendOperation = h.results.sendCurrent.mock.calls[0][0];
    h.results.establish.mockResolvedValueOnce({
      kind: "failure",
      status: 403,
      code: "verification_invalid",
      retryAfter: 0,
    });
    await h.controller.submitCurrentCode("000000");
    const snapshot = h.controller.snapshot();
    expect(snapshot).toMatchObject({
      kind: "coding",
      stage: "current",
      notice: "wrong_code",
    });
    expect(h.material()!.active).toMatchObject({
      step: "establish",
      messageId,
      attempted: false,
    });
    expect(h.material()!.active?.code).toBeUndefined();
    expect(h.material()!.active?.operation).toBeUndefined();
    await h.controller.submitCurrentCode("012345");
    expect(h.results.establish.mock.calls[1][0]).not.toBe(sendOperation);
    expect(h.results.establish.mock.calls[1][0]).not.toBe(
      h.results.establish.mock.calls[0][0],
    );
  });

  it("keeps an unknown establish and never converts a recovery rejection into failure", async () => {
    const h = await ready();
    await h.controller.submitCurrent("13900000000", "challenge");
    h.results.establish.mockResolvedValueOnce({ kind: "disconnected" });
    await h.controller.submitCurrentCode("012345");
    const operation = h.results.establish.mock.calls[0][0];
    expect(h.controller.snapshot()).toMatchObject({
      kind: "unknown",
      step: "establish",
      canRetry: true,
    });
    expect(hasPendingResult(h.controller.snapshot())).toBe(true);
    h.results.establish.mockResolvedValueOnce({
      kind: "failure",
      status: 403,
      code: "verification_invalid",
      retryAfter: 0,
    });
    await h.controller.retry();
    expect(h.results.establish).toHaveBeenCalledTimes(1);
    h.advance(5_100);
    await h.controller.retry();
    expect(h.results.establish).toHaveBeenCalledTimes(2);
    expect(h.results.establish.mock.calls[1][0]).toBe(operation);
    expect(h.results.establish.mock.calls[1][2]).toEqual({
      messageId,
      code: "012345",
    });
    expect(h.controller.snapshot()).toMatchObject({
      kind: "unknown",
      step: "establish",
      notice: "refused",
      canRetry: false,
    });
    expect(h.material()!.active).toMatchObject({
      step: "establish",
      attempted: true,
      messageId,
      code: "012345",
    });
  });

  it("pauses a first rate limit, then retries the same unaccepted operation", async () => {
    const h = await ready();
    h.results.sendCurrent.mockResolvedValueOnce({
      kind: "failure",
      status: 429,
      code: "rate_limited",
      retryAfter: 0,
    });
    await h.controller.submitCurrent("13900000000", "challenge");
    const operation = h.results.sendCurrent.mock.calls[0][0];
    expect(h.controller.snapshot()).toMatchObject({
      kind: "limited",
      stage: "current",
    });
    expect(h.material()!.active).toMatchObject({ attempted: false, operation });
    await h.controller.retry();
    expect(h.results.sendCurrent).toHaveBeenCalledOnce();
    h.advance(5_100);
    await h.controller.retry();
    expect(h.results.sendCurrent).toHaveBeenCalledTimes(2);
    expect(h.results.sendCurrent.mock.calls[1][0]).toBe(operation);
  });

  it("keeps an unknown completion, queries without allowing new input, and reads the original result", async () => {
    const h = await ready();
    await h.controller.submitCurrent("13900000000", "challenge");
    await h.controller.submitCurrentCode("012345");
    await h.controller.submitNew("13800000000", "challenge2");
    await h.controller.submitNewCode("654321");
    h.results.complete.mockResolvedValueOnce({ kind: "disconnected" });
    await h.controller.confirm();
    const operation = h.results.complete.mock.calls[0][0];
    expect(h.controller.snapshot()).toMatchObject({
      kind: "unknown",
      step: "complete",
      canQuery: true,
    });
    expect(hasTransfer(h.controller.snapshot())).toBe(true);
    h.advance(5_100);

    h.results.read.mockResolvedValueOnce({
      kind: "ok",
      value: { status: "pending", expiresAt: expires },
    });
    await h.controller.query();
    expect(h.results.read).toHaveBeenCalledWith(
      sessionId,
      transferId,
      expect.anything(),
    );
    expect(h.controller.snapshot()).toMatchObject({
      kind: "query",
      status: "pending",
      unknown: true,
      canContinue: false,
    });
    expect(h.results.complete).toHaveBeenCalledOnce();
    const writes =
      h.results.sendCurrent.mock.calls.length +
      h.results.sendNew.mock.calls.length +
      h.results.establish.mock.calls.length;
    expect(writes).toBe(3);

    h.advance(5_100);
    h.results.complete.mockResolvedValueOnce({ kind: "ok", value: completion });
    await h.controller.retry();
    expect(h.results.complete).toHaveBeenCalledTimes(2);
    expect(h.results.complete.mock.calls[1][0]).toBe(operation);
    expect(h.results.complete.mock.calls[1][3]).toEqual({
      mobile: "13800000000",
      messageId,
      code: "654321",
    });
    expect(h.controller.snapshot()).toMatchObject({ kind: "known" });
  });

  it("treats a completed read as the result and a 404 as unknown, not failure", async () => {
    const h = await ready();
    await h.controller.submitCurrent("13900000000", "challenge");
    await h.controller.submitCurrentCode("012345");
    await h.controller.submitNew("13800000000", "challenge2");
    await h.controller.submitNewCode("654321");
    h.results.complete.mockResolvedValueOnce({ kind: "disconnected" });
    await h.controller.confirm();
    h.advance(5_100);
    h.results.read.mockResolvedValueOnce({
      kind: "failure",
      status: 404,
      code: "target_unavailable",
      retryAfter: 0,
    });
    await h.controller.query();
    expect(h.controller.snapshot()).toMatchObject({
      kind: "query",
      status: "unavailable",
      unknown: true,
    });
    h.results.read.mockResolvedValueOnce({
      kind: "ok",
      value: { status: "completed", completedAt: completion.completedAt },
    });
    await h.controller.query();
    expect(h.controller.snapshot()).toMatchObject({ kind: "known" });
    expect(h.material()!.known[0].transfer).toBe(transferId);
  });

  it("does not overwrite later material while verifying a rejected C1 proof", async () => {
    const h = setup();
    await h.controller.verify();
    let release: (() => void) | undefined;
    h.results.sendCurrent.mockImplementationOnce(async () => {
      h.build.mockImplementationOnce(
        () =>
          new Promise<boolean>((resolve) => {
            release = () => resolve(true);
          }),
      );
      return {
        kind: "failure",
        status: 401,
        code: "authentication_invalid",
        retryAfter: 0,
      };
    });
    const sending = h.controller.submitCurrent("13900000000", "original-token");
    await vi.waitFor(() => expect(release).toBeDefined());
    h.controller.suspend();
    const later = JSON.stringify({
      version: 1,
      known: [],
      active: null,
      closed: "unknown",
    });
    h.storage.setItem(KEY, later);
    release!();
    await sending;
    expect(h.storage.getItem(KEY)).toBe(later);
    expect(h.sessions.clear).not.toHaveBeenCalled();
  });

  it("clears only the original selection on a 401 and keeps the unknown record", async () => {
    const h = await ready();
    h.setCurrent({
      kind: "failure",
      status: 401,
      code: "authentication_invalid",
      retryAfter: 0,
    });
    h.results.sendCurrent.mockResolvedValueOnce({
      kind: "failure",
      status: 401,
      code: "authentication_invalid",
      retryAfter: 0,
    });
    await h.controller.submitCurrent("13900000000", "challenge");
    expect(h.sessions.clear).toHaveBeenCalledWith({
      id: sessionId,
      revision: "4ef1d157-e547-4d18-bf84-45a159709a4b",
    });
    expect(h.controller.snapshot()).toMatchObject({
      kind: "current",
      current: { kind: "expired" },
    });
    expect(h.material()!.active).toMatchObject({
      step: "current-send",
      attempted: true,
      reason: "denied",
    });
  });

  it.each(["current", "new"] as const)(
    "does not clear a valid Session on a %s C1 proof rejection",
    async (stage) => {
      const h = await ready();
      if (stage === "new") {
        await h.controller.submitCurrent("13900000000", "challenge");
        await h.controller.submitCurrentCode("012345");
      }
      const send =
        stage === "current" ? h.results.sendCurrent : h.results.sendNew;
      send.mockResolvedValueOnce({
        kind: "failure",
        status: 401,
        code: "authentication_invalid",
        retryAfter: 0,
      });
      if (stage === "current")
        await h.controller.submitCurrent("13900000000", "proof");
      else await h.controller.submitNew("13800000000", "proof");
      expect(h.sessions.clear).not.toHaveBeenCalled();
      expect((await h.sessions.read())?.session.id).toBe(sessionId);
      expect(h.controller.snapshot()).toMatchObject({
        kind: "unknown",
        canRetry: false,
      });
      expect(h.material()!.active!.token).toBe("proof");
    },
  );

  it("allows only a read from a same-account new session", async () => {
    const h = await ready();
    await h.controller.submitCurrent("13900000000", "challenge");
    await h.controller.submitCurrentCode("012345");
    h.newSession();
    await h.controller.verify();
    expect(h.controller.snapshot()).toMatchObject({
      kind: "query",
      status: "idle",
      canContinue: false,
    });
    h.results.read.mockResolvedValueOnce({
      kind: "ok",
      value: { status: "pending", expiresAt: expires },
    });
    await h.controller.query();
    expect(h.results.read).toHaveBeenCalledWith(
      session2,
      transferId,
      expect.anything(),
    );
    expect(h.controller.snapshot()).toMatchObject({
      kind: "query",
      status: "pending",
      canContinue: false,
    });
    expect(h.results.sendNew).not.toHaveBeenCalled();
    expect(h.results.establish).toHaveBeenCalledOnce();
  });

  it("conceals another account's record and does not show its result", async () => {
    const h = await ready();
    await h.controller.submitCurrent("13900000000", "challenge");
    await h.controller.submitCurrentCode("012345");
    h.setAccount(
      account(
        "019354fd-933c-7e6a-8912-345678901235",
        "018f3c1a-0000-7000-8000-000000000099",
      ),
    );
    await h.controller.verify();
    expect(h.controller.snapshot()).toEqual({
      kind: "other",
      account: expect.objectContaining({
        id: "019354fd-933c-7e6a-8912-345678901235",
      }),
    });
    expect(h.results.read).not.toHaveBeenCalled();
    expect(h.results.sendNew).not.toHaveBeenCalled();
  });

  it("turns damaged material into an unknown placeholder without any request", async () => {
    const h = await ready();
    h.storage.values.set(KEY, "{");
    await h.controller.verify();
    expect(h.controller.snapshot()).toMatchObject({
      kind: "closed",
      why: "unknown",
      canRestart: false,
    });
    expect(h.material()).toEqual({
      version: 1,
      known: [],
      active: null,
      closed: "unknown",
    });
    h.controller.restart();
    expect(h.controller.snapshot().kind).toBe("closed");
    expect(h.results.sendCurrent).not.toHaveBeenCalled();
  });

  it("closes an expired transfer to a locator and keeps an expired attempted write unknown", async () => {
    const h = await ready();
    h.storage.values.set(
      KEY,
      JSON.stringify({
        version: 1,
        known: [],
        closed: null,
        active: {
          step: "complete",
          account: accountId,
          person: personId,
          selection: {
            id: sessionId,
            revision: "4ef1d157-e547-4d18-bf84-45a159709a4b",
          },
          createdAt: h.clock - LIFETIME - 1,
          expiresAt: h.clock - 1,
          retryAt: h.clock + 5_000,
          attempted: true,
          operation: "00000000-0000-4000-8000-000000000009",
          transfer: transferId,
          currentMobile: "13900000000",
          newMobile: "13800000000",
          messageId,
          code: "654321",
        },
      }),
    );
    await h.controller.verify();
    expect(h.controller.snapshot()).toMatchObject({
      kind: "closed",
      why: "expired",
      canRestart: false,
      transfer: transferId,
    });
    expect(h.material()!.active).toMatchObject({
      step: "transfer",
      attempted: true,
      transfer: transferId,
    });
    expect(h.material()!.active?.code).toBeUndefined();
    expect(h.material()!.active?.currentMobile).toBeUndefined();
    expect(h.material()!.active?.newMobile).toBeUndefined();
    expect(h.results.read).not.toHaveBeenCalled();
    h.controller.restart();
    expect(h.controller.snapshot().kind).toBe("closed");
    await h.controller.query();
    expect(h.results.read).not.toHaveBeenCalled();
    h.advance(5_100);
    h.results.read.mockResolvedValueOnce({
      kind: "ok",
      value: { status: "completed", completedAt: completion.completedAt },
    });
    await h.controller.query();
    expect(h.results.read).toHaveBeenCalledWith(
      sessionId,
      transferId,
      expect.any(AbortSignal),
    );
    expect(h.controller.snapshot().kind).toBe("known");
    expect(h.material()!.known[0].transfer).toBe(transferId);
    expect(h.material()!.active).toBeNull();
    expect(h.results.sendCurrent).not.toHaveBeenCalled();
    expect(h.results.sendNew).not.toHaveBeenCalled();
    expect(h.results.establish).not.toHaveBeenCalled();
    expect(h.results.complete).not.toHaveBeenCalled();
  });

  it("keeps a confirmed completion when current identity later fails", async () => {
    const h = await ready();
    await h.controller.submitCurrent("13900000000", "challenge");
    await h.controller.submitCurrentCode("012345");
    await h.controller.submitNew("13800000000", "challenge2");
    await h.controller.submitNewCode("654321");
    await h.controller.confirm();
    expect(h.controller.snapshot().kind).toBe("known");
    h.newSession();
    h.setCurrent({
      kind: "failure",
      status: 503,
      code: "dependency_unavailable",
      retryAfter: 0,
    });
    await h.controller.verify();
    expect(h.controller.snapshot()).toMatchObject({
      kind: "current",
      current: { kind: "unavailable" },
    });
    expect(h.material()!.known[0].transfer).toBe(transferId);
    h.advance(5_100);
    h.setAccount(account());
    await h.controller.verify();
    expect(h.controller.snapshot()).toMatchObject({ kind: "known" });
  });

  it("re-enters the new-number step after a verified collision and keeps the transfer", async () => {
    const h = await ready();
    await h.controller.submitCurrent("13900000000", "challenge");
    await h.controller.submitCurrentCode("012345");
    await h.controller.submitNew("13800000000", "challenge2");
    await h.controller.submitNewCode("654321");
    h.results.complete.mockResolvedValueOnce({
      kind: "failure",
      status: 409,
      code: "mobile_taken",
      retryAfter: 0,
    });
    await h.controller.confirm();
    expect(h.controller.snapshot()).toMatchObject({
      kind: "editing",
      stage: "new",
      notice: "mobile_taken",
    });
    expect(h.material()!.active).toMatchObject({
      step: "transfer",
      transfer: transferId,
      attempted: false,
    });
    expect(h.material()!.active?.newMobile).toBeUndefined();
    await h.controller.submitNew("13700000000", "challenge3");
    expect(h.results.sendNew).toHaveBeenCalledTimes(2);
  });

  it("ignores a late response after suspension and retains the attempted record", async () => {
    const h = await ready();
    let resolve!: (value: Result<typeof message>) => void;
    h.results.sendCurrent.mockImplementationOnce(
      () => new Promise((done) => (resolve = done)),
    );
    const pending = h.controller.submitCurrent("13900000000", "challenge");
    await vi.waitFor(() =>
      expect(h.results.sendCurrent).toHaveBeenCalledOnce(),
    );
    h.controller.suspend();
    resolve({ kind: "ok", value: message });
    await pending;
    await h.controller.verify();
    expect(h.controller.snapshot()).toMatchObject({
      kind: "unknown",
      step: "current-send",
    });
  });

  it.each(["suspend", "close", "new-account", "unannounced-account"] as const)(
    "retains a late completion after %s without restoring the old view",
    async (lifecycle) => {
      const h = await ready();
      await h.controller.submitCurrent("13900000000", "challenge");
      await h.controller.submitCurrentCode("012345");
      await h.controller.submitNew("13800000000", "challenge2");
      await h.controller.submitNewCode("654321");
      let resolve!: (value: Result<typeof completion>) => void;
      h.results.complete.mockImplementationOnce(
        () => new Promise((done) => (resolve = done)),
      );
      const pending = h.controller.confirm();
      await vi.waitFor(() => expect(h.results.complete).toHaveBeenCalledOnce());
      if (lifecycle === "close") h.controller.close();
      else if (lifecycle !== "unannounced-account") h.controller.suspend();
      if (lifecycle === "new-account" || lifecycle === "unannounced-account") {
        h.newSession();
        h.setAccount(
          account(
            "019354fd-933c-7e6a-8912-345678901235",
            "018f3c1a-0000-7000-8000-000000000099",
          ),
        );
        if (lifecycle === "new-account") await h.controller.verify();
      }
      const visible = h.controller.snapshot();
      resolve({ kind: "ok", value: completion });
      await pending;
      expect(h.material()!.known).toEqual([
        {
          account: accountId,
          person: personId,
          transfer: transferId,
          completedAt: completion.completedAt,
        },
      ]);
      expect(h.material()!.active).toBeNull();
      if (lifecycle === "unannounced-account")
        expect(h.controller.snapshot().kind).toBe("other");
      else expect(h.controller.snapshot()).toBe(visible);
      expect(h.sessions.clear).toHaveBeenCalledWith({
        id: sessionId,
        revision: "4ef1d157-e547-4d18-bf84-45a159709a4b",
      });
      if (lifecycle === "new-account" || lifecycle === "unannounced-account")
        expect((await h.sessions.read())?.session.id).toBe(session2);
    },
  );

  it("does not advance an old send after an unannounced selection change", async () => {
    const h = await ready();
    let resolve!: (value: Result<typeof message>) => void;
    h.results.sendCurrent.mockImplementationOnce(
      () => new Promise((done) => (resolve = done)),
    );
    const pending = h.controller.submitCurrent("13900000000", "challenge");
    await vi.waitFor(() =>
      expect(h.results.sendCurrent).toHaveBeenCalledOnce(),
    );
    h.newSession();
    h.setAccount(account("019354fd-933c-7e6a-8912-345678901235", personId));
    resolve({ kind: "ok", value: message });
    await pending;
    expect(h.controller.snapshot().kind).toBe("other");
    expect(h.material()!.active).toMatchObject({
      step: "current-send",
      attempted: true,
      account: accountId,
    });
    expect(h.sessions.clear).not.toHaveBeenCalled();
  });

  it.each(["changed", "unavailable"] as const)(
    "conceals the original completion when selection cleanup is %s",
    async (cleanup) => {
      const h = await ready();
      await h.controller.submitCurrent("13900000000", "challenge");
      await h.controller.submitCurrentCode("012345");
      await h.controller.submitNew("13800000000", "challenge2");
      await h.controller.submitNewCode("654321");
      h.sessions.clear.mockImplementationOnce(async () => {
        h.newSession();
        h.setAccount(account("019354fd-933c-7e6a-8912-345678901235", personId));
        if (cleanup === "unavailable") throw new Error("storage unavailable");
        return false;
      });
      await h.controller.confirm();
      expect(h.controller.snapshot()).toMatchObject({
        kind: "editing",
        account: { id: "019354fd-933c-7e6a-8912-345678901235" },
      });
      expect(h.material()!.known[0]).toMatchObject({
        account: accountId,
        transfer: transferId,
      });
      expect((await h.sessions.read())?.session.id).toBe(session2);
    },
  );

  it("merges a late completion into the latest stored record without removing another transfer", async () => {
    const h = await ready();
    await h.controller.submitCurrent("13900000000", "challenge");
    await h.controller.submitCurrentCode("012345");
    await h.controller.submitNew("13800000000", "challenge2");
    await h.controller.submitNewCode("654321");
    let resolve!: (value: Result<typeof completion>) => void;
    h.results.complete.mockImplementationOnce(
      () => new Promise((done) => (resolve = done)),
    );
    const pending = h.controller.confirm();
    await vi.waitFor(() => expect(h.results.complete).toHaveBeenCalledOnce());
    const newer = {
      ...h.material()!.active!,
      transfer: "018f3c1a-0000-7000-8000-000000000004",
      operation: "00000000-0000-4000-8000-000000000099",
    };
    h.controller.close();
    h.storage.values.set(
      KEY,
      JSON.stringify({ version: 1, known: [], active: newer, closed: null }),
    );
    resolve({ kind: "ok", value: completion });
    await pending;
    expect(h.material()!.active).toEqual(newer);
    expect(h.material()!.known).toEqual([
      {
        account: accountId,
        person: personId,
        transfer: transferId,
        completedAt: completion.completedAt,
      },
    ]);
  });

  it("refuses restart while an unresolved write exists", async () => {
    const h = await ready();
    h.results.sendCurrent.mockResolvedValueOnce({
      kind: "failure",
      status: 503,
      code: "outcome_unknown",
      retryAfter: 0,
    });
    await h.controller.submitCurrent("13900000000", "challenge");
    expect(h.controller.snapshot()).toMatchObject({
      kind: "unknown",
      step: "current-send",
    });
    h.controller.restart();
    expect(h.controller.snapshot()).toMatchObject({
      kind: "unknown",
      step: "current-send",
    });
  });

  it("clears only the original selection when a status read is rejected", async () => {
    const h = await ready();
    await h.controller.submitCurrent("13900000000", "challenge");
    await h.controller.submitCurrentCode("012345");
    h.newSession();
    await h.controller.verify();
    h.results.read.mockResolvedValueOnce({
      kind: "failure",
      status: 401,
      code: "authentication_invalid",
      retryAfter: 0,
    });
    await h.controller.query();
    expect(h.sessions.clear).toHaveBeenCalledWith({
      id: session2,
      revision: "4ef1d157-e547-4d18-bf84-45a159709a4d",
    });
    expect(h.controller.snapshot()).toMatchObject({
      kind: "current",
      current: { kind: "expired" },
    });
    expect(h.material()!.active).toMatchObject({
      step: "transfer",
      transfer: transferId,
    });
  });

  it("keeps the original unknown across a failed status read", async () => {
    const h = await ready();
    await h.controller.submitCurrent("13900000000", "challenge");
    await h.controller.submitCurrentCode("012345");
    await h.controller.submitNew("13800000000", "challenge2");
    await h.controller.submitNewCode("654321");
    h.results.complete.mockResolvedValueOnce({ kind: "disconnected" });
    await h.controller.confirm();
    h.advance(5_100);
    h.results.read.mockResolvedValueOnce({
      kind: "failure",
      status: 503,
      code: "dependency_unavailable",
      retryAfter: 3,
    });
    await h.controller.query();
    expect(h.controller.snapshot()).toMatchObject({
      kind: "query",
      status: "failed",
      unknown: true,
    });
    expect(h.material()!.active).toMatchObject({
      step: "complete",
      attempted: true,
      transfer: transferId,
    });
  });

  it("retains unknown completion when GET observes an expired transfer", async () => {
    const h = await ready();
    await h.controller.submitCurrent("13900000000", "challenge");
    await h.controller.submitCurrentCode("012345");
    await h.controller.submitNew("13800000000", "challenge2");
    await h.controller.submitNewCode("654321");
    h.results.complete.mockResolvedValueOnce({ kind: "disconnected" });
    await h.controller.confirm();
    h.advance(5_100);
    h.results.read.mockResolvedValueOnce({
      kind: "ok",
      value: { status: "expired" },
    });
    await h.controller.query();
    expect(h.controller.snapshot()).toMatchObject({
      kind: "query",
      status: "expired",
      unknown: true,
      canContinue: false,
    });
    h.controller.restart();
    expect(h.material()!.active).toMatchObject({
      transfer: transferId,
      attempted: true,
    });
    expect(h.results.complete).toHaveBeenCalledOnce();
  });

  it.each(["disconnected", "unavailable"] as const)(
    "persists %s query backoff across expiry and re-entry",
    async (failure) => {
      const h = await ready();
      await h.controller.submitCurrent("13900000000", "challenge");
      await h.controller.submitCurrentCode("012345");
      h.advance(LIFETIME - 1_000);
      h.results.read.mockResolvedValueOnce(
        failure === "disconnected"
          ? { kind: "disconnected" }
          : {
              kind: "failure",
              status: 503,
              code: "dependency_unavailable",
              retryAfter: 8_000,
            },
      );
      await h.controller.query();
      const deadline = h.clock + (failure === "disconnected" ? 5_000 : 8_000);
      expect(h.material()!.active!.retryAt).toBe(deadline);
      h.advance(2_000);
      h.controller.suspend();
      await h.controller.verify();
      await h.controller.query();
      expect(h.results.read).toHaveBeenCalledOnce();
      expect(h.material()!.active!.retryAt).toBe(deadline);
      h.advance(6_001);
      await h.controller.query();
      expect(h.results.read).toHaveBeenCalledTimes(2);
    },
  );

  it.each([true, false])(
    "saves a late completed GET without showing another account's result (notified: %s)",
    async (notified) => {
      const h = await ready();
      await h.controller.submitCurrent("13900000000", "challenge");
      await h.controller.submitCurrentCode("012345");
      let resolve!: (
        result: Awaited<ReturnType<typeof h.results.read>>,
      ) => void;
      h.results.read.mockImplementationOnce(
        () =>
          new Promise((done) => {
            resolve = done;
          }),
      );
      const pending = h.controller.query();
      await vi.waitFor(() => expect(h.results.read).toHaveBeenCalledOnce());
      if (notified) h.controller.suspend();
      h.newSession();
      h.setAccount(account("019354fd-933c-7e6a-8912-345678901235", personId));
      const view = h.controller.snapshot();
      resolve({
        kind: "ok",
        value: { status: "completed", completedAt: completion.completedAt },
      });
      await pending;
      if (notified) expect(h.controller.snapshot()).toBe(view);
      else expect(h.controller.snapshot().kind).toBe("other");
      expect(h.material()!.known).toEqual([
        {
          account: accountId,
          person: personId,
          transfer: transferId,
          completedAt: completion.completedAt,
        },
      ]);
      expect(h.sessions.clear).not.toHaveBeenCalled();
    },
  );

  it("keeps a known completion account-scoped and hidden from another account", async () => {
    const h = await ready();
    await h.controller.submitCurrent("13900000000", "challenge");
    await h.controller.submitCurrentCode("012345");
    await h.controller.submitNew("13800000000", "challenge2");
    await h.controller.submitNewCode("654321");
    await h.controller.confirm();
    expect(h.controller.snapshot().kind).toBe("known");
    h.controller.suspend();
    expect(h.controller.snapshot()).toMatchObject({
      kind: "current",
      current: { kind: "checking" },
    });
    await h.controller.verify();
    expect(h.controller.snapshot()).toMatchObject({
      kind: "current",
      current: { kind: "anonymous" },
    });
    h.newSession();
    h.setAccount(
      account(
        "019354fd-933c-7e6a-8912-345678901235",
        "018f3c1a-0000-7000-8000-000000000099",
      ),
    );
    await h.controller.verify();
    expect(h.controller.snapshot()).toMatchObject({
      kind: "editing",
      stage: "current",
    });
    expect(h.material()!.known[0].account).toBe(accountId);
    h.sessions.read.mockResolvedValueOnce(null);
    await h.controller.verify();
    expect(h.controller.snapshot()).toMatchObject({
      kind: "current",
      current: { kind: "anonymous" },
    });
    h.setAccount(account());
    await h.controller.verify();
    expect(h.controller.snapshot()).toMatchObject({ kind: "known" });
  });

  it("forces a fixed-intent return to read only even on the same selection", async () => {
    const h = setup({ readonly: true });
    h.storage.values.set(
      KEY,
      JSON.stringify({
        version: 1,
        known: [],
        closed: null,
        active: {
          step: "transfer",
          account: accountId,
          person: personId,
          selection: {
            id: sessionId,
            revision: "4ef1d157-e547-4d18-bf84-45a159709a4b",
          },
          createdAt: h.clock,
          expiresAt: h.clock + LIFETIME,
          retryAt: 0,
          attempted: false,
          transfer: transferId,
        },
      }),
    );
    await h.controller.verify();
    expect(h.controller.snapshot()).toMatchObject({
      kind: "query",
      status: "idle",
      canContinue: false,
    });
    expect(h.results.sendCurrent).not.toHaveBeenCalled();
  });

  it("returns a wrong completion code to new-code entry with a new operation", async () => {
    const h = await ready();
    await h.controller.submitCurrent("13900000000", "challenge");
    await h.controller.submitCurrentCode("012345");
    await h.controller.submitNew("13800000000", "challenge2");
    await h.controller.submitNewCode("654321");
    const first = h.material()!.active!.operation!;
    h.results.complete.mockResolvedValueOnce({
      kind: "failure",
      status: 403,
      code: "verification_invalid",
      retryAfter: 0,
    });
    await h.controller.confirm();
    expect(h.controller.snapshot()).toMatchObject({
      kind: "coding",
      stage: "new",
      notice: "wrong_code",
    });
    expect(h.material()!.active).toMatchObject({
      step: "new-code",
      attempted: false,
      messageId,
    });
    expect(h.material()!.active?.code).toBeUndefined();
    expect(h.material()!.active?.operation).toBeUndefined();
    await h.controller.submitNewCode("111111");
    expect(h.material()!.active!.operation).not.toBe(first);
  });

  it("does not turn a failed pre-request save into an unknown write", async () => {
    const h = await ready();
    const original = h.storage.setItem;
    h.storage.setItem = () => {
      throw new DOMException("denied", "QuotaExceededError");
    };
    await h.controller.submitCurrent("13900000000", "challenge");
    expect(h.results.sendCurrent).not.toHaveBeenCalled();
    expect(h.controller.snapshot().kind).toBe("storage");
    expect(h.controller.snapshot()).toMatchObject({ pending: false });
    h.storage.setItem = original;
    await h.controller.verify();
    expect(h.controller.snapshot()).toMatchObject({
      kind: "editing",
      stage: "current",
    });
    expect(h.material()?.active?.attempted).toBe(false);
  });

  it("re-sends the new number within the same transfer and keeps the entered number", async () => {
    const h = await ready();
    await h.controller.submitCurrent("13900000000", "challenge");
    await h.controller.submitCurrentCode("012345");
    await h.controller.submitNew("13800000000", "challenge2");
    h.controller.resend();
    expect(h.controller.snapshot()).toMatchObject({
      kind: "editing",
      stage: "new",
      mobile: "13800000000",
    });
    expect(h.material()!.active).toMatchObject({
      step: "new-send",
      transfer: transferId,
      newMobile: "13800000000",
      attempted: false,
    });
    expect(h.material()!.active?.token).toBeUndefined();
    expect(h.material()!.active?.messageId).toBeUndefined();
  });
});
