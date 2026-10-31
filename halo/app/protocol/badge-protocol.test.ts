import { afterEach, expect, it, vi } from "vitest";
import {
  application,
  applications,
  badgeResource,
  renderApplication,
} from "./applications";
import {
  applicationMaterial,
  accountId,
  applicationId,
} from "../../tests/application-material";

afterEach(() => vi.unstubAllGlobals());
const signal = () => new AbortController().signal;
function ready() {
  return {
    ...applicationMaterial(),
    status: "ready",
    reviewerId: accountId,
    reviewerName: "林*",
    reviewedAt: "2026-10-31T01:15:00Z",
    badge: { id: applicationId(91) },
  };
}
it("accepts the formal ready shape and rejects a missing product", async () => {
  const item = ready();
  const fetch = vi.fn(async () =>
    Response.json({ code: "ok", access: "full", application: item }),
  );
  vi.stubGlobal("fetch", fetch);
  expect((await application("selected", item.id, signal())).kind).toBe("ok");
  const { badge, ...withoutProduct } = item;
  fetch.mockImplementation(async () =>
    Response.json({ code: "ok", access: "full", application: withoutProduct }),
  );
  expect(await application("selected", item.id, signal())).toEqual({
    kind: "untrusted",
  });
});
it("projects ready restricted records to exactly the four permitted fields", async () => {
  const item = ready();
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
  const result = await applications("selected", 1, signal());
  expect(result.kind).toBe("ok");
  if (result.kind !== "ok") throw new Error("Expected restricted summary");
  expect(Object.keys(result.value.applications[0]).sort()).toEqual([
    "id",
    "kind",
    "status",
    "submittedAt",
  ]);
});
it("preserves a retained product on a full cancelled detail", async () => {
  const item = { ...ready(), status: "cancelled" };
  vi.stubGlobal(
    "fetch",
    vi.fn(async () =>
      Response.json({ code: "ok", access: "full", application: item }),
    ),
  );
  expect(await application("selected", item.id, signal())).toMatchObject({
    kind: "ok",
    value: { application: { status: "cancelled", badge: item.badge } },
  });
});
it("uses the declared generation route and an empty body without retrying a 503", async () => {
  const fetch = vi.fn(async () =>
    Response.json({ code: "outcome_unknown" }, { status: 503 }),
  );
  vi.stubGlobal("fetch", fetch);
  expect(
    await renderApplication("selected", applicationId(40), signal()),
  ).toMatchObject({ kind: "failure", status: 503, code: "outcome_unknown" });
  expect(fetch).toHaveBeenCalledOnce();
  expect(fetch.mock.calls[0]).toMatchObject([
    `/api/v1/workforce/applications/${applicationId(40)}/renders`,
    {
      method: "POST",
      body: "{}",
      credentials: "same-origin",
      cache: "no-store",
      redirect: "error",
      headers: { "Session-Id": "selected" },
    },
  ]);
});
it("performs a new protected GET on every byte read", async () => {
  const fetch = vi.fn(
    async () =>
      new Response(new Uint8Array([1, 2]), {
        headers: { "Content-Type": "image/webp", "Content-Length": "2" },
      }),
  );
  vi.stubGlobal("fetch", fetch);
  expect(
    (await badgeResource("selected", applicationId(91), signal())).kind,
  ).toBe("ok");
  expect(
    (await badgeResource("selected", applicationId(91), signal())).kind,
  ).toBe("ok");
  expect(fetch).toHaveBeenCalledTimes(2);
  expect(fetch.mock.calls[0]).toMatchObject([
    `/api/v1/workforce/badges/${applicationId(91)}.webp`,
    {
      credentials: "same-origin",
      cache: "no-store",
      redirect: "error",
      headers: { "Session-Id": "selected" },
    },
  ]);
});
