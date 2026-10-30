import { describe, expect, it } from "vitest";
import { Current } from "./current";
import { sameSelection, type BrowserSession, type Selection } from "./session";
import type { Account, CurrentResult } from "../protocol/session";

const first = "56ad73b5-d5b9-4cb7-aa22-7c446b97247e";
const second = "0d3d3fb2-e285-4123-b2be-bee3fe77cd02";
const firstRevision = "73a321f3-2e9a-4c41-aeb3-1b24bd5e7264";
const secondRevision = "b3ae485b-e99f-48ef-b6df-ee18f415da26";

function choice(id = first, revision = firstRevision): BrowserSession {
  return {
    version: 1,
    revision,
    session: { id, expiresAt: "2035-01-01T00:00:00Z" },
    attempt: null,
  };
}

function account(id = first): Account {
  return {
    id,
    personId: first,
    revisionId: second,
    name: id === first ? "测试甲" : "测试乙",
    idNumber: { suffix: "002X" },
    binding: null,
    avatar: {
      revision: first,
      href: `https://fixture.invalid/api/v1/avatars/${"a".repeat(64)}.webp?key_id=7&expires_at=2051222400&signature=${"_".repeat(42)}8`,
      expiresAt: "2035-01-01T00:00:00Z",
      mediaType: "image/webp",
      width: 512,
      height: 512,
    },
  };
}

function deferred<T>() {
  let resolve!: (result: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

function setup(
  read: (id: string, signal: AbortSignal) => Promise<CurrentResult>,
  build: () => Promise<boolean> = async () => true,
) {
  let selected: BrowserSession | null = choice();
  let now = 10_000;
  const cleared: Selection[] = [];
  const sessions = {
    async read() {
      return selected;
    },
    async clear(expected: Selection) {
      cleared.push(expected);
      if (!sameSelection(selected, expected)) return false;
      selected = null;
      return true;
    },
  };
  const current = new Current({
    sessions,
    current: read,
    build,
    now: () => now,
  });
  return {
    current,
    sessions,
    cleared,
    select(value: BrowserSession | null) {
      selected = value;
    },
    selected: () => selected,
    advance: (milliseconds: number) => {
      now += milliseconds;
    },
  };
}

describe("trusted current-account reads", () => {
  it("requires a fresh document after an untrusted response across foreground checks", async () => {
    let reads = 0;
    const fixture = setup(async () => {
      reads++;
      return { kind: "untrusted" };
    });
    await fixture.current.verify();
    expect(fixture.current.snapshot()).toEqual({ kind: "reload" });
    fixture.current.suspend();
    await fixture.current.verify();
    expect(fixture.current.snapshot()).toEqual({ kind: "reload" });
    expect(reads).toBe(1);
    expect(fixture.selected()?.session?.id).toBe(first);
    expect(fixture.cleared).toEqual([]);
  });

  it.each([
    ["session", second],
    ["revision", first],
  ])(
    "requires a fresh document after an untrusted response despite a changed %s",
    async (_, id) => {
      const response = deferred<CurrentResult>();
      const started = deferred<void>();
      let reads = 0;
      const fixture = setup(async () => {
        reads++;
        started.resolve();
        return response.promise;
      });
      const work = fixture.current.verify();
      await started.promise;
      const replacement = choice(id, secondRevision);
      fixture.select(replacement);
      response.resolve({ kind: "untrusted" });
      await work;
      expect(fixture.current.snapshot()).toEqual({ kind: "reload" });
      fixture.current.suspend();
      await fixture.current.verify();
      expect(fixture.current.snapshot()).toEqual({ kind: "reload" });
      expect(reads).toBe(1);
      expect(fixture.selected()).toEqual(replacement);
      expect(fixture.cleared).toEqual([]);
    },
  );

  it("discards an untrusted response from a superseded generation", async () => {
    const oldResponse = deferred<CurrentResult>();
    const newResponse = deferred<CurrentResult>();
    const oldStarted = deferred<void>();
    const newStarted = deferred<void>();
    const reads: string[] = [];
    const fixture = setup(async (id) => {
      reads.push(id);
      if (id === first) {
        oldStarted.resolve();
        return oldResponse.promise;
      }
      newStarted.resolve();
      return newResponse.promise;
    });
    const oldWork = fixture.current.verify();
    await oldStarted.promise;
    fixture.select(choice(second, secondRevision));
    const newWork = fixture.current.verify();
    await newStarted.promise;
    oldResponse.resolve({ kind: "untrusted" });
    await oldWork;
    expect(fixture.current.snapshot()).toEqual({ kind: "checking" });
    newResponse.resolve({ kind: "account", account: account(second) });
    await newWork;
    expect(fixture.current.snapshot()).toEqual({
      kind: "ready",
      selection: { id: second, revision: secondRevision },
      account: account(second),
    });
    expect(reads).toEqual([first, second]);
    expect(fixture.cleared).toEqual([]);
  });

  it.each(["changed", "failed"] as const)(
    "does not read private data after a %s build check",
    async (result) => {
      let reads = 0;
      const fixture = setup(
        async () => {
          reads++;
          return { kind: "account", account: account() };
        },
        async () => {
          if (result === "failed") throw new Error("fixture build unavailable");
          return false;
        },
      );
      await fixture.current.verify();
      fixture.current.suspend();
      await fixture.current.verify();
      expect(fixture.current.snapshot()).toEqual({ kind: "reload" });
      expect(reads).toBe(0);
      expect(fixture.cleared).toEqual([]);
    },
  );

  it("waits for the build check and discards it when the page leaves", async () => {
    const build = deferred<boolean>();
    let reads = 0;
    const fixture = setup(
      async () => {
        reads++;
        return { kind: "account", account: account() };
      },
      () => build.promise,
    );
    const work = fixture.current.verify();
    await Promise.resolve();
    expect(reads).toBe(0);
    fixture.current.suspend();
    build.resolve(true);
    await work;
    expect(reads).toBe(0);
    expect(fixture.current.snapshot()).toEqual({ kind: "checking" });
  });

  it("conceals a failed document and never accepts a late account or starts another read", async () => {
    const response = deferred<CurrentResult>();
    const started = deferred<void>();
    let reads = 0;
    const fixture = setup(async () => {
      reads++;
      started.resolve();
      return response.promise;
    });
    const work = fixture.current.verify();
    await started.promise;
    fixture.current.reload();
    expect(fixture.current.snapshot()).toEqual({ kind: "reload" });
    response.resolve({ kind: "account", account: account() });
    await work;
    await fixture.current.verify();
    fixture.current.suspend();
    expect(fixture.current.snapshot()).toEqual({ kind: "reload" });
    expect(reads).toBe(1);
  });

  it("has no private projection until the cookie-backed read succeeds, and hides it synchronously on suspension", async () => {
    const response = deferred<CurrentResult>();
    const fixture = setup(async () => response.promise);
    const work = fixture.current.verify();
    expect(fixture.current.snapshot()).toEqual({ kind: "checking" });
    response.resolve({ kind: "account", account: account() });
    await work;
    expect(fixture.current.snapshot()).toMatchObject({
      kind: "ready",
      account: { name: "测试甲" },
    });
    fixture.current.suspend();
    expect(fixture.current.snapshot()).toEqual({ kind: "checking" });
  });

  it("discards an old account response and independently verifies the newly selected account", async () => {
    const oldResponse = deferred<CurrentResult>();
    const newResponse = deferred<CurrentResult>();
    const oldStarted = deferred<void>();
    const newStarted = deferred<void>();
    const fixture = setup(async (id) => {
      if (id === first) {
        oldStarted.resolve();
        return oldResponse.promise;
      }
      newStarted.resolve();
      return newResponse.promise;
    });
    const observed: string[] = [];
    fixture.current.subscribe(() => {
      const state = fixture.current.snapshot();
      if (state.kind === "ready") observed.push(state.account.id);
    });
    const work = fixture.current.verify();
    await oldStarted.promise;
    fixture.select(choice(second, secondRevision));
    oldResponse.resolve({ kind: "account", account: account(first) });
    await work;
    await newStarted.promise;
    expect(fixture.current.snapshot().kind).toBe("checking");
    expect(observed).toEqual([]);
    newResponse.resolve({ kind: "account", account: account(second) });
    await expect.poll(() => fixture.current.snapshot().kind).toBe("ready");
    expect(observed).toEqual([second]);
  });

  it("does not clear a later selection when an old request reports 401", async () => {
    const response = deferred<CurrentResult>();
    const started = deferred<void>();
    const fixture = setup(async (id) => {
      if (id === second) return { kind: "account", account: account(second) };
      started.resolve();
      return response.promise;
    });
    const work = fixture.current.verify();
    await started.promise;
    fixture.select(choice(second, secondRevision));
    response.resolve({
      kind: "failure",
      status: 401,
      code: "authentication_invalid",
      retryAfter: 0,
    });
    await work;
    await expect.poll(() => fixture.current.snapshot().kind).toBe("ready");
    expect(fixture.selected()?.session?.id).toBe(second);
    expect(fixture.cleared).toEqual([]);
  });

  it("discards an old response when the same session has a new local revision", async () => {
    const oldResponse = deferred<CurrentResult>();
    const newResponse = deferred<CurrentResult>();
    const oldStarted = deferred<void>();
    const newStarted = deferred<void>();
    let reads = 0;
    const fixture = setup(async () => {
      if (++reads === 1) {
        oldStarted.resolve();
        return oldResponse.promise;
      }
      newStarted.resolve();
      return newResponse.promise;
    });
    const accepted: Selection[] = [];
    fixture.current.subscribe(() => {
      const state = fixture.current.snapshot();
      if (state.kind === "ready") accepted.push(state.selection);
    });
    const work = fixture.current.verify();
    await oldStarted.promise;
    fixture.select(choice(first, secondRevision));
    oldResponse.resolve({ kind: "account", account: account() });
    await work;
    await newStarted.promise;
    expect(accepted).toEqual([]);
    expect(fixture.current.snapshot()).toEqual({ kind: "checking" });
    newResponse.resolve({ kind: "account", account: account() });
    await expect.poll(() => fixture.current.snapshot().kind).toBe("ready");
    expect(accepted).toEqual([{ id: first, revision: secondRevision }]);
    expect(fixture.cleared).toEqual([]);
  });

  it("conceals a denied account without clearing the selected session", async () => {
    let reads = 0;
    const fixture = setup(async () =>
      ++reads === 1
        ? { kind: "account", account: account() }
        : {
            kind: "failure",
            status: 403,
            code: "permission_denied",
            retryAfter: 0,
          },
    );
    await fixture.current.verify();
    expect(fixture.current.snapshot().kind).toBe("ready");
    await fixture.current.verify();
    expect(fixture.current.snapshot()).toEqual({ kind: "denied" });
    expect(fixture.selected()).toEqual(choice());
    expect(fixture.cleared).toEqual([]);
  });

  it("conceals disconnected account data and retries only after backoff", async () => {
    let reads = 0;
    const fixture = setup(async () =>
      ++reads === 2
        ? { kind: "disconnected" }
        : { kind: "account", account: account() },
    );
    await fixture.current.verify();
    expect(fixture.current.snapshot().kind).toBe("ready");
    await fixture.current.verify();
    expect(fixture.current.snapshot()).toEqual({
      kind: "unavailable",
      retryAt: 15_000,
    });
    await fixture.current.verify();
    expect(reads).toBe(2);
    expect(fixture.selected()).toEqual(choice());
    expect(fixture.cleared).toEqual([]);
    fixture.advance(5_000);
    await fixture.current.verify();
    expect(reads).toBe(3);
    expect(fixture.current.snapshot().kind).toBe("ready");
  });

  it("also compares selection inside clearing when another login wins between the two operations", async () => {
    const fixture = setup(async () => ({
      kind: "failure",
      status: 401,
      code: "authentication_invalid",
      retryAfter: 0,
    }));
    const clear = fixture.sessions.clear;
    let entered = false;
    fixture.sessions.clear = async (expected) => {
      if (!entered) {
        entered = true;
        fixture.select(choice(second, secondRevision));
        fixture.current.suspend();
      }
      return clear(expected);
    };
    await fixture.current.verify();
    expect(fixture.selected()?.session?.id).toBe(second);
    expect(fixture.current.snapshot()).toEqual({ kind: "checking" });
  });

  it("clears only the matching invalid selection", async () => {
    const fixture = setup(async () => ({
      kind: "failure",
      status: 401,
      code: "authentication_invalid",
      retryAfter: 0,
    }));
    await fixture.current.verify();
    expect(fixture.current.snapshot()).toEqual({
      kind: "expired",
      selection: { id: first, revision: firstRevision },
    });
    expect(fixture.selected()).toBeNull();
    expect(fixture.cleared).toEqual([{ id: first, revision: firstRevision }]);
  });

  it("retains the selected session on dependency failure and obeys longer server backoff", async () => {
    let calls = 0;
    const fixture = setup(async () => {
      calls++;
      return calls === 1
        ? {
            kind: "failure",
            status: 429,
            code: "rate_limited",
            retryAfter: 30_000,
          }
        : {
            kind: "failure",
            status: 503,
            code: "dependency_unavailable",
            retryAfter: 0,
          };
    });
    await fixture.current.verify();
    expect(fixture.current.snapshot()).toEqual({
      kind: "limited",
      retryAt: 40_000,
    });
    await fixture.current.verify();
    expect(calls).toBe(1);
    fixture.advance(30_000);
    await fixture.current.verify();
    expect(fixture.current.snapshot()).toEqual({
      kind: "unavailable",
      retryAt: 45_000,
    });
    expect(fixture.selected()?.session?.id).toBe(first);
    expect(fixture.cleared).toEqual([]);
  });

  it("does not resurrect private content from a response after the page is hidden", async () => {
    const response = deferred<CurrentResult>();
    const started = deferred<void>();
    const fixture = setup(async () => {
      started.resolve();
      return response.promise;
    });
    const work = fixture.current.verify();
    await started.promise;
    fixture.current.suspend();
    response.resolve({ kind: "account", account: account() });
    await work;
    expect(fixture.current.snapshot()).toEqual({ kind: "checking" });
  });

  it("fails closed when the persistent choice cannot be read", async () => {
    let calls = 0;
    const fixture = setup(async () => {
      calls++;
      return { kind: "account", account: account() };
    });
    fixture.sessions.read = async () => {
      throw new Error("fixture storage refusal");
    };
    await fixture.current.verify();
    expect(fixture.current.snapshot()).toEqual({ kind: "storage" });
    expect(calls).toBe(0);
  });
});
