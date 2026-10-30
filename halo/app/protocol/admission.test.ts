import { afterEach, describe, expect, it, vi } from "vitest";
import { admit, admissionCandidate, institutionRole } from "./admission";
const id = "00000000-0000-4000-8000-000000000001";
const other = "00000000-0000-4000-8000-000000000002";
const role = {
  id,
  changeId: id,
  kind: "default",
  name: "普通成员",
  level: 1,
  permissions: [],
};
const identity = { personId: other, revisionId: other, name: "同*" };
const input = {
  entry: "institution" as const,
  target: {
    accountId: other,
    personId: other,
    revisionId: other,
    mobile: "13800000002",
    assignmentId: other,
    suspensionId: null,
    grant: null,
  },
  role: { id, changeId: id },
};
function receipt() {
  const grant = {
    id: other,
    subject: identity,
    operator: { personId: id, revisionId: other, name: "办*" },
    operationId: id,
    grantedAt: "2026-10-30T00:00:00Z",
    sequence: 3,
    status: "terminated",
  };
  return {
    code: "ok",
    grant,
    assignment: {
      id,
      grantId: other,
      subject: identity,
      operator: grant.operator,
      role,
      operationId: id,
      assignedAt: grant.grantedAt,
      sequence: 1,
    },
  };
}
function respond(body: unknown, status = 201) {
  const fetch = vi
    .fn()
    .mockResolvedValue(
      new Response(JSON.stringify(body), {
        status,
        headers: { "content-type": "application/json" },
      }),
    );
  vi.stubGlobal("fetch", fetch);
  return fetch;
}
afterEach(() => vi.unstubAllGlobals());
describe("admission transport", () => {
  it("accepts fixed original identities with later current status, and sends exact institution input", async () => {
    const fetch = respond(receipt());
    const signal = new AbortController().signal;
    expect(await admit(id, input, id, other, signal)).toMatchObject({
      kind: "complete",
    });
    expect(fetch).toHaveBeenCalledWith(
      "/api/v1/workforce/grants",
      expect.objectContaining({
        body: JSON.stringify(input),
        signal,
        cache: "no-store",
        redirect: "error",
        headers: expect.objectContaining({
          "Operation-Id": id,
          "Session-Id": other,
        }),
      }),
    );
  });
  it.each([
    "operation",
    "operator",
    "subject",
    "role",
    "grant",
    "first-assignment",
  ])("rejects contradictory %s receipts", async (mismatch) => {
    const result = receipt();
    if (mismatch === "operation") result.grant.operationId = other;
    if (mismatch === "operator") result.grant.operator.personId = other;
    if (mismatch === "subject") result.grant.subject.revisionId = id;
    if (mismatch === "role")
      result.assignment.role = { ...role, changeId: other };
    if (mismatch === "grant") result.assignment.grantId = id;
    if (mismatch === "first-assignment") result.assignment.sequence = 2;
    respond(result);
    expect(
      await admit(id, input, id, id, new AbortController().signal),
    ).toEqual({ kind: "untrusted" });
  });
  it("does not turn dependencies or malformed candidate pages into empty results", async () => {
    respond({ code: "dependency_unavailable" }, 503);
    expect(
      await admissionCandidate(
        id,
        "13800000002",
        1,
        new AbortController().signal,
      ),
    ).toMatchObject({ kind: "failure", code: "dependency_unavailable" });
    respond(
      { code: "ok", role: { ...role, permissions: ["institution.workforce"] } },
      200,
    );
    expect(
      await institutionRole(id, new AbortController().signal),
    ).toMatchObject({ kind: "role" });
    respond({ code: "ok", count: 0, roles: [] }, 200);
    expect(
      await admissionCandidate(
        id,
        "13800000002",
        1,
        new AbortController().signal,
      ),
    ).toEqual({ kind: "untrusted" });
  });
});
