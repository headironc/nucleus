import { test, expect, type Page } from "@playwright/test";
import { build } from "vite";
import tailwindcss from "@tailwindcss/vite";
import { resolve } from "node:path";

const origin = "http://authentication.localhost";
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
    define: { __BUILD_ID__: JSON.stringify("authentication-copy-fixture") },
    build: {
      write: false,
      emptyOutDir: false,
      cssCodeSplit: false,
      assetsInlineLimit: 0,
      rolldownOptions: {
        input: resolve("tests/fixtures/authentication-copy.tsx"),
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
test.beforeEach(async ({ context }) => {
  await context.route("**/*", async (route) => {
    const url = new URL(route.request().url());
    if (url.origin !== origin) {
      await route.abort();
      return;
    }
    if (url.pathname === "/build.json") {
      await route.fulfill({ json: { buildId: "authentication-copy-fixture" } });
      return;
    }
    if (url.pathname === "/api/v1/settings") {
      await route.fulfill({
        json: {
          code: "ok",
          settings: {
            challenge: { turnstile: { siteKey: "1x00000000000000000000AA" } },
          },
        },
      });
      return;
    }
    if (url.pathname.startsWith("/api/")) {
      await route.abort();
      return;
    }
    await route.fulfill(
      resources.get(url.pathname) ?? { contentType: "text/html", body: html },
    );
  });
  await context.addInitScript(() => {
    window.turnstile = {
      render(element, options) {
        const button = document.createElement("button");
        button.type = "button";
        button.textContent = "完成安全验证（测试替身）";
        button.onclick = () => options.callback("fixture-token");
        element.append(button);
        return "fixture-widget";
      },
      reset() {},
      remove() {},
    };
  });
});
async function capture(page: Page, name: string) {
  await expect
    .poll(() =>
      page.evaluate(() => document.documentElement.scrollWidth <= innerWidth),
    )
    .toBe(true);
  await page.screenshot({
    path: test.info().outputPath(name + ".png"),
    fullPage: true,
  });
}
async function seed(page: Page, stage: "login" | "register") {
  await page.goto(origin);
  await page.waitForFunction(() => !!window.seedAuthentication);
  await page.evaluate((stage) => window.seedAuthentication(stage), stage);
  await page.goto(`${origin}/authentication/${stage}`);
}
for (const width of [320, 1440])
  for (const colorScheme of ["light", "dark"] as const) {
    test(`home and authentication tasks at ${width} ${colorScheme}`, async ({
      page,
    }) => {
      await page.setViewportSize({ width, height: 1000 });
      await page.emulateMedia({ colorScheme });
      await page.goto(origin);
      await expect(
        page.getByRole("heading", { name: "个人账户与员工服务", exact: true }),
      ).toBeVisible();
      await expect(
        page.getByRole("link", { name: "打开个人账户", exact: true }),
      ).toHaveAttribute("href", "/account");
      await capture(page, "home");
      await page.getByRole("link", { name: "手机号登录" }).click();
      await expect(
        page.getByRole("heading", { name: "获取短信验证码", exact: true }),
      ).toBeVisible();
      await capture(page, "codes");
      await page
        .getByRole("button", { name: "完成安全验证（测试替身）" })
        .click();
      await page
        .getByRole("button", { name: "获取验证码", exact: true })
        .click();
      await expect(page.getByLabel("手机号", { exact: true })).toBeFocused();
      await expect(
        page.getByText("请填写 11 位中国大陆手机号，不含空格或区号。", {
          exact: true,
        }),
      ).toBeVisible();
      await capture(page, "codes-input-error");
      let posts = 0;
      await page.route("**/api/v1/codes", async (route) => {
        posts++;
        await route.abort();
      });
      await page.getByLabel("手机号", { exact: true }).fill("13900000000");
      await page
        .getByRole("button", { name: "获取验证码", exact: true })
        .click();
      await expect(
        page.getByRole("heading", {
          name: "暂时无法确认发送结果",
          exact: true,
        }),
      ).toBeFocused();
      await capture(page, "codes-unknown");
      await page.reload();
      await expect(
        page.getByRole("button", { name: "重试获取验证码", exact: true }),
      ).toBeEnabled();
      expect(posts).toBe(1);
      await page.evaluate(() => sessionStorage.clear());
      await seed(page, "login");
      await expect(
        page.getByRole("heading", { name: "手机号登录", exact: true }),
      ).toBeVisible();
      await capture(page, "login");
      await page.getByRole("button", { name: "验证并继续" }).click();
      await expect(
        page.getByRole("textbox", { name: "短信验证码", exact: true }),
      ).toBeFocused();
      await expect(page.getByRole("alert")).toHaveText(
        "请输入 6 位短信验证码。",
      );
      await capture(page, "login-input-error");
      let logins = 0;
      await page.route("**/api/v1/sessions", async (route) => {
        logins++;
        await route.abort();
      });
      await page
        .getByRole("textbox", { name: "短信验证码", exact: true })
        .fill("012345");
      await page.getByRole("button", { name: "验证并继续" }).click();
      await expect(
        page.getByRole("heading", {
          name: "暂时无法确认是否已登录",
          exact: true,
        }),
      ).toBeFocused();
      await capture(page, "login-unknown");
      await page.reload();
      await expect(
        page.getByRole("button", { name: "查看登录结果", exact: true }),
      ).toBeVisible();
      expect(logins).toBe(1);
      await page.evaluate(() => sessionStorage.clear());
      await seed(page, "register");
      await expect(
        page.getByRole("heading", { name: "创建个人账户", exact: true }),
      ).toBeVisible();
      await capture(page, "register");
      await page.getByRole("button", { name: "创建账户", exact: true }).click();
      await expect(page.getByLabel("姓名", { exact: true })).toBeFocused();
      await capture(page, "register-input-error");
    });
  }
test("a terminal unknown SMS result offers no unavailable result query", async ({
  page,
}) => {
  await page.goto(origin);
  await page.evaluate(() => {
    sessionStorage.setItem(
      "halo.c1",
      JSON.stringify({ version: 1, closed: true, delivery: "unknown" }),
    );
  });
  await page.goto(`${origin}/authentication/codes`);
  await expect(
    page.getByRole("heading", { name: "暂时无法确认发送结果", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText("请先查看手机短信。", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: /查看发送结果|获取验证码/ }),
  ).toHaveCount(0);
  await expect(page.getByText(/稍后查看发送结果/)).toHaveCount(0);
  await capture(page, "codes-terminal-unknown");
});
test("a first SMS limit and its restored attempt describe a send-capable retry", async ({
  page,
}) => {
  await page.setViewportSize({ width: 320, height: 1000 });
  await page.emulateMedia({ colorScheme: "dark" });
  const requests: { operation: string; input: unknown }[] = [];
  await page.route("**/api/v1/codes", async (route) => {
    requests.push({
      operation: route.request().headers()["operation-id"],
      input: route.request().postDataJSON(),
    });
    if (requests.length === 1)
      await route.fulfill({
        status: 429,
        headers: { "Retry-After": "1" },
        json: { code: "rate_limited" },
      });
    else
      await route.fulfill({
        json: {
          code: "ok",
          message: {
            id: "019354fd-933c-7e6a-8912-345678901234",
            delivery: "sent",
            expiresAt: new Date(Date.now() + 600_000).toISOString(),
          },
        },
      });
  });
  await page.goto(`${origin}/authentication/codes`);
  await page.getByLabel("手机号", { exact: true }).fill("13900000000");
  await page.getByRole("button", { name: "完成安全验证（测试替身）" }).click();
  await page.getByRole("button", { name: "获取验证码", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "请稍后再试", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: /秒后可重试/ }),
  ).toBeDisabled();
  await expect(page.getByRole("button", { name: /查看发送结果/ })).toHaveCount(
    0,
  );
  expect(requests).toHaveLength(1);
  await capture(page, "codes-first-limit");
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "暂时无法确认发送结果", exact: true }),
  ).toBeVisible();
  await expect(page.getByRole("button", { name: /查看发送结果/ })).toHaveCount(
    0,
  );
  await expect(
    page.getByRole("button", { name: "重试获取验证码", exact: true }),
  ).toBeEnabled();
  expect(requests).toHaveLength(1);
  await capture(page, "codes-restored-limit");
  await page
    .getByRole("button", { name: "重试获取验证码", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "验证码已提交发送", exact: true }),
  ).toBeVisible();
  expect(requests).toHaveLength(2);
  expect(requests[1]).toEqual(requests[0]);
});
test("a trusted sending receipt retains its result action after reload", async ({
  page,
}) => {
  let posts = 0;
  await page.route("**/api/v1/codes", async (route) => {
    posts++;
    await route.fulfill({
      json: {
        code: "ok",
        message: {
          id: "019354fd-933c-7e6a-8912-345678901234",
          delivery: "sending",
          expiresAt: new Date(Date.now() + 600_000).toISOString(),
        },
      },
    });
  });
  await page.goto(`${origin}/authentication/codes`);
  await page.getByLabel("手机号", { exact: true }).fill("13900000000");
  await page.getByRole("button", { name: "完成安全验证（测试替身）" }).click();
  await page.getByRole("button", { name: "获取验证码", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "验证码正在发送", exact: true }),
  ).toBeVisible();
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "验证码正在发送", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: /秒后可查看结果/ }),
  ).toBeDisabled();
  await expect(
    page.getByRole("button", { name: /重试获取验证码/ }),
  ).toHaveCount(0);
  expect(posts).toBe(1);
});
test("a restored mobile refusal does not promise the phone number's current account or login", async ({
  page,
}) => {
  await page.setViewportSize({ width: 320, height: 1000 });
  await seed(page, "register");
  await expect(
    page.getByRole("heading", { name: "创建个人账户", exact: true }),
  ).toBeVisible();
  let registrations = 0;
  await page.route("**/api/v1/registrations", async (route) => {
    registrations++;
    await route.fulfill({ status: 409, json: { code: "mobile_taken" } });
  });
  await page.getByLabel("姓名", { exact: true }).fill("示例用户");
  await page
    .getByLabel("居民身份证号", { exact: true })
    .fill("11010519491231002X");
  await page.getByRole("button", { name: "创建账户", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "请重新验证手机号", exact: true }),
  ).toBeVisible();
  await page.reload();
  await expect(
    page.getByText("本次注册未完成。请重新验证手机号，再按页面提示继续。", {
      exact: true,
    }),
  ).toBeVisible();
  await expect(page.getByText(/该手机号已有账户|验证手机号后登录/)).toHaveCount(
    0,
  );
  await expect(
    page.getByRole("link", { name: "重新验证手机号", exact: true }),
  ).toHaveAttribute("href", "/authentication/codes");
  await expect(page.getByRole("textbox")).toHaveCount(0);
  expect(registrations).toBe(1);
  await capture(page, "register-restored-mobile-refusal");
});
test("a restored identity-number refusal describes its original registration time", async ({
  page,
}) => {
  await page.setViewportSize({ width: 320, height: 1000 });
  await seed(page, "register");
  await expect(
    page.getByRole("heading", { name: "创建个人账户", exact: true }),
  ).toBeVisible();
  let registrations = 0;
  await page.route("**/api/v1/registrations", async (route) => {
    registrations++;
    await route.fulfill({ status: 409, json: { code: "id_number_taken" } });
  });
  await page.getByLabel("姓名", { exact: true }).fill("示例用户");
  await page
    .getByLabel("居民身份证号", { exact: true })
    .fill("11010519491231002X");
  await page.getByRole("button", { name: "创建账户", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "请检查居民身份证号", exact: true }),
  ).toBeVisible();
  await page.reload();
  await expect(
    page.getByText("本次注册时，该身份证号已被使用。请核对填写的号码。", {
      exact: true,
    }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "返回填写资料", exact: true }),
  ).toBeVisible();
  await expect(page.getByRole("textbox")).toHaveCount(0);
  expect(registrations).toBe(1);
  await capture(page, "register-restored-identity-refusal");
});
test("a created account remains confirmed after local cleanup fails", async ({
  page,
}) => {
  await page.setViewportSize({ width: 320, height: 1000 });
  await page.emulateMedia({ colorScheme: "dark" });
  await seed(page, "register");
  await expect(
    page.getByRole("heading", { name: "创建个人账户", exact: true }),
  ).toBeVisible();
  let registrations = 0;
  await page.route("**/api/v1/registrations", async (route) => {
    registrations++;
    await route.fulfill({
      status: 201,
      json: {
        code: "ok",
        session: {
          id: "019354fd-933c-7e6a-8912-345678901299",
          expiresAt: "2035-01-02T00:00:00Z",
        },
      },
    });
  });
  await page.evaluate(() => {
    const set = Storage.prototype.setItem;
    Storage.prototype.setItem = function (key, value) {
      if (key.startsWith("halo.r2.session."))
        throw new Error("Fixture cleanup refusal");
      set.call(this, key, value);
    };
  });
  await page.getByLabel("姓名", { exact: true }).fill("示例用户");
  await page
    .getByLabel("居民身份证号", { exact: true })
    .fill("11010519491231002X");
  await page.getByRole("button", { name: "创建账户", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "账户已创建", exact: true }),
  ).toBeFocused();
  await expect(
    page.getByText("暂时无法打开个人账户，请重新检查。", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "创建账户", exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "重新检查", exact: true }),
  ).toBeVisible();
  expect(registrations).toBe(1);
  await capture(page, "register-created-cleanup-failure");
});
test("root failure offers a working reload and 404 offers the home route", async ({
  page,
}) => {
  let loads = 0;
  page.on("request", (request) => {
    if (request.isNavigationRequest() && request.url() === `${origin}/failure`)
      loads++;
  });
  await page.goto(`${origin}/failure`);
  await expect(
    page.getByRole("heading", { name: "页面暂时无法加载" }),
  ).toBeVisible();
  await capture(page, "root-failure");
  await page.getByRole("button", { name: "重新加载", exact: true }).click();
  await expect.poll(() => loads).toBe(2);
  await page.goto(`${origin}/missing`);
  await expect(
    page.getByRole("heading", { name: "找不到这个页面" }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "重新加载", exact: true }),
  ).toHaveCount(0);
  await page
    .getByRole("link", { name: "返回首页", exact: true })
    .last()
    .click();
  await expect(
    page.getByRole("heading", { name: "个人账户与员工服务" }),
  ).toBeVisible();
});
