import { test, expect } from "@playwright/test";
import { build } from "vite";
import tailwindcss from "@tailwindcss/vite";
import { resolve } from "node:path";
import { applicationId } from "./application-material";

const origin = "http://cancellation-dialog.localhost";
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
    define: { __BUILD_ID__: JSON.stringify("cancellation-dialog-fixture") },
    build: {
      write: false,
      cssCodeSplit: false,
      assetsInlineLimit: 0,
      rolldownOptions: {
        input: resolve("tests/fixtures/cancellation-dialog.tsx"),
        output: { entryFileNames: "fixture.js" },
      },
    },
  });
  if (Array.isArray(result) || !("output" in result))
    throw new Error("Expected one fixture build");
  let styles = "";
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
      styles += `<link rel="stylesheet" href="/${item.fileName}">`;
  }
  html = `<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><link rel="icon" href="data:,">${styles}</head><body><div id="fixture"></div><script type="module" src="/fixture.js"></script></body></html>`;
});
test.beforeEach(async ({ page }) => {
  await page.route(`${origin}/**`, (route) => {
    const resource = resources.get(new URL(route.request().url()).pathname);
    return route.fulfill(resource ?? { body: html, contentType: "text/html" });
  });
});

for (const colorScheme of ["light", "dark"] as const) {
  test(`320px ${colorScheme}: manager reason, safe focus and pending actions`, async ({
    page,
  }, info) => {
    await page.setViewportSize({ width: 320, height: 640 });
    await page.emulateMedia({ colorScheme });
    await page.goto(origin + "/?mode=manager&status=approved");
    const trigger = page.getByRole("button", { name: "取消申请", exact: true });
    await trigger.click();
    const dialog = page.getByRole("dialog");
    const keep = dialog.getByRole("button", { name: "保留申请", exact: true });
    const confirm = dialog.getByRole("button", {
      name: "取消申请",
      exact: true,
    });
    const reason = dialog.getByRole("textbox", { name: "取消说明" });
    await expect(keep).toBeFocused();
    await expect(dialog).toContainText("申请人如需办理，须重新申请。");
    await expect(dialog).toContainText("仅用于管理记录，不向申请人展示。");
    await reason.fill(" \n ");
    await confirm.click();
    await expect(reason).toBeFocused();
    await expect(reason).toHaveAttribute("aria-invalid", "true");
    await expect(page.getByLabel("已提交次数")).toHaveText("0");
    const bounds = await dialog.boundingBox();
    const action = await confirm.boundingBox();
    const error = await dialog.getByRole("alert").boundingBox();
    expect(bounds!.x).toBeGreaterThanOrEqual(0);
    expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(320);
    expect(action!.y + action!.height).toBeLessThanOrEqual(640);
    expect(error!.y).toBeGreaterThan(bounds!.y);
    expect(error!.y + error!.height).toBeLessThan(action!.y);
    await page.screenshot({
      path: info.outputPath(`manager-error-320-${colorScheme}.png`),
    });
    await reason.press("Escape");
    await expect(dialog).toHaveCount(0);
    await expect(trigger).toBeFocused();
    await trigger.click();
    await expect(reason).toHaveValue("");
    const oversized = "界".repeat(2800);
    await reason.fill(oversized);
    await confirm.click();
    await expect(dialog.getByRole("alert")).toHaveText(
      "取消说明过长，请精简后再提交。",
    );
    await expect(reason).toHaveValue(oversized);
    await expect(page.getByLabel("已提交次数")).toHaveText("0");
    const note =
      "本申请的科室需要重新核对，本次仅取消这份申请。\n\n请核对完整姓名、科室、职称和领取地点后重新申请。".repeat(
        12,
      );
    await reason.fill(note);
    await expect(reason).toHaveValue(note);
    await confirm.click();
    await expect(page.getByLabel("已提交次数")).toHaveText("1");
    await expect(page.getByLabel("已提交说明")).toHaveText(note);
    await expect(
      dialog.getByRole("button", { name: "正在取消…", exact: true }),
    ).toBeDisabled();
    await expect(keep).toBeDisabled();
    await page.keyboard.press("Escape");
    await expect(dialog).toBeVisible();
    await expect(page.getByLabel("已提交次数")).toHaveText("1");
  });

  for (const status of ["submitted", "approved"] as const) {
    test(`320px ${colorScheme}: self ${status} confirmation has no management reason`, async ({
      page,
    }, info) => {
      await page.setViewportSize({ width: 320, height: 640 });
      await page.emulateMedia({ colorScheme });
      await page.goto(origin + "/?status=" + status);
      const trigger = page.getByRole("button", {
        name: "取消申请",
        exact: true,
      });
      await trigger.click();
      const dialog = page.getByRole("dialog");
      await expect(
        dialog.getByRole("button", { name: "保留申请", exact: true }),
      ).toBeFocused();
      await expect(dialog.getByRole("textbox")).toHaveCount(0);
      await expect(dialog).toContainText("需要办理时，请重新申请。");
      await expect(dialog).not.toContainText("仅用于管理记录");
      await expect(dialog).toContainText(
        status === "submitted" ? "待审核" : "审核通过",
      );
      await page.screenshot({
        path: info.outputPath(`self-${status}-320-${colorScheme}.png`),
      });
      await dialog
        .getByRole("button", { name: "保留申请", exact: true })
        .click();
      await expect(trigger).toBeFocused();
      await trigger.click();
      await dialog
        .getByRole("button", { name: "取消申请", exact: true })
        .click();
      await expect(page.getByLabel("已提交次数")).toHaveText("1");
      await expect(page.getByLabel("已提交说明")).toHaveText("");
    });
  }

  test(`320px ${colorScheme}: restricted confirmation contains only the allowed summary`, async ({
    page,
  }, info) => {
    await page.setViewportSize({ width: 320, height: 640 });
    await page.emulateMedia({ colorScheme });
    await page.goto(origin + "/?restricted=1&status=approved");
    await page.getByRole("button", { name: "取消申请", exact: true }).click();
    const dialog = page.getByRole("dialog");
    await expect(dialog.locator("dt")).toHaveText([
      "申请编号",
      "申请类型",
      "申请状态",
      "提交时间",
    ]);
    await expect(dialog.locator("dd").first()).toHaveText(applicationId(40));
    await expect(dialog).toContainText("审核通过");
    await expect(dialog).not.toContainText("陈欧阳");
    await expect(dialog).not.toContainText("12605");
    await expect(dialog.getByRole("textbox")).toHaveCount(0);
    await expect(dialog.getByRole("img")).toHaveCount(0);
    await page.screenshot({
      path: info.outputPath(`restricted-320-${colorScheme}.png`),
    });
    await dialog.getByRole("button", { name: "取消申请", exact: true }).click();
    await expect(page.getByLabel("已提交次数")).toHaveText("1");
  });
}
