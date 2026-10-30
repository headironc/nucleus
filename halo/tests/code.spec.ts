import { test, expect, type Page } from "@playwright/test";
import { build } from "vite";
import tailwindcss from "@tailwindcss/vite";
import { resolve } from "node:path";

const origin = "http://127.0.0.1:47777";
const resources = new Map<string, { body: string | Buffer; contentType: string }>();
let unexpected: string[];

test.beforeAll(async () => {
  const result = await build({
    configFile: false,
    publicDir: false,
    plugins: [tailwindcss()],
    logLevel: "error",
    build: {
      write: false,
      emptyOutDir: false,
      cssCodeSplit: false,
      rolldownOptions: {
        input: resolve("tests/fixtures/code.tsx"),
        output: { entryFileNames: "fixture.js" },
      },
    },
  });
  if (Array.isArray(result) || !("output" in result))
    throw new Error("Expected one in-memory fixture build");
  const styles: string[] = [];
  for (const item of result.output) {
    const script = item.type === "chunk";
    resources.set("/" + item.fileName, {
      body: script ? item.code : Buffer.from(item.source),
      contentType: script ? "text/javascript" : "text/css",
    });
    if (!script) styles.push(`<link rel="stylesheet" href="/${item.fileName}">`);
  }
  resources.set("/", {
    contentType: "text/html",
    body: `<!doctype html><html lang="zh-CN"><head><meta charset="utf-8">
      <meta name="viewport" content="width=device-width, initial-scale=1">
      <link rel="icon" href="data:,">${styles.join("")}</head><body>
      <div id="fixture"></div><script type="module" src="/fixture.js"></script></body></html>`,
  });
});

test.beforeEach(async ({ page }) => {
  unexpected = [];
  await page.route("**/*", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const resource = url.origin === origin && resources.get(url.pathname);
    if (request.method() === "GET" && resource) await route.fulfill(resource);
    else {
      unexpected.push(request.method() + " " + request.url());
      await route.abort();
    }
  });
  await page.goto(origin);
  await expect(page.locator(".code-slots input")).toHaveCount(6);
});

test.afterEach(() => expect(unexpected).toEqual([]));

async function value(page: Page, expected: string) {
  await expect(page.getByLabel("受控值")).toHaveText(expected);
  await expect
    .poll(() =>
      page.locator(".code-slots input").evaluateAll((inputs) =>
        inputs.map((input) => (input as HTMLInputElement).value).join(""),
      ),
    )
    .toBe(expected);
}

async function paste(page: Page, index: number, text: string) {
  await page.locator(".code-slots input").nth(index).evaluate(
    (input, text) => {
      const data = new DataTransfer();
      data.setData("text/plain", text);
      input.dispatchEvent(new ClipboardEvent("paste", {
        clipboardData: data,
        bubbles: true,
        cancelable: true,
      }));
    },
    text,
  );
}

// DOM events model IME and SMS autofill; they do not certify a device keyboard.
async function inputEvent(
  page: Page,
  selector: string,
  text: string,
  composing = false,
) {
  await page.locator(selector).evaluate((element, { text, composing }) => {
    const input = element as HTMLInputElement;
    Object.getOwnPropertyDescriptor(
      HTMLInputElement.prototype, "value",
    )!.set!.call(input, text);
    input.dispatchEvent(new InputEvent("input", {
      bubbles: true,
      data: text,
      isComposing: composing,
      inputType: composing ? "insertCompositionText" : "insertReplacementText",
    }));
  }, { text, composing });
}

test("parent updates and keyboard editing retain a continuous value and leading zero", async ({ page }) => {
  const slots = page.locator(".code-slots input");
  await page.getByText("短信验证码", { exact: true }).click();
  await expect(slots.first()).toBeFocused();
  await page.keyboard.type("01");
  await value(page, "01");
  await expect(slots.nth(2)).toBeFocused();
  await slots.nth(5).focus();
  await expect(slots.nth(2)).toBeFocused();
  await page.keyboard.press("ArrowRight");
  await expect(slots.nth(2)).toBeFocused();
  await page.keyboard.press("ArrowLeft");
  await expect(slots.nth(1)).toBeFocused();
  await page.keyboard.press("Home");
  await expect(slots.first()).toBeFocused();
  await page.keyboard.press("End");
  await expect(slots.nth(2)).toBeFocused();
  await page.keyboard.press("Backspace");
  await value(page, "0");
  await expect(slots.nth(1)).toBeFocused();
  await page.getByRole("button", { name: "设置示例值" }).click();
  await value(page, "012345");
  await slots.nth(2).focus();
  await page.keyboard.press("Delete");
  await value(page, "01345");
  await expect(slots.nth(2)).toBeFocused();
  await page.keyboard.press("Backspace");
  await value(page, "0145");
  await expect(slots.nth(1)).toBeFocused();
  await page.keyboard.press("End");
  await expect(slots.nth(4)).toBeFocused();
  await page.getByRole("button", { name: "清空", exact: true }).click();
  await value(page, "");
  await slots.last().focus();
  await expect(slots.first()).toBeFocused();
});

test("paste replaces a full code or overwrites a shorter segment and rejects invalid text as a whole", async ({ page }) => {
  await paste(page, 0, "012345");
  await value(page, "012345");
  await expect(page.locator(".code-slots input").last()).toBeFocused();
  await paste(page, 2, "99");
  await value(page, "019945");
  await expect(page.locator(".code-slots input").nth(4)).toBeFocused();
  for (const text of ["12345678", "12 345", "12345\n", "１２３４５６", "123a45"]) {
    await paste(page, 0, text);
    await expect(page.getByRole("alert")).not.toBeEmpty();
    await value(page, "019945");
  }
  await paste(page, 4, "12345");
  await expect(page.getByRole("alert")).toContainText("不能超过 6 位");
  await value(page, "019945");
  await paste(page, 5, "001234");
  await value(page, "001234");
  await expect(page.getByRole("alert")).toBeEmpty();
});

test("composition waits for its complete result and validates it without filtering", async ({ page }) => {
  await paste(page, 0, "012345");
  await expect(page.locator(".code-slots input").last()).toBeFocused();
  const first = page.locator(".code-slots input").first();
  await first.focus();
  await first.dispatchEvent("compositionstart");
  await inputEvent(page, "#login-code", "98", true);
  await expect(page.getByLabel("受控值")).toHaveText("012345");
  await page.keyboard.press("Backspace");
  await expect(page.getByLabel("受控值")).toHaveText("012345");
  await first.evaluate((input) => {
    Object.getOwnPropertyDescriptor(
      HTMLInputElement.prototype, "value",
    )!.set!.call(input, "98中");
    input.dispatchEvent(new CompositionEvent("compositionend", {
      bubbles: true, data: "98中",
    }));
  });
  await value(page, "012345");
  await expect(page.getByRole("alert")).toContainText("其他字符");
  await first.dispatchEvent("compositionstart");
  await inputEvent(page, "#login-code", "654321", true);
  await expect(page.getByLabel("受控值")).toHaveText("012345");
  await first.dispatchEvent("compositionend", { data: "654321" });
  await value(page, "654321");
  await expect(page.getByRole("alert")).toBeEmpty();
});

test("simulated SMS autofill retains the whole value or rejects it without truncation", async ({ page }) => {
  const hidden = "#login-code-hidden-input";
  await expect(page.locator(hidden)).toHaveAttribute("autocomplete", "off");
  await page.locator(".code-slots input").nth(3).fill("012345");
  await value(page, "012345");
  for (const text of ["12345678", "01 2345", "１２３４５６"]) {
    await inputEvent(page, hidden, text);
    await expect(page.getByRole("alert")).not.toBeEmpty();
    await value(page, "012345");
  }
  await inputEvent(page, hidden, "001234");
  await value(page, "001234");
  await expect(page.getByRole("alert")).toBeEmpty();
});

test("labels, hints, errors and disabled state describe all six slots", async ({ page }) => {
  const slots = page.locator(".code-slots input");
  for (let index = 0; index < 6; index++) {
    await expect(slots.nth(index)).toHaveAccessibleName(
      index === 0 ? "短信验证码" : `第 ${index + 1} 位，共 6 位`,
    );
    await expect(slots.nth(index)).toHaveAttribute("inputmode", "numeric");
    await expect(slots.nth(index)).toHaveAttribute("autocomplete", "one-time-code");
    await expect(slots.nth(index)).toHaveAttribute(
      "aria-describedby", "code-hint code-error",
    );
    await expect(slots.nth(index)).toHaveAttribute("aria-invalid", "false");
  }
  await page.getByLabel("缺位", { exact: true }).check();
  await expect(page.getByRole("alert")).toHaveText("请输入完整的 6 位验证码。");
  await page.getByLabel("缺位", { exact: true }).uncheck();
  await page.getByLabel("错码", { exact: true }).check();
  await expect(page.getByRole("alert")).toContainText("验证码不正确");
  for (const input of await slots.all()) {
    await expect(input).toHaveAttribute("aria-invalid", "true");
    await expect(input).toHaveAccessibleDescription(/验证码不正确/);
  }
  await page.getByLabel("错码", { exact: true }).uncheck();
  await page.getByRole("button", { name: "设置示例值" }).click();
  await page.getByLabel("禁用", { exact: true }).check();
  for (const input of await slots.all()) await expect(input).toBeDisabled();
  await paste(page, 0, "654321");
  await value(page, "012345");
  await page.getByLabel("禁用", { exact: true }).uncheck();
  await slots.first().focus();
  await page.keyboard.type("9");
  await value(page, "912345");
});

test("disabling an active composition discards its result and permits editing after unlock", async ({ page }) => {
  const first = page.locator(".code-slots input").first();
  await page.getByRole("button", { name: "设置示例值" }).click();
  await first.focus();
  await first.dispatchEvent("compositionstart");
  await inputEvent(page, "#login-code", "98", true);
  await expect(page.getByLabel("受控值")).toHaveText("012345");
  await page.getByLabel("禁用", { exact: true }).check();
  await expect(first).toBeDisabled();
  await first.evaluate((input) => {
    Object.getOwnPropertyDescriptor(
      HTMLInputElement.prototype, "value",
    )!.set!.call(input, "654321");
    input.dispatchEvent(new CompositionEvent("compositionend", {
      bubbles: true, data: "654321",
    }));
  });
  await expect(page.getByLabel("输入变更次数")).toHaveText("0");
  await value(page, "012345");
  await page.getByLabel("禁用", { exact: true }).uncheck();
  await first.focus();
  await page.keyboard.type("9");
  await value(page, "912345");
  await expect(page.getByLabel("输入变更次数")).toHaveText("1");
});

test("disabled slots reject late input, hidden autofill and paste without changing controlled state", async ({ page }) => {
  await page.getByRole("button", { name: "设置示例值" }).click();
  await page.getByLabel("禁用", { exact: true }).check();
  await inputEvent(page, "#login-code", "000000");
  await expect(page.getByLabel("输入变更次数")).toHaveText("0");
  await value(page, "012345");
  await inputEvent(page, "#login-code", "9");
  await value(page, "012345");
  await inputEvent(page, "#login-code-hidden-input", "111111");
  await paste(page, 0, "222222");
  await value(page, "012345");
  await expect(page.getByLabel("输入变更次数")).toHaveText("0");
  await expect(page.getByRole("alert")).toBeEmpty();
});

for (const replacement of ["012345", ""] as const) {
  test(`parent replacement with ${JSON.stringify(replacement)} clears only the stale local format error`, async ({ page }) => {
    if (replacement === "")
      await page.getByRole("button", { name: "设置示例值" }).click();
    const original = replacement === "" ? "012345" : "";
    await paste(page, 0, "12 345");
    await value(page, original);
    await expect(page.getByRole("alert")).toContainText("其他字符");
    await page.getByLabel("缺位", { exact: true }).check();
    await page.getByLabel("缺位", { exact: true }).uncheck();
    await expect(page.getByRole("alert")).toContainText("其他字符");
    await page.getByRole("button", {
      name: replacement === "" ? "清空" : "设置示例值", exact: true,
    }).click();
    await value(page, replacement);
    await expect(page.getByRole("alert")).toBeEmpty();
    for (const input of await page.locator(".code-slots input").all())
      await expect(input).toHaveAttribute("aria-invalid", "false");
    await page.getByLabel("错码", { exact: true }).check();
    await expect(page.getByRole("alert")).toContainText("验证码不正确");
    await expect(page.getByLabel("输入变更次数")).toHaveText("0");
  });
}

for (const width of [1440, 768, 390, 320]) {
  test(`six slots fit and keep visible keyboard focus at ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 900 });
    await paste(page, 0, "012345");
    await value(page, "012345");
    const slots = page.locator(".code-slots input");
    await expect(slots.last()).toBeFocused();
    const boxes = await slots.evaluateAll((inputs) =>
      inputs.map((input) => {
        const { x, y, width, height } = input.getBoundingClientRect();
        return { x, y, width, height };
      }),
    );
    expect(boxes.every((box) =>
      box.y === boxes[0].y && box.width > 20 && box.height > 30 &&
      box.x >= 0 && box.x + box.width <= width,
    )).toBe(true);
    expect(await page.evaluate(() =>
      document.documentElement.scrollWidth <= innerWidth,
    )).toBe(true);
    await slots.first().focus();
    await page.keyboard.press("ArrowRight");
    await expect(slots.nth(1)).toBeFocused();
    await expect(slots.nth(1)).toHaveCSS("outline-style", "solid");
    await page.screenshot({
      path: `test-results/code-input-${width}.png`, fullPage: true,
    });
  });
}
