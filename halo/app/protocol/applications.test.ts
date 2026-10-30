import { afterEach, describe, expect, it, vi } from "vitest";
import {
  application,
  applications,
  applicationResource,
  previewApplication,
  submitApplication,
  sameApplicationContent,
} from "./applications";
import {
  applicationId,
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
