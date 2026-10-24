import { afterEach, describe, expect, it, vi } from "vitest";
import {
  complete,
  establish,
  read,
  sendCurrentCode,
  sendNewCode,
  validCompleteInput,
  validEstablishInput,
  validSendInput,
  validState,
} from "./mobile";

afterEach(() => vi.unstubAllGlobals());

const operation = "4ef1d157-e547-4d18-bf84-45a159709a4b";
const session = "ffffffff-ffff-ffff-ffff-ffffffffffff";
const messageId = "018f3c1a-0000-7000-8000-000000000002";
const transferId = "018f3c1a-0000-7000-8000-000000000003";
const message = {
  id: messageId,
  delivery: "sent",
  expiresAt: "2035-01-01T00:00:00Z",
};
const signal = new AbortController().signal;

function mock(response: Response) {
  const fetch = vi.fn().mockResolvedValue(response);
  vi.stubGlobal("fetch", fetch);
  return fetch;
}

describe("mobile change request binding", () => {
  it("sends the current-number code with its own session and exact body", async () => {
    const fetch = mock(Response.json({ code: "ok", message }));
    expect(
      await sendCurrentCode(
        operation,
        session,
        { mobile: "13900000000", token: "candidate" },
        signal,
      ),
    ).toEqual({ kind: "ok", value: message });
    expect(fetch).toHaveBeenCalledOnce();
    const [url, init] = fetch.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/v1/accounts/current/codes");
    expect(init.method).toBe("POST");
    expect(init.credentials).toBe("same-origin");
    expect(init.cache).toBe("no-store");
    expect(init.redirect).toBe("error");
    expect(init.body).toBe(
      JSON.stringify({ mobile: "13900000000", token: "candidate" }),
    );
    expect(init.headers).toEqual({
      "Operation-Id": operation,
      "Session-Id": session,
      "Content-Type": "application/json",
      Accept: "application/json",
    });
  });

  it("binds the new-number code to the transfer path", async () => {
    const fetch = mock(Response.json({ code: "ok", message }));
    expect(
      await sendNewCode(
        operation,
        session,
        transferId,
        { mobile: "13800000000", token: "candidate" },
        signal,
      ),
    ).toEqual({ kind: "ok", value: message });
    const [url, init] = fetch.mock.calls[0] as [string, RequestInit];
    expect(url).toBe(`/api/v1/mobile-changes/${transferId}/codes`);
    expect(init.method).toBe("POST");
    expect(JSON.parse(String(init.body))).toEqual({
      mobile: "13800000000",
      token: "candidate",
    });
  });

  it("establishes a transfer and rejects a non-201 success", async () => {
    const transfer = { id: transferId, expiresAt: "2035-01-01T00:00:00Z" };
    const fetch = mock(
      Response.json({ code: "ok", transfer }, { status: 201 }),
    );
    expect(
      await establish(
        operation,
        session,
        { messageId, code: "012345" },
        signal,
      ),
    ).toEqual({ kind: "ok", value: transfer });
    const [url, init] = fetch.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/v1/mobile-changes");
    expect(init.method).toBe("POST");
    expect(JSON.parse(String(init.body))).toEqual({
      messageId,
      code: "012345",
    });

    mock(Response.json({ code: "ok", transfer }));
    expect(
      await establish(
        operation,
        session,
        { messageId, code: "012345" },
        signal,
      ),
    ).toEqual({ kind: "untrusted" });
  });

  it("completes with PUT and requires the original transfer identity", async () => {
    const completion = {
      id: transferId,
      completedAt: "2035-01-01T00:00:00Z",
    };
    const fetch = mock(Response.json({ code: "ok", completion }));
    expect(
      await complete(
        operation,
        session,
        transferId,
        { mobile: "13800000000", messageId, code: "012345" },
        signal,
      ),
    ).toEqual({ kind: "ok", value: completion });
    const [url, init] = fetch.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/v1/accounts/current/mobile");
    expect(init.method).toBe("PUT");
    expect(JSON.parse(String(init.body))).toEqual({
      mobile: "13800000000",
      messageId,
      code: "012345",
    });

    for (const value of [
      { ...completion, id: "018f3c1a-0000-7000-8000-000000000004" },
      { ...completion, completedAt: "tomorrow" },
      { ...completion, mobile: "13800000000" },
    ]) {
      mock(Response.json({ code: "ok", completion: value }));
      expect(
        await complete(
          operation,
          session,
          transferId,
          { mobile: "13800000000", messageId, code: "012345" },
          signal,
        ),
      ).toEqual({ kind: "untrusted" });
    }
  });

  it("reads status with no body or query and enforces the exact branch", async () => {
    const fetch = mock(
      Response.json({
        code: "ok",
        transfer: { status: "pending", expiresAt: "2035-01-01T00:00:00Z" },
      }),
    );
    expect(await read(session, transferId, signal)).toEqual({
      kind: "ok",
      value: { status: "pending", expiresAt: "2035-01-01T00:00:00Z" },
    });
    const [url, init] = fetch.mock.calls[0] as [string, RequestInit];
    expect(url).toBe(`/api/v1/mobile-changes/${transferId}`);
    expect(init.method).toBeUndefined();
    expect(init.body).toBeUndefined();
    expect(init.headers).toEqual({
      "Operation-Id": expect.any(String),
      "Session-Id": session,
      Accept: "application/json",
    });
    expect(
      /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(
        String((init.headers as Record<string, string>)["Operation-Id"]),
      ),
    ).toBe(true);

    for (const value of [
      {
        status: "pending",
        expiresAt: "2035-01-01T00:00:00Z",
        completedAt: "2035-01-01T00:00:00Z",
      },
      { status: "completed" },
      { status: "completed", completedAt: "tomorrow" },
      { status: "expired", expiresAt: "2035-01-01T00:00:00Z" },
      { status: "pending", expiresAt: "2035-01-01T00:00:00Z", extra: true },
      { status: "unknown" },
    ]) {
      mock(Response.json({ code: "ok", transfer: value }));
      expect(await read(session, transferId, signal)).toEqual({
        kind: "untrusted",
      });
    }
  });
});

describe("mobile change failures", () => {
  it.each([
    ["400", "invalid_input"],
    ["401", "authentication_invalid"],
    ["403", "verification_invalid"],
    ["409", "request_mismatch"],
    ["410", "verification_required"],
    ["429", "rate_limited"],
    ["503", "outcome_unknown"],
  ])("preserves a %s %s failure and Retry-After", async (status, code) => {
    mock(
      Response.json(
        { code },
        {
          status: Number(status),
          headers: status === "429" ? { "Retry-After": "7" } : {},
        },
      ),
    );
    expect(
      await sendCurrentCode(
        operation,
        session,
        { mobile: "13900000000", token: "candidate" },
        signal,
      ),
    ).toEqual({
      kind: "failure",
      status: Number(status),
      code,
      retryAfter: status === "429" ? 7_000 : 0,
    });
  });

  it("rejects unknown codes, contradictory statuses, wrong media and missing fields", async () => {
    for (const response of [
      Response.json({ code: "invented" }, { status: 503 }),
      Response.json({ code: "rate_limited" }, { status: 403 }),
      new Response(JSON.stringify({ code: "ok", message }), {
        headers: { "Content-Type": "text/html" },
      }),
      Response.json({ code: "ok" }),
      Response.json({ code: "ok", message: { ...message, delivery: "done" } }),
      new Response("{", {
        status: 500,
        headers: { "Content-Type": "application/json" },
      }),
    ]) {
      mock(response);
      expect(
        await sendCurrentCode(
          operation,
          session,
          { mobile: "13900000000", token: "candidate" },
          signal,
        ),
      ).toEqual({ kind: "untrusted" });
    }
  });

  it("reports a disconnected request without retrying", async () => {
    const fetch = vi.fn().mockRejectedValue(new Error("network"));
    vi.stubGlobal("fetch", fetch);
    expect(await read(session, transferId, signal)).toEqual({
      kind: "disconnected",
    });
    expect(fetch).toHaveBeenCalledOnce();
  });
});

describe("generated mobile-change inputs", () => {
  it("accepts only the exact controlled fields", () => {
    expect(validSendInput({ mobile: "13900000000", token: "x" })).toBe(true);
    expect(validSendInput({ mobile: " 13900000000", token: "x" })).toBe(false);
    expect(validSendInput({ mobile: "13900000000", token: "" })).toBe(false);
    expect(
      validSendInput({ mobile: "13900000000", token: "x", purpose: "login" }),
    ).toBe(false);
    expect(validEstablishInput({ messageId, code: "012345" })).toBe(true);
    expect(validEstablishInput({ messageId, code: "12345" })).toBe(false);
    expect(validEstablishInput({ messageId, code: "0123456" })).toBe(false);
    expect(validEstablishInput({ messageId, code: "01234５" })).toBe(false);
    expect(
      validCompleteInput({
        mobile: "13800000000",
        messageId,
        code: "012345",
      }),
    ).toBe(true);
    expect(
      validCompleteInput({
        mobile: "13800000000",
        messageId,
        code: "012345",
        transferId,
      }),
    ).toBe(false);
  });

  it("validates status branches directly", () => {
    expect(
      validState({ status: "pending", expiresAt: "2035-01-01T00:00:00Z" }),
    ).toBe(true);
    expect(
      validState({ status: "completed", completedAt: "2035-01-01T00:00:00Z" }),
    ).toBe(true);
    expect(validState({ status: "expired" })).toBe(true);
    expect(
      validState({ status: "expired", completedAt: "2035-01-01T00:00:00Z" }),
    ).toBe(false);
  });
});
