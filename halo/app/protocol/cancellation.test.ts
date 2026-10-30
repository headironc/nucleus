import { afterEach, describe, expect, it, vi } from "vitest";
import { cancel, validInput, validResult } from "./cancellation";

const sessionId = "f9bd2899-fb4d-4212-a831-15168b01e783";
const operation = "3f8a6a6e-1b2c-4d5e-8f90-123456789abc";
const accountId = "0b6e3f8a-3c2d-4e5f-8a9b-0c1d2e3f4a5b";
const personId = "1c7f4a9b-4d3e-4f60-9b0c-1d2e3f4a5b6c";
const revisionId = "2d8a5b0c-5e4f-4071-8c1d-2e3f4a5b6c7d";
const input = { accountId, personId, revisionId };
const result = {
  id: "9e2f1a3b-6c7d-4e8f-9a0b-1c2d3e4f5a6b",
  cancelledAt: "2026-10-25T12:34:56Z",
};

afterEach(() => vi.unstubAllGlobals());

describe("current account cancellation protocol", () => {
  it("accepts only the exact trusted target projection", () => {
    expect(validInput(input)).toBe(true);
    for (const value of [
      null,
      {},
      { ...input, extra: true },
      { ...input, accountId: "not-an-id" },
      { ...input, personId: undefined },
      { ...input, revisionId: 1 },
      { accountId, personId },
    ])
      expect(validInput(value)).toBe(false);
  });

  it("posts the fixed operation, original session and exact body", async () => {
    const fetch = vi
      .fn()
      .mockResolvedValue(Response.json({ code: "ok", cancellation: result }));
    vi.stubGlobal("fetch", fetch);
    expect(
      await cancel(operation, sessionId, input, new AbortController().signal),
    ).toEqual({ kind: "cancelled", result });
    expect(fetch).toHaveBeenCalledOnce();
    const [path, init] = fetch.mock.calls[0];
    expect(path).toBe("/api/v1/accounts/current/cancellations");
    expect(init).toMatchObject({
      method: "POST",
      credentials: "same-origin",
      cache: "no-store",
      redirect: "error",
      body: JSON.stringify(input),
    });
    expect(init.headers).toEqual({
      "Operation-Id": operation,
      "Session-Id": sessionId,
      "Content-Type": "application/json",
      Accept: "application/json",
    });
  });

  it("does not send an invalid target", async () => {
    const fetch = vi.fn();
    vi.stubGlobal("fetch", fetch);
    expect(
      await cancel(
        operation,
        sessionId,
        { ...input, accountId: "invalid" },
        new AbortController().signal,
      ),
    ).toEqual({ kind: "untrusted" });
    expect(fetch).not.toHaveBeenCalled();
  });

  it("keeps the server-generated cancellation ID distinct from the operation", async () => {
    const fetch = vi
      .fn()
      .mockResolvedValue(Response.json({ code: "ok", cancellation: result }));
    vi.stubGlobal("fetch", fetch);
    const value = await cancel(
      operation,
      sessionId,
      input,
      new AbortController().signal,
    );
    expect(value).toEqual({ kind: "cancelled", result });
    if (value.kind !== "cancelled") throw new Error("unreachable");
    expect(value.result.id).not.toBe(operation);
  });

  it.each([
    [400, "invalid_input"],
    [401, "authentication_invalid"],
    [403, "permission_denied"],
    [409, "target_changed"],
    [409, "request_mismatch"],
    [413, "invalid_input"],
    [415, "invalid_input"],
    [422, "invalid_input"],
    [429, "rate_limited"],
    [500, "internal_error"],
    [503, "dependency_unavailable"],
    [503, "outcome_unknown"],
  ] as const)("returns the generated %s %s failure", async (status, code) => {
    const fetch = vi
      .fn()
      .mockResolvedValue(Response.json({ code }, { status }));
    vi.stubGlobal("fetch", fetch);
    expect(
      await cancel(operation, sessionId, input, new AbortController().signal),
    ).toEqual({ kind: "failure", status, code, retryAfter: 0 });
  });

  it("reads a bounded-integer Retry-After on a 429", async () => {
    const fetch = vi
      .fn()
      .mockResolvedValue(
        Response.json(
          { code: "rate_limited" },
          { status: 429, headers: { "Retry-After": "1" } },
        ),
      );
    vi.stubGlobal("fetch", fetch);
    expect(
      await cancel(operation, sessionId, input, new AbortController().signal),
    ).toEqual({
      kind: "failure",
      status: 429,
      code: "rate_limited",
      retryAfter: 1_000,
    });
  });

  it.each([
    [
      "wrong media type",
      () =>
        new Response(JSON.stringify({ code: "ok", cancellation: result }), {
          headers: { "Content-Type": "text/plain" },
        }),
    ],
    [
      "unknown code",
      () => Response.json({ code: "created", cancellation: result }),
    ],
    ["missing cancellation", () => Response.json({ code: "ok" })],
    [
      "missing ID",
      () =>
        Response.json({
          code: "ok",
          cancellation: { cancelledAt: result.cancelledAt },
        }),
    ],
    [
      "malformed ID",
      () =>
        Response.json({ code: "ok", cancellation: { ...result, id: "id" } }),
    ],
    [
      "malformed date",
      () =>
        Response.json({
          code: "ok",
          cancellation: { ...result, cancelledAt: "later" },
        }),
    ],
    [
      "invalid JSON",
      () =>
        new Response("{", { headers: { "Content-Type": "application/json" } }),
    ],
    [
      "status outside the contract",
      () =>
        Response.json({ code: "ok", cancellation: result }, { status: 201 }),
    ],
    [
      "failure code outside the status contract",
      () => Response.json({ code: "internal_error" }, { status: 400 }),
    ],
  ])("treats %s as untrusted", async (_name, response) => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(response()));
    expect(
      await cancel(operation, sessionId, input, new AbortController().signal),
    ).toEqual({ kind: "untrusted" });
  });

  it("treats a network failure or abort as a disconnected wait", async () => {
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new TypeError("offline")));
    expect(
      await cancel(operation, sessionId, input, new AbortController().signal),
    ).toEqual({ kind: "disconnected" });
    const controller = new AbortController();
    vi.stubGlobal(
      "fetch",
      vi.fn(
        (_path: string, init: RequestInit) =>
          new Promise((_resolve, reject) => {
            init.signal?.addEventListener("abort", () =>
              reject(new DOMException("aborted", "AbortError")),
            );
          }),
      ),
    );
    const pending = cancel(operation, sessionId, input, controller.signal);
    controller.abort();
    expect(await pending).toEqual({ kind: "disconnected" });
  });

  it("validates a saved receipt using the generated result contract", () => {
    expect(validResult(result)).toBe(true);
    for (const value of [
      null,
      {},
      { ...result, id: "invalid" },
      { ...result, cancelledAt: "later" },
      { ...result, id: undefined },
    ])
      expect(validResult(value)).toBe(false);
  });
});
