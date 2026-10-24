import { afterEach, describe, expect, it, vi } from "vitest";
import {
  AFFILIATION_CODES,
  affiliationValues,
  createAffiliation,
  currentAffiliation,
  type NamedAffiliation,
} from "./affiliation";

const id = "00000000-0000-4000-8000-000000000001";
const affiliation = {
  value: "04" as const,
  createdAt: "2026-10-25T03:04:05+08:00",
};
const names = [
  "中共党员",
  "中共预备党员",
  "共青团员",
  "民革会员",
  "民盟盟员",
  "民建会员",
  "民进会员",
  "农工党党员",
  "致公党党员",
  "九三学社社员",
  "台盟盟员",
  "无党派民主人士",
  "群众",
];
const affiliations: NamedAffiliation[] = AFFILIATION_CODES.map((value, index) => ({
  value,
  name: names[index],
}));
afterEach(() => vi.unstubAllGlobals());

function respond(
  body: unknown,
  status = 200,
  type = "application/json",
  headers: Record<string, string> = {},
) {
  const fetch = vi.fn().mockResolvedValue(
    new Response(JSON.stringify(body), {
      status,
      headers: { "content-type": type, ...headers },
    }),
  );
  vi.stubGlobal("fetch", fetch);
  return fetch;
}

describe("current affiliation read", () => {
  it("projects only the trusted value and effective time", async () => {
    const fetch = respond({ code: "ok", affiliation });
    const signal = new AbortController().signal;
    expect(await currentAffiliation(id, signal)).toEqual({
      kind: "affiliation",
      affiliation,
    });
    expect(fetch).toHaveBeenCalledWith(
      "/api/v1/accounts/current/affiliation",
      expect.objectContaining({
        signal,
        credentials: "same-origin",
        cache: "no-store",
        redirect: "error",
        headers: {
          "Operation-Id": expect.stringMatching(
            /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i,
          ),
          "Session-Id": id,
          Accept: "application/json",
        },
      }),
    );
  });

  it("accepts only the trusted unset rejection", async () => {
    respond({ code: "affiliation_unset" }, 404);
    expect(await currentAffiliation(id, new AbortController().signal)).toEqual({
      kind: "unset",
    });
    respond({ code: "grant_absent" }, 404);
    expect(await currentAffiliation(id, new AbortController().signal)).toEqual({
      kind: "untrusted",
    });
    respond({ code: "affiliation_unset" }, 503);
    expect(await currentAffiliation(id, new AbortController().signal)).toEqual({
      kind: "untrusted",
    });
  });

  it.each([
    [{ code: "ok" }, 200, "application/json"],
    [{ code: "ok", affiliation: { ...affiliation, value: "14" } }, 200, "application/json"],
    [{ code: "ok", affiliation: { ...affiliation, extra: true } }, 200, "application/json"],
    [{ code: "ok", affiliation: { ...affiliation, createdAt: "not-a-date" } }, 200, "application/json"],
    [{ code: "ok", affiliation: { ...affiliation, createdAt: "2026-02-30T00:00:00Z" } }, 200, "application/json"],
    [{ code: "ok", affiliation, extra: true }, 200, "application/json"],
    [{ code: "ok", affiliation }, 200, "text/html"],
  ])("rejects malformed or untrusted success (%#)", async (body, status, type) => {
    respond(body, status as number, type as string);
    expect(await currentAffiliation(id, new AbortController().signal)).toEqual({
      kind: "untrusted",
    });
  });

  it("preserves retry metadata and disconnection without inventing unset", async () => {
    respond(
      { code: "rate_limited" },
      429,
      "application/json",
      { "Retry-After": "19" },
    );
    expect(await currentAffiliation(id, new AbortController().signal)).toEqual({
      kind: "failure",
      status: 429,
      code: "rate_limited",
      retryAfter: 19_000,
    });
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(Error("offline")));
    expect(await currentAffiliation(id, new AbortController().signal)).toEqual({
      kind: "disconnected",
    });
  });
});

describe("fixed affiliation vocabulary", () => {
  it("requires all 13 ascending code and name pairs", async () => {
    respond({ code: "ok", affiliations });
    expect(
      await affiliationValues(id, new AbortController().signal),
    ).toEqual({ kind: "values", affiliations });
  });

  it.each([
    [{ code: "ok", affiliations: affiliations.slice(1) }],
    [{ code: "ok", affiliations: [...affiliations, affiliations[0]] }],
    [{ code: "ok", affiliations: [...affiliations].reverse() }],
    [
      {
        code: "ok",
        affiliations: affiliations.map((entry, index) =>
          index === 3 ? { ...entry, name: names[2] } : entry,
        ),
      },
    ],
    [
      {
        code: "ok",
        affiliations: affiliations.map((entry, index) =>
          index === 5 ? { ...entry, name: `${entry.name} ` } : entry,
        ),
      },
    ],
    [
      {
        code: "ok",
        affiliations: affiliations.map((entry, index) =>
          index === 6 ? { ...entry, extra: true } : entry,
        ),
      },
    ],
    [{ code: "ok", affiliations, extra: true }],
    [{ code: "ok", affiliations: { entries: affiliations } }],
  ])("rejects incomplete, reordered or untrusted vocabulary (%#)", async (body) => {
    respond(body);
    expect(
      await affiliationValues(id, new AbortController().signal),
    ).toEqual({ kind: "untrusted" });
  });

  it("preserves a retryable failure", async () => {
    respond({ code: "dependency_unavailable" }, 503);
    expect(
      await affiliationValues(id, new AbortController().signal),
    ).toEqual({
      kind: "failure",
      status: 503,
      code: "dependency_unavailable",
      retryAfter: 0,
    });
  });
});

describe("first affiliation write", () => {
  const operation = "4ef1d157-e547-4d18-bf84-45a159709a4b";

  it("sends the exact original operation, session and body", async () => {
    const fetch = respond({ code: "ok" });
    const signal = new AbortController().signal;
    expect(await createAffiliation(operation, id, "04", signal)).toEqual({
      kind: "ok",
    });
    expect(fetch).toHaveBeenCalledWith(
      "/api/v1/accounts/current/affiliation",
      expect.objectContaining({
        method: "PUT",
        body: '{"affiliation":"04"}',
        signal,
        credentials: "same-origin",
        cache: "no-store",
        redirect: "error",
        headers: {
          "Operation-Id": operation,
          "Session-Id": id,
          "Content-Type": "application/json",
          Accept: "application/json",
        },
      }),
    );
  });

  it.each([
    [{ code: "ok", affiliation }, 200, "application/json"],
    [{ code: "not_ok" }, 200, "application/json"],
    [{ code: "ok" }, 200, "text/plain"],
  ])("never infers value, time or identity from a receipt (%#)", async (body, status, type) => {
    respond(body, status as number, type as string);
    expect(
      await createAffiliation(operation, id, "04", new AbortController().signal),
    ).toEqual({ kind: "untrusted" });
  });

  it("exposes each rejection as a failure without a result", async () => {
    for (const [status, code] of [
      [400, "invalid_input"],
      [401, "authentication_invalid"],
      [403, "permission_denied"],
      [409, "state_conflict"],
      [409, "request_mismatch"],
      [429, "rate_limited"],
      [503, "dependency_unavailable"],
    ] as const) {
      respond({ code }, status, "application/json", { "Retry-After": "7" });
      expect(
        await createAffiliation(
          operation,
          id,
          "04",
          new AbortController().signal,
        ),
      ).toEqual({
        kind: "failure",
        status,
        code,
        retryAfter: 7_000,
      });
    }
  });
});
