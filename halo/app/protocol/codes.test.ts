import { afterEach, describe, it, expect, vi } from "vitest";
import { send, validInput } from "./codes";
import { decode } from "./response";

afterEach(() => vi.unstubAllGlobals());

const message = {
  id: "d4ed08c1-ae0e-4184-96e5-38aa94081661",
  delivery: "sent",
  expiresAt: "2035-01-01T00:00:00Z",
};
const endpoint = "POST /api/v1/codes";
describe("C1 retry-after milliseconds", () => {
  it.each([
    [null, 0],
    ["", 0],
    ["0", 0],
    ["5", 5000],
    ["0005", 5000],
    ["9007199254740", 9007199254740000],
    ["9007199254741", 9007199254740991],
    ["9".repeat(400), 9007199254740991],
    ["-1", 0],
    ["+1", 0],
    ["1.5", 0],
    ["1e3", 0],
    ["Infinity", 0],
    ["5, 6", 0],
    ["Wed, 21 Oct 2035 07:28:00 GMT", 0],
  ])(
    "bounds the supplied delay %s without retrying",
    async (header, expected) => {
      const response = Response.json(
        { code: "rate_limited" },
        {
          status: 429,
          headers: header === null ? {} : { "Retry-After": header },
        },
      );
      const fetch = vi.fn().mockResolvedValue(response);
      vi.stubGlobal("fetch", fetch);
      expect(
        await send(
          "d4ed08c1-ae0e-4184-96e5-38aa94081661",
          { mobile: "13900000000", token: "proof" },
          new AbortController().signal,
        ),
      ).toEqual({
        kind: "failure",
        code: "rate_limited",
        retryAfter: expected,
      });
      expect(fetch).toHaveBeenCalledOnce();
    },
  );
});

describe("generated C1 response validation", () => {
  it.each([
    { status: 403, code: "permission_denied", field: "message", value: message },
    { status: 429, code: "rate_limited", field: "message", value: null },
    { status: 503, code: "outcome_unknown", field: "progress", value: { status: "pending", retryAfterSeconds: 5 } },
    { status: 503, code: "dependency_unavailable", field: "completion", value: {} },
    { status: 403, code: "permission_denied", field: "unknown", value: "fixture" },
  ])("rejects a C1 $status error containing $field without retrying", async ({ status, code, field, value }) => {
    const fetch = vi.fn().mockResolvedValue(Response.json({ code, [field]: value }, { status }));
    vi.stubGlobal("fetch", fetch);
    expect(await send(message.id, { mobile: "13900000000", token: "proof" }, new AbortController().signal)).toEqual({ kind: "untrusted" });
    expect(fetch).toHaveBeenCalledOnce();
  });

  it.each([
    { status: 403, code: "permission_denied" },
    { status: 429, code: "rate_limited" },
    { status: 503, code: "outcome_unknown" },
  ])("preserves a code-only C1 $status failure and header metadata", async ({ status, code }) => {
    const fetch = vi.fn().mockResolvedValue(Response.json({ code }, { status, headers: { "Retry-After": "3" } }));
    vi.stubGlobal("fetch", fetch);
    expect(await send(message.id, { mobile: "13900000000", token: "proof" }, new AbortController().signal)).toEqual({ kind: "failure", code, retryAfter: 3000 });
    expect(fetch).toHaveBeenCalledOnce();
  });

  it("accepts actual message variants and rejects absent, null or malformed required values", async () => {
    for (const delivery of ["sent", "sending", "unknown", "failed"]) {
      expect(
        await decode(
          Response.json({ code: "ok", message: { ...message, delivery } }),
          endpoint,
        ),
      ).toBeDefined();
    }
    for (const value of [
      { code: "ok" },
      { code: "ok", message: null },
      { code: "other", message },
      { code: "ok", message: { ...message, delivery: "delivered" } },
      { code: "ok", message: { ...message, id: "invalid" } },
      { code: "ok", message: { ...message, expiresAt: "tomorrow" } },
      { code: "ok", progress: {} },
    ])
      expect(await decode(Response.json(value), endpoint)).toBeUndefined();
  });
  it("rejects media, HTTP-code contradictions and unknown statuses", async () => {
    for (const response of [
      new Response(JSON.stringify({ code: "ok", message }), {
        headers: { "Content-Type": "text/html" },
      }),
      Response.json({ code: "ok", message }, { status: 503 }),
      Response.json({ code: "rate_limited" }, { status: 403 }),
      Response.json({ code: "invented" }, { status: 503 }),
      Response.json({ code: "ok", message }, { status: 201 }),
      new Response("{", { headers: { "Content-Type": "application/json" } }),
    ])
      expect(await decode(response, endpoint)).toBeUndefined();
    expect(
      await decode(
        Response.json({ code: "outcome_unknown" }, { status: 503 }),
        endpoint,
      ),
    ).toEqual({ code: "outcome_unknown" });
  });
  it("uses the exported input contract without trimming or adding fields", () => {
    expect(validInput({ mobile: "13900000000", token: "proof" })).toBe(true);
    for (const input of [
      { mobile: " 13900000000", token: "proof" },
      { mobile: "+8613900000000", token: "proof" },
      { mobile: "13900000000", token: "" },
      { mobile: "13900000000", token: "x".repeat(2049) },
      { mobile: "13900000000", token: "proof", purpose: "authentication" },
    ])
      expect(validInput(input)).toBe(false);
  });
});
