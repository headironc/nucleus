import { test, expect, type Page, type Route } from "@playwright/test";
import { build } from "vite";
import tailwindcss from "@tailwindcss/vite";
import { resolve } from "node:path";
import { widget } from "./widget";

declare global {
  interface Window {
    __widgets?: Array<{
      options: { action: string; cData: string };
    }>;
  }
}

// Controlled response fixture: the real page, protocol decoders, Recovery
// controller, IndexedDB selection, CodeInput and Turnstile wrapper. Database,
// cookie and Nginx effects belong to the real combination acceptance.
const origin = "http://mobile-change.localhost";
const sessionA = "019354fd-933c-7e6a-8912-345678901234";
const sessionB = "019354fd-933c-7e6a-8912-345678901235";
const accountA = {
  account: "019354fd-933c-7e6a-8912-345678901236",
  person: "018f3c1a-0000-7000-8000-000000000001",
  name: "示例用户",
};
const accountB = {
  account: "019354fd-933c-7e6a-8912-345678901237",
  person: "018f3c1a-0000-7000-8000-000000000009",
  name: "另一位用户",
};
const messageId = "018f3c1a-0000-7000-8000-000000000002";
const transferId = "018f3c1a-0000-7000-8000-000000000003";
const expiresAt = "2035-01-01T00:00:00Z";
const completedAt = "2026-10-24T12:00:00Z";
const currentMobile = "13900000000";
const newMobile = "13800000000";
const currentCode = "012345";
const newCode = "654321";

type Person = typeof accountA;
type Call = {
  method: string;
  path: string;
  operation: string;
  session: string;
  body: string | null;
};

const resources = new Map<
  string,
  { body: string | Buffer; contentType: string }
>();
let html: string;
let people: Record<string, Person>;
let calls: Call[];
let status: "pending" | "completed" | "expired";
let sendFault: "none" | "disconnect" | "limited";
let delivery: "sending" | "sent";
let establishFault: "none" | "disconnect" | "wrong-code";
let completeFault: "none" | "disconnect";
let queryFault: "none" | "unavailable";
let marker: boolean;

function accountResponse(person: Person) {
  return {
    code: "ok",
    account: {
      id: person.account,
      personId: person.person,
      revisionId: person.person,
      name: person.name,
      idNumber: { suffix: "012X" },
      binding: null,
      avatar: {
        revision: person.account,
        href: `https://fixture.invalid/api/v1/avatars/${"a".repeat(64)}.webp?key_id=7&expires_at=2051222400&signature=${Buffer.alloc(32, 255).toString("base64url")}`,
        expiresAt,
        mediaType: "image/webp",
        width: 512,
        height: 512,
      },
    },
  };
}

function message() {
  return {
    code: "ok",
    message: { id: messageId, delivery, expiresAt },
  };
}

const material = (page: Page) =>
  page.evaluate(() =>
    JSON.parse(sessionStorage.getItem("halo.mobile-change") ?? "null"),
  );

async function paste(page: Page, value: string) {
  await page
    .locator(".code-slots input")
    .first()
    .evaluate((input, text) => {
      const data = new DataTransfer();
      data.setData("text/plain", text);
      input.dispatchEvent(
        new ClipboardEvent("paste", {
          clipboardData: data,
          bubbles: true,
          cancelable: true,
        }),
      );
    }, value);
}

async function clearBackoff(page: Page) {
  await page.evaluate(() => {
    const record = JSON.parse(
      sessionStorage.getItem("halo.mobile-change") ?? "null",
    );
    if (record?.active) {
      record.active.retryAt = 0;
      sessionStorage.setItem("halo.mobile-change", JSON.stringify(record));
    }
  });
  await page.reload();
}

async function choose(page: Page, session: string, person: Person) {
  people[session] = person;
  await page.evaluate(async (value) => {
    const attempt = await window.mobileSessions.begin(crypto.randomUUID());
    if (
      !(await window.mobileSessions.accept(attempt, {
        id: value,
        expiresAt: "2035-01-01T00:00:00Z",
      }))
    )
      throw Error("selection changed");
  }, session);
}

test.beforeAll(async () => {
  const result = await build({
    configFile: false,
    publicDir: false,
    plugins: [tailwindcss()],
    logLevel: "error",
    define: { __BUILD_ID__: JSON.stringify("mobile-change-fixture") },
    build: {
      write: false,
      emptyOutDir: false,
      cssCodeSplit: false,
      assetsInlineLimit: 0,
      rolldownOptions: {
        input: resolve("tests/fixtures/mobile-change.tsx"),
        output: { entryFileNames: "fixture.js" },
      },
    },
  });
  if (Array.isArray(result) || !("output" in result))
    throw Error("Expected one fixture build");
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
  html = `<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><link rel="icon" href="data:,">${styles.join("")}</head><body><div id="fixture"></div><script type="module" src="/fixture.js"></script></body></html>`;
});

test.beforeEach(async ({ page }) => {
  people = {};
  calls = [];
  status = "pending";
  sendFault = "none";
  delivery = "sent";
  establishFault = "none";
  completeFault = "none";
  queryFault = "none";
  marker = false;
  await page.route(
    "https://challenges.cloudflare.com/turnstile/v0/api.js?render=explicit",
    (route) => route.fulfill({ contentType: "text/javascript", body: widget }),
  );
  await page.route(origin + "/**", async (route: Route) => {
    const request = route.request();
    const url = new URL(request.url());
    const path = url.pathname;
    if (path === "/build.json") {
      await route.fulfill({ json: { buildId: "mobile-change-fixture" } });
      return;
    }
    if (path === "/api/v1/settings") {
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
    if (path === "/api/v1/accounts/current") {
      const person = people[request.headers()["session-id"]];
      if (!person) {
        await route.fulfill({
          status: 401,
          json: { code: "authentication_invalid" },
        });
        return;
      }
      await route.fulfill({ json: accountResponse(person) });
      return;
    }
    const call: Call = {
      method: request.method(),
      path,
      operation: request.headers()["operation-id"],
      session: request.headers()["session-id"],
      body: request.postData(),
    };
    if (
      path === "/api/v1/accounts/current/codes" ||
      path === "/api/v1/mobile-changes" ||
      path === "/api/v1/accounts/current/mobile" ||
      path === `/api/v1/mobile-changes/${transferId}` ||
      path === `/api/v1/mobile-changes/${transferId}/codes`
    )
      calls.push(call);
    if (path === "/api/v1/accounts/current/codes") {
      if (sendFault === "limited") {
        await route.fulfill({
          status: 429,
          headers: { "Retry-After": "7" },
          json: { code: "rate_limited" },
        });
        return;
      }
      if (sendFault === "disconnect") {
        await route.abort("connectionreset");
        return;
      }
      await route.fulfill({ json: message() });
      return;
    }
    if (path === "/api/v1/mobile-changes") {
      if (establishFault === "disconnect") {
        await route.abort("connectionreset");
        return;
      }
      if (establishFault === "wrong-code") {
        await route.fulfill({
          status: 403,
          json: { code: "verification_invalid" },
        });
        return;
      }
      await route.fulfill({
        status: 201,
        json: { code: "ok", transfer: { id: transferId, expiresAt } },
      });
      return;
    }
    if (path === `/api/v1/mobile-changes/${transferId}/codes`) {
      if (sendFault === "limited") {
        await route.fulfill({
          status: 429,
          headers: { "Retry-After": "7" },
          json: { code: "rate_limited" },
        });
        return;
      }
      if (sendFault === "disconnect") {
        await route.abort("connectionreset");
        return;
      }
      await route.fulfill({ json: message() });
      return;
    }
    if (path === "/api/v1/accounts/current/mobile") {
      if (completeFault === "disconnect") {
        await route.abort("connectionreset");
        return;
      }
      status = "completed";
      await route.fulfill({
        json: { code: "ok", completion: { id: transferId, completedAt } },
      });
      return;
    }
    if (path === `/api/v1/mobile-changes/${transferId}`) {
      if (queryFault === "unavailable") {
        await route.fulfill({
          status: 503,
          json: { code: "dependency_unavailable" },
        });
        return;
      }
      await route.fulfill({
        json: {
          code: "ok",
          transfer:
            status === "completed"
              ? { status, completedAt }
              : status === "expired"
                ? { status }
                : { status, expiresAt },
        },
      });
      return;
    }
    if (path.startsWith("/api/")) {
      await route.abort();
      return;
    }
    const resource = resources.get(path);
    if (resource) {
      await route.fulfill(resource);
      return;
    }
    const codes = "script-src 'self' https://challenges.cloudflare.com";
    const policy = [
      "default-src 'self'",
      path === "/account/mobile" ? codes : "script-src 'self'",
      "style-src-elem 'self'",
      "style-src-attr 'unsafe-inline'",
      "img-src 'self'",
      "connect-src 'self'",
      path === "/account/mobile"
        ? "frame-src https://challenges.cloudflare.com"
        : "frame-src 'none'",
      "object-src 'none'",
      "frame-ancestors 'none'",
      "form-action 'self'",
      "base-uri 'self'",
    ].join("; ");
    await route.fulfill({
      body: html,
      contentType: "text/html",
      headers: { "Content-Security-Policy": policy },
    });
  });
  await page.goto(origin);
  await page.waitForFunction(() => !!window.mobileSessions);
  await choose(page, sessionA, accountA);
});

async function open(page: Page) {
  await page.goto(origin + "/account/mobile");
  await expect(page.getByLabel("当前手机号", { exact: true })).toBeFocused();
  await expect(
    page.getByRole("heading", { name: "填写当前手机号", exact: true }),
  ).toBeVisible();
}

async function enterCurrent(page: Page) {
  await page.getByLabel("当前手机号", { exact: true }).fill(currentMobile);
  await page.getByRole("button", { name: "完成安全验证（测试替身）" }).click();
  await page.getByRole("button", { name: "获取验证码" }).click();
  await expect(
    page.getByRole("heading", { name: "输入当前手机号收到的验证码" }),
  ).toBeVisible();
}

async function establish(page: Page) {
  await enterCurrent(page);
  await paste(page, currentCode);
  await page.getByRole("button", { name: "验证并继续" }).click();
  await expect(
    page.getByRole("heading", { name: "填写新手机号" }),
  ).toBeVisible();
}

async function enterNew(page: Page) {
  await page.getByLabel("新手机号", { exact: true }).fill(newMobile);
  await page.getByRole("button", { name: "完成安全验证（测试替身）" }).click();
  await page.getByRole("button", { name: "获取新手机号验证码" }).click();
  await expect(
    page.getByRole("heading", { name: "输入新手机号收到的验证码" }),
  ).toBeVisible();
}

async function confirm(page: Page) {
  await enterNew(page);
  await paste(page, newCode);
  await page.getByRole("button", { name: "继续", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "确认更换手机号" }),
  ).toBeVisible();
}

test("a failed startup read does not call an earlier completion unsubmitted", async ({
  page,
}) => {
  await open(page);
  await establish(page);
  await confirm(page);
  await page.getByRole("button", { name: "确认更换手机号" }).click();
  await expect(
    page.getByRole("heading", { name: "手机号已更换", exact: true }),
  ).toBeVisible();
  expect((await material(page)).known).toHaveLength(1);
  await choose(page, sessionB, accountA);
  await expect(
    page.getByRole("heading", { name: "此前的手机号更换已完成", exact: true }),
  ).toBeVisible();
  const writes = calls.filter((call) => call.method !== "GET");
  await page.addInitScript(() => {
    const get = Storage.prototype.getItem;
    Storage.prototype.getItem = function (key) {
      if (this === sessionStorage && key === "halo.mobile-change")
        throw new Error("Fixture stored-result read unavailable");
      return get.call(this, key);
    };
  });
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "此前操作仍待确认", exact: true }),
  ).toBeVisible();
  await expect(page.locator("main")).not.toContainText("尚未提交");
  await expect(page.getByText(accountA.name, { exact: true })).toBeVisible();
  await expect(
    page.getByRole("link", { name: "打开个人账户", exact: true }),
  ).toHaveAttribute("href", "/account");
  await expect(page.getByRole("button", { name: "查看更换结果" })).toHaveCount(
    0,
  );
  await expect(page.locator(".mobile-change-time")).toHaveCount(0);
  expect(calls.filter((call) => call.method !== "GET")).toEqual(writes);
  for (const width of [320, 1440]) {
    await page.setViewportSize({ width, height: 1000 });
    await page.screenshot({
      path: test
        .info()
        .outputPath(`stored-completion-read-failure-${width}.png`),
      fullPage: true,
    });
  }
});

test("an unknown early send with no transfer promises no login-to-query recovery", async ({
  page,
}) => {
  await open(page);
  sendFault = "disconnect";
  await page.getByLabel("当前手机号", { exact: true }).fill(currentMobile);
  await page.getByRole("button", { name: "完成安全验证（测试替身）" }).click();
  await page.getByRole("button", { name: "获取验证码", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "验证码发送待确认", exact: true }),
  ).toBeVisible();
  const send = calls.at(-1)!;
  expect((await material(page)).active.transfer).toBeUndefined();
  await page.route(origin + "/api/v1/accounts/current", (route) =>
    route.fulfill({ status: 401, json: { code: "authentication_invalid" } }),
  );
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect(
    page.getByRole("heading", { name: "请重新登录", exact: true }),
  ).toBeVisible();
  await expect(page.locator("main")).toContainText("本次操作结果尚未确认");
  await expect(page.locator("main")).not.toContainText("继续查看更换结果");
  await expect(
    page.getByRole("link", { name: "手机号登录", exact: true }),
  ).toHaveAttribute("href", "/authentication/codes?return=mobile-change");
  await page.unroute(origin + "/api/v1/accounts/current");
  await choose(page, sessionB, accountA);
  await expect(
    page.getByRole("heading", { name: "此前操作仍待确认", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "查看更换结果", exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "获取验证码", exact: true }),
  ).toHaveCount(0);
  expect(
    calls.filter((call) => call.path === "/api/v1/accounts/current/codes"),
  ).toEqual([send]);
  expect(
    calls.filter(
      (call) =>
        call.path.startsWith("/api/v1/mobile-changes/") &&
        call.method === "GET",
    ),
  ).toHaveLength(0);
  for (const width of [320, 1440]) {
    await page.setViewportSize({ width, height: 1000 });
    await page.screenshot({
      path: test.info().outputPath(`no-transfer-after-login-${width}.png`),
      fullPage: true,
    });
  }
});

test("continuing the original change and reading its result remain distinct actions", async ({
  page,
}) => {
  await open(page);
  await establish(page);
  await confirm(page);
  completeFault = "disconnect";
  await page
    .getByRole("button", { name: "确认更换手机号", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "更换结果尚不确定", exact: true }),
  ).toBeVisible();
  const put = calls.at(-1)!;
  const sends = calls.filter((call) => call.path.endsWith("/codes"));
  await clearBackoff(page);
  await expect(
    page.getByRole("button", { name: "继续本次更换", exact: true }),
  ).toBeEnabled();
  const pendingResponse = page.waitForResponse(
    (response) =>
      response.url() === origin + `/api/v1/mobile-changes/${transferId}` &&
      response.request().method() === "GET",
  );
  await page.getByRole("button", { name: "查看更换结果", exact: true }).click();
  expect(await (await pendingResponse).json()).toMatchObject({
    transfer: { status: "pending" },
  });
  await expect(
    page.getByRole("heading", { name: "更换结果尚不确定", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "查看更换结果", exact: true }),
  ).toHaveClass("primary");
  expect(calls.filter((call) => call.method === "PUT")).toEqual([put]);
  expect(calls.filter((call) => call.path.endsWith("/codes"))).toEqual(sends);
  await expect(
    page.getByRole("button", { name: "继续本次更换", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "查看更换结果", exact: true }),
  ).toBeVisible();
  for (const colorScheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme });
    for (const width of [320, 1440]) {
      await page.setViewportSize({ width, height: 1000 });
      expect(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= innerWidth,
        ),
      ).toBe(true);
      await page.screenshot({
        path: test
          .info()
          .outputPath(`unknown-two-actions-${colorScheme}-${width}.png`),
        fullPage: true,
      });
    }
  }
  completeFault = "none";
  await page.getByRole("button", { name: "继续本次更换", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "手机号已更换", exact: true }),
  ).toBeVisible();
  expect(calls.filter((call) => call.method === "PUT")).toEqual([put, put]);
  expect(calls.filter((call) => call.path.endsWith("/codes"))).toEqual(sends);
});

for (const stage of ["current", "new"] as const) {
  test(`${stage} sending survives reload and retries the original send`, async ({
    page,
  }) => {
    await open(page);
    if (stage === "new") await establish(page);
    delivery = "sending";
    const label = stage === "current" ? "当前手机号" : "新手机号";
    const path =
      stage === "current"
        ? "/api/v1/accounts/current/codes"
        : `/api/v1/mobile-changes/${transferId}/codes`;
    await page
      .getByLabel(label, { exact: true })
      .fill(stage === "current" ? currentMobile : newMobile);
    await page
      .getByRole("button", { name: "完成安全验证（测试替身）" })
      .click();
    const startedAt = Date.now();
    await page
      .getByRole("button", {
        name: stage === "current" ? "获取验证码" : "获取新手机号验证码",
      })
      .click();
    await expect(
      page.getByRole("heading", { name: "验证码正在发送", exact: true }),
    ).toBeVisible();
    const original = calls.filter((call) => call.path === path)[0];
    const saved = await material(page);
    expect(saved.active.token).toBeTruthy();
    expect(saved.active.retryAt).toBeGreaterThanOrEqual(startedAt + 5000);
    await page.reload();
    await expect(
      page.getByRole("heading", { name: "验证码正在发送", exact: true }),
    ).toBeVisible();
    expect((await material(page)).active.retryAt).toBe(saved.active.retryAt);
    expect(calls.filter((call) => call.path === path)).toEqual([original]);
    await expect(page.getByRole("button", { name: /重新获取/ })).toHaveCount(0);
    await expect(
      page.getByRole("heading", { name: `输入${label}收到的验证码` }),
    ).toHaveCount(0);
    await expect(
      page.getByRole("button", { name: "完成安全验证（测试替身）" }),
    ).toHaveCount(0);
    delivery = "sent";
    await page
      .getByRole("button", { name: `重试获取${label}验证码`, exact: true })
      .click({ timeout: 10_000 });
    expect(Date.now()).toBeGreaterThanOrEqual(saved.active.retryAt);
    await expect(
      page.getByRole("heading", { name: `输入${label}收到的验证码` }),
    ).toBeVisible();
    expect(calls.filter((call) => call.path === path)).toEqual([
      original,
      original,
    ]);
    expect((await material(page)).active.token).toBeUndefined();
  });
}

test("four stages keep independent operations and a completed result", async ({
  page,
}) => {
  const capture = async (state: string) => {
    for (const colorScheme of ["light", "dark"] as const) {
      await page.emulateMedia({ colorScheme });
      for (const width of [320, 1440]) {
        await page.setViewportSize({ width, height: 1000 });
        expect(
          await page.evaluate(
            () => document.documentElement.scrollWidth <= innerWidth,
          ),
        ).toBe(true);
        await page.screenshot({
          path: test.info().outputPath(`${state}-${colorScheme}-${width}.png`),
          fullPage: true,
        });
      }
    }
  };
  await open(page);
  await enterCurrent(page);
  await capture("current-code");
  expect(calls[0].method).toBe("POST");
  expect(calls[0].path).toBe("/api/v1/accounts/current/codes");
  expect(calls[0].session).toBe(sessionA);
  expect(JSON.parse(calls[0].body!)).toEqual({
    mobile: currentMobile,
    token: `mobile:${calls[0].operation}`,
  });
  expect(await page.evaluate(() => window.__widgets?.[0]?.options.action)).toBe(
    "send_mobile_change_code",
  );
  expect(await page.evaluate(() => window.__widgets?.[0]?.options.cData)).toBe(
    calls[0].operation,
  );

  await paste(page, currentCode);
  await page.getByRole("button", { name: "验证并继续" }).click();
  await expect(
    page.getByRole("heading", { name: "填写新手机号" }),
  ).toBeVisible();
  await capture("new-mobile");
  expect(calls[1].method).toBe("POST");
  expect(calls[1].path).toBe("/api/v1/mobile-changes");
  expect(JSON.parse(calls[1].body!)).toEqual({
    messageId,
    code: currentCode,
  });
  expect(calls[1].operation).not.toBe(calls[0].operation);

  await enterNew(page);
  await capture("new-code");
  expect(calls[2].path).toBe(`/api/v1/mobile-changes/${transferId}/codes`);
  expect(JSON.parse(calls[2].body!)).toEqual({
    mobile: newMobile,
    token: `binding:${calls[2].operation}`,
  });
  expect(
    await page.evaluate(
      () => window.__widgets?.[window.__widgets.length - 1]?.options.action,
    ),
  ).toBe("send_mobile_binding_code");

  await paste(page, newCode);
  await page.getByRole("button", { name: "继续", exact: true }).click();
  await expect(page.getByText(newMobile)).toBeVisible();
  await expect(
    page.getByText("更换后，网站和小程序的全部登录将退出。", {
      exact: true,
    }),
  ).toBeVisible();
  await capture("confirm");
  await page.getByRole("button", { name: "确认更换手机号" }).click();
  await expect(
    page.getByRole("heading", { name: "手机号已更换" }),
  ).toBeVisible();
  await capture("completed");
  expect(calls[3].method).toBe("PUT");
  expect(calls[3].path).toBe("/api/v1/accounts/current/mobile");
  expect(JSON.parse(calls[3].body!)).toEqual({
    mobile: newMobile,
    messageId,
    code: newCode,
  });
  expect(
    new Set(
      calls
        .filter((call) => call.method === "POST" || call.method === "PUT")
        .map((call) => call.operation),
    ).size,
  ).toBe(4);
  expect(await material(page)).toEqual({
    version: 1,
    known: [
      {
        account: accountA.account,
        person: accountA.person,
        transfer: transferId,
        completedAt,
      },
    ],
    active: null,
    closed: null,
  });
  await expect(
    page.getByRole("link", { name: "使用新手机号登录", exact: true }),
  ).toHaveAttribute("href", "/authentication/codes?return=mobile-change");
  await page.evaluate(() => window.dispatchEvent(new Event("pagehide")));
  await expect(page.locator(".mobile-change-time")).toHaveCount(0);
  await page.evaluate(() => window.dispatchEvent(new Event("pageshow")));
  await expect(
    page.getByRole("heading", { name: "请使用手机号登录" }),
  ).toBeVisible();
  await expect(page.locator(".mobile-change-time")).toHaveCount(0);
  await choose(page, sessionB, accountB);
  await expect(page.getByText(accountB.name, { exact: true })).toBeVisible();
  await expect(page.locator(".mobile-change-time")).toHaveCount(0);
  await page.evaluate(async () => {
    const selected = await window.mobileSessions.read();
    if (selected?.session)
      await window.mobileSessions.clear({
        id: selected.session.id,
        revision: selected.revision,
      });
  });
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect(
    page.getByRole("heading", { name: "请使用手机号登录" }),
  ).toBeVisible();
  await expect(page.locator(".mobile-change-time")).toHaveCount(0);
  expect((await material(page)).known).toHaveLength(1);
});

test("save refusal, unknown send and unknown establish keep the original operation", async ({
  page,
}) => {
  await open(page);
  await page.evaluate(() => {
    const set = Storage.prototype.setItem;
    Storage.prototype.setItem = function (key, value) {
      if (this === sessionStorage && key === "halo.mobile-change") return;
      set.call(this, key, value);
    };
  });
  await page.getByLabel("当前手机号", { exact: true }).fill(currentMobile);
  await page.getByRole("button", { name: "完成安全验证（测试替身）" }).click();
  await page.getByRole("button", { name: "获取验证码" }).click();
  await expect(
    page.getByRole("heading", { name: "暂时无法继续" }),
  ).toBeVisible();
  expect(calls).toEqual([]);
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "填写当前手机号" }),
  ).toBeVisible();
  expect(calls).toEqual([]);

  sendFault = "disconnect";
  await page.getByLabel("当前手机号", { exact: true }).fill(currentMobile);
  await page.getByRole("button", { name: "完成安全验证（测试替身）" }).click();
  await page.getByRole("button", { name: "获取验证码" }).click();
  await expect(
    page.getByRole("heading", {
      name: "验证码发送待确认",
    }),
  ).toBeVisible();
  const operation = calls[0].operation;
  await expect(page.getByRole("button", { name: /秒后可重试/ })).toBeDisabled();
  await clearBackoff(page);
  await expect(
    page.getByRole("heading", {
      name: "验证码发送待确认",
    }),
  ).toBeVisible();
  sendFault = "none";
  await page.getByRole("button", { name: "重试获取当前手机号验证码" }).click();
  await expect(
    page.getByRole("heading", { name: "输入当前手机号收到的验证码" }),
  ).toBeVisible();
  expect(calls[1].operation).toBe(operation);
  expect(calls[1].body).toBe(calls[0].body);

  establishFault = "disconnect";
  await paste(page, currentCode);
  await page.getByRole("button", { name: "验证并继续" }).click();
  await expect(
    page.getByRole("heading", { name: "验证结果尚不确定" }),
  ).toBeVisible();
  await page.getByRole("link", { name: "打开个人账户" }).click();
  await expect(
    page.getByRole("heading", { name: "更换结果尚未确认" }),
  ).toBeVisible();
  await page.getByRole("button", { name: "留在此页" }).click();
  const establishOperation = calls[2].operation;
  await clearBackoff(page);
  await expect(
    page.getByRole("heading", {
      name: "验证结果尚不确定",
    }),
  ).toBeVisible();
  establishFault = "wrong-code";
  await page.getByRole("button", { name: "继续本次验证" }).click();
  await expect(
    page.getByRole("heading", {
      name: "验证结果尚不确定",
    }),
  ).toBeVisible();
  await expect(page.getByText("填写内容暂时无法使用。")).toBeVisible();
  expect(calls[3].operation).toBe(establishOperation);
  expect(calls[3].body).toBe(calls[2].body);
  await expect(page.getByRole("button", { name: "继续本次验证" })).toHaveCount(
    0,
  );
});

for (const action of ["current", "establish", "new", "complete"] as const) {
  for (const replay of [false, true]) {
    test(`untrusted ${action} write requires reload (replay: ${replay})`, async ({
      page,
    }) => {
      await page.clock.install({ time: new Date("2026-10-25T00:00:00Z") });
      await page.clock.pauseAt(new Date("2026-10-25T00:00:01Z"));
      await open(page);
      if (action === "establish") await enterCurrent(page);
      if (action === "new" || action === "complete") await establish(page);
      if (action === "complete") await confirm(page);
      const path =
        action === "current"
          ? "/api/v1/accounts/current/codes"
          : action === "establish"
            ? "/api/v1/mobile-changes"
            : action === "new"
              ? `/api/v1/mobile-changes/${transferId}/codes`
              : "/api/v1/accounts/current/mobile";
      const initial = calls.length;
      let attempts = 0;
      await page.route(origin + path, async (route) => {
        const request = route.request();
        calls.push({
          path,
          method: request.method(),
          body: request.postData(),
          operation: request.headers()["operation-id"],
          session: request.headers()["session-id"],
        });
        attempts++;
        if (replay && attempts === 1) await route.abort("connectionreset");
        else if (action === "current")
          await route.fulfill({
            contentType: "text/html",
            body: "unexpected response",
          });
        else if (action === "establish")
          await route.fulfill({ status: 201, json: { code: "future_code" } });
        else await route.fulfill({ json: { code: "ok" } });
      });
      if (action === "current" || action === "new") {
        await page
          .getByLabel(action === "current" ? "当前手机号" : "新手机号", {
            exact: true,
          })
          .fill(action === "current" ? currentMobile : newMobile);
        await page
          .getByRole("button", { name: "完成安全验证（测试替身）" })
          .click();
        await page
          .getByRole("button", {
            name: action === "current" ? "获取验证码" : "获取新手机号验证码",
          })
          .click();
      } else if (action === "establish") {
        await paste(page, currentCode);
        await page.getByRole("button", { name: "验证并继续" }).click();
      } else
        await page
          .getByRole("button", { name: "确认更换手机号", exact: true })
          .click();
      const retry =
        action === "establish"
          ? "继续本次验证"
          : action === "complete"
            ? "继续本次更换"
            : action === "current"
              ? "重试获取当前手机号验证码"
              : "重试获取新手机号验证码";
      if (replay) {
        await expect(
          page.getByRole("button", {
            name:
              action === "current" || action === "new"
                ? /秒后可重试/
                : /秒后可继续/,
          }),
        ).toBeDisabled();
        await page.clock.runFor(5000);
        await page.getByRole("button", { name: retry, exact: true }).click();
      }
      await expect(
        page.getByRole("button", { name: "重新加载" }),
      ).toBeVisible();
      const count = initial + (replay ? 2 : 1);
      expect(calls).toHaveLength(count);
      const saved = await material(page);
      expect(saved.active.attempted).toBe(true);
      expect(saved.active.operation).toBe(calls.at(-1)!.operation);
      expect(saved.known).toEqual([]);
      await page.clock.runFor(5000);
      await expect(
        page.getByRole("button", { name: retry, exact: true }),
      ).toHaveCount(0);
      expect(calls).toHaveLength(count);
      await page.unroute(origin + path);
      await page.getByRole("button", { name: "重新加载" }).click();
      await expect(
        page.getByRole("button", { name: retry, exact: true }),
      ).toBeEnabled();
      expect(calls).toHaveLength(count);
      expect(await material(page)).toEqual(saved);
      await page.getByRole("button", { name: retry, exact: true }).click();
      const heading =
        action === "current"
          ? "输入当前手机号收到的验证码"
          : action === "establish"
            ? "填写新手机号"
            : action === "new"
              ? "输入新手机号收到的验证码"
              : "手机号已更换";
      await expect(
        page.getByRole("heading", { name: heading, exact: true }),
      ).toBeVisible();
      expect(calls).toHaveLength(count + 1);
      expect(calls.at(-1)).toEqual(calls[initial]);
    });
  }
}

for (const stage of ["current", "new"] as const)
  for (const unknown of [false, true])
    test(`${stage} rate limiting preserves original uncertainty: ${unknown}`, async ({
      page,
    }) => {
      await open(page);
      if (stage === "new") await establish(page);
      const initial = calls.length;
      sendFault = unknown ? "disconnect" : "limited";
      await page
        .getByLabel(stage === "current" ? "当前手机号" : "新手机号", {
          exact: true,
        })
        .fill(stage === "current" ? currentMobile : newMobile);
      await page
        .getByRole("button", { name: "完成安全验证（测试替身）" })
        .click();
      await page
        .getByRole("button", {
          name: stage === "current" ? "获取验证码" : "获取新手机号验证码",
        })
        .click();
      if (unknown) {
        await expect(
          page.getByRole("heading", { name: "验证码发送待确认" }),
        ).toBeVisible();
        const operation = calls[initial].operation;
        await clearBackoff(page);
        sendFault = "limited";
        await page
          .getByRole("button", {
            name: `重试获取${stage === "current" ? "当前手机号" : "新手机号"}验证码`,
            exact: true,
          })
          .click();
        await expect(
          page.getByText("暂时无法继续，请稍后再试。本次结果仍未确认。", {
            exact: true,
          }),
        ).toBeVisible();
        expect(calls[initial + 1].operation).toBe(operation);
        await expect(
          page.getByText("暂时无法获取验证码，请稍后再试。", { exact: true }),
        ).toHaveCount(0);
      } else {
        await expect(
          page.getByText("暂时无法获取验证码，请稍后再试。", { exact: true }),
        ).toBeVisible();
      }
      const record = await material(page);
      expect(record.active.attempted).toBe(unknown);
      expect(record.active.retryAt).toBeGreaterThan(Date.now());
      await page.reload();
      expect((await material(page)).active).toEqual(record.active);
      if (!unknown) {
        await expect(
          page.getByRole("button", { name: "完成安全验证（测试替身）" }),
        ).toHaveCount(0);
        await clearBackoff(page);
        sendFault = "none";
        await page
          .getByRole("button", {
            name: `重试获取${stage === "current" ? "当前手机号" : "新手机号"}验证码`,
            exact: true,
          })
          .click();
        await expect(
          page.getByRole("heading", {
            name:
              stage === "current"
                ? "输入当前手机号收到的验证码"
                : "输入新手机号收到的验证码",
          }),
        ).toBeVisible();
        expect(calls[initial + 1]).toEqual(calls[initial]);
        expect(
          await page.evaluate(
            async () => (await window.mobileSessions.read())?.session?.id,
          ),
        ).toBe(sessionA);
      }
    });

test("a fresh history entry retains unresolved-operation protection after material loss", async ({
  page,
}) => {
  await open(page);
  sendFault = "disconnect";
  await page.getByLabel("当前手机号", { exact: true }).fill(currentMobile);
  await page.getByRole("button", { name: "完成安全验证（测试替身）" }).click();
  await page.getByRole("button", { name: "获取验证码" }).click();
  await expect(
    page.getByRole("heading", { name: "验证码发送待确认" }),
  ).toBeVisible();
  await page.goto(origin);
  await page.goto(origin + "/account/mobile");
  await expect(
    page.getByRole("heading", { name: "验证码发送待确认" }),
  ).toBeVisible();
  expect(await page.evaluate(() => history.state?.haloMobileChange)).toBe(true);
  await page.evaluate(() => sessionStorage.removeItem("halo.mobile-change"));
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "此前操作仍待确认" }),
  ).toBeVisible();
  await expect(page.getByLabel("当前手机号", { exact: true })).toHaveCount(0);
  expect(calls).toHaveLength(1);
});

test("new-code expiry clears its proof but preserves explicit use of the valid transfer", async ({
  page,
}) => {
  await page.clock.install();
  await open(page);
  await establish(page);
  const deadline = (await material(page)).active.transferExpiresAt;
  const now = await page.evaluate(() => Date.now());
  await page.route(
    origin + `/api/v1/mobile-changes/${transferId}/codes`,
    (route) =>
      route.fulfill({
        json: {
          code: "ok",
          message: {
            id: messageId,
            delivery: "sent",
            expiresAt: new Date(now + 60_000).toISOString(),
          },
        },
      }),
    { times: 1 },
  );
  await enterNew(page);
  const count = calls.length;
  await page.clock.fastForward(61_000);
  await expect(
    page.getByRole("heading", { name: "填写新手机号" }),
  ).toBeVisible();
  const active = (await material(page)).active;
  expect(active.expiresAt).toBe(deadline);
  for (const key of ["newMobile", "code", "token", "messageId"])
    expect(active[key]).toBeUndefined();
  expect(calls).toHaveLength(count);
  await enterNew(page);
  expect(calls[calls.length - 1].path).toBe(
    `/api/v1/mobile-changes/${transferId}/codes`,
  );
});

test("a pending status read cannot permit new input and a completed read confirms", async ({
  page,
}) => {
  await open(page);
  await establish(page);
  await confirm(page);
  completeFault = "disconnect";
  await page.getByRole("button", { name: "确认更换手机号" }).click();
  await expect(
    page.getByRole("heading", { name: "更换结果尚不确定" }),
  ).toBeVisible();
  const put = calls[calls.length - 1];

  const pendingResponse = page.waitForResponse(
    (response) =>
      response.url() === origin + `/api/v1/mobile-changes/${transferId}` &&
      response.request().method() === "GET",
  );
  await page.getByRole("button", { name: "查看更换结果" }).click();
  expect(await (await pendingResponse).json()).toMatchObject({
    transfer: { status: "pending" },
  });
  await expect(
    page.getByRole("heading", { name: "更换结果尚不确定" }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "查看更换结果", exact: true }),
  ).toHaveClass("primary");
  expect(
    await page.getByRole("button", { name: "继续验证新手机号" }).count(),
  ).toBe(0);
  expect(calls.filter((call) => call.method === "PUT")).toHaveLength(1);

  status = "completed";
  await page.getByRole("button", { name: "查看更换结果" }).click();
  await expect(
    page.getByRole("heading", { name: "此前的手机号更换已完成" }),
  ).toBeVisible();
  expect(calls.filter((call) => call.method === "PUT")).toHaveLength(1);
  expect(calls[calls.length - 1].method).toBe("GET");
  expect(put.method).toBe("PUT");
  expect(await material(page)).toMatchObject({
    known: [{ transfer: transferId }],
    active: null,
  });
});

test("proof expiry removes phone inputs but preserves explicit original-result recovery", async ({
  page,
}) => {
  await page.clock.install();
  await open(page);
  await establish(page);
  await confirm(page);
  completeFault = "disconnect";
  await page.getByRole("button", { name: "确认更换手机号" }).click();
  await expect(
    page.getByRole("heading", { name: "更换结果尚不确定" }),
  ).toBeVisible();
  const original = (await material(page)).active;
  expect(original).toMatchObject({
    newMobile,
    code: newCode,
    attempted: true,
    transfer: transferId,
  });
  expect(original.currentMobile).toBeUndefined();
  await page.clock.fastForward(10 * 60 * 1000 + 1);
  await expect
    .poll(async () => (await material(page)).active.currentMobile)
    .toBeUndefined();
  const expired = (await material(page)).active;
  expect(expired).toMatchObject({
    transfer: transferId,
    attempted: true,
    retryAt: original.retryAt,
  });
  for (const field of ["newMobile", "code", "token", "messageId", "operation"])
    expect(expired[field]).toBeUndefined();
  status = "completed";
  await page.getByRole("button", { name: "查看更换结果" }).click();
  await expect(
    page.getByRole("heading", { name: "此前的手机号更换已完成" }),
  ).toBeVisible();
  expect(calls.filter((call) => call.method === "PUT")).toHaveLength(1);
  expect(calls.filter((call) => call.method === "POST")).toHaveLength(3);
  expect(calls[calls.length - 1]).toMatchObject({
    method: "GET",
    path: `/api/v1/mobile-changes/${transferId}`,
  });
  expect(await material(page)).toMatchObject({
    known: [{ transfer: transferId, completedAt }],
    active: null,
  });
});

test("a consumed current-code deadline does not close the new transfer", async ({
  page,
}) => {
  await page.clock.install();
  await open(page);
  const started = await page.evaluate(() => Date.now());
  await page.route(
    origin + "/api/v1/accounts/current/codes",
    (route) =>
      route.fulfill({
        json: {
          code: "ok",
          message: {
            id: messageId,
            delivery: "sent",
            expiresAt: new Date(started + 300_000).toISOString(),
          },
        },
      }),
    { times: 1 },
  );
  await enterCurrent(page);
  await page.clock.fastForward(240_000);
  await paste(page, currentCode);
  await page.getByRole("button", { name: "验证并继续" }).click();
  await expect(
    page.getByRole("heading", { name: "填写新手机号" }),
  ).toBeVisible();
  expect((await material(page)).active.currentMobile).toBeUndefined();
  await page.clock.fastForward(61_000);
  await confirm(page);
  await page.getByRole("button", { name: "确认更换手机号" }).click();
  await expect(
    page.getByRole("heading", { name: "手机号已更换" }),
  ).toBeVisible();
});

test("only an explicit ordinary-entry action starts another change after completion", async ({
  page,
}) => {
  await open(page);
  await establish(page);
  await confirm(page);
  await page.getByRole("button", { name: "确认更换手机号" }).click();
  await expect(
    page.getByRole("heading", { name: "手机号已更换" }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "再次更换手机号" }),
  ).toHaveCount(0);
  await choose(page, sessionB, accountA);
  await page.goto(origin + "/account/mobile?return=mobile-change");
  await expect(
    page.getByRole("heading", { name: "此前的手机号更换已完成" }),
  ).toBeVisible();
  await expect(page.locator("main")).not.toContainText(
    "请使用新手机号重新登录",
  );
  await expect(
    page.getByRole("link", { name: "打开个人账户", exact: true }),
  ).toHaveClass("primary");
  await expect(
    page.getByRole("link", { name: "手机号登录", exact: true }),
  ).toBeVisible();
  for (const colorScheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme });
    for (const width of [320, 1440]) {
      await page.setViewportSize({ width, height: 1000 });
      expect(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= innerWidth,
        ),
      ).toBe(true);
      await page.screenshot({
        path: test
          .info()
          .outputPath(`historical-completion-${colorScheme}-${width}.png`),
        fullPage: true,
      });
    }
  }
  await expect(
    page.getByRole("button", { name: "再次更换手机号" }),
  ).toHaveCount(0);
  const writes = calls.filter((call) => call.method !== "GET").length;
  await page.goto(origin + "/account/mobile");
  await page.getByRole("button", { name: "再次更换手机号" }).click();
  await expect(page.getByLabel("当前手机号", { exact: true })).toBeFocused();
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "填写当前手机号", exact: true }),
  ).toBeVisible();
  expect(calls.filter((call) => call.method !== "GET")).toHaveLength(writes);
  expect((await material(page)).known[0]).toMatchObject({
    transfer: transferId,
    completedAt,
  });
  await enterCurrent(page);
  expect(calls[calls.length - 1].session).toBe(sessionB);
  expect(calls[calls.length - 1].operation).not.toBe(calls[0].operation);
  await page.route(
    origin + "/api/v1/mobile-changes",
    (route) =>
      route.fulfill({ status: 410, json: { code: "verification_required" } }),
    { times: 1 },
  );
  await paste(page, currentCode);
  await page.getByRole("button", { name: "验证并继续" }).click();
  await expect(
    page.getByRole("heading", { name: "本次更换无法继续" }),
  ).toBeVisible();
  await expect(page.getByRole("heading", { name: "手机号已更换" })).toHaveCount(
    0,
  );
  expect((await material(page)).known[0].transfer).toBe(transferId);
});

for (const stage of ["current", "new"] as const) {
  for (const replay of [false, true]) {
    test(`build drift stops ${stage} send (replay: ${replay})`, async ({
      page,
    }) => {
      await open(page);
      if (stage === "new") await establish(page);
      if (replay) sendFault = "disconnect";
      else
        await page.route(origin + "/build.json", (route) =>
          route.fulfill({ json: { buildId: "next-build" } }),
        );
      await page
        .getByLabel(stage === "current" ? "当前手机号" : "新手机号", {
          exact: true,
        })
        .fill(stage === "current" ? currentMobile : newMobile);
      await page
        .getByRole("button", { name: "完成安全验证（测试替身）" })
        .click();
      const before = calls.length;
      await page
        .getByRole("button", {
          name: stage === "current" ? "获取验证码" : "获取新手机号验证码",
        })
        .click();
      if (replay) {
        await expect(
          page.getByRole("heading", { name: "验证码发送待确认" }),
        ).toBeVisible();
        await clearBackoff(page);
        await expect(
          page.getByRole("button", {
            name: `重试获取${stage === "current" ? "当前手机号" : "新手机号"}验证码`,
          }),
        ).toBeEnabled();
        await page.route(origin + "/build.json", (route) =>
          route.fulfill({ json: { buildId: "next-build" } }),
        );
        await page
          .getByRole("button", {
            name: `重试获取${stage === "current" ? "当前手机号" : "新手机号"}验证码`,
          })
          .click();
      }
      await expect(
        page.getByRole("button", { name: "重新加载" }),
      ).toBeVisible();
      expect(calls).toHaveLength(before + (replay ? 1 : 0));
      expect((await material(page)).active).toMatchObject({
        attempted: replay,
        token: expect.any(String),
      });
    });
  }
}

for (const stage of ["current", "new"] as const) {
  test(`limited ${stage} retry survives build failure with its saved proof`, async ({
    page,
  }) => {
    await open(page);
    if (stage === "new") await establish(page);
    sendFault = "limited";
    await page
      .getByLabel(stage === "current" ? "当前手机号" : "新手机号", {
        exact: true,
      })
      .fill(stage === "current" ? currentMobile : newMobile);
    await page
      .getByRole("button", { name: "完成安全验证（测试替身）" })
      .click();
    await page
      .getByRole("button", {
        name: stage === "current" ? "获取验证码" : "获取新手机号验证码",
      })
      .click();
    await expect(
      page.getByRole("heading", { name: "请稍后重试" }),
    ).toBeVisible();
    const original = calls.at(-1)!;
    const count = calls.length;
    await clearBackoff(page);
    await expect(
      page.getByRole("button", {
        name: `重试获取${stage === "current" ? "当前手机号" : "新手机号"}验证码`,
        exact: true,
      }),
    ).toBeEnabled();
    await page.route(origin + "/build.json", (route) =>
      route.fulfill({ json: { buildId: "next-build" } }),
    );
    await page
      .getByRole("button", {
        name: `重试获取${stage === "current" ? "当前手机号" : "新手机号"}验证码`,
        exact: true,
      })
      .click();
    await expect(page.getByRole("button", { name: "重新加载" })).toBeVisible();
    expect(calls).toHaveLength(count);
    await page.unroute(origin + "/build.json");
    await page.getByRole("button", { name: "重新加载" }).click();
    await expect(
      page.getByRole("heading", { name: "请稍后重试" }),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: "完成安全验证（测试替身）" }),
    ).toHaveCount(0);
    sendFault = "none";
    await clearBackoff(page);
    await page
      .getByRole("button", {
        name: `重试获取${stage === "current" ? "当前手机号" : "新手机号"}验证码`,
        exact: true,
      })
      .click();
    await expect(
      page.getByRole("heading", {
        name:
          stage === "current"
            ? "输入当前手机号收到的验证码"
            : "输入新手机号收到的验证码",
      }),
    ).toBeVisible();
    expect(calls.at(-1)).toEqual(original);
  });

  test(`backgrounding a pending ${stage} send saves its five-second cooldown`, async ({
    page,
  }) => {
    await page.clock.install({ time: new Date("2026-10-25T00:00:00Z") });
    await page.clock.pauseAt(new Date("2026-10-25T00:00:01Z"));
    await open(page);
    if (stage === "new") await establish(page);
    const path =
      stage === "current"
        ? "/api/v1/accounts/current/codes"
        : `/api/v1/mobile-changes/${transferId}/codes`;
    const sends: Call[] = [];
    let release!: () => void;
    const held = new Promise<void>((resolve) => {
      release = resolve;
    });
    await page.route(origin + path, async (route) => {
      const request = route.request();
      sends.push({
        path,
        method: request.method(),
        body: request.postData(),
        operation: request.headers()["operation-id"],
        session: request.headers()["session-id"],
      });
      if (sends.length === 1) {
        await held;
        await route.abort("aborted").catch(() => {});
      } else await route.fulfill({ json: message() });
    });
    await page
      .getByLabel(stage === "current" ? "当前手机号" : "新手机号", {
        exact: true,
      })
      .fill(stage === "current" ? currentMobile : newMobile);
    await page
      .getByRole("button", { name: "完成安全验证（测试替身）" })
      .click();
    await page
      .getByRole("button", {
        name: stage === "current" ? "获取验证码" : "获取新手机号验证码",
      })
      .click();
    await expect.poll(() => sends.length).toBe(1);
    await page.evaluate(() => {
      window.dispatchEvent(new Event("pagehide"));
      window.dispatchEvent(new Event("pageshow"));
    });
    release();
    await expect(
      page.getByRole("heading", { name: "验证码发送待确认" }),
    ).toBeVisible();
    await page.clock.runFor(4999);
    expect(sends).toHaveLength(1);
    await expect(
      page.getByRole("button", { name: /秒后可重试/ }),
    ).toBeDisabled();
    await page.clock.runFor(1);
    await page
      .getByRole("button", {
        name: `重试获取${stage === "current" ? "当前手机号" : "新手机号"}验证码`,
        exact: true,
      })
      .click();
    await expect(
      page.getByRole("heading", {
        name:
          stage === "current"
            ? "输入当前手机号收到的验证码"
            : "输入新手机号收到的验证码",
      }),
    ).toBeVisible();
    expect(sends).toHaveLength(2);
    expect(sends[1]).toEqual(sends[0]);
  });
}

for (const action of ["complete", "query"] as const) {
  test(`a late ${action} observes an unannounced IndexedDB selection change`, async ({
    page,
  }) => {
    await open(page);
    await establish(page);
    await confirm(page);
    if (action === "query") {
      completeFault = "disconnect";
      await page.getByRole("button", { name: "确认更换手机号" }).click();
      await expect(
        page.getByRole("heading", { name: "更换结果尚不确定" }),
      ).toBeVisible();
      await clearBackoff(page);
      status = "completed";
    }
    let release!: () => void;
    let entered!: () => void;
    const held = new Promise<void>((done) => (release = done));
    const reached = new Promise<void>((done) => (entered = done));
    const path =
      action === "complete"
        ? "/api/v1/accounts/current/mobile"
        : `/api/v1/mobile-changes/${transferId}`;
    await page.route(
      origin + path,
      async (route) => {
        expect(route.request().headers()["session-id"]).toBe(sessionA);
        entered();
        await held;
        await route.fallback();
      },
      { times: 1 },
    );
    await page
      .getByRole("button", {
        name: action === "complete" ? "确认更换手机号" : "查看更换结果",
        exact: true,
      })
      .click();
    await reached;
    people[sessionB] = accountB;
    // Commit another source's choice without a BroadcastChannel notification.
    await page.evaluate(
      async ({ id, expiresAt }) => {
        await new Promise<void>((resolve, reject) => {
          const opening = indexedDB.open("halo.session", 1);
          opening.onerror = () => reject(opening.error);
          opening.onsuccess = () => {
            const database = opening.result;
            const transaction = database.transaction("selection", "readwrite");
            transaction.objectStore("selection").put(
              {
                version: 1,
                revision: crypto.randomUUID(),
                attempt: null,
                session: { id, expiresAt },
              },
              "current",
            );
            transaction.oncomplete = () => {
              database.close();
              resolve();
            };
            transaction.onabort = () => {
              database.close();
              reject(transaction.error);
            };
          };
        });
      },
      { id: sessionB, expiresAt },
    );
    release();
    await expect(
      page.getByRole("heading", { name: "当前账户已变化" }),
    ).toBeVisible();
    await expect(page.locator(".mobile-change-time")).toHaveCount(0);
    await expect
      .poll(async () => (await material(page)).known)
      .toEqual([
        {
          account: accountA.account,
          person: accountA.person,
          transfer: transferId,
          completedAt,
        },
      ]);
    expect(
      await page.evaluate(
        async () => (await window.mobileSessions.read())?.session?.id,
      ),
    ).toBe(sessionB);
    await page.evaluate(() => window.dispatchEvent(new Event("focus")));
    await expect(page.getByText(accountB.name, { exact: true })).toBeVisible();
    await expect(page.locator(".mobile-change-time")).toHaveCount(0);
  });
}

test("account entry, three leave prompts and keyboard focus", async ({
  page,
}) => {
  people[sessionA] = accountA;
  const accountDocument = await page.goto(origin + "/account");
  expect(accountDocument?.headers()["content-security-policy"]).toContain(
    "frame-src 'none'",
  );
  expect(accountDocument?.headers()["content-security-policy"]).not.toContain(
    "challenges.cloudflare.com",
  );
  await expect(page.getByRole("heading", { name: "个人账户" })).toBeVisible();
  const [navigation] = await Promise.all([
    page.waitForResponse(
      (response) =>
        response.request().isNavigationRequest() &&
        new URL(response.url()).pathname === "/account/mobile",
    ),
    page.getByRole("link", { name: "更换手机号" }).click(),
  ]);
  expect(navigation.headers()["content-security-policy"]).toContain(
    "frame-src https://challenges.cloudflare.com",
  );
  expect(navigation.headers()["content-security-policy"]).toContain(
    "script-src 'self' https://challenges.cloudflare.com",
  );
  await expect(page).toHaveURL(origin + "/account/mobile");
  await expect(page.getByLabel("当前手机号", { exact: true })).toBeFocused();

  await page.getByLabel("当前手机号", { exact: true }).fill(currentMobile);
  const back = page.getByRole("link", { name: "打开个人账户" });
  await back.click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "放弃当前填写？" }),
  ).toBeVisible();
  await expect(page.getByRole("button", { name: "留在此页" })).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
  await expect(back).toBeFocused();
  await expect(page).toHaveURL(origin + "/account/mobile");

  await page.getByRole("button", { name: "完成安全验证（测试替身）" }).click();
  await page.getByRole("button", { name: "获取验证码" }).click();
  await paste(page, currentCode);
  await page.getByRole("button", { name: "验证并继续" }).click();
  await expect(
    page.getByRole("heading", { name: "填写新手机号" }),
  ).toBeVisible();
  await page.getByRole("link", { name: "打开个人账户" }).click();
  await expect(
    page.getByRole("heading", { name: "暂时离开换号办理？" }),
  ).toBeVisible();
  await page.getByRole("button", { name: "留在此页" }).click();

  await page.getByLabel("新手机号", { exact: true }).fill(newMobile);
  await page.getByRole("button", { name: "完成安全验证（测试替身）" }).click();
  await page.getByRole("button", { name: "获取新手机号验证码" }).click();
  await paste(page, newCode);
  await page.getByRole("button", { name: "继续", exact: true }).click();
  await page.getByRole("button", { name: "确认更换手机号" }).click();
  await expect(
    page.getByRole("heading", { name: "手机号已更换" }),
  ).toBeVisible();
  await page.getByRole("link", { name: "打开个人账户" }).click();
  await expect(page).toHaveURL(origin + "/account");
});

test("responsive widths, dark theme and long account names stay readable", async ({
  page,
}) => {
  accountA.name = "阿依古丽·阿不都热依木";
  await open(page);
  await page.getByLabel("当前手机号", { exact: true }).fill(currentMobile);
  await expect(page.getByText("阿依古丽·阿不都热依木")).toBeVisible();
  for (const width of [1440, 768, 390, 320]) {
    await page.setViewportSize({ width, height: 900 });
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    ).toBe(true);
    const path = test.info().outputPath(`mobile-change-form-${width}.png`);
    await page.screenshot({ path, fullPage: true });
    await test
      .info()
      .attach(`light-${width}`, { path, contentType: "image/png" });
  }
  await page.emulateMedia({ colorScheme: "dark" });
  for (const width of [1440, 768, 390, 320]) {
    await page.setViewportSize({ width, height: 900 });
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    ).toBe(true);
    const path = test.info().outputPath(`mobile-change-form-dark-${width}.png`);
    await page.screenshot({ path, fullPage: true });
    await test
      .info()
      .attach(`dark-${width}`, { path, contentType: "image/png" });
  }
  await page.emulateMedia({ colorScheme: "light" });
  await page.setViewportSize({ width: 390, height: 900 });
  await page.getByRole("button", { name: "完成安全验证（测试替身）" }).click();
  await page.getByRole("button", { name: "获取验证码" }).click();
  await expect(page.locator(".code-slots input").first()).toBeFocused();
  await page.keyboard.type("01");
  await expect(page.locator(".code-slots input").nth(2)).toBeFocused();
});
