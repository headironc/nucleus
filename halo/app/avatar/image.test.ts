import { describe, expect, it } from "vitest";
import { Current } from "../authentication/current";
import type { BrowserSession } from "../authentication/session";
import type { Account, CurrentResult } from "../protocol/session";
import { Avatar } from "./image";

const id = "56ad73b5-d5b9-4cb7-aa22-7c446b97247e";
const revision = "73a321f3-2e9a-4c41-aeb3-1b24bd5e7264";

function selection(): BrowserSession {
  return {
    version: 1,
    revision,
    session: { id, expiresAt: "2035-01-01T00:00:00Z" },
    attempt: null,
  };
}

function account(
  avatarRevision = "a",
  href = `https://fixture.invalid/api/v1/avatars/${"a".repeat(64)}.webp?key_id=7&expires_at=2051222400&signature=${"_".repeat(42)}8`,
): Account {
  return {
    id,
    personId: id,
    revisionId: revision,
    name: "测试甲",
    idNumber: { suffix: "002X" },
    binding: null,
    avatar: {
      revision: avatarRevision,
      href,
      expiresAt: "2035-01-01T00:00:00Z",
      mediaType: "image/webp",
      width: 512,
      height: 512,
    },
  };
}

function setup(result: () => CurrentResult, wait = 35_000) {
  let selected: BrowserSession | null = selection();
  let now = 10_000;
  let reads = 0;
  const current = new Current({
    sessions: {
      async read() {
        return selected;
      },
      async clear() {
        selected = null;
        return true;
      },
    },
    build: async () => true,
    current: async () => {
      reads++;
      return result();
    },
    now: () => now,
  });
  const avatar = new Avatar({ current, wait, now: () => now });
  return {
    current,
    avatar,
    reads: () => reads,
    advance: (milliseconds: number) => {
      now += milliseconds;
    },
  };
}

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

describe("private avatar selection", () => {
  it("reuses an unexpired link only after the same identity is verified again", async () => {
    const first = account();
    const newer = account(
      "a",
      first.avatar.href.replace("key_id=7", "key_id=8"),
    );
    let result: CurrentResult = { kind: "account", account: first };
    const fixture = setup(() => result);
    await fixture.current.verify();
    fixture.current.suspend();
    expect(fixture.avatar.snapshot()).toEqual({ kind: "hidden" });
    result = { kind: "account", account: newer };
    await fixture.current.verify();
    expect(fixture.avatar.snapshot()).toMatchObject({
      kind: "loading",
      href: first.avatar.href,
    });
    fixture.avatar.close();
  });

  it("replaces the cached link at expiry and after an explicit image-failure retry", async () => {
    const first = account();
    first.avatar.expiresAt = new Date(11_000).toISOString();
    first.avatar.href = first.avatar.href.replace(
      "expires_at=2051222400",
      "expires_at=11",
    );
    const newer = account(
      "a",
      first.avatar.href.replace("expires_at=11", "expires_at=20"),
    );
    newer.avatar.expiresAt = new Date(20_000).toISOString();
    let result: CurrentResult = { kind: "account", account: first };
    const fixture = setup(() => result);
    await fixture.current.verify();
    fixture.advance(1_000);
    result = { kind: "account", account: newer };
    await fixture.current.verify();
    const loading = fixture.avatar.snapshot();
    expect(loading).toMatchObject({ kind: "loading", href: newer.avatar.href });
    if (loading.kind !== "loading") throw new Error("loading");
    fixture.avatar.error(loading.attempt);
    const fresh = account(
      "a",
      newer.avatar.href.replace("key_id=7", "key_id=8"),
    );
    fresh.avatar.expiresAt = newer.avatar.expiresAt;
    result = { kind: "account", account: fresh };
    await fixture.avatar.retry();
    expect(fixture.avatar.snapshot()).toMatchObject({
      kind: "loading",
      href: fresh.avatar.href,
    });
    fixture.avatar.close();
  });

  it("discards the concealed link when current verification fails", async () => {
    const first = account();
    const fresh = account(
      "a",
      first.avatar.href.replace("key_id=7", "key_id=8"),
    );
    let result: CurrentResult = { kind: "account", account: first };
    const fixture = setup(() => result);
    await fixture.current.verify();
    result = {
      kind: "failure",
      status: 503,
      code: "dependency_unavailable",
      retryAfter: 5_000,
    };
    await fixture.current.verify();
    expect(fixture.avatar.snapshot()).toEqual({ kind: "hidden" });
    fixture.advance(5_000);
    result = { kind: "account", account: fresh };
    await fixture.current.verify();
    expect(fixture.avatar.snapshot()).toMatchObject({
      kind: "loading",
      href: fresh.avatar.href,
    });
    fixture.avatar.close();
  });
  it("keeps pixels hidden until the original selection is confirmed", async () => {
    const href = account().avatar.href;
    const fixture = setup(() => ({ kind: "account", account: account() }));
    expect(fixture.avatar.snapshot()).toEqual({ kind: "hidden" });
    await fixture.current.verify();
    const loading = fixture.avatar.snapshot();
    expect(loading).toMatchObject({ kind: "loading", href });
    if (loading.kind !== "loading") throw new Error("loading");
    fixture.avatar.loaded(loading.attempt);
    await flush();
    expect(fixture.avatar.snapshot()).toEqual({
      kind: "ready",
      href,
      attempt: loading.attempt,
    });
  });

  it("keeps one attempt across loading and ready and separates a newer attempt", async () => {
    let result: CurrentResult = { kind: "account", account: account("a") };
    const fixture = setup(() => result);
    await fixture.current.verify();
    const first = fixture.avatar.snapshot();
    if (first.kind !== "loading") throw new Error("loading");
    fixture.avatar.loaded(first.attempt);
    await flush();
    expect(fixture.avatar.snapshot()).toEqual({
      kind: "ready",
      href: account("a").avatar.href,
      attempt: first.attempt,
    });

    result = { kind: "account", account: account("b") };
    await fixture.current.verify();
    const second = fixture.avatar.snapshot();
    if (second.kind !== "loading") throw new Error("second loading");
    expect(second.attempt).not.toBe(first.attempt);
    fixture.avatar.loaded(first.attempt);
    await flush();
    expect(fixture.avatar.snapshot()).toEqual(second);
    fixture.avatar.loaded(second.attempt);
    await flush();
    expect(fixture.avatar.snapshot()).toEqual({
      kind: "ready",
      href: account("b").avatar.href,
      attempt: second.attempt,
    });
  });

  it("re-verifies once before a fresh retry and then stops after failure", async () => {
    const fixture = setup(() => ({ kind: "account", account: account() }));
    await fixture.current.verify();
    let loading = fixture.avatar.snapshot();
    if (loading.kind !== "loading") throw new Error("loading");
    fixture.avatar.error(loading.attempt);
    expect(fixture.avatar.snapshot()).toEqual({ kind: "failed", retry: true });

    const reads = fixture.reads();
    await fixture.avatar.retry();
    expect(fixture.reads()).toBe(reads + 1);
    loading = fixture.avatar.snapshot();
    if (loading.kind !== "loading") throw new Error("loading after retry");
    fixture.avatar.error(loading.attempt);
    expect(fixture.avatar.snapshot()).toEqual({ kind: "failed", retry: false });
    expect(fixture.avatar.retry()).toBeUndefined();
  });

  it("carries the consumed retry budget across an unavailable recheck", async () => {
    let result: CurrentResult = { kind: "account", account: account() };
    const fixture = setup(() => result);
    await fixture.current.verify();
    let loading = fixture.avatar.snapshot();
    if (loading.kind !== "loading") throw new Error("loading");
    fixture.avatar.error(loading.attempt);

    result = {
      kind: "failure",
      status: 503,
      code: "dependency_unavailable",
      retryAfter: 5_000,
    };
    await fixture.avatar.retry();
    expect(fixture.avatar.snapshot()).toEqual({ kind: "hidden" });
    fixture.advance(5_000);
    result = { kind: "account", account: account() };
    await fixture.current.verify();
    loading = fixture.avatar.snapshot();
    if (loading.kind !== "loading") throw new Error("loading after recovery");
    fixture.avatar.error(loading.attempt);
    expect(fixture.avatar.snapshot()).toEqual({ kind: "failed", retry: false });
  });

  it("preserves image failure across rechecks until explicit retry or a new revision", async () => {
    let projected = account();
    const fixture = setup(() => ({ kind: "account", account: projected }));
    await fixture.current.verify();
    const first = fixture.avatar.snapshot();
    if (first.kind !== "loading") throw new Error("initial loading");
    fixture.avatar.error(first.attempt);

    fixture.current.suspend();
    expect(fixture.avatar.snapshot()).toEqual({ kind: "hidden" });
    await fixture.current.verify();
    expect(fixture.avatar.snapshot()).toEqual({ kind: "failed", retry: true });
    await fixture.avatar.retry();
    const retry = fixture.avatar.snapshot();
    if (retry.kind !== "loading") throw new Error("explicit retry loading");
    fixture.avatar.error(retry.attempt);

    for (let recheck = 0; recheck < 2; recheck++) {
      fixture.current.suspend();
      expect(fixture.avatar.snapshot()).toEqual({ kind: "hidden" });
      await fixture.current.verify();
      expect(fixture.avatar.snapshot()).toEqual({
        kind: "failed",
        retry: false,
      });
      expect(fixture.avatar.retry()).toBeUndefined();
    }

    projected = account("b");
    await fixture.current.verify();
    const fresh = fixture.avatar.snapshot();
    if (fresh.kind !== "loading") throw new Error("new revision loading");
    fixture.avatar.error(fresh.attempt);
    expect(fixture.avatar.snapshot()).toEqual({ kind: "failed", retry: true });
    fixture.avatar.close();
  });

  it("drops old pixels and ignores callbacks after suspension or a new revision", async () => {
    const first = account("a");
    let result: CurrentResult = { kind: "account", account: first };
    const fixture = setup(() => result);
    await fixture.current.verify();
    const old = fixture.avatar.snapshot();
    if (old.kind !== "loading") throw new Error("loading");

    fixture.current.suspend();
    expect(fixture.avatar.snapshot()).toEqual({ kind: "hidden" });
    fixture.avatar.loaded(old.attempt);
    await flush();
    expect(fixture.avatar.snapshot()).toEqual({ kind: "hidden" });

    const next = account(
      "b",
      `https://fixture.invalid/api/v1/avatars/${"b".repeat(64)}.webp?key_id=7&expires_at=2051222400&signature=${"_".repeat(42)}8`,
    );
    result = { kind: "account", account: next };
    await fixture.current.verify();
    const fresh = fixture.avatar.snapshot();
    expect(fresh).toMatchObject({ kind: "loading", href: next.avatar.href });
    if (fresh.kind !== "loading") throw new Error("fresh loading");
    fixture.avatar.error(old.attempt);
    expect(fixture.avatar.snapshot()).toEqual(fresh);
    fixture.avatar.error(fresh.attempt);
    expect(fixture.avatar.snapshot()).toEqual({ kind: "failed", retry: true });
  });

  it("bounds how long a pending image may remain private work", async () => {
    const fixture = setup(() => ({ kind: "account", account: account() }), 20);
    await fixture.current.verify();
    expect(fixture.avatar.snapshot().kind).toBe("loading");
    await new Promise((resolve) => setTimeout(resolve, 40));
    expect(fixture.avatar.snapshot()).toEqual({ kind: "failed", retry: true });
  });
});
