import { afterEach, describe, expect, it, vi } from "vitest";
import {
  application,
  applications,
  applicationResource,
  previewApplication,
  submitApplication,
  sameApplicationContent,
  managedApplications,
  managedApplication,
  reviewApplication,
} from "./applications";
import {
  applicationId,
  accountId,
  applicationMaterial,
  snapshotMaterial,
  photoMaterial,
} from "../../tests/application-material";

afterEach(() => vi.unstubAllGlobals());
const signal = () => new AbortController().signal;
const input = () => ({
  kind: "issuance" as const,
  selections: {
    departmentId: applicationId(5),
    rosterId: null,
    titleId: null,
    photoId: photoMaterial.photoId,
    pickup: snapshotMaterial().pickup,
  },
  expected: snapshotMaterial(),
});
describe("approval management reads", () => {
  it("consumes the formal camelCase decision fields and rejects a missing masked name or the withdrawn schema", async () => {
    const item = { ...applicationMaterial(), status: "approved", reviewerId: accountId, reviewerName: "林*", reviewedAt: "2026-10-31T01:15:00Z" };
    vi.stubGlobal("fetch", vi.fn(async () => Response.json({ code: "ok", application: item })));
    expect((await reviewApplication("selected", item.id, { decision: "approve" }, signal())).kind).toBe("ok");
    expect(await reviewApplication("selected", item.id, { decision: "reject", reason: "需更换照片" }, signal())).toEqual({ kind: "untrusted" });
    const { reviewerName, ...missingName } = item;
    vi.stubGlobal("fetch", vi.fn(async () => Response.json({ code: "ok", application: missingName })));
    expect(await reviewApplication("selected", item.id, { decision: "approve" }, signal())).toEqual({ kind: "untrusted" });
    const { reviewerId, reviewedAt, ...withoutReview } = missingName;
    vi.stubGlobal("fetch", vi.fn(async () => Response.json({ code: "ok", application: { ...withoutReview, reviewer_id: reviewerId, reviewed_at: reviewedAt, reviewerName } })));
    expect(await reviewApplication("selected", item.id, { decision: "approve" }, signal())).toEqual({ kind: "untrusted" });
    const reason = "请保留完整头部与肩部。\n\n请使用正面照片。";
    vi.stubGlobal("fetch", vi.fn(async () => Response.json({ code: "ok", access: "full", application: { ...item, status: "rejected", rejectionReason: reason } })));
    expect(await application("selected", item.id, signal())).toMatchObject({ kind: "ok", value: { application: { status: "rejected", reviewerName: "林*", rejectionReason: reason } } });
    vi.stubGlobal("fetch", vi.fn(async () => Response.json({ code: "ok", access: "restricted", count: 1, applications: [item] })));
    const restricted = await applications("selected", 1, signal());
    if (restricted.kind !== "ok") throw new Error("Expected trusted restricted summary");
    expect(Object.keys(restricted.value.applications[0]).sort()).toEqual(["id", "kind", "status", "submittedAt"]);
    vi.stubGlobal("fetch", vi.fn(async () => Response.json({ code: "ok", application: item }, { status: 201 })));
    expect(await submitApplication("selected", input(), signal())).toEqual({ kind: "untrusted" });
  });
  it("binds filters, page size, target ids and safe counts to the declared response", async () => {
    const item = applicationMaterial();
    const summary = {
      id: item.id,
      kind: item.kind,
      status: item.status,
      submittedAt: item.submittedAt,
      name: item.snapshot.name,
      printedNumber: item.snapshot.printedNumber,
      department: item.snapshot.department,
    };
    const fetch = vi.fn<typeof globalThis.fetch>(async () =>
      Response.json({ code: "ok", count: 1, applications: [summary] }),
    );
    vi.stubGlobal("fetch", fetch);
    expect(
      (await managedApplications("selected", 2, "submitted", signal())).kind,
    ).toBe("ok");
    expect(fetch.mock.calls[0][0]).toBe(
      "/api/v1/workforce/applications?page=2&size=20&status=submitted",
    );
    expect(
      await managedApplications("selected", 1, "approved", signal()),
    ).toEqual({ kind: "untrusted" });
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        Response.json({
          code: "ok",
          count: Number.MAX_SAFE_INTEGER + 1,
          applications: [],
        }),
      ),
    );
    expect(
      await managedApplications("selected", 1, "submitted", signal()),
    ).toEqual({ kind: "untrusted" });
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        Response.json({
          code: "ok",
          application: item,
          current: {
            name: item.snapshot.name,
            number: null,
            accountActive: false,
            participationActive: false,
            department: { name: item.snapshot.department.name, enabled: false },
            position: null,
            title: null,
          },
        }),
      ),
    );
    expect((await managedApplication("selected", item.id, signal())).kind).toBe(
      "ok",
    );
    expect(
      await managedApplication("selected", applicationId(99), signal()),
    ).toEqual({ kind: "untrusted" });
  });
  it("never accepts a terminal rejection through the restricted summary schema", async () => {
    const item = {
      id: applicationId(40),
      kind: "issuance",
      status: "rejected",
      submittedAt: "2026-10-30T18:00:00Z",
    };
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        Response.json({
          code: "ok",
          access: "restricted",
          count: 1,
          applications: [item],
        }),
      ),
    );
    expect(await applications("selected", 1, signal())).toEqual({
      kind: "untrusted",
    });
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        Response.json({ code: "ok", access: "restricted", application: item }),
      ),
    );
    expect(await application("selected", item.id, signal())).toEqual({
      kind: "untrusted",
    });
  });
});
describe("actual application contracts", () => {
  it.each([previewApplication, submitApplication])(
    "preserves a declared unavailable-photo rejection as a known failure",
    async (send) => {
      vi.stubGlobal(
        "fetch",
        vi.fn(async () =>
          Response.json({ code: "target_unavailable" }, { status: 404 }),
        ),
      );
      expect(await send("session", input(), signal())).toMatchObject({
        kind: "failure",
        status: 404,
        code: "target_unavailable",
      });
    },
  );
  it("accepts only a 201 containing the same confirmed snapshot", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        Response.json(
          { code: "ok", application: applicationMaterial() },
          { status: 201 },
        ),
      ),
    );
    expect((await submitApplication("session", input(), signal())).kind).toBe(
      "ok",
    );
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        Response.json(
          {
            code: "ok",
            application: {
              ...applicationMaterial(),
              snapshot: { ...snapshotMaterial(), name: "其他姓名" },
            },
          },
          { status: 201 },
        ),
      ),
    );
    expect(await submitApplication("session", input(), signal())).toEqual({
      kind: "untrusted",
    });
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        Response.json(
          { code: "ok", application: applicationMaterial() },
          { status: 200 },
        ),
      ),
    );
    expect(await submitApplication("session", input(), signal())).toEqual({
      kind: "untrusted",
    });
  });
  it("rejects a preview for a different photo or kind", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        Response.json({
          code: "ok",
          kind: "replacement",
          snapshot: snapshotMaterial(),
          photo: photoMaterial,
        }),
      ),
    );
    expect(await previewApplication("session", input(), signal())).toEqual({
      kind: "untrusted",
    });
  });
  it("binds a trusted preview to the selected department and pickup text", async () => {
    const original = input();
    const value = {
      code: "ok",
      kind: original.kind,
      snapshot: original.expected,
      photo: photoMaterial,
    };
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => Response.json(value)),
    );
    expect((await previewApplication("session", original, signal())).kind).toBe(
      "ok",
    );
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        Response.json({
          ...value,
          snapshot: {
            ...value.snapshot,
            department: { ...value.snapshot.department, id: applicationId(99) },
          },
        }),
      ),
    );
    expect(await previewApplication("session", original, signal())).toEqual({
      kind: "untrusted",
    });
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        Response.json({
          ...value,
          snapshot: { ...value.snapshot, pickup: "另一个窗口" },
        }),
      ),
    );
    expect(await previewApplication("session", original, signal())).toEqual({
      kind: "untrusted",
    });
  });
  it("preserves unknown writes and declared data_changed rather than retrying", async () => {
    const fetch = vi.fn(async () => {
      throw new Error("connection lost");
    });
    vi.stubGlobal("fetch", fetch);
    expect(await submitApplication("session", input(), signal())).toEqual({
      kind: "disconnected",
    });
    expect(fetch).toHaveBeenCalledOnce();
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        Response.json({ code: "data_changed" }, { status: 409 }),
      ),
    );
    expect(await submitApplication("session", input(), signal())).toMatchObject(
      { kind: "failure", code: "data_changed", status: 409 },
    );
  });
  it("does not interpret a read failure as an empty list and rejects unsafe counts", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        Response.json({ code: "dependency_unavailable" }, { status: 503 }),
      ),
    );
    expect(await applications("session", 1, signal())).toMatchObject({
      kind: "failure",
      status: 503,
    });
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        Response.json({
          code: "ok",
          access: "full",
          applications: [],
          count: Number.MAX_SAFE_INTEGER + 1,
        }),
      ),
    );
    expect(await applications("session", 1, signal())).toEqual({
      kind: "untrusted",
    });
  });
  it("retains only the allowed restricted summary and binds detail to its requested id", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        Response.json({
          code: "ok",
          access: "restricted",
          count: 1,
          applications: [applicationMaterial()],
        }),
      ),
    );
    const result = await applications("session", 1, signal());
    expect(result).toMatchObject({
      kind: "ok",
      value: { access: "restricted" },
    });
    if (result.kind === "ok")
      expect(Object.keys(result.value.applications[0]).sort()).toEqual([
        "id",
        "kind",
        "status",
        "submittedAt",
      ]);
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        Response.json({
          code: "ok",
          access: "full",
          application: applicationMaterial(),
        }),
      ),
    );
    expect(await application("session", applicationId(99), signal())).toEqual({
      kind: "untrusted",
    });
  });
  it("reads authenticated media without a correlation header and validates JSON failures", async () => {
    const fetch = vi.fn(
      async () =>
        new Response(new Uint8Array([1, 2, 3]), {
          headers: { "Content-Type": "font/otf" },
        }),
    );
    vi.stubGlobal("fetch", fetch);
    expect(
      (await applicationResource("session", applicationId(20), signal())).kind,
    ).toBe("ok");
    expect(fetch).toHaveBeenCalledWith(
      expect.any(String),
      expect.objectContaining({
        headers: { "Session-Id": "session" },
        cache: "no-store",
      }),
    );
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        Response.json({ code: "target_unavailable" }, { status: 404 }),
      ),
    );
    expect(
      await applicationResource("session", applicationId(20), signal()),
    ).toMatchObject({ kind: "failure", status: 404 });
  });
  it("compares actual content independently of object key ordering", () => {
    expect(
      sameApplicationContent(
        { a: 1, b: { c: [2, 3] } },
        { b: { c: [2, 3] }, a: 1 },
      ),
    ).toBe(true);
    expect(sameApplicationContent({ a: [2, 3] }, { a: [3, 2] })).toBe(false);
  });
});
