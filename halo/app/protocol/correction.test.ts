import { afterEach, describe, expect, it, vi } from "vitest";
import { history } from "./correction";

const id = "56ad73b5-d5b9-4cb7-aa22-7c446b97247e";
const record = {
  id,
  before: { name: "阿依古丽·阿不都拉", idNumber: { suffix: "0007" } },
  after: { name: "阿依古丽·阿不都热依木", idNumber: { suffix: "002X" } },
  correctedAt: "2026-10-23T01:25:00.123456Z",
};
const response = () => ({
  code: "ok",
  count: 1,
  corrections: [structuredClone(record)],
});
function serve(value: unknown, status = 200, headers = {}) {
  const fetcher = vi.fn().mockResolvedValue(
    new Response(JSON.stringify(value), {
      status,
      headers: { "content-type": "application/json", ...headers },
    }),
  );
  vi.stubGlobal("fetch", fetcher);
  return fetcher;
}
const read = (page = 1, size = 20) =>
  history(id, page, size, new AbortController().signal);
afterEach(() => vi.unstubAllGlobals());

describe("own correction history protocol", () => {
  it("reads only the selected session's endpoint and preserves canonical facts", async () => {
    const fetcher = serve(response());
    expect(await read()).toEqual({
      kind: "history",
      count: 1,
      corrections: [record],
    });
    const [url, options] = fetcher.mock.calls[0];
    expect(url).toBe("/api/v1/accounts/current/corrections?page=1&size=20");
    expect(options).toMatchObject({
      credentials: "same-origin",
      cache: "no-store",
      redirect: "error",
      headers: { "Session-Id": id, Accept: "application/json" },
    });
    expect(options.headers["Operation-Id"]).toMatch(
      /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
    );
    expect(options.body).toBeUndefined();
    expect(options.method).toBeUndefined();
  });

  it.each([
    [0, 1],
    [3, 3],
  ])("keeps trusted count %s on empty page %s", async (count, page) => {
    serve({ code: "ok", count, corrections: [] });
    expect(await read(page, 2)).toEqual({
      kind: "history",
      count,
      corrections: [],
    });
  });

  it.each([
    [401, "authentication_invalid"],
    [403, "permission_denied"],
    [429, "rate_limited"],
    [503, "dependency_unavailable"],
    [400, "invalid_input"],
  ])("keeps status %s separate from an empty history", async (status, code) => {
    serve({ code }, status, { "retry-after": "17" });
    expect(await read()).toEqual({
      kind: "failure",
      status,
      code,
      retryAfter: 17000,
    });
  });

  it.each([
    ["unknown code", { code: "new_failure" }, 503, {}],
    ["wrong status", { code: "authentication_invalid" }, 403, {}],
    ["wrong media", response(), 200, { "content-type": "text/html" }],
    ["unsupported status", response(), 201, {}],
    ["missing count", { code: "ok", corrections: [] }, 200, {}],
    ["unknown shape", { ...response(), operator: id }, 200, {}],
  ])("rejects %s", async (_, value, status, headers) => {
    serve(value, status, headers);
    expect(await read()).toEqual({ kind: "untrusted" });
  });

  it.each([-1, 0.5, Number.MAX_SAFE_INTEGER + 1, 9223372036854775807])(
    "rejects unusable count %s",
    async (count) => {
      serve({ ...response(), count });
      expect(await read()).toEqual({ kind: "untrusted" });
    },
  );

  it("accepts exact large counts without imposing a backend limit", async () => {
    serve({
      code: "ok",
      count: Number.MAX_SAFE_INTEGER,
      corrections: [record],
    });
    expect((await read(Number.MAX_SAFE_INTEGER, 1)).kind).toBe("history");
  });

  it.each([
    "",
    " leading",
    "trailing\u0085",
    "\u0085leading",
    "e\u0301",
    "a\nb",
    "a".repeat(65),
    "\ud800",
  ])(
    "rejects a noncanonical name %j without repairing history",
    async (name) => {
      const value = response();
      value.corrections[0].before.name = name;
      serve(value);
      expect(await read()).toEqual({ kind: "untrusted" });
    },
  );

  it.each(["000x", "123456789012345678", "１２３４", "123", "12AX"])(
    "rejects malformed suffix %s",
    async (suffix) => {
      const value = response();
      value.corrections[0].after.idNumber.suffix = suffix;
      serve(value);
      expect(await read()).toEqual({ kind: "untrusted" });
    },
  );

  it.each([
    "2026-02-30T00:00:00Z",
    "2026-10-23",
    "2026-10-23T25:00:00Z",
    "2026-10-23T00:00:60Z",
  ])("rejects invalid display time %s", async (correctedAt) => {
    const value = response();
    value.corrections[0].correctedAt = correctedAt;
    serve(value);
    expect(await read()).toEqual({ kind: "untrusted" });
  });

  it("rejects unknown per-record and identity fields, duplicate IDs and inconsistent pages", async () => {
    for (const value of [
      {
        ...response(),
        corrections: [{ ...record, operator: { name: "Hidden" } }],
      },
      {
        ...response(),
        corrections: [
          { ...record, before: { ...record.before, fullNumber: "hidden" } },
        ],
      },
      { ...response(), count: 2, corrections: [record, record] },
      { ...response(), count: 0 },
      { ...response(), count: 2 },
      { ...response(), corrections: [] },
    ]) {
      serve(value);
      expect(await read()).toEqual({ kind: "untrusted" });
    }
  });

  it("does not retry a disconnected read", async () => {
    const fetcher = vi.fn().mockRejectedValue(new TypeError("offline"));
    vi.stubGlobal("fetch", fetcher);
    expect(await read()).toEqual({ kind: "disconnected" });
    expect(fetcher).toHaveBeenCalledTimes(1);
  });
});
