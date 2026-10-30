import { afterEach, describe, expect, it, vi } from "vitest";
import { revoke, readRevocations } from "./revocation";

const operation = "b4e3bb16-fec5-4d86-9d2b-d172b8ea5000";
const original = "a4e3bb16-fec5-4d86-9d2b-d172b8ea5000";
const current = "c4e3bb16-fec5-4d86-9d2b-d172b8ea5000";
const revokedAt = "2026-10-23T00:00:00Z";
const signal = new AbortController().signal;
afterEach(() => vi.unstubAllGlobals());
function respond(body: unknown, status = 200, type = "application/json") {
  const fetch = vi.fn(
    async () =>
      new Response(JSON.stringify(body), {
        status,
        headers: { "Content-Type": type, "Retry-After": "19" },
      }),
  );
  vi.stubGlobal("fetch", fetch);
  return fetch;
}

describe("the current logout protocol consumer", () => {
  it("sends one bodyless DELETE bound to the original operation and selected session", async () => {
    const completion = { id: operation, count: 1, revokedAt };
    const fetch = respond({ code: "ok", completion });
    expect(await revoke(operation, original, signal)).toEqual({
      kind: "complete",
      completion,
    });
    expect(fetch).toHaveBeenCalledExactlyOnceWith("/api/v1/sessions/current", {
      method: "DELETE",
      headers: {
        "Operation-Id": operation,
        "Session-Id": original,
        Accept: "application/json",
      },
      credentials: "same-origin",
      cache: "no-store",
      redirect: "error",
      signal,
    });
  });
  it.each([
    { id: current, count: 1, revokedAt },
    { id: operation, count: 2, revokedAt },
    { id: operation, count: 1 },
    { id: operation, count: 1, revokedAt: "yesterday" },
  ])("rejects an unbound or malformed completion %j", async (completion) => {
    respond({ code: "ok", completion });
    expect(await revoke(operation, original, signal)).toEqual({
      kind: "untrusted",
    });
  });
  it.each([
    [200, "unknown"],
    [401, "ok"],
    [503, "invented_error"],
  ])("rejects status %s with code %s", async (status, code) => {
    respond({ code }, Number(status));
    expect(await revoke(operation, original, signal)).toEqual({
      kind: "untrusted",
    });
  });
  it("preserves a valid refusal and backoff without retry", async () => {
    const fetch = respond({ code: "rate_limited" }, 429);
    expect(await revoke(operation, original, signal)).toEqual({
      kind: "failure",
      status: 429,
      code: "rate_limited",
      retryAfter: 19_000,
    });
    expect(fetch).toHaveBeenCalledTimes(1);
  });
  it("uses a fresh query identifier and the original operation in the query string", async () => {
    const collection = {
      code: "ok",
      original: { scope: "current" as const, initiatorId: original },
      count: 1,
      revocations: [{ id: original, revokedAt }],
    };
    const fetch = respond(collection);
    expect(await readRevocations(operation, original, current, signal)).toEqual(
      { kind: "collection", collection },
    );
    const [url, options] = fetch.mock.calls[0] as unknown as [
      string,
      RequestInit,
    ];
    expect(url).toBe(`/api/v1/revocations?operation_id=${operation}`);
    expect(options.method).toBeUndefined();
    expect((options.headers as Record<string, string>)["Session-Id"]).toBe(
      current,
    );
    expect(
      (options.headers as Record<string, string>)["Operation-Id"],
    ).not.toBe(operation);
  });
  it("retains the distinction between an empty result and failure", async () => {
    const collection = {
      code: "ok",
      original: null,
      count: 0,
      revocations: [],
    };
    respond(collection);
    expect(await readRevocations(operation, original, current, signal)).toEqual(
      { kind: "collection", collection },
    );
  });
  it("does not reinterpret a singleton all logout as current logout", async () => {
    respond({
      code: "ok",
      original: { scope: "all", initiatorId: original },
      count: 1,
      revocations: [{ id: original, revokedAt }],
    });
    expect(await readRevocations(operation, original, current, signal)).toEqual(
      { kind: "untrusted" },
    );
  });
  it.each([
    {
      code: "ok",
      original: { scope: "current" as const, initiatorId: original },
      count: 1,
      revocations: [],
    },
    {
      code: "ok",
      original: { scope: "current" as const, initiatorId: original },
      count: 1,
      revocations: [{ id: current, revokedAt }],
    },
    {
      code: "ok",
      original: { scope: "current" as const, initiatorId: original },
      count: 2,
      revocations: [
        { id: original, revokedAt },
        { id: current, revokedAt },
      ],
    },
  ])(
    "rejects a collection that does not prove exactly the original current logout %j",
    async (collection) => {
      respond(collection);
      expect(
        await readRevocations(operation, original, current, signal),
      ).toEqual({ kind: "untrusted" });
    },
  );
  it("does not trust HTML from a proxy", async () => {
    respond({}, 200, "text/html");
    expect(await revoke(operation, original, signal)).toEqual({
      kind: "untrusted",
    });
  });
});
