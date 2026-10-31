import { test, expect, type Page } from "@playwright/test";
import { build } from "vite";
import tailwindcss from "@tailwindcss/vite";
import { resolve } from "node:path";

const origin = "http://application-photo.localhost";
const resources = new Map<string, { body: string | Buffer; contentType: string }>();
let html: string;
test.beforeAll(async ({ browser }) => {
  console.info(`Application photo browser: ${browser.version()}`);
  const result = await build({ configFile: false, publicDir: false, plugins: [tailwindcss()], logLevel: "error", build: {
    write: false, emptyOutDir: false, cssCodeSplit: false, assetsInlineLimit: 0,
    rolldownOptions: { input: resolve("tests/fixtures/application-photo.tsx"), output: { entryFileNames: "fixture.js" } },
  } });
  if (Array.isArray(result) || !("output" in result)) throw new Error("Expected one fixture build");
  const styles: string[] = [];
  for (const item of result.output) {
    const contentType = item.fileName.endsWith(".js") ? "text/javascript" : item.fileName.endsWith(".css") ? "text/css" : item.fileName.endsWith(".svg") ? "image/svg+xml" : "image/png";
    resources.set("/" + item.fileName, { body: item.type === "chunk" ? item.code : Buffer.from(item.source), contentType });
    if (contentType === "text/css") styles.push(`<link rel="stylesheet" href="/${item.fileName}">`);
  }
  html = `<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><link rel="icon" href="data:,">${styles.join("")}</head><body><div id="fixture"></div><script type="module" src="/fixture.js"></script></body></html>`;
});
test.beforeEach(async ({ page }) => {
  await page.route("**/*", async (route) => {
    const url = new URL(route.request().url());
    if (url.origin !== origin) return route.abort();
    const resource = resources.get(url.pathname);
    if (resource) return route.fulfill(resource);
    if (url.pathname === "/" || url.pathname === "/micrometers") return route.fulfill({ contentType: "text/html", body: html });
    return route.abort();
  });
  await page.goto(origin);
  await page.waitForFunction(() => !!window.applicationPhoto);
});

async function choose(page: Page, width = 800, height = 600) {
  // Actual browser-encoded pixels: a circle reveals any stretching in the crop.
  const base64 = await page.evaluate(({ width, height }) => {
    const canvas = document.createElement("canvas"); canvas.width = width; canvas.height = height;
    const context = canvas.getContext("2d")!;
    context.fillStyle = "#f4ede0"; context.fillRect(0, 0, width, height);
    context.fillStyle = "#34624e"; context.beginPath(); context.arc(width / 2, height / 2, Math.min(width, height) / 4, 0, Math.PI * 2); context.fill();
    context.fillStyle = "#9b4239"; context.fillRect(0, 0, width / 8, height);
    return canvas.toDataURL("image/png").split(",")[1];
  }, { width, height });
  await page.locator('input[type="file"]').setInputFiles({ name: "本人明确裁剪的照片.png", mimeType: "image/png", buffer: Buffer.from(base64, "base64") });
  await expect(page.getByRole("img", { name: "工牌照片裁剪预览" })).toBeVisible();
}

test("confirms the displayed crop as PNG bytes without stretching the circle or retaining source metadata", async ({ page }) => {
  await choose(page);
  await expect(page.getByRole("button", { name: "确认裁剪效果", exact: true })).toBeEnabled();
  await page.getByRole("button", { name: "确认裁剪效果", exact: true }).click();
  await expect(page.getByRole("button", { name: "已确认裁剪效果" })).toBeDisabled();
  const result = await page.evaluate(async () => {
    const state = window.applicationPhoto.snapshot();
    if (state.kind !== "editing" || !state.confirmed) throw new Error("Expected confirmed photo");
    const bitmap = await createImageBitmap(state.confirmed.file);
    const canvas = document.createElement("canvas"); canvas.width = bitmap.width; canvas.height = bitmap.height;
    const context = canvas.getContext("2d")!; context.drawImage(bitmap, 0, 0); bitmap.close();
    const green = (x: number, y: number) => { const pixel = context.getImageData(x, y, 1, 1).data; return pixel[1] > pixel[0] && pixel[1] > pixel[2]; };
    let horizontal = 0, vertical = 0;
    for (let x = 0; x < canvas.width; x++) if (green(x, canvas.height / 2)) horizontal++;
    for (let y = 0; y < canvas.height; y++) if (green(canvas.width / 2, y)) vertical++;
    return { width: canvas.width, height: canvas.height, type: state.confirmed.file.type, horizontal, vertical, hasExif: new TextDecoder().decode(await state.confirmed.file.arrayBuffer()).includes("eXIf") };
  });
  expect(result).toEqual({ width: 450, height: 600, type: "image/png", horizontal: 300, vertical: 300, hasExif: false });
  await page.getByLabel("放大", { exact: true }).focus();
  await page.keyboard.press("ArrowRight");
  await expect(page.getByRole("button", { name: "确认裁剪效果", exact: true })).toBeEnabled();
  expect(await page.evaluate(() => { const s = window.applicationPhoto.snapshot(); return s.kind === "editing" && s.confirmed === null; })).toBe(true);
});

test("removal returns focus to the file field and destroys the previous canvas", async ({ page }) => {
  await choose(page, 600, 800);
  const node = await page.getByRole("img").elementHandle();
  await page.getByRole("button", { name: "移除照片" }).click();
  await expect(page.locator('input[type="file"]')).toBeFocused();
  await expect(page.getByRole("img")).toHaveCount(0);
  expect(await node!.evaluate((canvas: HTMLCanvasElement) => [canvas.width, canvas.height])).toEqual([0, 0]);
});

test("rejects a subpixel crop rather than stretching a 3:4 image into a square", async ({ page }) => {
  await page.setViewportSize({ width: 320, height: 900 });
  await page.emulateMedia({ colorScheme: "dark" });
  await choose(page, 3, 4);
  await page.getByLabel("放大", { exact: true }).fill("4");
  await expect(page.getByRole("alert")).toContainText("裁剪区域过小");
  expect(await page.evaluate(() => window.applicationPhoto.snapshot())).toEqual({ kind: "error", error: "crop" });
  await page.screenshot({ path: test.info().outputPath("application-photo-crop-320-dark.png"), fullPage: true });
});

test("photo failures explain the actual input or processing limit without retaining a confirmation", async ({
  page,
}) => {
  await page.setViewportSize({ width: 320, height: 900 });
  await page.emulateMedia({ colorScheme: "dark" });
  const wide = await page.evaluate(() => {
    const canvas = document.createElement("canvas");
    canvas.width = 4097;
    canvas.height = 1;
    return canvas.toDataURL("image/png").split(",")[1];
  });
  // Two complete one-pixel APNG frames, with valid chunk checksums.
  const animation =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAACGFjVEwAAAACAAAAAPONk3AAAAAaZmNUTAAAAAAAAAABAAAAAQAAAAAAAAAAAAEACgAAWn8w0AAAAA1JREFUeJxj+M/A8B8ABQAB/4mZPR0AAAAaZmNUTAAAAAEAAAABAAAAAQAAAAAAAAAAAAEACgAAwQzaBAAAABFmZEFUAAAAAnicY2D4z/AfAAQBAf9i5+mcAAAAAElFTkSuQmCC";
  const cases = [
    {
      error: "size",
      type: "image/png",
      bytes: Buffer.alloc(8 * 1024 * 1024 + 1),
      copy: "照片超过 8 MiB，请选择较小的照片后重新裁剪。",
    },
    {
      error: "format",
      type: "image/svg+xml",
      bytes: Buffer.from("<svg/>"),
      copy: "请选择静态 JPEG、PNG 或 WebP 照片。",
    },
    {
      error: "animation",
      type: "image/png",
      bytes: Buffer.from(animation, "base64"),
      copy: "不支持动画图片，请选择静态 JPEG、PNG 或 WebP 照片。",
    },
    {
      error: "dimensions",
      type: "image/png",
      bytes: Buffer.from(wide, "base64"),
      copy: "照片每边最多 4096 像素，总像素最多 1600 万，请重新选择。",
    },
    {
      error: "damaged",
      type: "image/png",
      bytes: Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
      copy: "暂时无法读取这张照片，请重新选择。",
    },
  ];
  for (const item of cases) {
    await page
      .locator('input[type="file"]')
      .setInputFiles({
        name: "test-photo",
        mimeType: item.type,
        buffer: item.bytes,
      });
    await expect(page.getByRole("alert")).toHaveText(item.copy);
    expect(
      await page.evaluate(() => window.applicationPhoto.snapshot()),
    ).toEqual({ kind: "error", error: item.error });
    await expect(
      page.getByRole("button", { name: "已确认裁剪效果" }),
    ).toHaveCount(0);
    await page.screenshot({
      path: test
        .info()
        .outputPath(`application-photo-${item.error}-320-dark.png`),
      fullPage: true,
    });
  }
  await choose(page);
  // The real paint-failure entry point releases the previously displayed image.
  await page.evaluate(() => window.applicationPhoto.previewFailed());
  await expect(page.getByRole("alert")).toHaveText(
    "照片暂时无法处理，请重新选择，或换用支持图片处理的浏览器。",
  );
  await expect(page.getByRole("img", { name: "工牌照片裁剪预览" })).toHaveCount(
    0,
  );
  await page.screenshot({
    path: test.info().outputPath("application-photo-unavailable-320-dark.png"),
    fullPage: true,
  });
});

test("a micrometers slot accepts a normal photo while keeping the circle square in confirmed pixels", async ({ page }) => {
  await page.goto(origin + "/micrometers");
  await choose(page);
  await page.getByRole("button", { name: "确认裁剪效果", exact: true }).click();
  await expect(page.getByRole("button", { name: "已确认裁剪效果" })).toBeDisabled();
  const result = await page.evaluate(async () => {
    const state = window.applicationPhoto.snapshot();
    if (state.kind !== "editing" || !state.confirmed) throw new Error("Expected confirmed photo");
    const bitmap = await createImageBitmap(state.confirmed.file);
    const canvas = document.createElement("canvas"); canvas.width = bitmap.width; canvas.height = bitmap.height;
    const context = canvas.getContext("2d")!; context.drawImage(bitmap, 0, 0); bitmap.close();
    const green = (x: number, y: number) => { const pixel = context.getImageData(x, y, 1, 1).data; return pixel[1] > pixel[0] && pixel[1] > pixel[2]; };
    let horizontal = 0, vertical = 0;
    for (let x = 0; x < canvas.width; x++) if (green(x, canvas.height / 2)) horizontal++;
    for (let y = 0; y < canvas.height; y++) if (green(canvas.width / 2, y)) vertical++;
    return { width: canvas.width, height: canvas.height, horizontal, vertical };
  });
  expect(result).toEqual({ width: 414, height: 600, horizontal: 300, vertical: 300 });
});

test("the micrometers crop preview contains and centers the same integer image with transparent margins", async ({ page }) => {
  await page.goto(origin + "/micrometers");
  await choose(page, 5, 7);
  const canvas = page.getByRole("img", { name: "工牌照片裁剪预览" });
  const pixels = await canvas.evaluate((node: HTMLCanvasElement) => {
    const context = node.getContext("2d")!;
    const alpha = (x: number) => context.getImageData(x, node.height / 2, 1, 1).data[3];
    let left = 0, right = node.width - 1;
    while (left < node.width && alpha(left) === 0) left++;
    while (right >= 0 && alpha(right) === 0) right--;
    return { left, right, width: node.width, edgeLeft: alpha(0), center: alpha(node.width / 2), edgeRight: alpha(node.width - 1) };
  });
  expect(pixels.edgeLeft).toBe(0); expect(pixels.edgeRight).toBe(0); expect(pixels.center).toBe(255);
  expect(Math.abs(pixels.left - (pixels.width - 1 - pixels.right))).toBeLessThanOrEqual(1);
  await page.getByRole("button", { name: "确认裁剪效果", exact: true }).click();
  await expect(page.getByRole("button", { name: "已确认裁剪效果" })).toBeDisabled();
  expect(await page.evaluate(() => { const state = window.applicationPhoto.snapshot(); return state.kind === "editing" && state.confirmed ? [state.confirmed.width, state.confirmed.height] : null; })).toEqual([4, 7]);
  await page.screenshot({ path: `${process.env.HALO_APPLICATION_PNG ?? "test-results"}/application-photo-micrometers-contain.png`, fullPage: true });
});

for (const [orientation, expected] of [
  [2, ["green", "red", "yellow", "blue"]],
  [5, ["red", "blue", "green", "yellow"]],
  [6, ["blue", "red", "yellow", "green"]],
  [8, ["green", "yellow", "red", "blue"]],
] as const) {
  test(`preserves the shown JPEG orientation ${orientation} and strips its EXIF from confirmed bytes`, async ({ page }) => {
    const base64 = await page.evaluate((orientation) => {
      const rotated = orientation >= 5;
      const canvas = document.createElement("canvas"); canvas.width = rotated ? 160 : 120; canvas.height = rotated ? 120 : 160;
      const context = canvas.getContext("2d")!;
      for (const [index, color] of ["#ee1111", "#11bb11", "#1111ee", "#eeee11"].entries()) {
        context.fillStyle = color;
        context.fillRect((index % 2) * canvas.width / 2, Math.floor(index / 2) * canvas.height / 2, canvas.width / 2, canvas.height / 2);
      }
      return canvas.toDataURL("image/jpeg", 1).split(",")[1];
    }, orientation);
    const source = Buffer.from(base64, "base64");
    const exif = Buffer.from([255, 225, 0, 34, 69, 120, 105, 102, 0, 0, 73, 73, 42, 0, 8, 0, 0, 0, 1, 0, 18, 1, 3, 0, 1, 0, 0, 0, orientation, 0, 0, 0, 0, 0, 0, 0]);
    await page.locator('input[type="file"]').setInputFiles({ name: "oriented.jpg", mimeType: "image/jpeg", buffer: Buffer.concat([source.subarray(0, 2), exif, source.subarray(2)]) });
    await page.getByRole("button", { name: "确认裁剪效果", exact: true }).click();
    await expect(page.getByRole("button", { name: "已确认裁剪效果" })).toBeDisabled();
    const result = await page.evaluate(async () => {
      const state = window.applicationPhoto.snapshot();
      if (state.kind !== "editing" || !state.confirmed) throw new Error("Expected confirmed photo");
      const bitmap = await createImageBitmap(state.confirmed.file);
      const canvas = document.createElement("canvas"); canvas.width = bitmap.width; canvas.height = bitmap.height;
      const context = canvas.getContext("2d")!; context.drawImage(bitmap, 0, 0); bitmap.close();
      const colors = [[0.25, 0.25], [0.75, 0.25], [0.25, 0.75], [0.75, 0.75]].map(([x, y]) => {
        const [r, g, b] = context.getImageData(x * canvas.width, y * canvas.height, 1, 1).data;
        return r > 150 && g > 150 ? "yellow" : r > g && r > b ? "red" : g > b ? "green" : "blue";
      });
      const text = new TextDecoder().decode(await state.confirmed.file.arrayBuffer());
      return { colors, width: canvas.width, height: canvas.height, metadata: text.includes("Exif") || text.includes("eXIf") };
    });
    expect(result).toEqual({ colors: expected, width: 120, height: 160, metadata: false });
  });
}

for (const width of [320, 390, 768, 1440]) for (const colorScheme of ["light", "dark"] as const) {
  test(`photo layout ${width} ${colorScheme}`, async ({ page }) => {
    await page.setViewportSize({ width, height: 1000 }); await page.emulateMedia({ colorScheme });
    await choose(page);
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(width);
    await page.screenshot({ path: `${process.env.HALO_APPLICATION_PNG ?? "test-results"}/application-photo-${width}-${colorScheme}.png`, fullPage: true });
  });
}

test.describe("application choices from the implemented member directory", () => {
  test.beforeAll(async () => {
    const result = await build({ configFile: false, publicDir: false, plugins: [tailwindcss()], logLevel: "error", build: {
      write: false, emptyOutDir: false, cssCodeSplit: false, assetsInlineLimit: 0,
      rolldownOptions: { input: resolve("tests/fixtures/application-fields.tsx"), output: { entryFileNames: "fields.js" } },
    } });
    if (Array.isArray(result) || !("output" in result)) throw new Error("Expected one fixture build");
    const styles: string[] = [];
    for (const item of result.output) {
      const contentType = item.fileName.endsWith(".js") ? "text/javascript" : item.fileName.endsWith(".css") ? "text/css" : item.fileName.endsWith(".svg") ? "image/svg+xml" : "image/png";
      resources.set("/" + item.fileName, { body: item.type === "chunk" ? item.code : Buffer.from(item.source), contentType });
      if (contentType === "text/css") styles.push(`<link rel="stylesheet" href="/${item.fileName}">`);
    }
    const content = { contentType: "text/html", body: `<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><link rel="icon" href="data:,">${styles.join("")}</head><body><div id="fixture"></div><script type="module" src="/fields.js"></script></body></html>` };
    resources.set("/fields", content); resources.set("/intern", content); resources.set("/missing", content);
  });
  test.beforeEach(async ({ page }) => { await page.goto(origin + "/fields"); });

  test("requires explicit department confirmation and clears the selected position when department changes", async ({ page }) => {
    await page.setViewportSize({ width: 320, height: 900 });
    await expect(page.getByRole("checkbox")).not.toBeChecked();
    await page.getByLabel("职务（选填）", { exact: true }).selectOption({ label: "临床诊疗及患者全程照护协调负责人" });
    await expect(page.locator(".application-selected").filter({ hasText: "已选职务：" })).toContainText("临床诊疗及患者全程照护协调负责人");
    await page.screenshot({ path: test.info().outputPath("application-long-position-320-light.png"), fullPage: true });
    await page.getByRole("checkbox").check();
    await page.getByLabel("科室", { exact: true }).selectOption({ label: "综合临床医学诊疗与长期健康照护协作中心 / 复杂慢性疾病多学科协作及康复随访门诊" });
    await expect(page.getByRole("checkbox")).not.toBeChecked();
    await expect(page.getByLabel("职务（选填）", { exact: true })).toHaveValue("");
    expect(await page.evaluate(() => window.applicationChoices.rosterId)).toBe("");
    await page.getByRole("radio", { name: "增办", exact: true }).check();
    expect(await page.evaluate(() => window.applicationChoices.kind)).toBe("duplication");
    await page.getByRole("radio", { name: "换证", exact: true }).check();
    expect(await page.evaluate(() => window.applicationChoices.kind)).toBe("replacement");
  });

  test("honors the configured position switch while leaving titles independent", async ({ page }) => {
    await page.goto(origin + "/intern");
    await expect(page.getByLabel("职务（选填）", { exact: true })).toHaveCount(0);
    await expect(page.getByLabel("职称（选填）", { exact: true })).toBeVisible();
    await page.getByLabel("职称（选填）", { exact: true }).selectOption({ label: "全科医学主任医师" });
    await expect(page.getByText("已选职称：全科医学主任医师", { exact: true })).toBeVisible();
  });

  test("a missing rule for another subtype closes all application choices", async ({ page }) => {
    await page.goto(origin + "/missing");
    await expect(page.getByRole("alert")).toHaveText("暂时无法准备工牌申请，请稍后刷新重试。");
    await expect(page.getByRole("group", { name: "工牌申请内容" })).toHaveCount(0);
    await expect(page.getByRole("combobox")).toHaveCount(0);
  });

  for (const width of [320, 390, 768, 1440]) for (const colorScheme of ["light", "dark"] as const) {
    test(`complete preparation fields ${width} ${colorScheme}`, async ({ page }) => {
      await page.setViewportSize({ width, height: 1000 }); await page.emulateMedia({ colorScheme });
      await page.getByLabel("科室", { exact: true }).selectOption({ label: "综合临床医学诊疗与长期健康照护协作中心 / 复杂慢性疾病多学科协作及康复随访门诊" });
      await page.getByRole("checkbox").check();
      await page.getByLabel("职称（选填）", { exact: true }).selectOption({ label: "全科医学主任医师" });
      await page.getByLabel("领取地点", { exact: true }).selectOption({ label: "主院区综合服务大厅一层工牌办理窗口" });
      await choose(page);
      expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(width);
      await page.screenshot({ path: `${process.env.HALO_APPLICATION_PNG ?? "test-results"}/application-fields-${width}-${colorScheme}.png`, fullPage: true });
    });
  }
});
