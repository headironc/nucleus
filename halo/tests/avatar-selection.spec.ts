import { test, expect, type Page } from "@playwright/test";
import { build } from "vite";
import tailwindcss from "@tailwindcss/vite";
import { resolve } from "node:path";
import { readFile } from "node:fs/promises";

// This fixture exercises the real local candidate controller, container
// inspection and React panel. It performs no account read or upload; the
// unpublished upload protocol is intentionally absent.
const origin = "http://avatar.localhost";
const resources = new Map<
  string,
  { body: string | Buffer; contentType: string }
>();
let html: string;
let currentBytes: Buffer;

test.beforeAll(async ({ browser }) => {
  console.info(`Avatar selection browser: ${browser.version()}`);
  currentBytes = await readFile(resolve("../assets/avatars/0-cat.webp"));
  const result = await build({
    configFile: false,
    publicDir: false,
    plugins: [tailwindcss()],
    logLevel: "error",
    define: { __BUILD_ID__: JSON.stringify("avatar-fixture") },
    build: {
      write: false,
      emptyOutDir: false,
      cssCodeSplit: false,
      assetsInlineLimit: 0,
      rolldownOptions: {
        input: resolve("tests/fixtures/avatar-selection.tsx"),
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
  await context.route("**/*", async (route) => {
    const url = new URL(route.request().url());
    if (url.origin !== origin) {
      await route.abort();
      return;
    }
    if (url.pathname === "/current.webp") {
      await route.fulfill({ contentType: "image/webp", body: currentBytes });
      return;
    }
    const resource = resources.get(url.pathname);
    if (resource) {
      await route.fulfill(resource);
      return;
    }
    if (url.pathname === "/") {
      await route.fulfill({ contentType: "text/html", body: html });
      return;
    }
    await route.abort();
  });
  await page.goto(origin);
  await page.waitForFunction(() => !!window.avatarSelection);
});

async function previewState(page: Page) {
  return page.evaluate(() => ({
    decoded: window.avatarPreview.decoded,
    drawn: window.avatarPreview.drawn,
    closed: window.avatarPreview.closed,
  }));
}

test("decodes a real lossy WebP and paints its original left and right colors", async ({ page }) => {
  await page.locator('input[type="file"]').setInputFiles({
    name: "lossy.webp",
    mimeType: "image/webp",
    buffer: Buffer.from(
      "UklGRkAAAABXRUJQVlA4IDQAAACQAQCdASoQABAAAUAmJaACdLoAA5gA/vij//8QI//mwP/0uD8Ef+lwf/pcDh//S4GaAAAA",
      "base64",
    ),
  });
  const canvas = page.getByRole("img", { name: "新头像预览" });
  await expect(canvas).toBeVisible();
  const colors = await canvas.evaluate((node: HTMLCanvasElement) => {
    const context = node.getContext("2d")!;
    return {
      left: [...context.getImageData(node.width / 4, node.height / 2, 1, 1).data],
      right: [...context.getImageData(node.width * 3 / 4, node.height / 2, 1, 1).data],
      width: node.width,
      height: node.height,
    };
  });
  expect([colors.width, colors.height]).toEqual([126, 126]);
  expect(colors.left[0]).toBeGreaterThan(colors.left[2]);
  expect(colors.right[2]).toBeGreaterThan(colors.right[0]);
});

test("a missing canvas context closes the bitmap and does not offer a ready candidate", async ({ page }) => {
  await page.evaluate(() => {
    Object.defineProperty(HTMLCanvasElement.prototype, "getContext", { value: () => null });
  });
  await page.locator('input[type="file"]').setInputFiles({
    name: "avatar.webp",
    mimeType: "image/webp",
    buffer: currentBytes,
  });
  await expect(page.getByRole("alert")).toContainText("暂时无法在本地预览图片");
  await expect(page.getByRole("img", { name: "新头像预览" })).toHaveCount(0);
  expect(await page.evaluate(() => window.avatarSelection.snapshot())).toEqual({
    kind: "error",
    error: "preview-unavailable",
  });
  expect((await previewState(page)).closed).toBe(1);
});

for (const width of [320, 390, 768, 1440]) {
  for (const colorScheme of ["light", "dark"] as const) {
    test(`selection layout at ${width}px in ${colorScheme}`, async ({ page }) => {
      await page.setViewportSize({ width, height: 1000 });
      await page.emulateMedia({ colorScheme });
      await page.screenshot({
        path: `test-results/avatar-selection-empty-${colorScheme}-${width}.png`,
        fullPage: true,
      });
      await page.locator('input[type="file"]').setInputFiles({
        name: "本人选择的原始头像-averylonglocalimagename.webp",
        mimeType: "image/webp",
        buffer: currentBytes,
      });
      await expect(page.getByRole("img", { name: "新头像预览" })).toBeVisible();
      expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(width);
      await page.screenshot({
        path: `test-results/avatar-selection-ready-${colorScheme}-${width}.png`,
        fullPage: true,
      });
    });
  }
}

test.describe("high-density preview", () => {
  test.use({ deviceScaleFactor: 2 });
  test("uses display pixels without changing original dimensions", async ({ page }) => {
    await page.locator('input[type="file"]').setInputFiles({
      name: "avatar.webp", mimeType: "image/webp", buffer: currentBytes,
    });
    const canvas = page.getByRole("img", { name: "新头像预览" });
    await expect(canvas).toBeVisible();
    expect(await canvas.evaluate((node: HTMLCanvasElement) => [node.width, node.height])).toEqual([252, 252]);
    await expect(page.getByText("512 × 512 像素 · WebP")).toBeVisible();
  });
});

test("previews a chosen image, keeps current and new distinct, and discards it", async ({
  page,
}) => {
  const calls: string[] = [];
  page.on("request", (request) => {
    const url = new URL(request.url());
    if (url.origin === origin && url.pathname.startsWith("/api/"))
      calls.push(url.pathname);
  });

  await page.locator('input[type="file"]').setInputFiles({
    name: "avatar.webp",
    mimeType: "image/webp",
    buffer: currentBytes,
  });

  await expect(page.getByRole("heading", { name: "新头像" })).toBeVisible();
  await expect(page.getByRole("heading", { name: "当前头像" })).toBeVisible();
  await expect(
    page.getByRole("img", { name: "当前头像" }),
  ).toBeVisible();
  const preview = page.getByRole("img", { name: "新头像预览" });
  await expect(preview).toBeVisible();
  await expect
    .poll(() =>
      preview.evaluate((node: HTMLCanvasElement) => [
        node.width,
        node.height,
      ]),
    )
    .toEqual([126, 126]);
  await expect
    .poll(() => page.evaluate(() => window.avatarPreview.drawn))
    .toBeGreaterThan(0);
  await expect(page.getByText("512 × 512 像素 · WebP")).toBeVisible();
  await expect(page.getByText("已选择：avatar.webp")).toBeVisible();
  await expect(page.getByText("仅在本地预览，尚未提交。")).toBeVisible();
  await expect(
    page.getByText("新头像预览已准备好，请对比当前头像。"),
  ).toBeAttached();

  await page.setViewportSize({ width: 320, height: 1000 });
  await expect.poll(() => preview.evaluate((node: HTMLCanvasElement) => [node.width, node.height])).toEqual([94, 94]);
  const oldCanvas = await preview.elementHandle();
  const discard = page.getByRole("button", { name: "取消选择" });
  await discard.focus();
  await expect(discard).toBeFocused();
  await page.keyboard.press("Enter");

  await expect(page.getByRole("heading", { name: "新头像" })).toHaveCount(0);
  await expect(
    page.getByText("还没有选择图片。原头像会保留，选图本身不会更换头像。"),
  ).toBeVisible();
  await expect(page.locator('input[type="file"]')).toBeFocused();
  const state = await previewState(page);
  expect(state.decoded).toBe(1);
  expect(state.closed).toBe(1);
  expect(await oldCanvas!.evaluate((node: HTMLCanvasElement) => [node.width, node.height])).toEqual([0, 0]);
  expect(calls).toEqual([]);
});

test("rejects a supplied type that conflicts with the encoded format", async ({
  page,
}) => {
  await page.locator('input[type="file"]').setInputFiles({
    name: "avatar.jpg",
    mimeType: "image/jpeg",
    buffer: currentBytes,
  });
  await expect(page.getByRole("alert")).toHaveText(
    "请选择静态 PNG、JPEG 或 WebP，图片实际格式须与文件类型一致。",
  );
  await expect(page.getByRole("heading", { name: "新头像" })).toHaveCount(0);
  expect((await previewState(page)).decoded).toBe(0);
});

test("rejects oversized material before any preview resource exists", async ({
  page,
}) => {
  await page.locator('input[type="file"]').setInputFiles({
    name: "large.png",
    mimeType: "image/png",
    buffer: Buffer.alloc(2 * 1024 * 1024 + 1),
  });
  await expect(page.getByRole("alert")).toHaveText(
    "图片超过 2 MiB，请选择不超过此大小的原始文件。",
  );
  expect((await previewState(page)).decoded).toBe(0);
});

test("resolves an empty supplied type from the inspected bytes", async ({
  page,
}) => {
  await page.evaluate(async () => {
    const bytes = await (await fetch("/current.webp")).arrayBuffer();
    await window.avatarSelection.choose(
      new File([bytes], "avatar", { type: "" }),
    );
  });
  await expect(page.getByText("512 × 512 像素 · WebP")).toBeVisible();
  await expect(page.getByRole("img", { name: "新头像预览" })).toBeVisible();
});

test("previews a JPEG at its encoded orientation after removing EXIF", async ({
  page,
}) => {
  const result = await page.evaluate(async () => {
    function ascii(value: string): Uint8Array<ArrayBuffer> {
      const bytes = new Uint8Array(value.length);
      for (let index = 0; index < value.length; index++)
        bytes[index] = value.charCodeAt(index);
      return bytes;
    }
    function concat(parts: Uint8Array[]): Uint8Array<ArrayBuffer> {
      let length = 0;
      for (const part of parts) length += part.length;
      const result = new Uint8Array(length);
      let offset = 0;
      for (const part of parts) {
        result.set(part, offset);
        offset += part.length;
      }
      return result;
    }
    function contains(bytes: Uint8Array, marker: Uint8Array): boolean {
      for (let offset = 0; offset + marker.length <= bytes.length; offset++) {
        let found = true;
        for (let index = 0; index < marker.length; index++) {
          if (bytes[offset + index] !== marker[index]) {
            found = false;
            break;
          }
        }
        if (found) return true;
      }
      return false;
    }
    const canvas = document.createElement("canvas");
    canvas.width = 64;
    canvas.height = 64;
    const context = canvas.getContext("2d")!;
    context.fillStyle = "#ff0000";
    context.fillRect(0, 0, 32, 64);
    context.fillStyle = "#0000ff";
    context.fillRect(32, 0, 32, 64);
    const blob = await new Promise<Blob>((resolve, reject) =>
      canvas.toBlob(
        (value) => (value ? resolve(value) : reject(new Error("encode"))),
        "image/jpeg",
        0.95,
      ),
    );
    const original = new Uint8Array(await blob.arrayBuffer());
    // APP1 Exif with TIFF orientation 6 (rotate 90 degrees clockwise).
    const tiff = Uint8Array.of(
      0x49, 0x49, 0x2a, 0x00, 0x08, 0x00, 0x00, 0x00, 0x01, 0x00, 0x12, 0x01,
      0x03, 0x00, 0x01, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00,
      0x00, 0x00,
    );
    const payload = concat([
      Uint8Array.of(0x45, 0x78, 0x69, 0x66, 0, 0),
      tiff,
    ]);
    const app1 = concat([
      Uint8Array.of(0xff, 0xe1),
      Uint8Array.of((payload.length + 2) >>> 8, (payload.length + 2) & 0xff),
      payload,
    ]);
    const bytes = concat([
      original.subarray(0, 2),
      app1,
      original.subarray(2),
    ]);
    await window.avatarSelection.choose(
      new File([bytes], "rotated.jpg", { type: "image/jpeg" }),
    );
    const state = window.avatarSelection.snapshot();
    if (state.kind !== "ready") throw new Error("expected JPEG preview");
    const sample = document.createElement("canvas");
    sample.width = state.candidate.width;
    sample.height = state.candidate.height;
    const target = sample.getContext("2d")!;
    state.candidate.draw(target);
    const pixel = target.getImageData(8, 56, 1, 1).data;
    if (!window.avatarPreview.lastBlob)
      throw new Error("expected decoded preview material");
    const preview = new Uint8Array(
      await window.avatarPreview.lastBlob.arrayBuffer(),
    );
    return {
      mediaType: state.candidate.mediaType,
      width: state.candidate.width,
      height: state.candidate.height,
      red: pixel[0],
      blue: pixel[2],
      hasExif: contains(preview, ascii("Exif\0\0")),
    };
  });
  expect(result.mediaType).toBe("image/jpeg");
  expect(result.width).toBe(64);
  expect(result.height).toBe(64);
  expect(result.hasExif).toBe(false);
  expect(result.red).toBeGreaterThan(result.blue);
});

test("previews a PNG at its encoded orientation after removing eXIf", async ({
  page,
}) => {
  const result = await page.evaluate(async () => {
    function ascii(value: string): Uint8Array<ArrayBuffer> {
      const bytes = new Uint8Array(value.length);
      for (let index = 0; index < value.length; index++)
        bytes[index] = value.charCodeAt(index);
      return bytes;
    }
    function concat(parts: Uint8Array[]): Uint8Array<ArrayBuffer> {
      let length = 0;
      for (const part of parts) length += part.length;
      const result = new Uint8Array(length);
      let offset = 0;
      for (const part of parts) {
        result.set(part, offset);
        offset += part.length;
      }
      return result;
    }
    function crc32(bytes: Uint8Array): number {
      let crc = 0xffffffff;
      for (const byte of bytes) {
        crc ^= byte;
        for (let bit = 0; bit < 8; bit++)
          crc = (crc >>> 1) ^ (0xedb88320 & -(crc & 1));
      }
      return (crc ^ 0xffffffff) >>> 0;
    }
    function chunk(type: string, data: Uint8Array): Uint8Array<ArrayBuffer> {
      const typeBytes = ascii(type);
      const result = new Uint8Array(12 + data.length);
      const view = new DataView(result.buffer);
      view.setUint32(0, data.length);
      result.set(typeBytes, 4);
      result.set(data, 8);
      view.setUint32(8 + data.length, crc32(concat([typeBytes, data])));
      return result;
    }
    function contains(bytes: Uint8Array, marker: Uint8Array): boolean {
      for (let offset = 0; offset + marker.length <= bytes.length; offset++) {
        let found = true;
        for (let index = 0; index < marker.length; index++) {
          if (bytes[offset + index] !== marker[index]) {
            found = false;
            break;
          }
        }
        if (found) return true;
      }
      return false;
    }
    const canvas = document.createElement("canvas");
    canvas.width = 64;
    canvas.height = 64;
    const context = canvas.getContext("2d")!;
    context.fillStyle = "#ff0000";
    context.fillRect(0, 0, 32, 64);
    context.fillStyle = "#0000ff";
    context.fillRect(32, 0, 32, 64);
    const blob = await new Promise<Blob>((resolve, reject) =>
      canvas.toBlob(
        (value) => (value ? resolve(value) : reject(new Error("encode"))),
        "image/png",
      ),
    );
    const original = new Uint8Array(await blob.arrayBuffer());
    const view = new DataView(
      original.buffer,
      original.byteOffset,
      original.byteLength,
    );
    const insert = 8 + 12 + view.getUint32(8);
    const tiff = Uint8Array.of(
      0x49, 0x49, 0x2a, 0x00, 0x08, 0x00, 0x00, 0x00, 0x01, 0x00, 0x12, 0x01,
      0x03, 0x00, 0x01, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00,
      0x00, 0x00,
    );
    const bytes = concat([
      original.subarray(0, insert),
      chunk("eXIf", tiff),
      original.subarray(insert),
    ]);
    await window.avatarSelection.choose(
      new File([bytes], "rotated.png", { type: "image/png" }),
    );
    const state = window.avatarSelection.snapshot();
    if (state.kind !== "ready") throw new Error("expected PNG preview");
    const sample = document.createElement("canvas");
    sample.width = state.candidate.width;
    sample.height = state.candidate.height;
    const target = sample.getContext("2d")!;
    state.candidate.draw(target);
    const pixel = target.getImageData(8, 56, 1, 1).data;
    if (!window.avatarPreview.lastBlob)
      throw new Error("expected decoded preview material");
    const preview = new Uint8Array(
      await window.avatarPreview.lastBlob.arrayBuffer(),
    );
    return {
      mediaType: state.candidate.mediaType,
      width: state.candidate.width,
      height: state.candidate.height,
      red: pixel[0],
      blue: pixel[2],
      hasExif: contains(preview, ascii("eXIf")),
    };
  });
  expect(result.mediaType).toBe("image/png");
  expect(result.width).toBe(64);
  expect(result.height).toBe(64);
  expect(result.hasExif).toBe(false);
  expect(result.red).toBeGreaterThan(result.blue);
});

test("previews a WebP after removing its EXIF chunk and flag", async ({
  page,
}) => {
  const result = await page.evaluate(async () => {
    function ascii(value: string): Uint8Array<ArrayBuffer> {
      const bytes = new Uint8Array(value.length);
      for (let index = 0; index < value.length; index++)
        bytes[index] = value.charCodeAt(index);
      return bytes;
    }
    function concat(parts: Uint8Array[]): Uint8Array<ArrayBuffer> {
      let length = 0;
      for (const part of parts) length += part.length;
      const result = new Uint8Array(length);
      let offset = 0;
      for (const part of parts) {
        result.set(part, offset);
        offset += part.length;
      }
      return result;
    }
    function uint32LE(value: number): Uint8Array {
      return Uint8Array.of(
        value & 0xff,
        (value >>> 8) & 0xff,
        (value >>> 16) & 0xff,
        (value >>> 24) & 0xff,
      );
    }
    function uint24LE(value: number): Uint8Array {
      return Uint8Array.of(
        value & 0xff,
        (value >>> 8) & 0xff,
        (value >>> 16) & 0xff,
      );
    }
    function chunk(type: string, data: Uint8Array): Uint8Array<ArrayBuffer> {
      return concat([
        ascii(type),
        uint32LE(data.length),
        data,
        data.length % 2 === 0 ? new Uint8Array() : Uint8Array.of(0),
      ]);
    }
    const original = new Uint8Array(
      await (await fetch("/current.webp")).arrayBuffer(),
    );
    const view = new DataView(
      original.buffer,
      original.byteOffset,
      original.byteLength,
    );
    if (String.fromCharCode(...original.subarray(12, 16)) !== "VP8L")
      throw new Error("fixture WebP is not VP8L");
    const length = view.getUint32(16, true);
    const data = original.subarray(20, 20 + length);
    const bits =
      data[1] + data[2] * 0x100 + data[3] * 0x10000 + data[4] * 0x1000000;
    const width = 1 + (bits & 0x3fff);
    const height = 1 + ((bits >>> 14) & 0x3fff);
    const vp8x = chunk(
      "VP8X",
      concat([
        Uint8Array.of(0x08, 0, 0, 0),
        uint24LE(width - 1),
        uint24LE(height - 1),
      ]),
    );
    const exif = chunk("EXIF", Uint8Array.of(0x49, 0x49, 0x2a, 0));
    const body = concat([
      ascii("WEBP"),
      vp8x,
      original.subarray(12),
      exif,
    ]);
    const bytes = concat([ascii("RIFF"), uint32LE(body.length), body]);
    await window.avatarSelection.choose(
      new File([bytes], "rotated.webp", { type: "image/webp" }),
    );
    const state = window.avatarSelection.snapshot();
    if (state.kind !== "ready") throw new Error("expected WebP preview");
    if (!window.avatarPreview.lastBlob)
      throw new Error("expected decoded preview material");
    const preview = new Uint8Array(
      await window.avatarPreview.lastBlob.arrayBuffer(),
    );
    const previewView = new DataView(
      preview.buffer,
      preview.byteOffset,
      preview.byteLength,
    );
    const chunks: { type: string; flags?: number }[] = [];
    let offset = 12;
    while (offset + 8 <= preview.length) {
      const type = String.fromCharCode(
        preview[offset],
        preview[offset + 1],
        preview[offset + 2],
        preview[offset + 3],
      );
      const size = previewView.getUint32(offset + 4, true);
      chunks.push({
        type,
        ...(type === "VP8X" ? { flags: preview[offset + 8] } : {}),
      });
      offset += 8 + size + (size & 1);
    }
    return {
      mediaType: state.candidate.mediaType,
      width: state.candidate.width,
      height: state.candidate.height,
      types: chunks.map((entry) => entry.type),
      flags: chunks[0]?.flags ?? 0,
      coherent: previewView.getUint32(4, true) === preview.length - 8,
    };
  });
  expect(result.mediaType).toBe("image/webp");
  expect(result.width).toBe(512);
  expect(result.height).toBe(512);
  expect(result.types).not.toContain("EXIF");
  expect(result.types[0]).toBe("VP8X");
  expect(result.flags & 0x08).toBe(0);
  expect(result.coherent).toBe(true);
});

test("rejects animation and non-square encoded dimensions with precise copy", async ({
  page,
}) => {
  const images = await page.evaluate(async () => {
    function ascii(value: string): Uint8Array<ArrayBuffer> {
      const bytes = new Uint8Array(value.length);
      for (let index = 0; index < value.length; index++)
        bytes[index] = value.charCodeAt(index);
      return bytes;
    }
    function concat(parts: Uint8Array[]): Uint8Array<ArrayBuffer> {
      let length = 0;
      for (const part of parts) length += part.length;
      const result = new Uint8Array(length);
      let offset = 0;
      for (const part of parts) {
        result.set(part, offset);
        offset += part.length;
      }
      return result;
    }
    function crc32(bytes: Uint8Array): number {
      let crc = 0xffffffff;
      for (const byte of bytes) {
        crc ^= byte;
        for (let bit = 0; bit < 8; bit++)
          crc = (crc >>> 1) ^ (0xedb88320 & -(crc & 1));
      }
      return (crc ^ 0xffffffff) >>> 0;
    }
    function chunk(type: string, data: Uint8Array): Uint8Array<ArrayBuffer> {
      const typeBytes = ascii(type);
      const result = new Uint8Array(12 + data.length);
      const view = new DataView(result.buffer);
      view.setUint32(0, data.length);
      result.set(typeBytes, 4);
      result.set(data, 8);
      view.setUint32(8 + data.length, crc32(concat([typeBytes, data])));
      return result;
    }
    function base64(bytes: Uint8Array): string {
      let value = "";
      for (const byte of bytes) value += String.fromCharCode(byte);
      return btoa(value);
    }
    async function png(
      width: number,
      height: number,
    ): Promise<Uint8Array<ArrayBuffer>> {
      const canvas = document.createElement("canvas");
      canvas.width = width;
      canvas.height = height;
      const context = canvas.getContext("2d")!;
      context.fillStyle = "#a3402b";
      context.fillRect(0, 0, width, height);
      const blob = await new Promise<Blob>((resolve, reject) =>
        canvas.toBlob(
          (value) => (value ? resolve(value) : reject(new Error("encode"))),
          "image/png",
        ),
      );
      return new Uint8Array(await blob.arrayBuffer());
    }
    const animated = await png(64, 64);
    const view = new DataView(
      animated.buffer,
      animated.byteOffset,
      animated.byteLength,
    );
    const insert = 8 + 12 + view.getUint32(8);
    const control = Uint8Array.of(0, 0, 0, 2, 0, 0, 0, 0);
    const animation = concat([
      animated.subarray(0, insert),
      chunk("acTL", control),
      animated.subarray(insert),
    ]);
    return { animation: base64(animation), wide: base64(await png(64, 32)) };
  });
  const input = page.locator('input[type="file"]');
  await input.setInputFiles({
    name: "animated.png",
    mimeType: "image/png",
    buffer: Buffer.from(images.animation, "base64"),
  });
  await expect(page.getByRole("alert")).toHaveText(
    "不支持动画图片，请选择静态 PNG、JPEG 或 WebP。",
  );
  await input.setInputFiles({
    name: "wide.png",
    mimeType: "image/png",
    buffer: Buffer.from(images.wide, "base64"),
  });
  await expect(page.getByRole("alert")).toHaveText(
    "请选择正方形原图，宽高均须为 1–1024 像素。这里不会自动裁剪或缩放。",
  );
  expect((await previewState(page)).decoded).toBe(0);
});
