import { test, expect } from "@playwright/test";
import { build } from "vite";
import tailwindcss from "@tailwindcss/vite";
import { resolve } from "node:path";

const origin = "http://review-dialog.localhost";
const resources = new Map<
  string,
  { body: string | Buffer; contentType: string }
>();
let html = "";
test.beforeAll(async () => {
  const result = await build({
    configFile: false,
    publicDir: false,
    plugins: [tailwindcss()],
    logLevel: "error",
    define: { __BUILD_ID__: JSON.stringify("review-dialog-fixture") },
    build: {
      write: false,
      emptyOutDir: false,
      cssCodeSplit: false,
      assetsInlineLimit: 0,
      rolldownOptions: {
        input: resolve("tests/fixtures/review-dialog.tsx"),
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
test.beforeEach(async ({ page }) => {
  await page.route(`${origin}/**`, (route) => {
    const resource = resources.get(new URL(route.request().url()).pathname);
    return route.fulfill(resource ?? { body: html, contentType: "text/html" });
  });
});

for (const colorScheme of ["light", "dark"] as const) {
  test(`320px ${colorScheme}: empty reason focuses the field, Escape restores focus, and long content keeps actions reachable`, async ({
    page,
  }, info) => {
    await page.setViewportSize({ width: 320, height: 640 });
    await page.emulateMedia({ colorScheme });
    await page.goto(origin);
    const trigger = page.getByRole("button", { name: "驳回申请", exact: true });
    await trigger.click();
    const dialog = page.getByRole("dialog");
    await expect(dialog).toBeVisible();
    await dialog.getByRole("button", { name: "确认驳回" }).click();
    const reason = page.getByRole("textbox", { name: "驳回理由" });
    await expect(reason).toBeFocused();
    await expect(reason).toHaveAttribute("aria-invalid", "true");
    await expect(page.getByLabel("已提交次数")).toHaveText("0");
    await page.screenshot({
      path: info.outputPath(`empty-reason-320-${colorScheme}.png`),
      fullPage: true,
    });
    const bounds = await dialog.boundingBox();
    expect(bounds!.x).toBeGreaterThanOrEqual(0);
    expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(320);
    const confirm = await dialog
      .getByRole("button", { name: "确认驳回" })
      .boundingBox();
    expect(confirm!.y + confirm!.height).toBeLessThanOrEqual(640);
    const error = await page.getByRole("alert").boundingBox();
    expect(error!.y).toBeGreaterThan(bounds!.y);
    expect(error!.y + error!.height).toBeLessThan(confirm!.y);
    await reason.press("Escape");
    await expect(dialog).toHaveCount(0);
    await expect(trigger).toBeFocused();
    await trigger.click();
    await reason.fill("理".repeat(3000));
    await page.getByRole("button", { name: "确认驳回" }).click();
    await expect(
      page.getByText("驳回理由过长，请精简后再提交。"),
    ).toBeVisible();
    await expect(page.getByLabel("已提交次数")).toHaveText("0");
    const fullReason =
      "请使用正面、无遮挡的照片。\n\n请保留完整头部和肩部，并核对完整姓名与科室。".repeat(
        40,
      );
    await reason.fill(fullReason);
    await page.getByRole("button", { name: "确认驳回" }).click();
    await expect(page.getByLabel("已提交次数")).toHaveText("1");
    await expect(page.getByLabel("已提交理由")).toHaveText(fullReason);
    await expect(
      page.getByRole("button", { name: "正在提交…", exact: true }),
    ).toBeDisabled();
    await page.keyboard.press("Escape");
    await expect(dialog).toBeVisible();
    await expect(page.getByLabel("已提交次数")).toHaveText("1");
  });
}
