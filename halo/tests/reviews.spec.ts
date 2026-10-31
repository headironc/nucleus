import { test, expect, type Page } from "@playwright/test";
import { build } from "vite";
import tailwindcss from "@tailwindcss/vite";
import { readFile } from "node:fs/promises";
import { createHash } from "node:crypto";
import { resolve } from "node:path";
import type {
  Application,
  ManagedApplicationDetail,
  ReviewInput,
  CancellationInput,
} from "../app/protocol/applications";
import {
  accountId,
  otherAccountId,
  accountMaterial,
  applicationId as id,
  applicationMaterial,
} from "./application-material";

const origin = "http://reviews.localhost";
const resources = new Map<
  string,
  { body: string | Buffer; contentType: string }
>();
let html = "",
  font: Buffer;
test.beforeAll(async () => {
  font = await readFile(
    resolve("../domain/tests/fixtures/fonts/NotoSansSC-Regular.otf"),
  );
  const protocol = await readFile(resolve("protocol/openapi.json"));
  console.info(
    `Approval protocol SHA256=${createHash("sha256").update(protocol).digest("hex")}; fixture font=${createHash("sha256").update(font).digest("hex")}`,
  );
  const result = await build({
    configFile: false,
    publicDir: false,
    plugins: [tailwindcss()],
    logLevel: "error",
    define: { __BUILD_ID__: JSON.stringify("reviews-fixture") },
    build: {
      write: false,
      emptyOutDir: false,
      cssCodeSplit: false,
      assetsInlineLimit: 0,
      rolldownOptions: {
        input: resolve("tests/fixtures/reviews.tsx"),
        output: { entryFileNames: "fixture.js" },
      },
    },
  });
  if (Array.isArray(result) || !("output" in result))
    throw new Error("Expected one fixture build");
  const styles: string[] = [];
  for (const item of result.output) {
    const contentType = item.fileName.endsWith(".js")
      ? "text/javascript"
      : item.fileName.endsWith(".css")
        ? "text/css"
        : item.fileName.endsWith(".svg")
          ? "image/svg+xml"
          : "image/png";
    resources.set("/" + item.fileName, {
      body: item.type === "chunk" ? item.code : Buffer.from(item.source),
      contentType,
    });
    if (contentType === "text/css")
      styles.push(`<link rel="stylesheet" href="/${item.fileName}">`);
  }
  html = `<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><link rel="icon" href="data:,">${styles.join("")}</head><body><div id="fixture"></div><script type="module" src="/fixture.js"></script></body></html>`;
});
function control() {
  const item = applicationMaterial();
  item.snapshot.department.name = "呼吸内科";
  item.snapshot.department.printed = "呼吸内科";
  item.snapshot.text.department[0].end = new TextEncoder().encode(
    "呼吸内科",
  ).byteLength;
  return {
    records: [item] as Application[],
    current: {
      name: item.snapshot.name,
      number: item.snapshot.number,
      accountActive: true,
      participationActive: true,
      department: { name: "呼吸内科", enabled: true },
      position: null,
      title: null,
    } as ManagedApplicationDetail["current"],
    denied: false,
    expired: false,
    restricted: false,
    grantFailure: false,
    mediaFailed: false,
    mediaLimited: false,
    readFailure: false,
    readDisconnected: false,
    limited: false,
    loseResponse: false,
    stateChanged: false,
    cancellationLost: false,
    cancellationRace: false,
    cancellationInvalid: false,
    cancellationStatus: 0,
    cancellationWrongId: false,
    cancellationReadStatus: 0,
    cancellationReadLost: false,
    cancellationGate: undefined as Promise<void> | undefined,
    cancellations: [] as CancellationInput[],
    gate: undefined as Promise<void> | undefined,
    ownGate: undefined as Promise<void> | undefined,
    decisionGate: undefined as Promise<void> | undefined,
    mediaGate: undefined as Promise<void> | undefined,
    photo: Buffer.alloc(0),
    background: Buffer.alloc(0),
    listQueries: [] as string[],
    ownReads: 0,
    detailReads: 0,
    resourceReads: [] as string[],
    decisions: [] as ReviewInput[],
    renders: 0,
    renderStatus: 0,
    renderCode: "dependency_unavailable",
    renderLost: false,
    renderReadFailure: false,
    renderGate: undefined as Promise<void> | undefined,
    badgeFailure: 0,
    managementUnavailable: false,
    badgeGate: undefined as Promise<void> | undefined,
    badgeReads: [] as { session: string | undefined; path: string }[],
  };
}
type Control = ReturnType<typeof control>;
function reviewed(
  item: Application,
  decision: "approve" | "reject",
  reason = "",
  reviewerId = accountId,
): Application {
  const review = {
    reviewerId,
    reviewerName: reviewerId === accountId ? "林*" : "周*",
    reviewedAt: "2026-10-31T01:15:00Z",
  };
  return decision === "approve"
    ? { ...item, ...review, status: "approved" }
    : {
        ...item,
        ...review,
        status: "rejected",
        rejectionReason: reason.trim(),
      };
}
async function install(page: Page, state: Control) {
  await page.route("**/*", async (route) => {
    const request = route.request(),
      url = new URL(request.url()),
      path = url.pathname;
    if (url.origin !== origin) return route.abort();
    if (path === "/build.json")
      return route.fulfill({ json: { buildId: "reviews-fixture" } });
    const session = request.headers()["session-id"];
    if (path === "/api/v1/accounts/current")
      return route.fulfill(
        state.expired
          ? { status: 401, json: { code: "authentication_invalid" } }
          : { json: accountMaterial(session) },
      );
    if (path === "/api/v1/workforce/current/grant") {
      if (state.grantFailure)
        return route.fulfill({
          status: 503,
          json: { code: "dependency_unavailable" },
        });
      return route.fulfill({
        json: {
          code: "ok",
          grant: {
            id: id(60),
            subject: { personId: session, revisionId: id(50), name: "测*" },
            operator: { personId: accountId, revisionId: id(50), name: "测*" },
            grantedAt: "2026-10-30T00:00:00Z",
            operationId: id(61),
            sequence: 1,
            status: state.restricted ? "suspended" : "active",
          },
        },
      });
    }
    if (path.endsWith("/reviews") && request.method() === "POST")
      state.decisions.push(request.postDataJSON() as ReviewInput);
    if (path.endsWith("/cancellations") && request.method() === "POST")
      state.cancellations.push(request.postDataJSON() as CancellationInput);
    if (path.startsWith("/api/")) {
      if (state.expired)
        return route.fulfill({
          status: 401,
          json: { code: "authentication_invalid" },
        });
      if (state.denied || session !== accountId)
        return route.fulfill({
          status: 403,
          json: { code: "permission_denied" },
        });
      if (state.limited)
        return route.fulfill({
          status: 429,
          headers: { "Retry-After": "7" },
          json: { code: "rate_limited" },
        });
      if (state.readFailure)
        return route.fulfill({
          status: 503,
          json: { code: "dependency_unavailable" },
        });
      if (state.readDisconnected && request.method() === "GET")
        return route.abort("connectionfailed");
    }
    if (path.endsWith("/renders") && request.method() === "POST") {
      state.renders++;
      expect(request.postDataJSON()).toEqual({});
      if (state.renderGate) await state.renderGate;
      const index = state.records.findIndex((item) =>
        path.endsWith(`/${item.id}/renders`),
      );
      const item = state.records[index];
      if (state.renderStatus) {
        if (
          state.renderStatus === 422 ||
          state.renderCode === "dependency_unavailable"
        )
          state.records[index] = {
            ...(reviewed(item, "approve") as Extract<
              Application,
              { status: "approved" }
            >),
            status: "approved",
            renderError: state.renderCode as
              "missing_glyph" | "text_overflow" | "dependency_unavailable",
          };
        if (state.renderReadFailure) state.readFailure = true;
        return route.fulfill({
          status: state.renderStatus,
          json: { code: state.renderCode },
        });
      }
      const { renderError: _error, ...approved } = reviewed(
        item,
        "approve",
      ) as Extract<Application, { status: "approved" }>;
      state.records[index] = {
        ...approved,
        status: "ready",
        badge: { id: id(91) },
      };
      if (state.renderLost) return route.abort("connectionfailed");
      return route.fulfill({
        json: {
          code: "ok",
          application: { id: item.id, status: "ready", badge: { id: id(91) } },
        },
      });
    }
    if (path.startsWith("/api/v1/workforce/badges/")) {
      state.badgeReads.push({ session, path });
      if (state.badgeGate) await state.badgeGate;
      if (state.badgeFailure)
        return route.fulfill({
          status: state.badgeFailure,
          json: {
            code:
              state.badgeFailure === 404
                ? "target_unavailable"
                : state.badgeFailure === 403
                  ? "permission_denied"
                  : "dependency_unavailable",
          },
        });
      return route.fulfill({
        contentType: "image/webp",
        headers: {
          "Content-Length": String(state.background.length),
          "Cache-Control": "no-store",
          "Content-Disposition": `attachment; filename="badge-${id(91)}.webp"`,
        },
        body: state.background,
      });
    }
    if (path.endsWith("/cancellations") && request.method() === "POST") {
      const item = state.records.find((item) =>
        path.endsWith(`/${item.id}/cancellations`),
      );
      if (!item)
        return route.fulfill({
          status: 404,
          json: { code: "target_unavailable" },
        });
      if (state.cancellationGate) await state.cancellationGate;
      if (state.cancellationRace) {
        state.cancellationRace = false;
        state.records[state.records.indexOf(item)] = reviewed(
          item,
          "approve",
          "",
        );
        return route.fulfill({ status: 409, json: { code: "state_changed" } });
      }
      if (state.cancellationInvalid)
        return route.fulfill({ status: 422, json: { code: "invalid_input" } });
      if (state.cancellationStatus)
        return route.fulfill({
          status: state.cancellationStatus,
          headers: { "Retry-After": "9" },
          json: {
            code:
              state.cancellationStatus === 429
                ? "rate_limited"
                : "outcome_unknown",
          },
        });
      item.status = "cancelled";
      if (state.cancellationLost) return route.abort("connectionfailed");
      return route.fulfill({
        json: {
          code: "ok",
          application: {
            id: state.cancellationWrongId ? id(99) : item.id,
            status: "cancelled",
          },
        },
      });
    }
    if (path.endsWith("/reviews") && request.method() === "POST") {
      const index = state.records.findIndex((item) =>
        path.endsWith(`/${item.id}/reviews`),
      );
      if (index < 0)
        return route.fulfill({
          status: 404,
          json: { code: "target_unavailable" },
        });
      const item = state.records[index],
        input = state.decisions.at(-1)!;
      if (state.stateChanged) {
        state.records[index] = reviewed(
          item,
          "reject",
          "申请已由另一位审核人处理，请重新准备正面照片。",
          otherAccountId,
        );
        return route.fulfill({ status: 409, json: { code: "state_changed" } });
      }
      if (item.status !== "submitted")
        return route.fulfill({ status: 409, json: { code: "state_changed" } });
      if (
        input.decision === "approve" &&
        (!state.current.accountActive || !state.current.participationActive)
      )
        return route.fulfill({
          status: 409,
          json: { code: "applicant_unavailable" },
        });
      if (state.decisionGate) await state.decisionGate;
      state.records[index] = reviewed(
        item,
        input.decision,
        input.decision === "reject" ? input.reason : "",
      );
      if (state.loseResponse) return route.abort("connectionfailed");
      return route.fulfill({
        json: { code: "ok", application: state.records[index] },
      });
    }
    if (
      path === "/api/v1/workforce/applications" &&
      request.method() === "GET"
    ) {
      state.listQueries.push(url.search);
      const status = url.searchParams.get("status") ?? "submitted",
        page = Number(url.searchParams.get("page") ?? 1);
      const matching = state.records.filter((item) => item.status === status);
      return route.fulfill({
        json: {
          code: "ok",
          count: matching.length,
          applications: matching
            .slice((page - 1) * 20, page * 20)
            .map(({ id, kind, status, submittedAt, snapshot }) => ({
              id,
              kind,
              status,
              submittedAt,
              name: snapshot.name,
              printedNumber: snapshot.printedNumber,
              department: snapshot.department,
            })),
        },
      });
    }
    if (path.startsWith("/api/v1/workforce/application-resources/")) {
      state.resourceReads.push(path);
      if (state.mediaGate) await state.mediaGate;
      if (state.mediaLimited)
        return route.fulfill({
          status: 429,
          headers: { "Retry-After": "12" },
          json: { code: "rate_limited" },
        });
      if (state.mediaFailed)
        return route.fulfill({
          status: 404,
          json: { code: "target_unavailable" },
        });
      if (path.endsWith(id(20)))
        return route.fulfill({
          contentType: "image/webp",
          body: state.background,
        });
      if (path.endsWith(id(21)))
        return route.fulfill({ contentType: "font/otf", body: font });
      if (path.endsWith(id(31)))
        return route.fulfill({ contentType: "image/webp", body: state.photo });
      throw new Error(
        "Unexpected resource outside this application's fixed references",
      );
    }
    if (
      path.startsWith("/api/v1/workforce/applications/") &&
      request.method() === "GET"
    ) {
      state.detailReads++;
      if (state.managementUnavailable) {
        if (state.gate) await state.gate;
        return route.fulfill({
          status: 403,
          json: { code: "permission_denied" },
        });
      }
      const item = state.records.find((item) => path.endsWith(item.id));
      if (!item)
        return route.fulfill({
          status: 404,
          json: { code: "target_unavailable" },
        });
      const value = structuredClone({
        code: "ok",
        application: item,
        current: state.current,
      });
      if (state.cancellations.length && item.status === "cancelled") {
        if (state.cancellationReadLost) return route.abort("connectionfailed");
        if (state.cancellationReadStatus)
          return route.fulfill({
            status: state.cancellationReadStatus,
            json: {
              code:
                state.cancellationReadStatus === 404
                  ? "target_unavailable"
                  : state.cancellationReadStatus === 403
                    ? "permission_denied"
                    : state.cancellationReadStatus === 429
                      ? "rate_limited"
                      : "dependency_unavailable",
            },
          });
      }
      if (state.gate) await state.gate;
      return route.fulfill({ json: value });
    }
    if (
      path === "/api/v1/workforce/current/applications" ||
      path.startsWith("/api/v1/workforce/current/applications/")
    ) {
      state.ownReads++;
      const matching = state.records.filter(
        (item) =>
          !state.restricted ||
          item.status === "submitted" ||
          item.status === "approved" ||
          item.status === "ready",
      );
      // The adapter must strip accidental extra private fields even from a restricted response.
      const selected = matching.find((item) => path.endsWith(item.id));
      if (state.cancellations.length && selected?.status === "cancelled") {
        if (state.cancellationReadLost) return route.abort("connectionfailed");
        if (state.cancellationReadStatus)
          return route.fulfill({
            status: state.cancellationReadStatus,
            json: {
              code:
                state.cancellationReadStatus === 404
                  ? "target_unavailable"
                  : state.cancellationReadStatus === 403
                    ? "permission_denied"
                    : state.cancellationReadStatus === 429
                      ? "rate_limited"
                      : "dependency_unavailable",
            },
          });
      }
      if (path !== "/api/v1/workforce/current/applications" && !selected)
        return route.fulfill({
          status: 404,
          json: { code: "target_unavailable" },
        });
      const body =
        path === "/api/v1/workforce/current/applications"
          ? { count: matching.length, applications: matching }
          : { application: selected };
      const value = structuredClone({
        code: "ok",
        access: state.restricted ? "restricted" : "full",
        ...body,
      });
      if (state.ownGate) await state.ownGate;
      return route.fulfill({ json: value });
    }
    const resource = resources.get(path);
    if (resource) return route.fulfill(resource);
    if (!path.startsWith("/api/"))
      return route.fulfill({ contentType: "text/html", body: html });
    throw new Error(`Unexpected API ${request.method()} ${path}`);
  });
  await page.goto(origin);
  const pixels = await page.evaluate(() => {
    const image = (width: number, height: number, photo: boolean) => {
      const canvas = document.createElement("canvas");
      canvas.width = width;
      canvas.height = height;
      const context = canvas.getContext("2d")!;
      context.fillStyle = photo ? "#cedace" : "#e4e9df";
      context.fillRect(0, 0, width, height);
      if (photo) {
        context.fillStyle = "#365c4e";
        context.beginPath();
        context.arc(width / 2, height / 3, width / 5, 0, Math.PI * 2);
        context.fill();
        context.fillRect(width / 4, height / 2, width / 2, height / 2);
      }
      return canvas.toDataURL("image/webp").split(",")[1];
    };
    return {
      photo: image(800, 1120, true),
      background: image(850, 540, false),
    };
  });
  state.photo = Buffer.from(pixels.photo, "base64");
  state.background = Buffer.from(pixels.background, "base64");
  await chooseSession(page, accountId);
}
async function chooseSession(page: Page, account: string) {
  await page.evaluate(async (id) => {
    const store = window.reviewSessions,
      attempt = await store.begin(crypto.randomUUID());
    if (
      !(await store.accept(attempt, { id, expiresAt: "2035-01-01T00:00:00Z" }))
    )
      throw new Error("Selection changed");
    window.dispatchEvent(new Event("focus"));
  }, account);
}

function readyRecord(item: Application): Application {
  return {
    ...reviewed(item, "approve"),
    status: "ready",
    badge: { id: id(91) },
  } as Application;
}

for (const manager of [false, true]) {
  test(`WebP download404 conceals private material before rechecking access, manager=${manager}`, async ({
    page,
  }) => {
    const state = control();
    state.records[0] = readyRecord(state.records[0]);
    let releasePreview!: () => void;
    state.mediaGate = new Promise<void>((resolve) => {
      releasePreview = resolve;
    });
    await install(page, state);
    await page.goto(
      origin +
        (manager ? "/workforce/reviews/" : "/workforce/applications/") +
        state.records[0].id,
    );
    const image = page.getByRole("img", { name: "已保存的工牌图片" });
    await expect(image).toBeVisible();
    const source = await image.getAttribute("src");
    let release!: () => void;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    state.badgeFailure = 404;
    if (manager) {
      state.managementUnavailable = true;
      state.gate = gate;
    } else {
      state.restricted = true;
      state.ownGate = gate;
    }
    await page.getByRole("button", { name: "下载图片", exact: true }).click();
    await expect(page.getByRole("img")).toHaveCount(0);
    expect(
      await page.evaluate(async (url) => {
        try {
          await fetch(url!);
          return true;
        } catch {
          return false;
        }
      }, source),
    ).toBe(false);
    await expect(
      page.getByText(state.records[0].snapshot.name, { exact: true }),
    ).toHaveCount(0);
    release();
    if (manager)
      await expect(
        page.getByText("目前无法查看工牌审核。", { exact: true }),
      ).toBeVisible();
    else
      await expect(
        page.getByText("当前仅可查看申请类型、状态和提交时间。", {
          exact: true,
        }),
      ).toBeVisible();
    releasePreview();
    await expect(page.getByRole("img")).toHaveCount(0);
    expect(state.badgeReads).toHaveLength(2);
    expect(
      (await page.evaluate(() => window.reviewSessions.read()))?.session?.id,
    ).toBe(accountId);
  });
}

test("WebP deadline preserves a completed badge while submitted resources remain stalled", async ({
  page,
}) => {
  await page.clock.install();
  const state = control();
  state.records[0] = readyRecord(state.records[0]);
  let release!: () => void;
  state.mediaGate = new Promise<void>((resolve) => {
    release = resolve;
  });
  await install(page, state);
  await page.goto(origin + "/workforce/reviews/" + state.records[0].id);
  const image = page.getByRole("img", { name: "已保存的工牌图片" });
  await expect(image).toBeVisible();
  await page.clock.fastForward(36000);
  await expect(image).toBeVisible();
  await expect(
    page.getByText("工牌预览暂时无法显示，请重新加载。", { exact: true }),
  ).toBeVisible();
  release();
  await expect(image).toBeVisible();
  expect(state.renders).toBe(0);
});

test("WebP generation refreshes actual detail and every download performs a new authorized GET", async ({
  page,
}, info) => {
  const state = control();
  state.records[0] = reviewed(state.records[0], "approve");
  await install(page, state);
  await page.goto(origin + "/workforce/reviews/" + state.records[0].id);
  await page.getByRole("button", { name: "生成工牌图片", exact: true }).click();
  await expect(
    page.getByRole("img", { name: "已保存的工牌图片" }),
  ).toBeVisible();
  expect(state.renders).toBe(1);
  expect(state.detailReads).toBeGreaterThanOrEqual(2);
  expect(state.badgeReads).toHaveLength(1);
  for (let count = 2; count <= 3; count++) {
    const downloaded = page.waitForEvent("download");
    await page.getByRole("button", { name: "下载图片", exact: true }).click();
    const file = await downloaded;
    expect(file.suggestedFilename()).toBe(`badge-${id(91)}.webp`);
    expect(await readFile((await file.path())!)).toEqual(state.background);
    expect(state.badgeReads).toHaveLength(count);
  }
  expect(
    state.badgeReads.every(
      (read) =>
        read.session === accountId && read.path.endsWith(id(91) + ".webp"),
    ),
  ).toBe(true);
  await page.screenshot({
    path: info.outputPath("webp-manager-ready.png"),
    fullPage: true,
  });
});

test("WebP 503 refreshes first, conceals uncertain content and never resends generation", async ({
  page,
}) => {
  await page.clock.install();
  const state = control();
  state.records[0] = reviewed(state.records[0], "approve");
  state.renderStatus = 503;
  state.renderCode = "outcome_unknown";
  state.renderReadFailure = true;
  await install(page, state);
  await page.goto(origin + "/workforce/reviews/" + state.records[0].id);
  await page.getByRole("button", { name: "生成工牌图片", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "暂时无法确认生成结果" }),
  ).toBeVisible();
  await expect(page.getByRole("img")).toHaveCount(0);
  expect(state.renders).toBe(1);
  state.readFailure = false;
  state.renderStatus = 0;
  await page.clock.fastForward(6000);
  await page.getByRole("button", { name: "刷新详情", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "生成工牌图片", exact: true }),
  ).toBeVisible();
  expect(state.renders).toBe(1);
  await page.getByRole("button", { name: "生成工牌图片", exact: true }).click();
  await expect(
    page.getByRole("img", { name: "已保存的工牌图片" }),
  ).toBeVisible();
  expect(state.renders).toBe(2);
});

test("WebP confirmed missing glyph is read from refreshed detail and remains a manual retry", async ({
  page,
}) => {
  const state = control();
  state.records[0] = reviewed(state.records[0], "approve");
  state.renderStatus = 422;
  state.renderCode = "missing_glyph";
  await install(page, state);
  await page.goto(origin + "/workforce/reviews/" + state.records[0].id);
  await page.getByRole("button", { name: "生成工牌图片", exact: true }).click();
  await expect(
    page.getByText("部分文字无法显示，工牌图片未生成。", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "重试生成", exact: true }),
  ).toBeEnabled();
  expect(state.detailReads).toBeGreaterThanOrEqual(2);
  expect(state.renders).toBe(1);
  let release!: () => void;
  state.renderGate = new Promise<void>((resolve) => {
    release = resolve;
  });
  state.renderStatus = 0;
  await page.getByRole("button", { name: "重试生成", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "正在生成工牌图片", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText("部分文字无法显示，工牌图片未生成。", { exact: true }),
  ).toHaveCount(0);
  release();
  await expect(
    page.getByRole("img", { name: "已保存的工牌图片" }),
  ).toBeVisible();
});

test("WebP download failure preserves the ready image and retries only a new file GET", async ({
  page,
}) => {
  await page.clock.install();
  const state = control();
  state.records[0] = readyRecord(state.records[0]);
  await install(page, state);
  await page.goto(origin + "/workforce/reviews/" + state.records[0].id);
  const image = page.getByRole("img", { name: "已保存的工牌图片" });
  await expect(image).toBeVisible();
  const source = await image.getAttribute("src");
  state.badgeFailure = 503;
  await page.getByRole("button", { name: "下载图片", exact: true }).click();
  await expect(
    page.getByText("暂时无法取得下载文件，请重新下载。", { exact: true }),
  ).toBeVisible();
  await expect(image).toHaveAttribute("src", source!);
  expect(state.records[0].status).toBe("ready");
  expect(state.renders).toBe(0);
  await page.clock.fastForward(6000);
  state.badgeFailure = 0;
  const downloaded = page.waitForEvent("download");
  await page.getByRole("button", { name: "重新下载", exact: true }).click();
  await downloaded;
  expect(state.badgeReads).toHaveLength(3);
  await expect(image).toHaveAttribute("src", source!);
});

for (const manager of [false, true]) {
  test(`WebP ready cancellation retains its stored image, manager=${manager}`, async ({
    page,
  }, info) => {
    const state = control();
    state.records[0] = readyRecord(state.records[0]);
    await install(page, state);
    if (!manager) {
      await page.setViewportSize({ width: 320, height: 740 });
      await page.emulateMedia({ colorScheme: "dark" });
    }
    await page.goto(
      origin +
        (manager ? "/workforce/reviews/" : "/workforce/applications/") +
        state.records[0].id,
    );
    await expect(
      page.getByRole("img", { name: "已保存的工牌图片" }),
    ).toBeVisible();
    await page.getByRole("button", { name: "取消申请", exact: true }).click();
    const dialog = page.getByRole("dialog");
    await expect(dialog).toContainText("工牌图片已生成，取消后仍会保留。");
    if (manager)
      await dialog.getByRole("textbox").fill("申请人确认不再办理本次工牌。");
    await dialog.getByRole("button", { name: "取消申请", exact: true }).click();
    await expect(
      page.getByRole("heading", { name: "工牌图片已保留", exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("img", { name: "已保存的工牌图片" }),
    ).toBeVisible();
    expect(state.cancellations[0]).toMatchObject({
      mode: manager ? "manager" : "self",
      expectedStatus: "ready",
    });
    await page.screenshot({
      path: info.outputPath(
        `webp-cancelled-${manager ? "manager" : "self-dark-320"}.png`,
      ),
      fullPage: true,
    });
  });
}

test("WebP restricted ready exposes four summary fields and never reads private files", async ({
  page,
}) => {
  const state = control();
  state.records[0] = readyRecord(state.records[0]);
  state.restricted = true;
  state.grantFailure = true;
  await install(page, state);
  await page.goto(origin + "/workforce/applications/" + state.records[0].id);
  await expect(
    page.getByText("当前仅可查看申请类型、状态和提交时间。", { exact: true }),
  ).toBeVisible();
  await expect(page.getByRole("img")).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "下载图片", exact: true }),
  ).toHaveCount(0);
  expect(state.badgeReads).toHaveLength(0);
  expect(state.resourceReads).toHaveLength(0);
  await page.getByRole("button", { name: "取消申请", exact: true }).click();
  await expect(page.getByRole("dialog").locator("dt")).toHaveText([
    "申请编号",
    "申请类型",
    "申请状态",
    "提交时间",
  ]);
  await page
    .getByRole("dialog")
    .getByRole("button", { name: "取消申请", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "申请已取消", exact: true }),
  ).toBeVisible();
  expect(state.cancellations[0]).toEqual({
    mode: "self",
    expectedStatus: "ready",
  });
});

test("WebP stored image remains readable when submitted preview resources fail", async ({
  page,
}) => {
  const state = control();
  state.records[0] = readyRecord(state.records[0]);
  state.mediaFailed = true;
  await install(page, state);
  await page.goto(origin + "/workforce/reviews/" + state.records[0].id);
  await expect(
    page.getByRole("img", { name: "已保存的工牌图片" }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "下载图片", exact: true }),
  ).toBeEnabled();
  await expect(
    page.getByText("工牌预览暂时无法显示，请重新加载。", { exact: true }),
  ).toBeVisible();
});

test("WebP hide revokes the visible image and rejects a late file response", async ({
  page,
}) => {
  const state = control();
  state.records[0] = readyRecord(state.records[0]);
  await install(page, state);
  await page.goto(origin + "/workforce/reviews/" + state.records[0].id);
  const image = page.getByRole("img", { name: "已保存的工牌图片" });
  await expect(image).toBeVisible();
  const source = await image.getAttribute("src");
  let release!: () => void;
  state.badgeGate = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.getByRole("button", { name: "下载图片", exact: true }).click();
  await expect.poll(() => state.badgeReads.length).toBe(2);
  await page.evaluate(() => window.dispatchEvent(new Event("pagehide")));
  await expect(page.getByRole("img")).toHaveCount(0);
  release();
  expect(
    await page.evaluate(async (url) => {
      try {
        await fetch(url!);
        return true;
      } catch {
        return false;
      }
    }, source),
  ).toBe(false);
  await expect(page.getByRole("img")).toHaveCount(0);
});

for (const [manager, status, disconnected] of [
  [false, 503, false],
  [true, 503, false],
  [false, 0, true],
  [true, 429, false],
  [false, 404, false],
  [true, 403, false],
] as const) {
  test(`WebP confirmed cancellation stays distinct from detail failure: manager=${manager}, status=${status}, disconnected=${disconnected}`, async ({
    page,
  }, info) => {
    await page.clock.install();
    const state = control();
    state.records[0] = readyRecord(state.records[0]);
    state.cancellationReadStatus = status;
    state.cancellationReadLost = disconnected;
    await install(page, state);
    await page.goto(
      origin +
        (manager ? "/workforce/reviews/" : "/workforce/applications/") +
        state.records[0].id,
    );
    await expect(
      page.getByRole("img", { name: "已保存的工牌图片" }),
    ).toBeVisible();
    await page.getByRole("button", { name: "取消申请", exact: true }).click();
    const dialog = page.getByRole("dialog");
    if (manager) await dialog.getByRole("textbox").fill("申请人确认停止办理。");
    await dialog.getByRole("button", { name: "取消申请", exact: true }).click();
    const notice = page.getByText("申请已取消，已生成的工牌图片仍保留。", {
      exact: true,
    });
    if (status === 403 || status === 404) {
      await expect(
        page.getByText(
          manager ? "目前无法查看工牌审核。" : "目前无法查看这份申请。",
          { exact: true },
        ),
      ).toBeVisible();
      await expect(notice).toHaveCount(0);
    } else {
      await expect(notice).toBeVisible();
      await expect(
        page.getByText("详情暂时无法加载，请刷新后重试。", { exact: true }),
      ).toBeVisible();
      await expect(
        page.getByRole("button", { name: /秒后可刷新/ }),
      ).toBeDisabled();
      await expect(page.getByRole("img")).toHaveCount(0);
      await page.screenshot({
        path: info.outputPath(
          `known-cancel-read-failure-${manager}-${status}-${disconnected}.png`,
        ),
        fullPage: true,
      });
      state.cancellationReadStatus = 0;
      state.cancellationReadLost = false;
      await page.clock.fastForward(6000);
      await page.getByRole("button", { name: "刷新详情", exact: true }).click();
      await expect(
        page.getByRole("heading", { name: "工牌图片已保留", exact: true }),
      ).toBeVisible();
    }
    expect(state.cancellations).toHaveLength(1);
    expect(
      (await page.evaluate(() => window.reviewSessions.read()))?.session?.id,
    ).toBe(accountId);
  });
}

for (const restricted of [false, true]) {
  for (const status of ["submitted", "approved"] as const) {
    test(`cancellation self ${status}, restricted=${restricted}: explicit confirmation and history projection`, async ({
      page,
    }, info) => {
      const state = control();
      state.restricted = restricted;
      state.grantFailure = restricted;
      if (status === "approved")
        state.records[0] = reviewed(state.records[0], "approve", "");
      const original = structuredClone(state.records[0]);
      await install(page, state);
      await page.goto(origin + "/workforce/applications/" + original.id);
      await page.getByRole("button", { name: "取消申请", exact: true }).click();
      const dialog = page.getByRole("dialog");
      await expect(dialog.getByRole("textbox")).toHaveCount(0);
      await expect(
        dialog.getByRole("button", { name: "保留申请" }),
      ).toBeFocused();
      if (restricted) {
        await expect(dialog.locator("dt")).toHaveText([
          "申请编号",
          "申请类型",
          "申请状态",
          "提交时间",
        ]);
        await expect(dialog).not.toContainText(original.snapshot.name);
        expect(state.resourceReads).toHaveLength(0);
      }
      await dialog
        .getByRole("button", { name: "取消申请", exact: true })
        .click();
      await expect(
        page.getByRole("heading", { name: "申请已取消", exact: true }),
      ).toBeVisible();
      expect(state.cancellations).toEqual([
        { mode: "self", expectedStatus: status },
      ]);
      await page.screenshot({
        path: info.outputPath(
          `self-${status}-${restricted ? "restricted" : "full"}-cancelled.png`,
        ),
      });
      await page.goto(origin + "/workforce/applications");
      if (restricted) {
        await expect(page.getByText("暂无可查看的工牌申请。")).toBeVisible();
        await expect(page.getByText("共 0 项 · 第 1 页")).toBeVisible();
      } else {
        await expect(page.locator(".application-list")).toContainText("已取消");
        await page.getByRole("link", { name: "查看详情" }).click();
        await expect(
          page.getByText(original.snapshot.name, { exact: true }),
        ).toBeVisible();
        await expect(
          page.getByRole("button", { name: "取消申请", exact: true }),
        ).toHaveCount(0);
        if (status === "approved") {
          await expect(page.getByText("取消前已审核通过。")).toBeVisible();
          await expect(page.getByText("林*", { exact: true })).toBeVisible();
        } else
          await expect(page.getByText("申请在审核前已取消。")).toBeVisible();
        await page.screenshot({
          path: info.outputPath(`self-${status}-history.png`),
        });
      }
    });
  }
}

for (const status of ["submitted", "approved"] as const) {
  test(`cancellation manager ${status}: inactive applicant, private reason and retained approval`, async ({
    page,
  }, info) => {
    const state = control();
    state.current.accountActive = false;
    state.current.participationActive = false;
    if (status === "approved")
      state.records[0] = reviewed(state.records[0], "approve", "");
    const original = structuredClone(state.records[0]);
    await install(page, state);
    await page.goto(origin + "/workforce/reviews/" + original.id);
    await page.getByRole("button", { name: "取消申请", exact: true }).click();
    const dialog = page.getByRole("dialog");
    await dialog.getByRole("button", { name: "取消申请", exact: true }).click();
    await expect(dialog.getByRole("alert")).toHaveText("请填写取消说明。");
    expect(state.cancellations).toHaveLength(0);
    await dialog
      .getByRole("textbox", { name: "取消说明" })
      .fill("重复申请，仅用于内部核对。");
    await dialog.getByRole("button", { name: "取消申请", exact: true }).click();
    await expect(
      page.getByRole("heading", { name: "申请已取消", exact: true }),
    ).toBeVisible();
    expect(state.cancellations).toEqual([
      {
        mode: "manager",
        expectedStatus: status,
        reason: "重复申请，仅用于内部核对。",
      },
    ]);
    await page.getByRole("button", { name: "查看申请记录" }).click();
    await expect(
      page.getByText(original.snapshot.name, { exact: true }),
    ).toBeVisible();
    await expect(
      page.getByText("重复申请，仅用于内部核对。", { exact: true }),
    ).toHaveCount(0);
    if (status === "approved") {
      await expect(page.getByText("取消前已审核通过。")).toBeVisible();
      await expect(page.getByText("林*", { exact: true })).toBeVisible();
    } else await expect(page.getByText("申请在审核前已取消。")).toBeVisible();
    await expect(
      page.getByText("申请已取消。申请人如需办理，须重新申请。"),
    ).toBeVisible();
    await page.screenshot({
      path: info.outputPath(`manager-${status}-history.png`),
    });
    await page.goto(origin + "/workforce/reviews");
    await page.getByRole("button", { name: "已取消", exact: true }).click();
    await expect(page.getByRole("table")).toContainText(original.snapshot.name);
    expect(state.listQueries.at(-1)).toBe("?page=1&size=20&status=cancelled");
  });
}

for (const mode of ["self", "manager"] as const) {
  for (const failedRead of ["disconnected", "503", "429"] as const) {
    test(`cancellation ${mode}: unknown GET ${failedRead} and cooldown retain the cancellation operation`, async ({
      page,
    }, info) => {
      const state = control();
      state.cancellationLost = true;
      await install(page, state);
      await page.clock.install();
      await page.goto(
        origin +
          `/workforce/${mode === "self" ? "applications" : "reviews"}/` +
          state.records[0].id,
      );
      await page.getByRole("button", { name: "取消申请", exact: true }).click();
      if (mode === "manager")
        await page.getByRole("textbox", { name: "取消说明" }).fill("重复申请");
      await page
        .getByRole("dialog")
        .getByRole("button", { name: "取消申请", exact: true })
        .click();
      await expect(
        page.getByRole("heading", { name: "暂时无法确认取消结果" }),
      ).toBeVisible();
      state.readDisconnected = failedRead === "disconnected";
      state.readFailure = failedRead === "503";
      state.limited = failedRead === "429";
      await page.getByRole("button", { name: "刷新详情", exact: true }).click();
      await expect(
        page.getByRole("button", { name: /秒后可刷新/ }),
      ).toBeDisabled();
      await expect(
        page.getByRole("heading", { name: "暂时无法确认取消结果" }),
      ).toBeVisible();
      await expect(page.getByRole("status")).toHaveText(
        "请刷新详情查看当前状态，暂时不要重复提交。",
      );
      await page.evaluate(() => window.dispatchEvent(new Event("focus")));
      await expect(
        page.getByRole("heading", { name: "暂时无法确认取消结果" }),
      ).toBeVisible();
      await expect(
        page.getByRole("button", { name: /秒后可刷新/ }),
      ).toBeDisabled();
      await expect(
        page.getByRole("heading", { name: "暂时无法确认审核结果" }),
      ).toHaveCount(0);
      expect(state.cancellations).toHaveLength(1);
      await page.screenshot({
        path: info.outputPath(`${mode}-unknown-get-${failedRead}.png`),
      });
    });
  }
}

for (const mode of ["self", "manager"] as const) {
  test(`cancellation ${mode}: a canvas failure cannot reopen a pending write`, async ({
    page,
  }) => {
    const state = control();
    await install(page, state);
    await page.goto(
      origin +
        `/workforce/${mode === "self" ? "applications" : "reviews"}/` +
        state.records[0].id,
    );
    await expect(page.getByRole("img", { name: "工牌预览" })).toBeVisible();
    let release!: () => void;
    state.cancellationGate = new Promise<void>((resolve) => {
      release = resolve;
    });
    await page.getByRole("button", { name: "取消申请", exact: true }).click();
    if (mode === "manager")
      await page.getByRole("textbox", { name: "取消说明" }).fill("重复申请");
    await page
      .getByRole("dialog")
      .getByRole("button", { name: "取消申请", exact: true })
      .click();
    await expect.poll(() => state.cancellations.length).toBe(1);
    await page.evaluate(async () => {
      const canvas = document.querySelector<HTMLCanvasElement>("canvas")!;
      canvas.getContext = () => {
        throw new Error("Controlled canvas failure");
      };
      canvas.style.width = "100px";
      await new Promise<void>((resolve) =>
        requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
      );
    });
    await expect(
      page.getByRole("dialog").getByRole("button", { name: "保留申请" }),
    ).toBeDisabled();
    expect(state.cancellations).toHaveLength(1);
    release();
    await expect(
      page.getByRole("heading", { name: "申请已取消", exact: true }),
    ).toBeVisible();
    expect(state.cancellations).toHaveLength(1);
  });
  for (const status of [422, 429, 503]) {
    test(`cancellation ${mode}: HTTP ${status} preserves the outcome and retry boundary`, async ({
      page,
    }) => {
      const state = control();
      state.cancellationInvalid = status === 422;
      state.cancellationStatus = status === 422 ? 0 : status;
      await install(page, state);
      await page.clock.install();
      await page.goto(
        origin +
          `/workforce/${mode === "self" ? "applications" : "reviews"}/` +
          state.records[0].id,
      );
      await page.getByRole("button", { name: "取消申请", exact: true }).click();
      if (mode === "manager")
        await page.getByRole("textbox", { name: "取消说明" }).fill("重复申请");
      await page
        .getByRole("dialog")
        .getByRole("button", { name: "取消申请", exact: true })
        .click();
      await expect(page.getByRole("dialog")).toHaveCount(0);
      if (status === 503)
        await expect(
          page.getByRole("heading", { name: "暂时无法确认取消结果" }),
        ).toBeVisible();
      else
        await expect(
          page.getByRole("heading", { name: "暂时无法确认取消结果" }),
        ).toHaveCount(0);
      await expect(
        page.getByRole("button", {
          name: status === 422 ? /秒后可刷新/ : /9 秒后可刷新/,
        }),
      ).toBeDisabled();
      expect(state.cancellations).toHaveLength(1);
      await expect(
        page.getByRole("button", { name: "取消申请", exact: true }),
      ).toHaveCount(0);
    });
  }
  for (const outcome of ["wrong-id", "timeout", "account-change"] as const) {
    test(`cancellation ${mode}: ${outcome} never accepts a late or wrong acknowledgement`, async ({
      page,
    }, info) => {
      const state = control();
      await install(page, state);
      await page.clock.install();
      await page.goto(
        origin +
          `/workforce/${mode === "self" ? "applications" : "reviews"}/` +
          state.records[0].id,
      );
      await page.getByRole("button", { name: "取消申请", exact: true }).click();
      if (mode === "manager")
        await page.getByRole("textbox", { name: "取消说明" }).fill("重复申请");
      let release!: () => void;
      if (outcome === "wrong-id") state.cancellationWrongId = true;
      else
        state.cancellationGate = new Promise<void>((resolve) => {
          release = resolve;
        });
      await page
        .getByRole("dialog")
        .getByRole("button", { name: "取消申请", exact: true })
        .click();
      await expect.poll(() => state.cancellations.length).toBe(1);
      if (outcome !== "wrong-id") {
        await expect(
          page.getByRole("dialog").getByRole("button", { name: "保留申请" }),
        ).toBeDisabled();
        await page.keyboard.press("Escape");
        await expect(page.getByRole("dialog")).toBeVisible();
        if (outcome === "account-change")
          await chooseSession(page, otherAccountId);
        else await page.clock.fastForward(35_100);
        release();
      }
      if (outcome !== "account-change")
        await expect(
          page.getByRole("heading", { name: "暂时无法确认取消结果" }),
        ).toBeVisible();
      else
        await expect(
          page.getByText(state.records[0].snapshot.name, { exact: true }),
        ).toHaveCount(0);
      await expect(
        page.getByRole("heading", { name: "申请已取消", exact: true }),
      ).toHaveCount(0);
      expect(state.cancellations).toHaveLength(1);
      await page.screenshot({
        path: info.outputPath(`${mode}-${outcome}.png`),
      });
    });
  }

  test(`cancellation ${mode}: approval wins and a new explicit confirmation binds approved`, async ({
    page,
  }, info) => {
    const state = control();
    state.cancellationRace = true;
    await install(page, state);
    await page.goto(
      origin +
        `/workforce/${mode === "self" ? "applications" : "reviews"}/` +
        state.records[0].id,
    );
    await page.getByRole("button", { name: "取消申请", exact: true }).click();
    if (mode === "manager")
      await page.getByRole("textbox", { name: "取消说明" }).fill("重复申请");
    await page
      .getByRole("dialog")
      .getByRole("button", { name: "取消申请", exact: true })
      .click();
    await expect(
      page.getByText("申请状态已更新。请核对当前状态后重新确认取消。"),
    ).toBeVisible();
    await expect(page.getByRole("dialog")).toHaveCount(0);
    expect(state.cancellations).toHaveLength(1);
    expect(state.cancellations[0].expectedStatus).toBe("submitted");
    await page.screenshot({
      path: info.outputPath(`${mode}-state-changed.png`),
    });
    await page.getByRole("button", { name: "取消申请", exact: true }).click();
    await expect(page.getByRole("dialog")).toContainText("审核通过");
    if (mode === "manager") {
      await expect(page.getByRole("textbox", { name: "取消说明" })).toHaveValue(
        "",
      );
      await page.getByRole("textbox", { name: "取消说明" }).fill("核对后取消");
    }
    await page
      .getByRole("dialog")
      .getByRole("button", { name: "取消申请", exact: true })
      .click();
    await expect(
      page.getByRole("heading", { name: "申请已取消", exact: true }),
    ).toBeVisible();
    expect(state.cancellations).toHaveLength(2);
    expect(state.cancellations[1].expectedStatus).toBe("approved");
  });

  test(`cancellation ${mode}: lost response only refreshes and never repeats POST`, async ({
    page,
  }, info) => {
    const state = control();
    state.cancellationLost = true;
    state.restricted = mode === "self";
    await install(page, state);
    await page.goto(
      origin +
        `/workforce/${mode === "self" ? "applications" : "reviews"}/` +
        state.records[0].id,
    );
    await page.getByRole("button", { name: "取消申请", exact: true }).click();
    if (mode === "manager")
      await page.getByRole("textbox", { name: "取消说明" }).fill("重复申请");
    await page
      .getByRole("dialog")
      .getByRole("button", { name: "取消申请", exact: true })
      .click();
    await expect(
      page.getByRole("heading", { name: "暂时无法确认取消结果" }),
    ).toBeVisible();
    expect(state.cancellations).toHaveLength(1);
    await page.getByRole("button", { name: "刷新详情", exact: true }).click();
    if (mode === "self") {
      await expect(
        page.getByText(
          "目前无法查看这份申请，仍无法确认刚才的取消是否成功。可返回列表查看当前允许访问的记录。",
        ),
      ).toBeVisible();
      await expect(
        page.getByRole("heading", { name: "申请已取消", exact: true }),
      ).toHaveCount(0);
    } else {
      await expect(page.locator(".review-meta")).toContainText("已取消");
      await expect(
        page.getByRole("button", { name: "取消申请", exact: true }),
      ).toHaveCount(0);
    }
    expect(state.cancellations).toHaveLength(1);
    await page.screenshot({
      path: info.outputPath(`${mode}-unknown-refreshed.png`),
    });
  });

  test(`cancellation ${mode}: permission loss never falls back or keeps private material`, async ({
    page,
  }) => {
    const state = control();
    await install(page, state);
    await page.goto(
      origin +
        `/workforce/${mode === "self" ? "applications" : "reviews"}/` +
        state.records[0].id,
    );
    await page.getByRole("button", { name: "取消申请", exact: true }).click();
    if (mode === "manager")
      await page.getByRole("textbox", { name: "取消说明" }).fill("重复申请");
    state.denied = true;
    await page
      .getByRole("dialog")
      .getByRole("button", { name: "取消申请", exact: true })
      .click();
    await expect(page.getByRole("dialog")).toHaveCount(0);
    await expect(
      page.getByText(state.records[0].snapshot.name, { exact: true }),
    ).toHaveCount(0);
    expect(state.cancellations).toHaveLength(1);
    expect(state.cancellations[0].mode).toBe(mode);
  });
}

test("management uses service status filters and real pagination; empty and failed reads stay distinct", async ({
  page,
}) => {
  const state = control();
  state.records = Array.from({ length: 21 }, (_, n) => ({
    ...structuredClone(state.records[0]),
    id: id(100 + n),
  }));
  await install(page, state);
  await page.goto(origin + "/workforce/reviews");
  await expect(page.getByRole("table")).toBeVisible();
  await expect(page.getByRole("row")).toHaveCount(21);
  await page.getByRole("button", { name: "下一页", exact: true }).click();
  await expect(page.getByRole("row")).toHaveCount(2);
  expect(state.listQueries.at(-1)).toBe("?page=2&size=20&status=submitted");
  await page.getByRole("button", { name: "审核通过", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "暂无审核通过的申请" }),
  ).toBeVisible();
  expect(state.listQueries.at(-1)).toBe("?page=1&size=20&status=approved");
  state.readFailure = true;
  await page.getByRole("button", { name: "刷新列表", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "工牌申请暂时无法加载" }),
  ).toBeVisible();
  await expect(page.getByText("暂无审核通过的申请")).toHaveCount(0);
});

test("approval confirms once, preserves fixed bytes and displays the trusted reviewer facts", async ({
  page,
}, info) => {
  const state = control();
  const original = structuredClone(state.records[0].snapshot);
  await install(page, state);
  await page.goto(origin + "/workforce/reviews/" + state.records[0].id);
  await expect(
    page.getByRole("button", { name: "批准申请", exact: true }),
  ).toBeEnabled();
  await page.getByRole("button", { name: "批准申请", exact: true }).click();
  expect(state.decisions).toHaveLength(0);
  await page.getByRole("button", { name: "确认批准", exact: true }).click();
  await expect(page.locator(".review-decision")).toContainText("林*");
  await expect(page.locator(".review-decision")).toContainText(
    "2026/10/31 09:15",
  );
  await expect(
    page.getByRole("button", { name: "批准申请", exact: true }),
  ).toHaveCount(0);
  expect(state.decisions).toEqual([{ decision: "approve" }]);
  expect(state.records[0].snapshot).toEqual(original);
  await page.screenshot({
    path: info.outputPath("management-approved-desktop.png"),
    fullPage: true,
  });
});

test("a lost decision response clears private details and offers only GET refresh of the real outcome", async ({
  page,
}, info) => {
  const state = control();
  state.loseResponse = true;
  await install(page, state);
  await page.goto(origin + "/workforce/reviews/" + state.records[0].id);
  await expect(
    page.getByRole("button", { name: "批准申请", exact: true }),
  ).toBeEnabled();
  await page.getByRole("button", { name: "批准申请", exact: true }).click();
  await page.getByRole("button", { name: "确认批准", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "暂时无法确认审核结果" }),
  ).toBeVisible();
  await expect(page.locator(".application-snapshot")).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "确认批准", exact: true }),
  ).toHaveCount(0);
  await page.screenshot({
    path: info.outputPath("management-unknown.png"),
    fullPage: true,
  });
  await page.getByRole("button", { name: "刷新详情", exact: true }).click();
  await expect(page.locator(".review-decision")).toContainText("林*");
  await expect(page.getByText("审核通过", { exact: true })).toBeVisible();
  expect(state.decisions).toHaveLength(1);
  expect(state.detailReads).toBe(2);
});

test("a concurrent decision remains authoritative", async ({ page }) => {
  const state = control();
  state.stateChanged = true;
  await install(page, state);
  await page.goto(origin + "/workforce/reviews/" + state.records[0].id);
  await expect(
    page.getByRole("button", { name: "批准申请", exact: true }),
  ).toBeEnabled();
  await page.getByRole("button", { name: "批准申请", exact: true }).click();
  await page.getByRole("button", { name: "确认批准", exact: true }).click();
  await expect(
    page.getByText("申请状态已更新，请查看当前结果。"),
  ).toBeVisible();
  await expect(page.locator(".review-decision")).toContainText("周*");
  await expect(page.locator(".review-reason")).toContainText(
    "申请已由另一位审核人处理",
  );
  expect(state.records[0].status).toBe("rejected");
  expect(state.decisions).toHaveLength(1);
});

test("eligibility lost after confirmation blocks approval, then an explicit rejection preserves the complete reason", async ({
  page,
}) => {
  const state = control();
  await install(page, state);
  await page.goto(origin + "/workforce/reviews/" + state.records[0].id);
  await expect(
    page.getByRole("button", { name: "批准申请", exact: true }),
  ).toBeEnabled();
  await page.getByRole("button", { name: "批准申请", exact: true }).click();
  state.current.participationActive = false;
  await page.getByRole("button", { name: "确认批准", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "批准申请", exact: true }),
  ).toBeDisabled();
  expect(state.records[0].status).toBe("submitted");
  await page.getByRole("button", { name: "驳回申请", exact: true }).click();
  const reason =
    "照片中面部不清晰，请换用正面照片。\n\n请保留完整头部与肩部，重新提交申请。";
  await page.getByRole("textbox", { name: "驳回理由" }).fill(reason);
  let release!: () => void;
  state.decisionGate = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.getByRole("button", { name: "确认驳回", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "正在提交…", exact: true }),
  ).toBeDisabled();
  release();
  await expect(page.locator(".review-reason p")).toHaveText(reason);
  await expect(page.locator(".review-facts")).toContainText("林*");
  expect(state.decisions).toEqual([
    { decision: "approve" },
    { decision: "reject", reason },
  ]);
  expect(state.records[0].status).toBe("rejected");
});

test("late approval response cannot restore another account's private data", async ({
  page,
}) => {
  const state = control();
  let release!: () => void;
  state.decisionGate = new Promise<void>((resolve) => {
    release = resolve;
  });
  await install(page, state);
  await page.goto(origin + "/workforce/reviews/" + state.records[0].id);
  await expect(
    page.getByRole("button", { name: "批准申请", exact: true }),
  ).toBeEnabled();
  await page.getByRole("button", { name: "批准申请", exact: true }).click();
  await page.getByRole("button", { name: "确认批准", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "正在提交…", exact: true }),
  ).toBeDisabled();
  await expect.poll(() => state.decisions.length).toBe(1);
  await chooseSession(page, otherAccountId);
  release();
  await expect(page.getByText("目前无法查看工牌审核。")).toBeVisible();
  await expect(page.locator(".application-snapshot")).toHaveCount(0);
  await expect(page.getByText("林*", { exact: true })).toHaveCount(0);
  expect(state.decisions).toHaveLength(1);
});

test("a management rate limit hides data and waits for an explicit refresh", async ({
  page,
}) => {
  const state = control();
  state.limited = true;
  await page.clock.install();
  await install(page, state);
  await page.goto(origin + "/workforce/reviews");
  await expect(
    page.getByRole("button", { name: "7 秒后可刷新", exact: true }),
  ).toBeDisabled();
  await expect(page.getByRole("table")).toHaveCount(0);
  state.limited = false;
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect(
    page.getByRole("heading", { name: "工牌申请暂时无法加载", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: /秒后可刷新$/ }),
  ).toBeDisabled();
  await page.clock.fastForward(7100);
  expect(state.listQueries).toHaveLength(0);
  await page.getByRole("button", { name: "重新加载", exact: true }).click();
  await expect(page.getByRole("table")).toBeVisible();
  expect(state.listQueries).toHaveLength(1);
});

test("media limits protect both refresh paths across focus while allowing an explicit rejection", async ({
  page,
}, testInfo) => {
  const state = control();
  state.mediaLimited = true;
  await page.clock.install();
  await install(page, state);
  await page.goto(origin + "/workforce/reviews/" + state.records[0].id);
  await expect(
    page.getByText("工牌预览暂时无法显示，请重新加载。", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: /秒后可刷新详情$/ }),
  ).toBeDisabled();
  await expect(
    page.getByRole("button", { name: /秒后可重新加载$/ }),
  ).toBeDisabled();
  const reads = state.detailReads,
    media = state.resourceReads.length;
  await page.getByRole("button", { name: "驳回申请", exact: true }).click();
  await page
    .getByRole("textbox", { name: "驳回理由" })
    .fill("请使用正面照片。");
  await page.getByRole("button", { name: "确认驳回", exact: true }).click();
  await expect(page.locator(".review-reason p")).toHaveText("请使用正面照片。");
  expect(state.decisions).toHaveLength(1);
  state.mediaLimited = false;
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect(
    page.getByRole("heading", { name: "工牌申请暂时无法加载", exact: true }),
  ).toBeVisible();
  await expect(page.locator(".application-snapshot")).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: /秒后可刷新$/ }),
  ).toBeDisabled();
  expect(state.detailReads).toBe(reads);
  expect(state.resourceReads).toHaveLength(media);
  await page.screenshot({
    path: testInfo.outputPath("media-retry-after-focus.png"),
    fullPage: true,
  });
  await page.clock.fastForward(12100);
  await page.getByRole("button", { name: "刷新详情", exact: true }).click();
  await expect(page.getByRole("img", { name: "工牌预览" })).toBeVisible();
  await expect(page.locator(".review-reason p")).toHaveText("请使用正面照片。");
  expect(state.detailReads).toBe(reads + 1);
  expect(state.decisions).toHaveLength(1);
});

test("a media deadline keeps trusted details readable and rejection available", async ({
  page,
}, testInfo) => {
  const state = control();
  let release!: () => void;
  state.mediaGate = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.clock.install();
  await page.setViewportSize({ width: 320, height: 640 });
  await install(page, state);
  await page.goto(origin + "/workforce/reviews/" + state.records[0].id);
  await expect(page.locator(".application-snapshot")).toContainText("测试同事");
  await page.clock.fastForward(35001);
  await expect(
    page.getByText("工牌预览暂时无法显示，请重新加载。", { exact: true }),
  ).toBeVisible();
  await expect(page.locator(".application-snapshot")).toContainText("测试同事");
  await expect(
    page.getByRole("heading", { name: "工牌申请暂时无法加载", exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "批准申请", exact: true }),
  ).toBeDisabled();
  await expect(
    page.getByRole("button", { name: "驳回申请", exact: true }),
  ).toBeEnabled();
  await page.screenshot({
    path: testInfo.outputPath("media-deadline-trusted-detail-320.png"),
    fullPage: true,
  });
  release();
  await expect(page.locator(".application-snapshot")).toContainText("测试同事");
  expect(state.decisions).toHaveLength(0);
});

test("own restricted reads remain available when the unrelated Grant read fails", async ({
  page,
}, info) => {
  const state = control();
  state.grantFailure = true;
  state.restricted = true;
  await install(page, state);
  await page.goto(origin + "/workforce/applications");
  await expect(
    page.getByText("当前仅可查看可取消申请的类型、状态和提交时间。", {
      exact: true,
    }),
  ).toBeVisible();
  await page.getByRole("link", { name: "查看详情", exact: true }).click();
  await expect(
    page.getByText("当前仅可查看申请类型、状态和提交时间。", { exact: true }),
  ).toBeVisible();
  await expect(page.locator(".application-snapshot")).toHaveCount(0);
  await expect(page.getByRole("img", { name: "工牌预览" })).toHaveCount(0);
  expect(state.resourceReads).toHaveLength(0);
  await page.screenshot({
    path: info.outputPath("own-restricted-grant-failed.png"),
    fullPage: true,
  });
});

for (const change of ["qualification", "account"] as const) {
  test(`own history conceals a late full response after ${change} changes`, async ({
    page,
  }) => {
    const state = control();
    let release!: () => void;
    state.ownGate = new Promise<void>((resolve) => {
      release = resolve;
    });
    await install(page, state);
    await page.goto(origin + "/workforce/applications/" + state.records[0].id);
    await expect.poll(() => state.ownReads).toBeGreaterThan(0);
    state.ownGate = undefined;
    if (change === "qualification") {
      state.restricted = true;
      await page.evaluate(() => window.dispatchEvent(new Event("focus")));
      await expect(
        page.getByText("当前仅可查看申请类型、状态和提交时间。", {
          exact: true,
        }),
      ).toBeVisible();
    } else {
      await chooseSession(page, otherAccountId);
      await expect(
        page.getByText("无法查看这项内容，请返回个人账户。", { exact: true }),
      ).toBeVisible();
    }
    release();
    await expect(page.locator(".application-snapshot")).toHaveCount(0);
    await expect(page.getByText("测试同事", { exact: true })).toHaveCount(0);
    await expect(page.getByRole("img", { name: "工牌预览" })).toHaveCount(0);
    expect(state.resourceReads).toHaveLength(0);
  });
}

test("an own media deadline retains fixed facts and trusted review results", async ({
  page,
}, info) => {
  const state = control();
  state.records[0] = reviewed(state.records[0], "approve");
  let release!: () => void;
  state.mediaGate = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.clock.install();
  await install(page, state);
  await page.goto(origin + "/workforce/applications/" + state.records[0].id);
  await expect(page.locator(".application-snapshot").first()).toContainText(
    "测试同事",
  );
  await expect(page.locator(".review-facts")).toContainText("林*");
  await page.clock.fastForward(35001);
  await expect(
    page.getByText("工牌预览暂时无法显示，请刷新详情。", { exact: true }),
  ).toBeVisible();
  await expect(page.locator(".application-snapshot").first()).toContainText(
    "测试同事",
  );
  await expect(page.locator(".review-facts")).toContainText("林*");
  await expect(
    page.getByRole("button", { name: /秒后可刷新详情$/ }),
  ).toBeDisabled();
  await page.screenshot({
    path: info.outputPath("own-media-deadline-trusted-review.png"),
    fullPage: true,
  });
  release();
  await expect(page.locator(".review-facts")).toContainText("林*");
});

test("own history shows all actual states and full reasons, while restricted history exposes only cancellable summaries", async ({
  page,
}, info) => {
  const state = control(),
    original = state.records[0];
  const reason =
    "请使用正面、无遮挡的照片。\n\n请保留完整头部和肩部，再核对完整姓名、工号与科室。\n\n"
      .repeat(12)
      .trim();
  state.records = [
    original,
    reviewed({ ...structuredClone(original), id: id(41) }, "approve"),
    reviewed({ ...structuredClone(original), id: id(42) }, "reject", reason),
  ];
  state.mediaFailed = true;
  await install(page, state);
  await page.goto(origin + "/workforce/applications");
  await expect(page.getByText("待审核", { exact: true })).toBeVisible();
  await expect(page.getByText("审核通过", { exact: true })).toBeVisible();
  await expect(page.getByText("审核未通过", { exact: true })).toBeVisible();
  await page
    .locator(".application-list li")
    .filter({ hasText: "审核未通过" })
    .getByRole("link", { name: "查看详情" })
    .click();
  await expect(page.locator(".review-reason p")).toHaveText(reason);
  await expect(page.locator(".review-facts")).toContainText("林*");
  await expect(
    page.getByText("工牌预览暂时无法显示，请刷新详情。"),
  ).toBeVisible();
  await page.screenshot({
    path: info.outputPath("own-rejected-full-reason-desktop.png"),
    fullPage: true,
  });
  state.restricted = true;
  state.resourceReads = [];
  await page.goto(origin + "/workforce/applications");
  await expect(page.locator(".application-list li")).toHaveCount(2);
  await expect(page.locator(".application-list")).not.toContainText(
    "审核未通过",
  );
  await expect(page.locator(".application-list")).not.toContainText("林*");
  await page
    .locator(".application-list li")
    .filter({ hasText: "审核通过" })
    .getByRole("link", { name: "查看详情" })
    .click();
  await expect(
    page.getByText("当前仅可查看申请类型、状态和提交时间。"),
  ).toBeVisible();
  await expect(page.locator(".review-facts")).toHaveCount(0);
  await expect(page.locator(".review-reason")).toHaveCount(0);
  await expect(page.locator(".application-snapshot")).toHaveCount(0);
  expect(state.resourceReads).toHaveLength(0);
  await page.screenshot({
    path: info.outputPath("own-restricted-approved.png"),
    fullPage: true,
  });
});

test("fixed contents stay separate from current differences, absent numbers and disabled directory values do not block approval", async ({
  page,
}, info) => {
  const state = control();
  state.current.name = "更正后的姓名";
  state.current.number = null;
  state.current.department = { name: "更名后的呼吸医学中心", enabled: false };
  await install(page, state);
  await page.goto(origin + "/workforce/reviews/" + state.records[0].id);
  await expect(page.getByRole("img", { name: "工牌预览" })).toBeVisible();
  await expect(
    page.getByRole("button", { name: "批准申请", exact: true }),
  ).toBeEnabled();
  await expect(
    page.getByText("当前没有工号，本申请仍使用提交时的工号。"),
  ).toBeVisible();
  await expect(page.locator(".application-snapshot")).toContainText("测试同事");
  await expect(page.locator(".application-snapshot")).not.toContainText(
    "更正后的姓名",
  );
  await expect(page.locator(".review-changes")).toContainText(
    "更名后的呼吸医学中心（已停用）",
  );
  expect(state.resourceReads).toHaveLength(3);
  await page.screenshot({
    path: info.outputPath("management-fixed-current-desktop.png"),
    fullPage: true,
  });
});

test("media failure and applicant ineligibility prohibit approval while retaining fixed facts and explicit rejection", async ({
  page,
}) => {
  const state = control();
  state.mediaFailed = true;
  state.current.accountActive = false;
  await install(page, state);
  await page.goto(origin + "/workforce/reviews/" + state.records[0].id);
  await expect(
    page.getByText("工牌预览暂时无法显示，请重新加载。"),
  ).toBeVisible();
  await expect(page.locator(".application-snapshot")).toContainText("测试同事");
  await expect(
    page.getByRole("button", { name: "批准申请", exact: true }),
  ).toBeDisabled();
  await page.getByRole("button", { name: "驳回申请", exact: true }).click();
  await expect(page.getByRole("dialog")).toBeVisible();
  await page.getByRole("button", { name: "确认驳回", exact: true }).click();
  await expect(page.getByRole("textbox", { name: "驳回理由" })).toBeFocused();
  expect(state.decisions).toHaveLength(0);
});

test("permission loss, account switching and late responses never restore old private content", async ({
  page,
}) => {
  const state = control();
  await install(page, state);
  await page.goto(origin + "/workforce/reviews/" + state.records[0].id);
  await expect(page.locator(".application-snapshot")).toBeVisible();
  let release!: () => void;
  state.gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.getByRole("button", { name: "刷新详情", exact: true }).click();
  await expect(page.locator(".application-snapshot")).toHaveCount(0);
  await chooseSession(page, otherAccountId);
  release();
  await expect(page.getByText("目前无法查看工牌审核。")).toBeVisible();
  await expect(page.locator(".application-snapshot")).toHaveCount(0);
  expect(state.decisions).toHaveLength(0);
});

for (const colorScheme of ["light", "dark"] as const) {
  test(`management and decided own history remain readable across widths in ${colorScheme}`, async ({
    page,
  }, info) => {
    const state = control(),
      original = structuredClone(state.records[0]);
    const reason =
      "请使用正面照片，保留完整头部和肩部。\n\n请核对姓名、工号、科室和领取地点后，重新提交申请。".repeat(
        8,
      );
    await page.emulateMedia({ colorScheme });
    await install(page, state);
    for (const width of [320, 768, 1440]) {
      state.records = [structuredClone(original)];
      await page.setViewportSize({ width, height: width === 320 ? 640 : 1000 });
      await page.goto(origin + "/workforce/reviews");
      await expect(page.getByRole("table")).toBeVisible();
      expect(
        await page.evaluate(() => document.documentElement.scrollWidth),
      ).toBeLessThanOrEqual(width);
      await page.screenshot({
        path: info.outputPath(`list-${width}-${colorScheme}.png`),
        fullPage: true,
      });
      await page
        .getByRole("link", { name: "查看测试同事的申请", exact: true })
        .click();
      await expect(
        page.getByRole("img", { name: "工牌预览", exact: true }),
      ).toBeVisible();
      await expect(
        page.getByRole("button", { name: "批准申请", exact: true }),
      ).toBeEnabled();
      expect(
        await page.evaluate(() => document.documentElement.scrollWidth),
      ).toBeLessThanOrEqual(width);
      if (width !== 768)
        await page.screenshot({
          path: info.outputPath(`detail-${width}-${colorScheme}.png`),
          fullPage: true,
        });
      if (width === 320) {
        await page
          .getByRole("button", { name: "批准申请", exact: true })
          .click();
        const dialog = page.getByRole("dialog"),
          bounds = await dialog.boundingBox();
        expect(bounds!.x).toBeGreaterThanOrEqual(0);
        expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(width);
        await expect(
          dialog.getByRole("button", { name: "确认批准", exact: true }),
        ).toBeVisible();
        await page.screenshot({
          path: info.outputPath(`approve-confirm-320-${colorScheme}.png`),
          fullPage: false,
        });
        await page
          .getByRole("button", { name: "暂不批准", exact: true })
          .click();
      }
      state.records = [reviewed(original, "reject", reason)];
      await page.goto(origin + "/workforce/applications/" + original.id);
      await expect(page.locator(".review-reason p")).toHaveText(reason);
      expect(
        await page.evaluate(() => document.documentElement.scrollWidth),
      ).toBeLessThanOrEqual(width);
      if (width === 320)
        await page.screenshot({
          path: info.outputPath(`own-full-reason-320-${colorScheme}.png`),
          fullPage: true,
        });
    }
  });
  test(`real management at 320px ${colorScheme} keeps long fixed facts, media failure and rejection validation readable`, async ({
    page,
  }, info) => {
    const state = control();
    const longName =
      "陈欧阳司徒慕容夏侯东方闻人长孙南宫诸葛公孙上官司马清和".repeat(2);
    state.records[0].snapshot.name = longName;
    state.mediaFailed = true;
    await page.setViewportSize({ width: 320, height: 640 });
    await page.emulateMedia({ colorScheme });
    await install(page, state);
    await page.goto(origin + "/workforce/reviews/" + state.records[0].id);
    await expect(
      page.getByText("工牌预览暂时无法显示，请重新加载。"),
    ).toBeVisible();
    await expect(page.locator(".application-snapshot dd").first()).toHaveText(
      longName,
    );
    expect(
      await page.evaluate(() => document.documentElement.scrollWidth),
    ).toBeLessThanOrEqual(320);
    await expect(
      page.getByRole("button", { name: "批准申请", exact: true }),
    ).toBeDisabled();
    await page.screenshot({
      path: info.outputPath(`detail-long-media-320-${colorScheme}.png`),
      fullPage: true,
    });
    await page.getByRole("button", { name: "驳回申请", exact: true }).click();
    await page.getByRole("button", { name: "确认驳回", exact: true }).click();
    await expect(page.getByRole("textbox", { name: "驳回理由" })).toBeFocused();
    const error = await page.getByRole("alert").boundingBox(),
      confirm = await page
        .getByRole("button", { name: "确认驳回", exact: true })
        .boundingBox();
    expect(error!.y + error!.height).toBeLessThan(confirm!.y);
    expect(confirm!.y + confirm!.height).toBeLessThanOrEqual(640);
    await page.screenshot({
      path: info.outputPath(`detail-rejection-invalid-320-${colorScheme}.png`),
      fullPage: false,
    });
    await page.keyboard.press("Escape");
    await expect(
      page.getByRole("button", { name: "驳回申请", exact: true }),
    ).toBeFocused();
    expect(state.decisions).toHaveLength(0);
  });
}
