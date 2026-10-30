import { test, expect, type Page } from "@playwright/test";
import { build } from "vite";
import tailwindcss from "@tailwindcss/vite";
import { readFile } from "node:fs/promises";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { resolve } from "node:path";
import { inspectImage } from "../app/avatar/format";
import type {
  Application,
  ApplicationInput,
  ApplicationPreview,
  ApplicationSnapshot,
  PreviewInput,
  PhotoMetadata,
} from "../app/protocol/applications";
import {
  accountId,
  otherAccountId,
  applicationId as id,
  accountMaterial,
  directoryMaterial,
  templateMaterial,
  fixedNumber,
  photoMaterial,
} from "./application-material";

// Real pages, generated contracts, IndexedDB, images and fonts; HTTP is controlled.
// This suite does not establish backend transactions or cross-engine PDF geometry.
const origin = "http://applications.localhost";
const resources = new Map<
  string,
  { body: string | Buffer; contentType: string }
>();
let html = "",
  chineseFont: Buffer,
  arabicFont: Buffer;
async function fixtureFont(name: string) {
  const path = `domain/tests/fixtures/fonts/${name}`;
  try {
    return await readFile(resolve("..", path));
  } catch (error) {
    if (
      (error as NodeJS.ErrnoException).code !== "ENOENT" ||
      !process.env.HALO_APPLICATION_FONT_REV
    )
      throw error;
    // Before backend integration, consume only its published Git asset revision.
    return execFileSync(
      "git",
      ["show", `${process.env.HALO_APPLICATION_FONT_REV}:${path}`],
      { cwd: resolve(".."), maxBuffer: 16 * 1024 * 1024 },
    );
  }
}
test.beforeAll(async () => {
  const protocol = await readFile(resolve("protocol/openapi.json"));
  console.info(
    `Application protocol snapshot: SHA256=${createHash("sha256").update(protocol).digest("hex")}; font source=${process.env.HALO_APPLICATION_FONT_REV ?? "working-tree"}`,
  );
  [chineseFont, arabicFont] = await Promise.all([
    fixtureFont("NotoSansSC-Regular.otf"),
    fixtureFont("NotoSansArabic.ttf"),
  ]);
  console.info(
    `Application fixture fonts: SC=${createHash("sha256").update(chineseFont).digest("hex")}; Arabic=${createHash("sha256").update(arabicFont).digest("hex")}`,
  );
  const result = await build({
    configFile: false,
    publicDir: false,
    plugins: [tailwindcss()],
    logLevel: "error",
    define: { __BUILD_ID__: JSON.stringify("applications-fixture") },
    build: {
      write: false,
      emptyOutDir: false,
      cssCodeSplit: false,
      assetsInlineLimit: 0,
      rolldownOptions: {
        input: resolve("tests/fixtures/applications.tsx"),
        output: { entryFileNames: "fixture.js" },
      },
    },
  });
  if (Array.isArray(result) || !("output" in result))
    throw new Error("Expected one build");
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
  html = `<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><link rel="icon" href="data:,">${styles.join("")}</head><body><div id="fixture"></div><script type="module" src="/fixture.js"></script></body></html>`;
});
function control() {
  return {
    grant: "active",
    number: fixedNumber as typeof fixedNumber | null,
    arabic: false,
    badFont: false,
    badLine: false,
    listFailure: false,
    sessionInvalid: false,
    previewFailure: "",
    photoUnavailable: "" as "" | "preview" | "submit",
    capacityExhausted: false,
    changed: false,
    photo: { ...photoMaterial },
    photoBytes: Buffer.alloc(0),
    background: Buffer.alloc(0),
    saved: [] as Application[],
    submissions: [] as ApplicationInput[],
    previews: [] as PreviewInput[],
    mediaHeaders: [] as Record<string, string>[],
    gate: undefined as Promise<void> | undefined,
    loseSubmitResponse: false,
  };
}
type Control = ReturnType<typeof control>;
async function webp(page: Page, png: Buffer) {
  const base64 = await page.evaluate(async (bytes) => {
    const raw = Uint8Array.from(atob(bytes), (char) => char.charCodeAt(0));
    const bitmap = await createImageBitmap(
      new Blob([raw], { type: "image/png" }),
    );
    const canvas = document.createElement("canvas");
    canvas.width = bitmap.width;
    canvas.height = bitmap.height;
    canvas.getContext("2d")!.drawImage(bitmap, 0, 0);
    bitmap.close();
    return canvas.toDataURL("image/webp", 1).split(",")[1];
  }, png.toString("base64"));
  return Buffer.from(base64, "base64");
}
async function snapshot(
  page: Page,
  state: Control,
  input: PreviewInput,
): Promise<ApplicationSnapshot> {
  const name = state.arabic ? "ليلى ABC" : "阿依古丽·买买提";
  const department = state.arabic
    ? "قسم الطب ABC"
    : directoryMaterial.departments.find(
        (item) => item.id === input.selections.departmentId,
      )!.name;
  const printedTitle = state.arabic
    ? "طبيب"
    : input.selections.titleId
      ? "主任医师"
      : "无";
  const text = await page.evaluate(
    ({ values, layout }) => {
      const face = [...document.fonts].find((font) =>
        font.family.startsWith("Application_"),
      );
      if (!face)
        throw new Error(
          "Fixture requires the same real loaded font as the page",
        );
      const context = document.createElement("canvas").getContext("2d")!;
      const output: Record<string, unknown[]> = {};
      for (const key of ["name", "number", "department", "title"] as const) {
        const spec = layout[key],
          original = values[key];
        context.font = `${spec.fontSizeUm}px "${face.family}"`;
        context.textAlign = "left";
        context.direction = /[\u0590-\u08ff]/.test(original) ? "rtl" : "ltr";
        const chunks: string[] = [];
        let current = "";
        // Test-only line material. Production consumes server shaping, never this code.
        for (const { segment } of new Intl.Segmenter("zh-CN", {
          granularity: "grapheme",
        }).segment(original)) {
          if (
            current &&
            context.measureText(current + segment).width >
              spec.area.widthUm - 200
          ) {
            chunks.push(current);
            current = "";
          }
          current += segment;
        }
        if (current) chunks.push(current);
        let at = 0;
        output[key] = chunks.map((text, index) => {
          const metrics = context.measureText(text),
            start = at;
          at += new TextEncoder().encode(text).byteLength;
          return {
            start,
            end: at,
            xUm:
              spec.area.xUm + Math.max(0, metrics.actualBoundingBoxLeft) + 10,
            baselineUm:
              spec.area.yUm +
              index * spec.lineHeightUm +
              metrics.actualBoundingBoxAscent +
              100,
            widthUm: metrics.width,
            direction: context.direction,
          };
        });
      }
      return output;
    },
    {
      values: { name, number: "12605", department, title: printedTitle },
      layout: templateMaterial.template.layout,
    },
  );
  const value: ApplicationSnapshot = {
    name,
    number: {
      subtype: fixedNumber.subtype,
      year: fixedNumber.year,
      serial: fixedNumber.serial,
    },
    printedNumber: "12605",
    department: {
      id: input.selections.departmentId,
      name: department,
      printed: department,
    },
    position: null,
    title: input.selections.titleId
      ? { id: input.selections.titleId, name: printedTitle }
      : null,
    printedTitle,
    photoId: state.photo.photoId,
    template: templateMaterial.template,
    pickup: input.selections.pickup,
    text: text as ApplicationSnapshot["text"],
  };
  if (state.badLine) value.text.name[0].end = 1;
  return value;
}
async function install(page: Page, state: Control) {
  await page.route("**/*", async (route) => {
    const request = route.request(),
      url = new URL(request.url()),
      path = url.pathname;
    if (url.origin !== origin) return route.abort();
    if (path === "/build.json")
      return route.fulfill({ json: { buildId: "applications-fixture" } });
    const session = request.headers()["session-id"];
    if (state.sessionInvalid && path.startsWith("/api/"))
      return route.fulfill({
        status: 401,
        json: { code: "authentication_invalid" },
      });
    if (path === "/api/v1/accounts/current")
      return route.fulfill({ json: accountMaterial(session) });
    if (path === "/api/v1/workforce/current/grant")
      return route.fulfill({
        json: {
          code: "ok",
          grant: {
            id: id(60),
            subject: { personId: session, revisionId: id(50), name: "示*" },
            operator: { personId: accountId, revisionId: id(50), name: "示*" },
            grantedAt: "2026-10-30T00:00:00Z",
            operationId: id(61),
            sequence: 1,
            status: state.grant,
          },
        },
      });
    if (path === "/api/v1/workforce/current/numbers")
      return route.fulfill({ json: { code: "ok", number: state.number } });
    if (path === "/api/v1/workforce/directories")
      return route.fulfill({ json: directoryMaterial });
    if (path === "/api/v1/workforce/application-template")
      return route.fulfill({ json: templateMaterial });
    if (path.startsWith("/api/v1/workforce/application-resources/")) {
      state.mediaHeaders.push(request.headers());
      if (state.grant !== "active" || session !== accountId)
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
        return route.fulfill({
          contentType: state.arabic ? "font/ttf" : "font/otf",
          body: state.badFont
            ? Buffer.from("bad font")
            : state.arabic
              ? arabicFont
              : chineseFont,
        });
      if (path.endsWith(state.photo.resourceId))
        return route.fulfill({
          contentType: "image/webp",
          body: state.photoBytes,
        });
      return route.fulfill({
        status: 404,
        json: { code: "target_unavailable" },
      });
    }
    if (path === "/api/v1/workforce/application-photos") {
      if (state.capacityExhausted)
        return route.fulfill({
          status: 503,
          json: { code: "dependency_unavailable" },
        });
      const bytes = request.postDataBuffer()!;
      const image = inspectImage(new Uint8Array(bytes));
      if (
        image.kind !== "valid" ||
        request.headers()["content-type"] !== "image/png"
      )
        throw new Error("Expected confirmed real PNG");
      state.photo.width = image.width;
      state.photo.height = image.height;
      state.photoBytes = await webp(page, bytes);
      return route.fulfill({
        status: 201,
        json: { code: "ok", ...state.photo },
      });
    }
    if (path === "/api/v1/workforce/applications/preview") {
      const input = request.postDataJSON() as PreviewInput;
      state.previews.push(input);
      if (state.photoUnavailable === "preview")
        return route.fulfill({
          status: 404,
          json: { code: "target_unavailable" },
        });
      if (state.previewFailure)
        return route.fulfill({
          status: 422,
          json: { code: state.previewFailure },
        });
      const value: ApplicationPreview = {
        code: "ok",
        kind: input.kind,
        snapshot: await snapshot(page, state, input),
        photo: state.photo,
      };
      return route.fulfill({ json: value });
    }
    if (path === "/api/v1/workforce/applications") {
      const input = request.postDataJSON() as ApplicationInput;
      state.submissions.push(input);
      if (state.photoUnavailable === "submit")
        return route.fulfill({
          status: 404,
          json: { code: "target_unavailable" },
        });
      if (state.gate) await state.gate;
      if (state.changed)
        return route.fulfill({ status: 409, json: { code: "data_changed" } });
      const application: Application = {
        id: id(100 + state.saved.length),
        kind: input.kind,
        status: "submitted",
        submittedAt: "2026-10-30T18:00:00Z",
        snapshot: input.expected,
        photo: state.photo,
      };
      state.saved.push(application);
      if (state.loseSubmitResponse) return route.abort();
      return route
        .fulfill({ status: 201, json: { code: "ok", application } })
        .catch(() => {});
    }
    if (
      path === "/api/v1/workforce/current/applications" ||
      path.startsWith("/api/v1/workforce/current/applications/")
    ) {
      if (state.listFailure)
        return route.fulfill({
          status: 503,
          json: { code: "dependency_unavailable" },
        });
      const source = session === accountId ? state.saved : [];
      const access = state.grant === "active" ? "full" : "restricted";
      const items =
        access === "full"
          ? source
          : source.map(({ id, kind, status, submittedAt }) => ({
              id,
              kind,
              status,
              submittedAt,
            }));
      if (path.endsWith("/applications")) {
        const page = Number(url.searchParams.get("page") ?? "1");
        return route.fulfill({
          json: {
            code: "ok",
            access,
            count: items.length,
            applications: [...items]
              .reverse()
              .slice((page - 1) * 20, page * 20),
          },
        });
      }
      const item = items.find((item) => path.endsWith(item.id));
      return item
        ? route.fulfill({ json: { code: "ok", access, application: item } })
        : route.fulfill({ status: 404, json: { code: "target_unavailable" } });
    }
    if (path.startsWith("/api/")) return route.abort();
    return route.fulfill(
      resources.get(path) ?? { contentType: "text/html", body: html },
    );
  });
}
async function chooseSession(page: Page, session = accountId) {
  await page.evaluate(async (id) => {
    const store = window.applicationSessions;
    const attempt = await store.begin(crypto.randomUUID());
    if (
      !(await store.accept(attempt, { id, expiresAt: "2035-01-01T00:00:00Z" }))
    )
      throw new Error("Selection changed");
  }, session);
}
async function open(
  page: Page,
  state: Control,
  path = "/workforce/applications/new",
) {
  await install(page, state);
  await page.goto(origin + path);
  state.background = Buffer.from(
    await page.evaluate(() => {
      const canvas = document.createElement("canvas");
      canvas.width = 850;
      canvas.height = 540;
      const context = canvas.getContext("2d")!;
      context.fillStyle = "#f4efe4";
      context.fillRect(0, 0, 850, 540);
      context.fillStyle = "#56735d";
      context.fillRect(0, 0, 850, 35);
      context.fillStyle = "#c8d2c6";
      context.fillRect(50, 80, 250, 350);
      return canvas.toDataURL("image/webp", 1).split(",")[1];
    }),
    "base64",
  );
  await chooseSession(page);
}
async function prepare(page: Page) {
  await expect(page.getByLabel("实际科室", { exact: true })).toBeVisible();
  await page
    .getByRole("checkbox", { name: "我确认本次工牌使用上述科室" })
    .check();
  await page.getByLabel("领取地点", { exact: true }).selectOption({ index: 1 });
  const image = await page.evaluate(() => {
    const canvas = document.createElement("canvas");
    canvas.width = 800;
    canvas.height = 1200;
    const c = canvas.getContext("2d")!;
    c.clearRect(0, 0, 800, 1200);
    c.fillStyle = "#b9cbd0";
    c.fillRect(0, 0, 800, 1200);
    c.clearRect(0, 0, 160, 1200);
    c.fillStyle = "#ddbc9a";
    c.beginPath();
    c.arc(420, 360, 180, 0, Math.PI * 2);
    c.fill();
    c.fillStyle = "#365c4e";
    c.beginPath();
    c.ellipse(420, 980, 280, 400, 0, 0, Math.PI * 2);
    c.fill();
    return canvas.toDataURL("image/png").split(",")[1];
  });
  await page.locator('input[type="file"]').setInputFiles({
    name: "fictional-portrait.png",
    mimeType: "image/png",
    buffer: Buffer.from(image, "base64"),
  });
  await page.getByRole("button", { name: "确认裁剪效果", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "已确认裁剪效果" }),
  ).toBeDisabled();
}
async function preview(page: Page) {
  await page.getByRole("button", { name: "预览工牌", exact: true }).click();
  await expect(page.getByRole("dialog")).toBeVisible();
  await expect(
    page.getByRole("img", { name: "本次工牌完整预览" }),
  ).toBeVisible();
  await expect
    .poll(() =>
      page
        .getByRole("img", { name: "本次工牌完整预览" })
        .evaluate((node: HTMLCanvasElement) => {
          const context = node.getContext("2d")!;
          const pixel = (x: number, y: number) => [
            ...context.getImageData(
              (x * node.width) / 85000,
              (y * node.width) / 85000,
              1,
              1,
            ).data,
          ];
          const torso = pixel(18000, 35000),
            transparent = pixel(6500, 30000);
          return (
            torso[3] === 255 &&
            [54, 92, 78].every(
              (value, index) => Math.abs(torso[index] - value) < 10,
            ) &&
            transparent[3] === 255 &&
            [200, 210, 198].every(
              (value, index) => Math.abs(transparent[index] - value) < 10,
            )
          );
        }),
    )
    .toBe(true);
}

test("all kinds confirm the actual snapshot, submit once, and read it unchanged from own history", async ({
  page,
}) => {
  const state = control();
  await open(page, state);
  for (const [kind, label] of [
    ["issuance", "新办"],
    ["duplication", "增办"],
    ["replacement", "换证"],
  ] as const) {
    if (state.saved.length)
      await page.goto(origin + "/workforce/applications/new");
    await prepare(page);
    await page.getByRole("radio", { name: label, exact: true }).check();
    await preview(page);
    await expect(
      page.getByRole("heading", { name: "确认本次工牌申请" }),
    ).toBeFocused();
    await page
      .getByRole("button", { name: "确认并提交申请", exact: true })
      .click();
    await expect(
      page.getByRole("heading", { name: "工牌申请已提交" }),
    ).toBeVisible();
    expect(state.submissions.at(-1)?.kind).toBe(kind);
    await page.getByRole("link", { name: "查看已保存的申请" }).click();
    await expect(
      page.getByRole("img", { name: "本次工牌完整预览" }),
    ).toBeVisible();
    await expect(
      page.getByText("阿依古丽·买买提", { exact: true }),
    ).toBeVisible();
  }
  expect(state.submissions).toHaveLength(3);
  expect(state.saved).toHaveLength(3);
  expect(
    state.mediaHeaders.every(
      (headers) =>
        headers["session-id"] === accountId && !headers["operation-id"],
    ),
  ).toBe(true);
  expect(
    await page.evaluate(() => [sessionStorage.length, localStorage.length]),
  ).toEqual([0, 0]);
});
test("an uncertain committed submission offers only a list refresh and never resends", async ({
  page,
}) => {
  const state = control();
  state.loseSubmitResponse = true;
  await open(page, state);
  await prepare(page);
  await preview(page);
  await page
    .getByRole("button", { name: "确认并提交申请", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "未能确认，请刷新查看" }),
  ).toBeVisible();
  await expect(page.getByRole("button", { name: /提交/ })).toHaveCount(0);
  await page.getByRole("link", { name: "刷新申请列表" }).click();
  await expect(
    page.getByRole("link", { name: "查看详情", exact: true }),
  ).toHaveCount(1);
  expect(state.submissions).toHaveLength(1);
});
test("data_changed discards the accepted preview and requires a fresh explicit preview", async ({
  page,
}) => {
  const state = control();
  state.changed = true;
  await open(page, state);
  await prepare(page);
  await preview(page);
  await page
    .getByRole("button", { name: "确认并提交申请", exact: true })
    .click();
  await expect(page.getByRole("alert")).toContainText("工牌内容已变化");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  expect(state.submissions).toHaveLength(1);
  expect(state.saved).toHaveLength(0);
});
test("photo capacity refusal stays editable without preview, submission or automatic upload retry", async ({
  page,
}) => {
  const state = control();
  state.capacityExhausted = true;
  let uploads = 0;
  page.on("request", (request) => {
    if (request.url().endsWith("/api/v1/workforce/application-photos"))
      uploads++;
  });
  await open(page, state);
  await prepare(page);
  await page.clock.install();
  await page.getByRole("button", { name: "预览工牌", exact: true }).click();
  await expect(page.getByRole("alert")).toContainText(
    "暂时无法加载，请刷新后重试。",
  );
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(
    page.getByRole("heading", { name: "未能确认，请刷新查看" }),
  ).toHaveCount(0);
  await page.clock.fastForward(6000);
  await expect(
    page.getByRole("button", { name: "预览工牌", exact: true }),
  ).toBeEnabled();
  expect(uploads).toBe(1);
  expect(state.previews).toHaveLength(0);
  expect(state.submissions).toHaveLength(0);
  expect(state.saved).toHaveLength(0);
});
for (const phase of ["preview", "submit"] as const) {
  test(`an unavailable photo at ${phase} returns to editing without replay or an unknown outcome`, async ({
    page,
  }) => {
    const state = control();
    state.photoUnavailable = phase;
    await open(page, state);
    await prepare(page);
    if (phase === "preview")
      await page.getByRole("button", { name: "预览工牌", exact: true }).click();
    else {
      await preview(page);
      await page
        .getByRole("button", { name: "确认并提交申请", exact: true })
        .click();
    }
    await expect(page.getByRole("alert")).toContainText("无法使用这项内容");
    await expect(page.getByRole("dialog")).toHaveCount(0);
    await expect(
      page.getByRole("heading", { name: "未能确认，请刷新查看" }),
    ).toHaveCount(0);
    expect(state.previews).toHaveLength(1);
    expect(state.submissions).toHaveLength(phase === "submit" ? 1 : 0);
    expect(state.saved).toHaveLength(0);
  });
}
test("missing fonts and invalid UTF-8 line ranges cannot become a confirmed preview", async ({
  page,
}) => {
  const state = control();
  state.badFont = true;
  await open(page, state);
  await expect(
    page.getByText("工牌预览无法完整显示，请联系管理员检查字体、照片和模板。", {
      exact: true,
    }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "预览工牌", exact: true }),
  ).toHaveCount(0);
  state.badFont = false;
  state.badLine = true;
  await page.goto(origin + "/workforce/applications/new");
  await prepare(page);
  await page.getByRole("button", { name: "预览工牌", exact: true }).click();
  await expect(page.getByRole("alert")).toContainText("工牌预览无法完整显示");
  await expect(
    page.getByRole("button", { name: "确认并提交申请", exact: true }),
  ).toHaveCount(0);
  expect(state.submissions).toHaveLength(0);
});
test("no current number still permits full history; suspended service exposes summaries without resource reads", async ({
  page,
}) => {
  const state = control();
  await open(page, state);
  await prepare(page);
  await preview(page);
  await page
    .getByRole("button", { name: "确认并提交申请", exact: true })
    .click();
  await expect(page.getByText("工牌申请已提交", { exact: true })).toBeVisible();
  state.number = null;
  await page.goto(origin + "/workforce/applications");
  await page.getByRole("link", { name: "查看详情", exact: true }).click();
  await expect(
    page.getByRole("img", { name: "本次工牌完整预览" }),
  ).toBeVisible();
  for (const status of ["suspended", "terminated"]) {
    state.grant = status;
    state.mediaHeaders = [];
    await page.goto(origin + `/workforce/applications/${state.saved[0].id}`);
    await expect(
      page.getByText("当前仅能查看申请摘要，工牌内容和照片暂不开放。", {
        exact: true,
      }),
    ).toBeVisible();
    await expect(
      page.getByRole("img", { name: "本次工牌完整预览" }),
    ).toHaveCount(0);
    await expect(
      page.getByText("阿依古丽·买买提", { exact: true }),
    ).toHaveCount(0);
    expect(state.mediaHeaders).toHaveLength(0);
  }
});
test("a failed list is not displayed as no applications", async ({ page }) => {
  const state = control();
  state.listFailure = true;
  await open(page, state, "/workforce/applications");
  await expect(
    page.getByText("暂时无法加载，请刷新后重试。", { exact: true }),
  ).toBeVisible();
  await expect(page.getByText("暂无申请", { exact: true })).toHaveCount(0);
});
test("leaving clears the photo canvas and a later account selection cannot accept the old submit response", async ({
  page,
}) => {
  const state = control();
  let release!: () => void;
  state.gate = new Promise((resolve) => {
    release = resolve;
  });
  await open(page, state);
  await prepare(page);
  const canvas = await page
    .getByRole("img", { name: "工牌照片裁剪预览" })
    .elementHandle();
  await page.evaluate(() => window.dispatchEvent(new Event("pagehide")));
  expect(
    await canvas!.evaluate((node: HTMLCanvasElement) => [
      node.width,
      node.height,
    ]),
  ).toEqual([0, 0]);
  await page.evaluate(() => window.dispatchEvent(new Event("pageshow")));
  await prepare(page);
  await preview(page);
  await page
    .getByRole("button", { name: "确认并提交申请", exact: true })
    .click();
  await expect.poll(() => state.submissions.length).toBe(1);
  await chooseSession(page, otherAccountId);
  release();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(page.getByText("阿依古丽·买买提", { exact: true })).toHaveCount(
    0,
  );
  await expect(
    page.getByRole("heading", { name: "工牌申请已提交" }),
  ).toHaveCount(0);
  expect(state.submissions).toHaveLength(1);
});
for (const width of [320, 390, 768, 1440])
  for (const colorScheme of ["light", "dark"] as const) {
    test(`real preparation and confirmation ${width} ${colorScheme}`, async ({
      page,
    }) => {
      const state = control();
      await page.setViewportSize({ width, height: 1000 });
      await page.emulateMedia({ colorScheme });
      await open(page, state);
      await prepare(page);
      expect(
        await page.evaluate(() => document.documentElement.scrollWidth),
      ).toBe(width);
      await page.screenshot({
        path: `${process.env.HALO_APPLICATION_PNG ?? "test-results"}/application-ready-${width}-${colorScheme}.png`,
        fullPage: true,
      });
      await preview(page);
      await expect(
        page.getByRole("heading", { name: "确认本次工牌申请" }),
      ).toBeFocused();
      await page.screenshot({
        path: `${process.env.HALO_APPLICATION_PNG ?? "test-results"}/application-confirm-${width}-${colorScheme}.png`,
        fullPage: true,
      });
      await page.keyboard.press("Escape");
      await expect(
        page.getByRole("button", { name: "预览工牌", exact: true }),
      ).toBeFocused();
    });
  }
test("mixed RTL and Latin text consumes UTF-8 ranges and the configured Arabic font", async ({
  page,
}) => {
  const state = control();
  state.arabic = true;
  await open(page, state);
  await prepare(page);
  await page
    .getByLabel("职称（选填）", { exact: true })
    .selectOption({ index: 1 });
  await preview(page);
  await expect(page.getByText("ليلى ABC", { exact: true })).toBeVisible();
  await page.screenshot({
    path: `${process.env.HALO_APPLICATION_PNG ?? "test-results"}/application-confirm-rtl.png`,
    fullPage: true,
  });
  expect(state.previews).toHaveLength(1);
  expect(state.submissions).toHaveLength(0);
});

test("320 by 640 keeps confirmation actions visible while the complete content scrolls", async ({
  page,
}) => {
  const state = control();
  await page.setViewportSize({ width: 320, height: 640 });
  await open(page, state);
  await prepare(page);
  await preview(page);
  const button = page.getByRole("button", {
    name: "确认并提交申请",
    exact: true,
  });
  const before = await button.boundingBox();
  expect(before!.y).toBeGreaterThanOrEqual(0);
  expect(before!.y + before!.height).toBeLessThanOrEqual(640);
  const content = page.locator(".application-dialog-content");
  expect(
    await content.evaluate((node) => node.scrollHeight > node.clientHeight),
  ).toBe(true);
  await content.evaluate((node) => {
    node.scrollTop = node.scrollHeight;
  });
  const after = await button.boundingBox();
  expect(after!.y).toBe(before!.y);
  await page.screenshot({
    path: `${process.env.HALO_APPLICATION_PNG ?? "test-results"}/application-confirm-320x640.png`,
    fullPage: true,
  });
  await button.click();
  await expect(
    page.getByRole("heading", { name: "工牌申请已提交" }),
  ).toBeVisible();
});

test("the submission deadline stays unknown even when a late server result completes", async ({
  page,
}) => {
  const state = control();
  let release!: () => void;
  state.gate = new Promise((resolve) => {
    release = resolve;
  });
  await open(page, state);
  await prepare(page);
  await preview(page);
  await page.clock.install();
  await page
    .getByRole("button", { name: "确认并提交申请", exact: true })
    .click();
  await expect.poll(() => state.submissions.length).toBe(1);
  await page.clock.fastForward(35_001);
  await expect(
    page.getByRole("heading", { name: "未能确认，请刷新查看" }),
  ).toBeVisible();
  release();
  await expect.poll(() => state.saved.length).toBe(1);
  await expect(
    page.getByRole("heading", { name: "未能确认，请刷新查看" }),
  ).toBeVisible();
  expect(state.submissions).toHaveLength(1);
});

test("refreshing eligibility can reopen the form after service restoration", async ({
  page,
}) => {
  const state = control();
  state.grant = "suspended";
  await open(page, state);
  await expect(
    page.getByText(
      "当前员工服务不可用于新申请。你仍可查看允许访问的申请记录。",
      { exact: true },
    ),
  ).toBeVisible();
  await expect(
    page.getByRole("link", { name: "申请工号", exact: true }),
  ).toHaveCount(0);
  state.grant = "active";
  await page.getByRole("button", { name: "刷新办理内容", exact: true }).click();
  await expect(page.getByLabel("实际科室", { exact: true })).toBeVisible();
  expect(state.submissions).toHaveLength(0);
});

test("own history uses page, size and count, including multiple applications with the same timestamp", async ({
  page,
}) => {
  const state = control();
  await open(page, state);
  await prepare(page);
  await preview(page);
  await page
    .getByRole("button", { name: "确认并提交申请", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "工牌申请已提交" }),
  ).toBeVisible();
  const first = state.saved[0];
  for (let index = 1; index <= 20; index++)
    state.saved.push({ ...first, id: id(100 + index) });
  await page.goto(origin + "/workforce/applications");
  await expect(
    page.getByRole("link", { name: "查看详情", exact: true }),
  ).toHaveCount(20);
  await expect(
    page.getByText("共 21 项 · 第 1 页", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("link", { name: "查看详情", exact: true }).first(),
  ).toHaveAttribute("href", `/workforce/applications/${id(120)}`);
  await page.getByRole("button", { name: "下一页", exact: true }).click();
  await expect(
    page.getByRole("link", { name: "查看详情", exact: true }),
  ).toHaveCount(1);
  await expect(
    page.getByRole("button", { name: "下一页", exact: true }),
  ).toBeDisabled();
  await page.getByRole("button", { name: "刷新列表", exact: true }).click();
  await expect(
    page.getByText("共 21 项 · 第 1 页", { exact: true }),
  ).toBeVisible();
});

test("session rejection removes the visible snapshot and clears only the selected session", async ({
  page,
}) => {
  const state = control();
  await open(page, state);
  await prepare(page);
  await preview(page);
  await page
    .getByRole("button", { name: "确认并提交申请", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "工牌申请已提交" }),
  ).toBeVisible();
  await page.getByRole("link", { name: "查看已保存的申请" }).click();
  await expect(
    page.getByRole("img", { name: "本次工牌完整预览" }),
  ).toBeVisible();
  state.sessionInvalid = true;
  await page.getByRole("button", { name: "刷新详情", exact: true }).click();
  await expect(
    page.getByRole("link", { name: "手机号登录", exact: true }),
  ).toBeVisible();
  await expect(page.getByRole("img", { name: "本次工牌完整预览" })).toHaveCount(
    0,
  );
  await expect(page.getByText("阿依古丽·买买提", { exact: true })).toHaveCount(
    0,
  );
  expect(
    await page.evaluate(
      async () => (await window.applicationSessions.read())?.session ?? null,
    ),
  ).toBe(null);
});
