import { afterEach, describe, expect, it, vi } from "vitest";
import { currentNumber } from "./number";

const id = "00000000-0000-4000-8000-000000000001";
const value = { id, subtype: 902, year: 2026, serial: 5 };
function respond(
  body: unknown,
  status = 200,
  type = "application/json",
  headers = {},
) {
  const fetch = vi
    .fn()
    .mockResolvedValue(
      new Response(JSON.stringify(body), {
        status,
        headers: { "content-type": type, ...headers },
      }),
    );
  vi.stubGlobal("fetch", fetch);
  return fetch;
}
const read = () => currentNumber(id, new AbortController().signal);
afterEach(() => vi.unstubAllGlobals());
describe("current workforce number protocol", () => {
  it.each([
    [100, "临床"],
    [200, "护理"],
    [300, "医技"],
    [400, "药剂"],
    [500, "财务"],
    [600, "行政"],
    [700, "后勤"],
    [801, "第三方公司"],
    [802, "保安"],
    [803, "物业"],
    [804, "志愿者"],
    [901, "见习"],
    [902, "实习"],
    [903, "进修生"],
    [904, "学生"],
    [1000, "临时人员"],
  ])("preserves fixed subtype %i as %s", async (subtype, name) => {
    respond({ code: "ok", number: { ...value, subtype } });
    expect(await read()).toMatchObject({
      kind: "number",
      number: { subtype: name },
    });
  });
  it.each([
    [902, 2026, 5, "92605"],
    [1000, 2026, 1, "102601"],
    [100, 1, 9, "10109"],
    [100, 1900, 100, "100100"],
    [100, 2_147_483_647, 999, "147999"],
  ])(
    "formats complete stored values without a current-year rule",
    async (subtype, year, serial, display) => {
      const fetch = respond({
        code: "ok",
        number: { ...value, subtype, year, serial },
      });
      expect(await read()).toEqual({
        kind: "number",
        number: {
          display,
          subtype:
            subtype === 1000 ? "临时人员" : subtype === 902 ? "实习" : "临床",
        },
      });
      expect(fetch).toHaveBeenCalledWith(
        "/api/v1/workforce/current/numbers",
        expect.objectContaining({
          credentials: "same-origin",
          cache: "no-store",
          redirect: "error",
          headers: expect.objectContaining({
            "Session-Id": id,
            "Operation-Id": expect.any(String),
          }),
        }),
      );
    },
  );
  it.each([
    {},
    { number: undefined },
    { code: "ok", number: {} },
    { code: "unknown", number: null },
    ...[0, 800, 805, 905, 1001, 100.5].map((subtype) => ({
      code: "ok",
      number: { ...value, subtype },
    })),
    ...[0, -1, 2.5, 2_147_483_648].map((year) => ({
      code: "ok",
      number: { ...value, year },
    })),
    ...[0, -1, 1000, 1.5].map((serial) => ({
      code: "ok",
      number: { ...value, serial },
    })),
    { code: "ok", number: { ...value, id: "bad" } },
  ])("rejects malformed or invalid values %j", async (body) => {
    respond(body);
    expect(await read()).toEqual({ kind: "untrusted" });
  });
  it("keeps a trusted null distinct from failure", async () => {
    respond({ code: "ok", number: null });
    expect(await read()).toEqual({ kind: "number", number: null });
    respond({ code: "dependency_unavailable" }, 503);
    expect(await read()).toEqual({
      kind: "failure",
      status: 503,
      code: "dependency_unavailable",
      retryAfter: 0,
    });
    respond({ code: "rate_limited" }, 429, "application/json", {
      "Retry-After": "61",
    });
    expect(await read()).toMatchObject({ kind: "failure", retryAfter: 61_000 });
    respond({ code: "ok", number: null }, 200, "text/html");
    expect(await read()).toEqual({ kind: "untrusted" });
    respond({ code: "ok", number: null }, 503);
    expect(await read()).toEqual({ kind: "untrusted" });
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new Error("offline")));
    expect(await read()).toEqual({ kind: "disconnected" });
  });
});
