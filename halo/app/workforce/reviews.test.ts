import { afterEach, expect, it, vi } from "vitest";
import { Current } from "../authentication/current";
import { Reviews } from "./reviews";
import type { ApplicationArtwork, TemplateArtwork } from "./artwork";
import {
  accountId,
  accountMaterial,
  applicationMaterial,
  applicationId as id,
} from "../../tests/application-material";

afterEach(() => vi.useRealTimers());
type API = NonNullable<ConstructorParameters<typeof Reviews>[1]["api"]>;
async function setup() {
  const item = applicationMaterial();
  const detail = {
    code: "ok",
    application: item,
    current: {
      name: item.snapshot.name,
      number: item.snapshot.number,
      accountActive: true,
      participationActive: true,
      department: { name: item.snapshot.department.name, enabled: true },
      position: null,
      title: null,
    },
  };
  const sessions = {
    read: vi.fn(async () => ({
      version: 1 as const,
      revision: id(90),
      session: { id: accountId, expiresAt: "2035-01-01T00:00:00Z" },
      attempt: null,
    })),
    clear: vi.fn(async () => true),
  };
  const identity = new Current({
    sessions,
    build: async () => true,
    current: async () => ({
      kind: "account",
      account: accountMaterial().account,
    }),
    now: Date.now,
  });
  const artwork = { close: vi.fn() } as unknown as ApplicationArtwork;
  const template = {
    close: vi.fn(),
    preview: vi.fn(async () => ({ kind: "ok", value: artwork })),
  } as unknown as TemplateArtwork;
  const api: API = {
    list: vi.fn<API["list"]>(async () => ({
      kind: "ok",
      value: { code: "ok", count: 0, applications: [] },
    })),
    detail: vi.fn<API["detail"]>(async () => ({ kind: "ok", value: detail })),
    review: vi.fn<API["review"]>(async () => ({ kind: "disconnected" })),
    cancellation: vi.fn<API["cancellation"]>(async () => ({
      kind: "disconnected",
    })),
    resource: vi.fn<API["resource"]>(async () => ({
      kind: "ok",
      value: {
        blob: new Blob(["fixture"], { type: "image/webp" }),
        mediaType: "image/webp",
      },
    })),
    artwork: vi.fn<API["artwork"]>(async () => ({
      kind: "ok",
      value: template,
    })),
  };
  const controller = new Reviews(
    { kind: "detail", id: item.id },
    { current: identity, build: async () => true, now: Date.now, api },
  );
  await identity.verify();
  await vi.waitFor(() =>
    expect(controller.snapshot()).toMatchObject({
      kind: "detail",
      media: { kind: "ready" },
    }),
  );
  return { controller, identity, api, sessions, artwork, template, detail };
}
it("a disconnected decision exposes only a fresh GET and never automatically resends", async () => {
  const { controller, api } = await setup();
  controller.confirm("approve");
  await controller.submit("");
  expect(controller.snapshot()).toMatchObject({ kind: "unknown" });
  expect(api.review).toHaveBeenCalledOnce();
  await controller.load();
  expect(controller.snapshot()).toMatchObject({
    kind: "detail",
    result: { application: { status: "submitted" } },
    notice: expect.stringContaining("当前仍待审核"),
  });
  expect(api.detail).toHaveBeenCalledTimes(2);
  expect(api.review).toHaveBeenCalledOnce();
  controller.close();
});
it("a failed GET after an unknown decision keeps that decision unknown until a trusted current read", async () => {
  vi.useFakeTimers();
  const { controller, api, detail } = await setup();
  controller.confirm("approve");
  await controller.submit("");
  vi.mocked(api.detail).mockResolvedValue({
    kind: "failure",
    status: 503,
    code: "dependency_unavailable",
    retryAfter: 0,
  });
  await controller.load();
  expect(controller.snapshot()).toMatchObject({
    kind: "unknown",
    message: "请刷新详情查看，暂时不要重复提交。",
  });
  controller.confirm("approve");
  await controller.submit("");
  expect(api.review).toHaveBeenCalledOnce();
  await vi.advanceTimersByTimeAsync(5001);
  vi.mocked(api.detail).mockResolvedValue({ kind: "ok", value: detail });
  await controller.load();
  expect(controller.snapshot()).toMatchObject({
    kind: "detail",
    result: { application: { status: "submitted" } },
    notice: "当前仍待审核，请查看申请内容后再作决定。",
  });
  expect(api.review).toHaveBeenCalledOnce();
  controller.close();
});
it("rejecting before media completes keeps the trusted result and does not leave an orphaned loading state", async () => {
  const { controller, api, detail } = await setup();
  let release!: (value: Awaited<ReturnType<API["artwork"]>>) => void;
  vi.mocked(api.artwork).mockImplementation(
    () =>
      new Promise((resolve) => {
        release = resolve;
      }),
  );
  const reading = controller.load();
  await vi.waitFor(() =>
    expect(controller.snapshot()).toMatchObject({
      kind: "detail",
      media: { kind: "loading" },
    }),
  );
  vi.mocked(api.review).mockResolvedValue({
    kind: "ok",
    value: {
      ...detail.application,
      status: "rejected",
      reviewerId: accountId,
      reviewerName: "林*",
      reviewedAt: "2026-10-31T01:15:00Z",
      rejectionReason: "需要正面照片",
    },
  });
  controller.confirm("reject");
  await controller.submit("需要正面照片");
  expect(controller.snapshot()).toMatchObject({
    kind: "detail",
    result: { application: { status: "rejected" } },
    media: { kind: "failed" },
    pending: false,
  });
  const discarded = { close: vi.fn() } as unknown as TemplateArtwork;
  release({ kind: "ok", value: discarded });
  await reading;
  expect(discarded.close).toHaveBeenCalledOnce();
  expect(controller.snapshot()).toMatchObject({
    kind: "detail",
    result: { application: { status: "rejected" } },
  });
  controller.close();
});
it("a late decision cannot reverse the deadline's unknown state or restore released media", async () => {
  vi.useFakeTimers();
  const { controller, api, artwork, template, detail } = await setup();
  let release!: (result: Awaited<ReturnType<API["review"]>>) => void;
  vi.mocked(api.review).mockImplementation(
    () =>
      new Promise((resolve) => {
        release = resolve;
      }),
  );
  controller.confirm("approve");
  const sending = controller.submit("");
  await vi.waitFor(() => expect(api.review).toHaveBeenCalledOnce());
  await vi.advanceTimersByTimeAsync(35001);
  expect(controller.snapshot()).toMatchObject({ kind: "unknown" });
  expect(artwork.close).toHaveBeenCalledOnce();
  expect(template.close).toHaveBeenCalledOnce();
  release({ kind: "ok", value: detail.application });
  await sending;
  expect(controller.snapshot()).toMatchObject({ kind: "unknown" });
  expect(api.review).toHaveBeenCalledOnce();
  controller.close();
});
it("permission rejection synchronously hides application and media without clearing a valid session", async () => {
  const { controller, identity, api, sessions, artwork } = await setup();
  vi.mocked(api.detail).mockResolvedValue({
    kind: "failure",
    status: 403,
    code: "permission_denied",
    retryAfter: 0,
  });
  await controller.load();
  expect(identity.snapshot()).toMatchObject({ kind: "denied" });
  expect(controller.snapshot()).toEqual({ kind: "hidden" });
  expect(artwork.close).toHaveBeenCalledOnce();
  expect(sessions.clear).not.toHaveBeenCalled();
  controller.close();
});
it("read limits retain a non-private retry state across foreground identity checks", async () => {
  vi.useFakeTimers();
  const { controller, identity, api, detail } = await setup();
  vi.mocked(api.detail).mockResolvedValue({
    kind: "failure",
    status: 429,
    code: "rate_limited",
    retryAfter: 12000,
  });
  await controller.load();
  expect(controller.snapshot()).toMatchObject({ kind: "error" });
  const reads = vi.mocked(api.detail).mock.calls.length;
  await controller.load();
  expect(api.detail).toHaveBeenCalledTimes(reads);
  identity.suspend();
  await identity.verify();
  expect(controller.snapshot()).toMatchObject({ kind: "error" });
  expect(api.detail).toHaveBeenCalledTimes(reads);
  await vi.advanceTimersByTimeAsync(12001);
  vi.mocked(api.detail).mockResolvedValue({ kind: "ok", value: detail });
  await controller.load();
  expect(api.detail).toHaveBeenCalledTimes(reads + 1);
  expect(controller.snapshot()).toMatchObject({ kind: "detail" });
  controller.close();
});
it("media limits block detail refresh and foreground reads while retaining explicit rejection", async () => {
  vi.useFakeTimers();
  const { controller, identity, api, detail } = await setup();
  vi.mocked(api.resource).mockResolvedValue({
    kind: "failure",
    status: 429,
    code: "rate_limited",
    retryAfter: 12000,
  });
  await controller.load();
  expect(controller.snapshot()).toMatchObject({
    kind: "detail",
    media: { kind: "failed" },
  });
  const reads = vi.mocked(api.detail).mock.calls.length;
  const media = vi.mocked(api.resource).mock.calls.length;
  await controller.load();
  expect(api.detail).toHaveBeenCalledTimes(reads);
  expect(api.resource).toHaveBeenCalledTimes(media);
  controller.confirm("reject");
  expect(controller.snapshot()).toMatchObject({ confirmation: "reject" });
  vi.mocked(api.review).mockResolvedValue({
    kind: "failure",
    status: 409,
    code: "state_changed",
    retryAfter: 0,
  });
  await controller.submit("需要正面照片");
  expect(api.review).toHaveBeenCalledOnce();
  expect(controller.snapshot()).toMatchObject({ kind: "error" });
  identity.suspend();
  await identity.verify();
  expect(controller.snapshot()).toMatchObject({ kind: "error" });
  expect(api.detail).toHaveBeenCalledTimes(reads);
  await vi.advanceTimersByTimeAsync(12001);
  vi.mocked(api.resource).mockResolvedValue({
    kind: "ok",
    value: { blob: new Blob(["fixture"]), mediaType: "image/webp" },
  });
  await controller.load();
  expect(controller.snapshot()).toMatchObject({
    kind: "detail",
    result: detail,
    media: { kind: "ready" },
  });
  controller.close();
});
it.each(["submitted", "rejected"] as const)(
  "a media deadline preserves trusted %s facts and releases a late resource",
  async (status) => {
    vi.useFakeTimers();
    const { controller, api, detail } = await setup();
    const value =
      status === "submitted"
        ? detail
        : {
            ...detail,
            application: {
              ...detail.application,
              status,
              reviewerId: accountId,
              reviewerName: "林*",
              reviewedAt: "2026-10-31T01:15:00Z",
              rejectionReason: "需要正面照片",
            },
          };
    vi.mocked(api.detail).mockResolvedValue({ kind: "ok", value });
    let release!: (result: Awaited<ReturnType<API["artwork"]>>) => void;
    vi.mocked(api.artwork).mockImplementation(
      () =>
        new Promise((resolve) => {
          release = resolve;
        }),
    );
    const reading = controller.load();
    await vi.waitFor(() =>
      expect(controller.snapshot()).toMatchObject({
        kind: "detail",
        media: { kind: "loading" },
      }),
    );
    await vi.advanceTimersByTimeAsync(35001);
    expect(controller.snapshot()).toMatchObject({
      kind: "detail",
      result: value,
      media: { kind: "failed" },
      pending: false,
    });
    if (status === "submitted") {
      controller.confirm("approve");
      expect(controller.snapshot()).not.toHaveProperty("confirmation");
      controller.confirm("reject");
      expect(controller.snapshot()).toMatchObject({ confirmation: "reject" });
    }
    const discarded = { close: vi.fn() } as unknown as TemplateArtwork;
    release({ kind: "ok", value: discarded });
    await reading;
    expect(discarded.close).toHaveBeenCalledOnce();
    expect(controller.snapshot()).toMatchObject({
      kind: "detail",
      result: value,
      media: { kind: "failed" },
    });
    expect(api.review).not.toHaveBeenCalled();
    controller.close();
  },
);
