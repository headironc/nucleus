import { test, expect } from "@playwright/test";
import { build } from "vite";
import tailwindcss from "@tailwindcss/vite";
import { resolve } from "node:path";

const origin = "http://badge-image.localhost";
const resources = new Map<
  string,
  { body: string | Buffer; contentType: string }
>();
let html: string;

test.beforeAll(async ({ browser }) => {
  console.info(`Badge image display browser: ${browser.version()}`);
  const result = await build({
    configFile: false,
    publicDir: false,
    plugins: [tailwindcss()],
    logLevel: "error",
    define: { __BUILD_ID__: JSON.stringify("badge-image-fixture") },
    build: {
      write: false,
      emptyOutDir: false,
      cssCodeSplit: false,
      assetsInlineLimit: 0,
      rolldownOptions: {
        input: resolve("tests/fixtures/badge-image.tsx"),
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
  await context.route("**/*", async (route) => {
    const url = new URL(route.request().url());
    if (url.origin !== origin) return route.abort();
    const resource = resources.get(url.pathname);
    if (resource) return route.fulfill(resource);
    if (url.pathname === "/")
      return route.fulfill({ contentType: "text/html", body: html });
    return route.abort();
  });
  await page.goto(origin);
  await page.waitForFunction(() => !!window.badgeFixture);
  await page.evaluate(() => window.badgeFixture.ready());
  await expect(
    page.getByRole("img", { name: "已保存的工牌图片" }),
  ).toBeVisible();
});

test("opens the stored image and returns focus to its trigger on Escape", async ({
  page,
}, info) => {
  await page.getByRole("button", { name: "预览图片", exact: true }).click();
  const dialog = page.getByRole("dialog");
  await expect(
    dialog.getByRole("heading", { name: "工牌图片", exact: true }),
  ).toBeFocused();
  await expect(dialog.getByRole("img")).toBeVisible();
  await page.screenshot({ path: info.outputPath("preview-desktop.png") });
  await page.keyboard.press("Escape");
  await expect(dialog).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "预览图片", exact: true }),
  ).toBeFocused();
});

test("preserves the preview during a failed download and invokes a fresh retry action", async ({
  page,
}) => {
  const image = page.getByRole("img", { name: "已保存的工牌图片" });
  const source = await image.getAttribute("src");
  await page.getByRole("button", { name: "下载图片", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "准备下载…", exact: true }),
  ).toBeDisabled();
  expect(await page.evaluate(() => window.badgeFixture.downloads)).toBe(1);
  await page.evaluate(() => window.badgeFixture.downloadFailed());
  await expect(image).toBeVisible();
  await expect(image).toHaveAttribute("src", source!);
  await expect(page.getByRole("status")).toHaveText(
    "暂时无法取得下载文件，请重新下载。",
  );
  await page.getByRole("button", { name: "重新下载", exact: true }).click();
  expect(await page.evaluate(() => window.badgeFixture.downloads)).toBe(2);
  await expect(image).toHaveAttribute("src", source!);
});

test("keeps an expanded preview available when its independent download fails", async ({
  page,
}, info) => {
  await page.getByRole("button", { name: "预览图片", exact: true }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByRole("button", { name: "下载图片", exact: true }).click();
  await page.evaluate(() => window.badgeFixture.downloadFailed());
  await expect(dialog.getByRole("img")).toBeVisible();
  await expect(dialog.getByRole("status")).toHaveText(
    "暂时无法取得下载文件，请重新下载。",
  );
  await page.screenshot({
    path: info.outputPath("preview-download-error.png"),
  });
  await dialog.getByRole("button", { name: "重新下载", exact: true }).click();
  expect(await page.evaluate(() => window.badgeFixture.downloads)).toBe(2);
  await expect(dialog.getByRole("img")).toBeVisible();
});

test("removes an open private image when its owner hides the page", async ({
  page,
}) => {
  const source = await page.getByRole("img").getAttribute("src");
  await page.getByRole("button", { name: "预览图片", exact: true }).click();
  await page.evaluate(() => window.badgeFixture.hide());
  await expect(page.getByRole("dialog")).toHaveCount(0);
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
});

test("keeps the generated fact and offers a read after preview failure", async ({
  page,
}) => {
  await page.evaluate(() => window.badgeFixture.mediaFailed());
  await expect(page.getByRole("status")).toHaveText("工牌文件暂时无法读取");
  await expect(page.getByText("工牌图片已生成", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "重新读取", exact: true }).click();
  expect(await page.evaluate(() => window.badgeFixture.reads)).toBe(1);
  await expect(
    page.getByRole("button", { name: "生成工牌图片", exact: true }),
  ).toHaveCount(0);
});

test("fits a 320 pixel dark viewport with an open preview", async ({
  page,
}, info) => {
  await page.setViewportSize({ width: 320, height: 740 });
  await page.emulateMedia({ colorScheme: "dark" });
  await page.getByRole("button", { name: "预览图片", exact: true }).click();
  await expect(page.getByRole("dialog").getByRole("img")).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(
    320,
  );
  const bounds = await page.getByRole("dialog").boundingBox();
  expect(bounds!.x).toBeGreaterThanOrEqual(0);
  expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(320);
  await page
    .getByRole("dialog")
    .getByRole("button", { name: "下载图片", exact: true })
    .click();
  const pending = page
    .getByRole("dialog")
    .getByRole("button", { name: "准备下载…", exact: true });
  await expect(pending).toBeDisabled();
  expect((await pending.boundingBox())!.height).toBeLessThanOrEqual(56);
  await page.screenshot({ path: info.outputPath("preview-dark-320.png") });
});
