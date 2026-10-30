import { afterEach, describe, expect, it, vi } from "vitest";
import { terminate, workforce } from "./workforce";

const id = "00000000-0000-4000-8000-000000000001";
const identity = { personId: id, revisionId: id, name: "示*" };
const receipt = {
  code: "ok",
  grant: {
    id,
    subject: identity,
    operator: identity,
    grantedAt: "2026-10-23T00:00:00Z",
    sequence: 1,
    operationId: id,
    status: "active",
  },
};
afterEach(() => vi.unstubAllGlobals());
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
describe("self Workforce transport", () => {
  it.each(["active", "suspended", "terminated"])(
    "projects only %s from the generated contract",
    async (status) => {
      const fetch = respond({
        ...receipt,
        grant: { ...receipt.grant, status },
      });
      const signal = new AbortController().signal;
      expect(await workforce(id, signal)).toEqual({
        kind: "status",
        status,
        grant: { id, personId: id },
      });
      expect(fetch).toHaveBeenCalledWith(
        "/api/v1/workforce/current/grant",
        expect.objectContaining({
          signal,
          credentials: "same-origin",
          cache: "no-store",
          redirect: "error",
          headers: expect.objectContaining({ "Session-Id": id }),
        }),
      );
    },
  );
  it("accepts only the endpoint's trusted absence", async () => {
    respond({ code: "grant_absent" }, 404);
    expect(await workforce(id, new AbortController().signal)).toEqual({
      kind: "status",
      status: "absent",
      grant: null,
    });
  });
  it.each([
    [{ code: "role_absent" }, 404, "application/json"],
    [{ code: "grant_absent" }, 503, "application/json"],
    [receipt, 200, "text/html"],
    [
      { ...receipt, grant: { ...receipt.grant, status: "unknown" } },
      200,
      "application/json",
    ],
    [{ code: "ok", grant: { status: "active" } }, 200, "application/json"],
    [{ ...receipt, code: "not_ok" }, 200, "application/json"],
  ])(
    "rejects status, code, media and schema contradictions",
    async (body, status, type) => {
      respond(body, status as number, type as string);
      expect(await workforce(id, new AbortController().signal)).toEqual({
        kind: "untrusted",
      });
    },
  );
  it("preserves dependency and rate-limit failures without inventing absence", async () => {
    for (const [status, code] of [
      [503, "dependency_unavailable"],
      [429, "rate_limited"],
    ] as const) {
      respond({ code }, status, "application/json", { "Retry-After": "19" });
      expect(await workforce(id, new AbortController().signal)).toEqual({
        kind: "failure",
        status,
        code,
        retryAfter: 19_000,
      });
    }
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(Error("offline")));
    expect(await workforce(id, new AbortController().signal)).toEqual({
      kind: "disconnected",
    });
  });
});

describe("self termination transport", () => {
  const operation = "4ef1d157-e547-4d18-bf84-45a159709a4b";
  const session = "019354fd-933c-7e6a-8912-345678901234";
  const receipt = {
    code: "ok",
    termination: {
      id,
      subject: identity,
      operator: identity,
      mode: "self",
      terminatedAt: "2026-10-24T00:00:00Z",
      sequence: 2,
      operationId: operation,
    },
  };
  it("sends the empty-body POST to the exact Grant with the original identity", async () => {
    const fetch = respond(receipt, 201);
    const signal = new AbortController().signal;
    expect(await terminate(operation, id, id, session, signal)).toEqual({
      kind: "termination",
      terminatedAt: "2026-10-24T00:00:00Z",
    });
    expect(fetch).toHaveBeenCalledWith(
      `/api/v1/workforce/grants/${id}/terminations`,
      expect.objectContaining({
        method: "POST",
        signal,
        credentials: "same-origin",
        cache: "no-store",
        redirect: "error",
        headers: expect.objectContaining({
          "Operation-Id": operation,
          "Session-Id": session,
        }),
      }),
    );
    const init = fetch.mock.calls[0][1] as RequestInit;
    expect(init.body).toBeUndefined();
    expect(
      Object.keys(init.headers as Record<string, string>).map((name) =>
        name.toLowerCase(),
      ),
    ).not.toContain("content-type");
  });
  it.each([
    ["operation", { ...receipt, termination: { ...receipt.termination, operationId: session } }],
    ["grant", { ...receipt, termination: { ...receipt.termination, id: session } }],
    ["mode", { ...receipt, termination: { ...receipt.termination, mode: "manager" } }],
    [
      "subject",
      {
        ...receipt,
        termination: {
          ...receipt.termination,
          subject: { ...identity, personId: session },
        },
      },
    ],
    [
      "operator",
      {
        ...receipt,
        termination: {
          ...receipt.termination,
          operator: { ...identity, personId: session },
        },
      },
    ],
    ["code", { ...receipt, code: "not_ok" }],
  ])("rejects a 201 that does not bind the original %s", async (_, body) => {
    respond(body, 201);
    expect(
      await terminate(operation, id, id, session, new AbortController().signal),
    ).toEqual({ kind: "untrusted" });
  });
  it.each([
    [{ code: "rate_limited" }, 429, "rate_limited"],
    [{ code: "request_mismatch" }, 409, "request_mismatch"],
    [{ code: "target_unavailable" }, 404, "target_unavailable"],
    [{ code: "state_conflict" }, 409, "state_conflict"],
  ])("preserves the server rejection %j", async (body, status, code) => {
    respond(body, status as number, "application/json", { "Retry-After": "7" });
    expect(
      await terminate(operation, id, id, session, new AbortController().signal),
    ).toEqual({
      kind: "failure",
      status,
      code,
      retryAfter: 7_000,
    });
  });
  it("rejects a success delivered with the wrong media type", async () => {
    respond(receipt, 201, "text/html");
    expect(
      await terminate(operation, id, id, session, new AbortController().signal),
    ).toEqual({ kind: "untrusted" });
  });
  it("reports a lost connection without inventing a result", async () => {
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(Error("offline")));
    expect(
      await terminate(operation, id, id, session, new AbortController().signal),
    ).toEqual({ kind: "disconnected" });
  });
});
