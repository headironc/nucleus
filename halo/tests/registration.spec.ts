import { test, expect, type Page } from "@playwright/test";
import { build } from "vite";
import tailwindcss from "@tailwindcss/vite";
import { resolve } from "node:path";

const origin = "http://registration.localhost";
const operation = "9ea8cfed-85c5-4f44-8cf6-093a0f919b4c";
const session = {
  id: "019354fd-933c-7e6a-8912-345678901234",
  expiresAt: "2035-01-02T00:00:00Z",
};
const resources = new Map<
  string,
  { body: string | Buffer; contentType: string }
>();
let html: string;

test.beforeAll(async () => {
  const result = await build({
    configFile: false,
    publicDir: false,
    plugins: [tailwindcss()],
    logLevel: "error",
    define: { __BUILD_ID__: JSON.stringify("registration-fixture") },
    build: {
      write: false,
      emptyOutDir: false,
      cssCodeSplit: false,
      assetsInlineLimit: 0,
      rolldownOptions: {
        input: resolve("tests/fixtures/registration.tsx"),
        output: { entryFileNames: "fixture.js" },
      },
    },
  });
  if (Array.isArray(result) || !("output" in result))
    throw new Error("Expected one fixture build");
  const styles: string[] = [];
  for (const item of result.output) {
    const script = item.type === "chunk";
    const contentType =
      script || item.fileName.endsWith(".js")
        ? "text/javascript"
        : item.fileName.endsWith(".css")
          ? "text/css"
          : item.fileName.endsWith(".svg")
            ? "image/svg+xml"
            : "image/png";
    resources.set("/" + item.fileName, {
      body: script ? item.code : Buffer.from(item.source),
      contentType,
    });
    if (contentType === "text/css")
      styles.push(`<link rel="stylesheet" href="/${item.fileName}">`);
  }
  html = `<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><link rel="icon" href="data:,">${styles.join("")}</head><body><div id="fixture"></div><script type="module" src="/fixture.js"></script></body></html>`;
});
test.beforeEach(async ({ context, page }) => {
  await context.route(`${origin}/**`, async (route) => {
    const url = new URL(route.request().url());
    if (url.pathname === "/build.json") {
      await route.fulfill({ json: { buildId: "registration-fixture" } });
      return;
    }
    if (url.pathname.startsWith("/api/")) {
      await route.abort();
      return;
    }
    const resource = resources.get(url.pathname);
    await route.fulfill(resource ?? { contentType: "text/html", body: html });
  });
  await page.goto(origin);
  await page.waitForFunction(() => !!window.registrationSessions);
  await page.evaluate(
    async ({ operation, id }) => {
      const attempt = await window.registrationSessions.begin(operation);
      const now = Date.now();
      sessionStorage.setItem(
        "halo.r1",
        JSON.stringify({
          version: 1,
          kind: "registration",
          attempt,
          registration: {
            id,
            token: "A".repeat(43),
            expiresAt: new Date(now + 600_000).toISOString(),
          },
          createdAt: now,
          expiresAt: now + 600_000,
        }),
      );
    },
    { operation, id: session.id },
  );
  await page.goto(`${origin}/authentication/register`);
  await expect(
    page.getByRole("heading", { name: "创建个人账户" }),
  ).toBeVisible();
});
async function fill(page: Page) {
  await page.getByLabel("姓名", { exact: true }).fill("示例用户");
  await page
    .getByLabel("居民身份证号", { exact: true })
    .fill("11010519491231002X");
}
test("real registration page keeps drafts private, focuses errors and fits all four widths", async ({
  page,
}) => {
  await page.getByRole("button", { name: "创建账户" }).click();
  await expect(page.getByLabel("姓名", { exact: true })).toBeFocused();
  await fill(page);
  expect(
    await page.evaluate(() => sessionStorage.getItem("halo.r2")),
  ).toBeNull();
  for (const width of [320, 390, 768, 1440]) {
    await page.setViewportSize({ width, height: 1000 });
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    ).toBe(true);
    await page.screenshot({
      path: test.info().outputPath(`r2-form-${width}.png`),
      fullPage: true,
    });
  }
  await page.getByLabel("居民身份证号", { exact: true }).focus();
  await page.keyboard.press("Tab");
  await expect(page.getByRole("button", { name: "创建账户" })).toBeFocused();
  page.on("dialog", (dialog) => dialog.accept());
  await page.reload();
  await expect(page.getByLabel("姓名", { exact: true })).toHaveValue("");
  await expect(page.getByLabel("居民身份证号", { exact: true })).toHaveValue(
    "",
  );
});
test("failed pre-submit persistence returns to editing without an automatic POST", async ({
  page,
}) => {
  let posts = 0;
  await page.route("**/api/v1/registrations", async (route) => {
    posts++;
    await route.abort();
  });
  await page.evaluate(() => {
    const set = Storage.prototype.setItem;
    Storage.prototype.setItem = function (key, value) {
      if (key === "halo.r2") {
        Storage.prototype.setItem = set;
        throw new Error("fixture quota");
      }
      set.call(this, key, value);
    };
  });
  await fill(page);
  await page.getByRole("button", { name: "创建账户" }).click();
  await expect(
    page.getByText("本次尚未提交注册，请重新检查。", { exact: false }),
  ).toBeVisible();
  expect(posts).toBe(0);
  await page.getByRole("button", { name: "重新检查" }).click();
  await expect(
    page.getByRole("heading", { name: "创建个人账户" }),
  ).toBeVisible();
  await expect(page.getByLabel("姓名", { exact: true })).toHaveValue(
    "示例用户",
  );
  expect(posts).toBe(0);
});
test("unknown result reload keeps original input and proof mismatch stops original recovery", async ({
  page,
}) => {
  const requests: { operation: string; input: unknown }[] = [];
  await page.route("**/api/v1/registrations", async (route) => {
    requests.push({
      operation: route.request().headers()["operation-id"],
      input: route.request().postDataJSON(),
    });
    if (requests.length === 1) await route.abort();
    else
      await route.fulfill({ status: 409, json: { code: "request_mismatch" } });
  });
  await fill(page);
  await page.getByRole("button", { name: "创建账户" }).click();
  await expect(
    page.getByRole("heading", { name: "暂时无法确认注册结果" }),
  ).toBeVisible();
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "暂时无法确认注册结果" }),
  ).toBeVisible();
  expect(requests).toHaveLength(1);
  await expect(
    page.getByRole("button", { name: "查看注册结果" }),
  ).toBeEnabled();
  await page.getByRole("button", { name: "查看注册结果" }).click();
  await expect(
    page.getByRole("heading", { name: "暂时无法确认注册结果" }),
  ).toBeVisible();
  expect(requests).toHaveLength(2);
  expect(requests[1]).toEqual(requests[0]);
  await page.reload();
  await expect(page.getByRole("button", { name: "查看注册结果" })).toHaveCount(
    0,
  );
  await expect(page.getByRole("link", { name: "检查登录状态" })).toBeVisible();
  expect(
    await page.evaluate(() => sessionStorage.getItem("halo.r2")),
  ).toContain("11010519491231002X");
  await expect(page.getByRole("textbox")).toHaveCount(0);
});
test("a delayed R2 success cannot replace another tab's actual IndexedDB selection", async ({
  page,
  context,
}) => {
  let captured!: () => void;
  let release!: () => void;
  const entered = new Promise<void>((resolve) => {
    captured = resolve;
  });
  const waiting = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route("**/api/v1/registrations", async (route) => {
    captured();
    await waiting;
    await route
      .fulfill({ status: 201, json: { code: "ok", session } })
      .catch(() => {});
  });
  await fill(page);
  await page.getByRole("button", { name: "创建账户" }).click();
  await entered;
  const other = await context.newPage();
  await other.goto(origin);
  await other.waitForFunction(() => !!window.registrationSessions);
  const later = { ...session, id: "019354fd-933c-7e6a-8912-345678901299" };
  await other.evaluate(async (later) => {
    const attempt = await window.registrationSessions.begin(
      crypto.randomUUID(),
    );
    await window.registrationSessions.accept(attempt, later);
  }, later);
  release();
  await page.bringToFront();
  await expect(
    page.getByRole("heading", { name: "登录状态有变化" }),
  ).toBeVisible();
  expect(
    (await other.evaluate(() => window.registrationSessions.read()))?.session,
  ).toEqual(later);
  await expect(page.getByRole("textbox")).toHaveCount(0);
  await other.close();
});
