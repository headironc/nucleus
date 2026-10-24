import { afterEach, describe, expect, it, vi } from "vitest";
import { register, validInput } from "./registration";

const operation = "9ea8cfed-85c5-4f44-8cf6-093a0f919b4c";
const input = {
  id: "019354fd-933c-7e6a-8912-345678901234",
  token: "A".repeat(43),
  name: "测试",
  idNumber: "11010519491231002X",
};
const session = {
  id: "019354fd-933c-7e6a-8912-345678901235",
  expiresAt: "2035-01-02T00:00:00Z",
};
afterEach(() => vi.unstubAllGlobals());
function response(status: number, body: unknown, type = "application/json") {
  return vi.stubGlobal(
    "fetch",
    vi.fn(
      async () =>
        new Response(JSON.stringify(body), {
          status,
          headers: { "Content-Type": type, "Retry-After": "17" },
        }),
    ),
  );
}
describe("registration wire contract", () => {
  it.each([200, 201])(
    "accepts the actual %i session and sends the original input once",
    async (status) => {
      response(status, { code: "ok", session });
      const signal = new AbortController().signal;
      expect(await register(operation, input, signal)).toEqual({
        kind: "session",
        session,
      });
      expect(fetch).toHaveBeenCalledExactlyOnceWith(
        "/api/v1/registrations",
        expect.objectContaining({
          method: "POST",
          body: JSON.stringify(input),
          signal,
          credentials: "same-origin",
          redirect: "error",
          headers: expect.objectContaining({ "Operation-Id": operation }),
        }),
      );
    },
  );
  it.each(["request_mismatch", "mobile_taken", "id_number_taken"])(
    "accepts only the documented 409 code %s",
    async (code) => {
      response(409, { code });
      expect(
        await register(operation, input, new AbortController().signal),
      ).toEqual({ kind: "failure", code, status: 409, retryAfter: 17000 });
    },
  );
  it.each([
    [201, { code: "rate_limited", session }],
    [
      200,
      { code: "ok", progress: { status: "pending", retryAfterSeconds: 5 } },
    ],
    [
      201,
      {
        code: "ok",
        session: { expiresAt: session.expiresAt, token: "A".repeat(43) },
      },
    ],
    [409, { code: "authentication_invalid" }],
    [429, { code: "invented" }],
  ])("keeps status/branch/code mismatch %i unknown", async (status, body) => {
    response(status as number, body);
    expect(
      await register(operation, input, new AbortController().signal),
    ).toEqual({ kind: "untrusted" });
  });
  it("rejects another media type and never retries transport failures", async () => {
    response(201, { code: "ok", session }, "text/plain");
    expect(
      await register(operation, input, new AbortController().signal),
    ).toEqual({ kind: "untrusted" });
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => {
        throw new TypeError("offline");
      }),
    );
    expect(
      await register(operation, input, new AbortController().signal),
    ).toEqual({ kind: "disconnected" });
    expect(fetch).toHaveBeenCalledTimes(1);
  });
  it("uses the generated input fields without adding derived mobile or source", () => {
    expect(validInput(input)).toBe(true);
    expect(validInput({ ...input, mobile: "13900000000" })).toBe(false);
    expect(validInput({ ...input, token: null })).toBe(false);
  });
});
