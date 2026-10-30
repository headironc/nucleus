import { test, expect, type Page } from "@playwright/test";
import { build } from "vite";
import tailwindcss from "@tailwindcss/vite";
import { resolve } from "node:path";
import { writeFile } from "node:fs/promises";

// This fixture exercises the real page, protocol decoder and IndexedDB selection.
// Controlled responses do not establish the backend's transaction or cookie effects.
const origin = "http://logout.localhost";
const session = {
  id: "019354fd-933c-7e6a-8912-345678901234",
  expiresAt: "2035-01-01T00:00:00Z",
};
const newer = { ...session, id: "019354fd-933c-7e6a-8912-345678901235" };
const otherSession = { ...session, id: "019354fd-933c-7e6a-8912-345678901236" };
const account = "00000000-0000-0000-0000-000000000000";
const otherAccount = "ffffffff-ffff-ffff-ffff-ffffffffffff";
const revokedAt = "2026-10-23T00:00:00Z";
const resources = new Map<
  string,
  { body: string | Buffer; contentType: string }
>();
let html: string;

test.beforeAll(async ({ browser }) => {
  console.info(`Logout browser: ${browser.version()}`);
  const result = await build({
    configFile: false,
    publicDir: false,
    plugins: [tailwindcss()],
    logLevel: "error",
    define: { __BUILD_ID__: JSON.stringify("logout-fixture") },
    build: {
      write: false,
      emptyOutDir: false,
      cssCodeSplit: false,
      assetsInlineLimit: 0,
      rolldownOptions: {
        input: resolve("tests/fixtures/logout.tsx"),
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

async function choose(page: Page, value: typeof session) {
  await page.evaluate(async (value) => {
    const attempt = await window.logoutSessions.begin(crypto.randomUUID());
    const accepted = await window.logoutSessions.accept(attempt, value);
    if (!accepted) throw new Error("Fixture selection was superseded");
  }, value);
}

test.beforeEach(async ({ context, page }) => {
  await context.route(`${origin}/**`, async (route) => {
    const url = new URL(route.request().url());
    if (url.pathname === "/build.json") {
      await route.fulfill({ json: { buildId: "logout-fixture" } });
      return;
    }
    if (url.pathname === "/api/v1/accounts/current") {
      const selected = route.request().headers()["session-id"];
      if (![session.id, newer.id, otherSession.id].includes(selected)) {
        await route.fulfill({
          status: 401,
          json: { code: "authentication_invalid" },
        });
        return;
      }
      const other = selected === otherSession.id;
      await route.fulfill({
        json: {
          code: "ok",
          account: {
            id: other ? otherAccount : account,
            personId: account,
            revisionId: account,
            name: other ? "Other Fixture" : "Original Fixture",
            idNumber: { suffix: "002X" },
            binding: null,
            avatar: {
              revision: account,
              href: `https://fixture.invalid/api/v1/avatars/${"a".repeat(64)}.webp?key_id=7&expires_at=2051222400&signature=${Buffer.alloc(32, 255).toString("base64url")}`,
              expiresAt: session.expiresAt,
              mediaType: "image/webp",
              width: 512,
              height: 512,
            },
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
  await page.goto(origin);
  await page.waitForFunction(() => !!window.logoutSessions);
  await choose(page, session);
  await page.goto(`${origin}/sessions/logout`);
  await expect(
    page.getByRole("heading", { name: "确认退出本次登录？", exact: true }),
  ).toBeVisible();
});

test("all logout confirmation, long identity, avatar failure and responsive concealment", async ({
  page,
  context,
}) => {
  const longName = "阿依古丽·阿不都热依木·买买提明";
  await page.route("**/api/v1/accounts/current", (route) =>
    route.fulfill({
      json: {
        code: "ok",
        account: {
          id: account,
          personId: account,
          revisionId: account,
          name: longName,
          idNumber: { suffix: "002X" },
          binding: null,
          avatar: {
            revision: account,
            href: `https://fixture.invalid/api/v1/avatars/${"a".repeat(64)}.webp?key_id=7&expires_at=2051222400&signature=${Buffer.alloc(32, 255).toString("base64url")}`,
            expiresAt: session.expiresAt,
            mediaType: "image/webp",
            width: 512,
            height: 512,
          },
        },
      },
    }),
  );
  await page.route("https://fixture.invalid/**", (route) => route.abort());
  let writes = 0;
  page.on("request", (request) => {
    if (request.method() === "DELETE") writes++;
  });
  await page.goto(`${origin}/sessions/logout-all`);
  await expect(page.getByText(longName, { exact: true })).toBeVisible();
  await expect(
    page.getByRole("img", { name: "头像暂时无法加载" }),
  ).toBeVisible();
  for (const colorScheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme });
    for (const width of [1440, 768, 390, 320]) {
      await page.setViewportSize({ width, height: 1000 });
      expect(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= innerWidth,
        ),
      ).toBe(true);
      await page.evaluate(() =>
        scrollTo(0, document.documentElement.scrollHeight),
      );
      await expect(page.locator(".site-footer")).toBeInViewport();
      await page.screenshot({
        path: `test-results/all-logout-bottom-${colorScheme}-${width}.png`,
      });
      await page.evaluate(async () => {
        scrollTo(0, 0);
        await document.fonts.ready;
        await new Promise<void>((resolve) =>
          requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
        );
      });
      await page.screenshot({
        path: `test-results/all-logout-${colorScheme}-${width}.png`,
        fullPage: true,
      });
    }
  }
  const trigger = page.getByRole("button", {
    name: "退出全部设备",
    exact: true,
  });
  for (const close of ["Escape", "暂不退出", "关闭"]) {
    await trigger.focus();
    await page.keyboard.press("Enter");
    await expect(page.getByRole("dialog")).toBeVisible();
    await expect(
      page.getByRole("button", { name: "暂不退出", exact: true }),
    ).toBeFocused();
    if (close === "Escape") await page.keyboard.press("Escape");
    else await page.getByRole("button", { name: close, exact: true }).click();
    await expect(page.getByRole("dialog")).toHaveCount(0);
    await expect(trigger).toBeFocused();
    expect(writes).toBe(0);
  }
  await trigger.click();
  await page.screenshot({
    path: "test-results/all-logout-confirm-dark-320.png",
    fullPage: true,
  });
  await page.unroute("**/api/v1/accounts/current");
  await page.route("**/api/v1/accounts/current", (route) =>
    route.fulfill({ status: 403, json: { code: "permission_denied" } }),
  );
  const other = await context.newPage();
  await other.goto(origin);
  await other.waitForFunction(() => !!window.logoutSessions);
  await choose(other, otherSession);
  await page.bringToFront();
  await expect(
    page.getByRole("heading", { name: "暂时无法检查登录状态", exact: true }),
  ).toBeVisible();
  await expect(page.locator("dialog")).toHaveCount(0);
  await expect(page.locator("main")).not.toContainText(longName);
  await expect(page.locator("main")).not.toContainText("退出全部设备");
  expect(writes).toBe(0);
  await other.close();
});

test("all mismatch survives failed save and fixed login return with a newer selection", async ({
  page,
}) => {
  let writes = 0;
  let operation = "";
  await page.route("**/api/v1/sessions", async (route) => {
    writes++;
    operation = route.request().headers()["operation-id"];
    await page.evaluate(() => {
      const set = Storage.prototype.setItem;
      Storage.prototype.setItem = function (key, value) {
        if (this === sessionStorage && key === "halo.logout-all")
          throw new DOMException(
            "Fixture storage refusal",
            "QuotaExceededError",
          );
        return set.call(this, key, value);
      };
    });
    await route.fulfill({ status: 409, json: { code: "request_mismatch" } });
  });
  await page.goto(`${origin}/sessions/logout-all`);
  await page.getByRole("button", { name: "退出全部设备", exact: true }).click();
  await page
    .getByRole("button", { name: "确认退出全部设备", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "暂时无法继续", exact: true }),
  ).toBeVisible();
  page.on("dialog", (dialog) => dialog.accept());
  await page.goto(`${origin}/sessions/logout-all?return=all-logout-result`);
  await choose(page, newer);
  await expect(
    page.getByRole("heading", { name: "暂时无法确认退出结果", exact: true }),
  ).toBeVisible();
  let reads = 0;
  await page.route("**/api/v1/revocations?*", async (route) => {
    reads++;
    expect(
      new URL(route.request().url()).searchParams.get("operation_id"),
    ).toBe(operation);
    expect(route.request().headers()["operation-id"]).not.toBe(operation);
    expect(route.request().headers()["session-id"]).toBe(newer.id);
    await route.fulfill({
      json: {
        code: "ok",
        count: 1,
        revocations: [{ id: newer.id, revokedAt }],
        original: { scope: "current", initiatorId: newer.id },
      },
    });
  });
  const query = page.getByRole("button", {
    name: "查看退出结果",
    exact: true,
  });
  await expect(query).toBeEnabled();
  await query.click();
  await expect(
    page.getByRole("heading", { name: "已查到此前退出记录", exact: true }),
  ).toBeVisible();
  await expect(page.locator("main")).toContainText(
    "不表示本次退出全部设备已完成",
  );
  await expect(
    page.getByRole("heading", { name: "本次全部退出已完成", exact: true }),
  ).toHaveCount(0);
  expect(
    await page.evaluate(
      async () => (await window.logoutSessions.read())?.session?.id,
    ),
  ).toBe(newer.id);
  expect(writes).toBe(1);
  expect(reads).toBe(1);
  await page.getByRole("link", { name: "返回首页", exact: true }).click();
  await expect(page.getByRole("dialog")).toContainText("此前退出");
  await page.getByRole("button", { name: "留在此页", exact: true }).click();
  await expect(
    page.getByRole("link", { name: "返回首页", exact: true }),
  ).toBeFocused();
});

test("all late completion preserves another tab's newer selection and login attempt", async ({
  page,
  context,
}) => {
  let release!: () => void;
  const waiting = new Promise<void>((resolve) => {
    release = resolve;
  });
  let sent!: () => void;
  const sending = new Promise<void>((resolve) => {
    sent = resolve;
  });
  let writes = 0;
  await page.route("**/api/v1/sessions", async (route) => {
    writes++;
    sent();
    await waiting;
    await route.fulfill({
      json: {
        code: "ok",
        completion: {
          id: route.request().headers()["operation-id"],
          count: 2,
          revokedAt,
        },
      },
    });
  });
  await page.goto(`${origin}/sessions/logout-all`);
  await page.getByRole("button", { name: "退出全部设备", exact: true }).click();
  await page
    .getByRole("button", { name: "确认退出全部设备", exact: true })
    .click();
  await sending;
  const other = await context.newPage();
  await other.goto(origin);
  await other.waitForFunction(() => !!window.logoutSessions);
  await choose(other, newer);
  const attempt = await other.evaluate(() =>
    window.logoutSessions.begin(crypto.randomUUID()),
  );
  release();
  await page.bringToFront();
  await expect(
    page.getByRole("button", {
      name: /查看此前退出结果|查看退出结果|秒后可查看/,
    }),
  ).toBeVisible();
  const selected = await page.evaluate(() => window.logoutSessions.read());
  expect(selected?.session?.id).toBe(newer.id);
  expect(selected?.attempt).toBe(attempt.operation);
  expect(writes).toBe(1);
  await other.close();
});

test("explicit confirmation, failed first save and responsive account concealment", async ({
  page,
}) => {
  let deletes = 0;
  page.on("request", (request) => {
    if (request.method() === "DELETE") deletes++;
  });
  for (const colorScheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme });
    for (const width of [1440, 768, 390, 320]) {
      await page.setViewportSize({ width, height: 1000 });
      await expect(
        page.getByText("Original Fixture", { exact: true }),
      ).toBeVisible();
      expect(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= innerWidth,
        ),
      ).toBe(true);
      const layout = await page.evaluate(() => ({
        scrollY,
        height: document.documentElement.scrollHeight,
        elements: [".site-header", "main", ".logout-guide", ".site-footer"].map(
          (selector) => {
            const nodes = document.querySelectorAll(selector);
            return {
              selector,
              count: nodes.length,
              bounds: nodes[0]?.getBoundingClientRect().toJSON(),
            };
          },
        ),
      }));
      for (const element of layout.elements) expect(element.count).toBe(1);
      expect(layout.scrollY).toBe(0);
      expect(layout.elements[2].bounds.bottom).toBeLessThanOrEqual(
        layout.elements[3].bounds.top,
      );
      await writeFile(
        `test-results/logout-layout-${colorScheme}-${width}.json`,
        JSON.stringify(layout, null, 2),
      );
      // Paint offscreen content before fullPage capture. Chrome 155 repeated
      // earlier pixels after responsive resizing despite correct DOM geometry.
      await page.evaluate(() =>
        scrollTo(0, document.documentElement.scrollHeight),
      );
      await expect(page.locator(".site-footer")).toBeInViewport();
      await page.screenshot({
        path: `test-results/logout-bottom-${colorScheme}-${width}.png`,
      });
      await page.evaluate(async () => {
        scrollTo(0, 0);
        await document.fonts.ready;
        await new Promise<void>((resolve) =>
          requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
        );
      });
      await page.screenshot({
        path: `test-results/logout-confirm-${colorScheme}-${width}.png`,
        fullPage: true,
      });
    }
  }
  await page.evaluate(() => {
    const set = Storage.prototype.setItem;
    Storage.prototype.setItem = function (key, value) {
      if (this === sessionStorage && key === "halo.logout") {
        Storage.prototype.setItem = set;
        throw new DOMException("Fixture quota", "QuotaExceededError");
      }
      set.call(this, key, value);
    };
  });
  await page
    .getByRole("button", { name: "确认退出本次登录", exact: true })
    .click();
  await expect(
    page.getByText("本次退出尚未提交", { exact: false }),
  ).toBeVisible();
  await expect(page.getByText("Original Fixture", { exact: true })).toHaveCount(
    0,
  );
  expect(deletes).toBe(0);
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "确认退出本次登录？", exact: true }),
  ).toBeVisible();
  expect(deletes).toBe(0);
  expect(
    await page.evaluate(() => sessionStorage.getItem("halo.logout")),
  ).toBeNull();
});

test("unknown original survives reload, explicit read and confirmed navigation without another DELETE", async ({
  page,
}) => {
  const writes: { operation: string; selected: string }[] = [];
  const reads: { operation: string; query: string | null; selected: string }[] =
    [];
  await page.route("**/api/v1/sessions/current", async (route) => {
    const headers = route.request().headers();
    writes.push({
      operation: headers["operation-id"],
      selected: headers["session-id"],
    });
    expect(route.request().postData()).toBeNull();
    await route.abort();
  });
  await page.route("**/api/v1/revocations?*", async (route) => {
    const headers = route.request().headers();
    reads.push({
      operation: headers["operation-id"],
      query: new URL(route.request().url()).searchParams.get("operation_id"),
      selected: headers["session-id"],
    });
    await route.fulfill({
      json: { code: "ok", original: null, count: 0, revocations: [] },
    });
  });
  await page
    .getByRole("button", { name: "确认退出本次登录", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "本次退出结果待确认", exact: true }),
  ).toBeVisible();
  for (const width of [1440, 768, 390, 320]) {
    await page.setViewportSize({ width, height: 1000 });
    await expect(
      page.getByRole("heading", { name: "查询不会再次退出", exact: true }),
    ).toBeVisible();
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    ).toBe(true);
    await page.screenshot({
      path: `test-results/logout-pending-${width}.png`,
      fullPage: true,
    });
  }
  page.on("dialog", (dialog) => dialog.accept());
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "本次退出结果待确认", exact: true }),
  ).toBeVisible();
  expect(writes).toHaveLength(1);
  expect(reads).toHaveLength(0);
  await page
    .getByRole("button", { name: "查看本次退出结果", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "暂未查到本次退出结果", exact: true }),
  ).toBeVisible();
  expect(reads).toHaveLength(1);
  expect(reads[0].query).toBe(writes[0].operation);
  expect(reads[0].operation).not.toBe(writes[0].operation);
  expect(reads[0].selected).toBe(session.id);
  const home = page
    .locator(".logout-panel")
    .getByRole("link", { name: "返回首页", exact: true });
  await home.click();
  await expect(page.getByRole("dialog")).toBeVisible();
  await expect(
    page.getByRole("button", { name: "留在此页", exact: true }),
  ).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(home).toBeFocused();
  await home.click();
  await page.getByRole("button", { name: "仍然离开", exact: true }).click();
  await expect(page.getByText("Public fixture", { exact: true })).toBeVisible();
  await page.goBack();
  await expect(
    page.getByRole("heading", { name: "暂未查到本次退出结果", exact: true }),
  ).toBeVisible();
  await home.click();
  await expect(page.getByRole("dialog")).toBeVisible();
  expect(writes).toHaveLength(1);
  expect(reads).toHaveLength(1);
});

test("an older unsent history entry preserves a later submitted logout on back navigation", async ({
  page,
}) => {
  let deletes = 0;
  await page.route("**/api/v1/sessions/current", async (route) => {
    deletes++;
    await route.abort();
  });
  await page.evaluate(() => {
    const set = Storage.prototype.setItem;
    Storage.prototype.setItem = function (key, value) {
      if (this === sessionStorage && key === "halo.logout") {
        Storage.prototype.setItem = set;
        throw new DOMException("Fixture quota", "QuotaExceededError");
      }
      set.call(this, key, value);
    };
  });
  await page
    .getByRole("button", { name: "确认退出本次登录", exact: true })
    .click();
  await expect(
    page.getByText("本次退出尚未提交", { exact: false }),
  ).toBeVisible();
  await page
    .locator(".logout-panel")
    .getByRole("link", { name: "返回首页", exact: true })
    .click();
  await expect(page.getByText("Public fixture", { exact: true })).toBeVisible();
  await page.goto(`${origin}/sessions/logout`);
  await page
    .getByRole("button", { name: "确认退出本次登录", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "本次退出结果待确认", exact: true }),
  ).toBeVisible();
  const saved = await page.evaluate(() =>
    sessionStorage.getItem("halo.logout"),
  );
  page.on("dialog", (dialog) => dialog.accept());
  await page.goBack();
  await expect(page.getByText("Public fixture", { exact: true })).toBeVisible();
  await page.goBack();
  await expect(
    page.getByRole("heading", { name: "本次退出结果待确认", exact: true }),
  ).toBeVisible();
  expect(await page.evaluate(() => sessionStorage.getItem("halo.logout"))).toBe(
    saved,
  );
  await expect(
    page.getByRole("button", { name: "确认退出本次登录", exact: true }),
  ).toHaveCount(0);
  expect(deletes).toBe(1);
});

test("leaving during logout preflight does not warn of a submitted operation or send DELETE", async ({
  page,
}) => {
  let entered!: () => void;
  let release!: () => void;
  const started = new Promise<void>((resolve) => {
    entered = resolve;
  });
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  let deletes = 0;
  page.on("request", (request) => {
    if (request.method() === "DELETE") deletes++;
  });
  await page.route("**/build.json", async (route) => {
    entered();
    await held;
    await route
      .fulfill({ json: { buildId: "logout-fixture" } })
      .catch(() => {});
  });
  await page
    .getByRole("button", { name: "确认退出本次登录", exact: true })
    .click();
  await started;
  await page
    .locator(".logout-panel")
    .getByRole("link", { name: "返回首页", exact: true })
    .click();
  try {
    expect(await page.getByRole("dialog").count()).toBe(0);
    await expect(
      page.getByText("Public fixture", { exact: true }),
    ).toBeVisible();
  } finally {
    release();
  }
  expect(deletes).toBe(0);
});

for (const response of [
  "permission_denied",
  "invalid_input",
  "untrusted-delete",
  "untrusted-query",
]) {
  test(`pending ${response} keeps navigation and unload warnings without replay`, async ({
    page,
  }) => {
    let deletes = 0;
    let queries = 0;
    await page.route("**/api/v1/sessions/current", async (route) => {
      deletes++;
      if (response === "untrusted-delete")
        await route.fulfill({ json: { code: "ok" } });
      else await route.abort();
    });
    await page.route("**/api/v1/revocations?*", async (route) => {
      queries++;
      await route.fulfill({
        status:
          response === "permission_denied"
            ? 403
            : response === "invalid_input"
              ? 400
              : 200,
        json: { code: response === "untrusted-query" ? "ok" : response },
      });
    });
    await page
      .getByRole("button", { name: "确认退出本次登录", exact: true })
      .click();
    if (response !== "untrusted-delete")
      await page
        .getByRole("button", { name: "查看本次退出结果", exact: true })
        .click();
    await expect(
      page.getByRole("heading", {
        name: response.startsWith("untrusted")
          ? "请重新加载页面"
          : "暂时无法查看退出结果",
        exact: true,
      }),
    ).toBeVisible();
    await expect(
      page.getByRole("heading", { name: "查询不会再次退出" }),
    ).toHaveCount(0);
    await page
      .locator(".logout-panel")
      .getByRole("link", { name: "返回首页", exact: true })
      .click();
    await expect(page.getByRole("dialog")).toBeVisible();
    await expect(page.getByRole("dialog")).toContainText(
      "离开不会取消已提交的退出",
    );
    await expect(page.getByRole("dialog")).not.toContainText("仍可查看");
    await page.getByRole("button", { name: "留在此页", exact: true }).click();
    const warning = page.waitForEvent("dialog");
    await page.evaluate(() => {
      setTimeout(() => location.reload(), 0);
    });
    const beforeUnload = await warning;
    expect(beforeUnload.type()).toBe("beforeunload");
    await beforeUnload.dismiss();
    expect(deletes).toBe(1);
    expect(queries).toBe(response === "untrusted-delete" ? 0 : 1);
    expect(
      await page.evaluate(
        () => JSON.parse(sessionStorage.getItem("halo.logout")!).outcome,
      ),
    ).toBe("pending");
  });
}

test("post-send storage failure keeps leave confirmation without another DELETE", async ({
  page,
}) => {
  let deletes = 0;
  await page.route("**/api/v1/sessions/current", async (route) => {
    deletes++;
    await page.evaluate(() => {
      const set = Storage.prototype.setItem;
      Storage.prototype.setItem = function (key, value) {
        if (this === sessionStorage && key === "halo.logout") {
          Storage.prototype.setItem = set;
          throw new DOMException("Fixture quota", "QuotaExceededError");
        }
        set.call(this, key, value);
      };
    });
    await route.abort();
  });
  await page
    .getByRole("button", { name: "确认退出本次登录", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "暂时无法继续", exact: true }),
  ).toBeVisible();
  await page
    .locator(".logout-panel")
    .getByRole("link", { name: "返回首页", exact: true })
    .click();
  await expect(page.getByRole("dialog")).toBeVisible();
  await page.getByRole("button", { name: "留在此页", exact: true }).click();
  const warning = page.waitForEvent("dialog");
  await page.evaluate(() => {
    setTimeout(() => location.reload(), 0);
  });
  const beforeUnload = await warning;
  expect(beforeUnload.type()).toBe("beforeunload");
  await beforeUnload.dismiss();
  await expect(
    page.getByRole("heading", { name: "暂时无法继续", exact: true }),
  ).toBeVisible();
  expect(deletes).toBe(1);
});

for (const outcome of ["complete", "rejected"] as const) {
  test(`persisted ${outcome} with failed selection cleanup has no unknown-outcome leave warning`, async ({
    page,
  }) => {
    let deletes = 0;
    await page.evaluate(() => {
      Object.getPrototypeOf(window.logoutSessions).clear = async () => {
        throw new Error("Fixture selection cleanup unavailable");
      };
    });
    await page.route("**/api/v1/sessions/current", async (route) => {
      deletes++;
      await route.fulfill(
        outcome === "complete"
          ? {
              json: {
                code: "ok",
                completion: {
                  id: route.request().headers()["operation-id"],
                  count: 1,
                  revokedAt,
                },
              },
            }
          : { status: 401, json: { code: "authentication_invalid" } },
      );
    });
    await page
      .getByRole("button", { name: "确认退出本次登录", exact: true })
      .click();
    await expect(
      page.getByRole("heading", { name: "暂时无法继续", exact: true }),
    ).toBeVisible();
    expect(
      await page.evaluate(
        () => JSON.parse(sessionStorage.getItem("halo.logout")!).outcome,
      ),
    ).toBe(outcome);
    let unloadWarnings = 0;
    page.on("dialog", async (dialog) => {
      unloadWarnings++;
      await dialog.dismiss();
    });
    await page
      .locator(".logout-panel")
      .getByRole("link", { name: "返回首页", exact: true })
      .click();
    await expect(
      page.getByText("Public fixture", { exact: true }),
    ).toBeVisible();
    expect(unloadWarnings).toBe(0);
    await expect(page.getByRole("dialog")).toHaveCount(0);
    expect(deletes).toBe(1);
  });
}

for (const outcome of ["complete", "rejected"] as const) {
  for (const reload of [false, true]) {
    test(`determined ${outcome} during held cleanup permits leaving (reload: ${reload})`, async ({
      page,
    }) => {
      await page.evaluate(() => {
        Object.getPrototypeOf(window.logoutSessions).clear = () =>
          new Promise(() => {});
      });
      let deletes = 0;
      await page.route("**/api/v1/sessions/current", async (route) => {
        deletes++;
        await route.fulfill(
          outcome === "complete"
            ? {
                json: {
                  code: "ok",
                  completion: {
                    id: route.request().headers()["operation-id"],
                    count: 1,
                    revokedAt,
                  },
                },
              }
            : { status: 401, json: { code: "authentication_invalid" } },
        );
      });
      await page
        .getByRole("button", { name: "确认退出本次登录", exact: true })
        .click();
      await expect(
        page.getByRole("heading", { name: "正在更新登录状态", exact: true }),
      ).toBeVisible();
      if (reload) {
        await page.evaluate(() =>
          window.dispatchEvent(new Event("halo:reload-required")),
        );
        await expect(
          page.getByRole("heading", { name: "请重新加载页面", exact: true }),
        ).toBeVisible();
      }
      let warnings = 0;
      page.on("dialog", async (dialog) => {
        warnings++;
        await dialog.dismiss();
      });
      await page
        .locator(".logout-panel")
        .getByRole("link", { name: "返回首页", exact: true })
        .click();
      await expect(
        page.getByText("Public fixture", { exact: true }),
      ).toBeVisible();
      expect(warnings).toBe(0);
      expect(deletes).toBe(1);
    });
  }
}

for (const result of ["complete", "rejected", "query"] as const) {
  test(`known ${result} survives a terminal save refusal without an unknown warning`, async ({
    page,
  }) => {
    const refuseSave = async () =>
      page.evaluate(() => {
        const set = Storage.prototype.setItem;
        Storage.prototype.setItem = function (key, value) {
          if (this === sessionStorage && key === "halo.logout") {
            Storage.prototype.setItem = set;
            throw new DOMException(
              "Fixture terminal quota",
              "QuotaExceededError",
            );
          }
          set.call(this, key, value);
        };
      });
    let deletes = 0;
    let queries = 0;
    await page.route("**/api/v1/sessions/current", async (route) => {
      deletes++;
      if (result === "query") {
        await route.abort();
        return;
      }
      await refuseSave();
      await route.fulfill(
        result === "complete"
          ? {
              json: {
                code: "ok",
                completion: {
                  id: route.request().headers()["operation-id"],
                  count: 1,
                  revokedAt,
                },
              },
            }
          : { status: 401, json: { code: "authentication_invalid" } },
      );
    });
    await page.route("**/api/v1/revocations?*", async (route) => {
      queries++;
      await refuseSave();
      await route.fulfill({
        json: {
          code: "ok",
          count: 1,
          revocations: [{ id: session.id, revokedAt }],
          original: { scope: "current", initiatorId: session.id },
        },
      });
    });
    await page
      .getByRole("button", { name: "确认退出本次登录", exact: true })
      .click();
    if (result === "query")
      await page
        .getByRole("button", { name: "查看本次退出结果", exact: true })
        .click();
    await expect(
      page.getByRole("heading", { name: "暂时无法继续", exact: true }),
    ).toBeVisible();
    expect(
      await page.evaluate(() => {
        const event = new Event("beforeunload", { cancelable: true });
        window.dispatchEvent(event);
        return event.defaultPrevented;
      }),
    ).toBe(false);
    await page.getByRole("button", { name: "重新检查", exact: true }).click();
    await expect(
      page.getByRole("heading", {
        name: result === "rejected" ? "本次退出未受理" : "本次登录已退出",
        exact: true,
      }),
    ).toBeVisible();
    let warnings = 0;
    page.on("dialog", async (dialog) => {
      warnings++;
      await dialog.dismiss();
    });
    await page
      .locator(".logout-panel")
      .getByRole("link", { name: "返回首页", exact: true })
      .click();
    await expect(
      page.getByText("Public fixture", { exact: true }),
    ).toBeVisible();
    expect(warnings).toBe(0);
    expect(deletes).toBe(1);
    expect(queries).toBe(result === "query" ? 1 : 0);
  });
}

test("late DELETE completion cannot clear another tab's newer choice", async ({
  page,
  context,
}) => {
  let entered!: () => void;
  let release!: () => void;
  const started = new Promise<void>((resolve) => {
    entered = resolve;
  });
  const waiting = new Promise<void>((resolve) => {
    release = resolve;
  });
  let writes = 0;
  await page.route("**/api/v1/sessions/current", async (route) => {
    writes++;
    const operation = route.request().headers()["operation-id"];
    entered();
    await waiting;
    // Hiding the original tab may abort its local wait before this response arrives.
    await route
      .fulfill({
        json: {
          code: "ok",
          completion: { id: operation, count: 1, revokedAt },
        },
      })
      .catch(() => {});
  });
  await page
    .getByRole("button", { name: "确认退出本次登录", exact: true })
    .click();
  await started;
  const other = await context.newPage();
  await other.goto(origin);
  await other.waitForFunction(() => !!window.logoutSessions);
  await choose(other, otherSession);
  release();
  await page.bringToFront();
  await expect(
    page.getByRole("heading", { name: "当前账户已变化", exact: true }),
  ).toBeVisible();
  expect(
    (await other.evaluate(() => window.logoutSessions.read()))?.session,
  ).toEqual(otherSession);
  await expect(page.locator("main")).not.toContainText("退出");
  expect(
    await page.evaluate(
      () => JSON.parse(sessionStorage.getItem("halo.logout")!).outcome,
    ),
  ).toBe("pending");
  expect(writes).toBe(1);
});

for (const outcome of ["pending", "rejected"] as const) {
  test(`another account becoming anonymous conceals the old ${outcome} and its leave dialog`, async ({
    page,
    context,
  }) => {
    let writes = 0;
    await page.route("**/api/v1/sessions/current", async (route) => {
      writes++;
      if (outcome === "pending") await route.abort();
      else
        await route.fulfill({
          status: 403,
          json: { code: "permission_denied" },
        });
    });
    await page
      .getByRole("button", { name: "确认退出本次登录", exact: true })
      .click();
    await expect(
      page.getByRole("heading", {
        name: outcome === "pending" ? "本次退出结果待确认" : "本次退出未受理",
        exact: true,
      }),
    ).toBeVisible();
    const other = await context.newPage();
    await other.goto(origin);
    await other.waitForFunction(() => !!window.logoutSessions);
    await choose(other, otherSession);
    await page.bringToFront();
    await expect(
      page.getByRole("heading", { name: "当前账户已变化", exact: true }),
    ).toBeVisible();
    await other.evaluate(async () => {
      const choice = await window.logoutSessions.read();
      if (!choice?.session) throw new Error("Missing fixture choice");
      await window.logoutSessions.clear({
        id: choice.session.id,
        revision: choice.revision,
      });
    });
    await page.bringToFront();
    await expect(
      page.getByRole("heading", { name: "请登录后继续", exact: true }),
    ).toBeVisible();
    await expect(page.locator("main")).not.toContainText("退出");
    await expect(page.getByRole("dialog")).toHaveCount(0);
    await page.reload();
    await expect(
      page.getByRole("heading", { name: "请登录后继续", exact: true }),
    ).toBeVisible();
    await expect(page.locator("main")).not.toContainText("退出");
    await page.getByRole("link", { name: "返回首页", exact: true }).click();
    await expect(
      page.getByText("Public fixture", { exact: true }),
    ).toBeVisible();
    expect(writes).toBe(1);
  });
}

test("same-account GET preserves the new selection and later anonymous entry conceals the historical receipt", async ({
  page,
  context,
}) => {
  let writes = 0;
  let queried: { operation: string | null; selected: string } | undefined;
  await page.route("**/api/v1/sessions/current", async (route) => {
    writes++;
    await route.abort();
  });
  await page.route("**/api/v1/revocations?*", async (route) => {
    queried = {
      operation: new URL(route.request().url()).searchParams.get(
        "operation_id",
      ),
      selected: route.request().headers()["session-id"],
    };
    await route.fulfill({
      json: {
        code: "ok",
        count: 1,
        revocations: [{ id: session.id, revokedAt }],
        original: { scope: "current", initiatorId: session.id },
      },
    });
  });
  await page
    .getByRole("button", { name: "确认退出本次登录", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "本次退出结果待确认", exact: true }),
  ).toBeVisible();
  const original = await page.evaluate(
    () => JSON.parse(sessionStorage.getItem("halo.logout")!).operation,
  );
  const other = await context.newPage();
  await other.goto(origin);
  await other.waitForFunction(() => !!window.logoutSessions);
  await choose(other, newer);
  await page.bringToFront();
  await page
    .getByRole("button", { name: "查看本次退出结果", exact: true })
    .click();
  await expect(
    page.getByText("当前的新登录仍然有效", { exact: false }),
  ).toBeVisible();
  expect(queried).toEqual({ operation: original, selected: newer.id });
  expect(
    (await other.evaluate(() => window.logoutSessions.read()))?.session,
  ).toEqual(newer);
  await choose(other, otherSession);
  await page.bringToFront();
  await expect(
    page.getByRole("heading", { name: "当前账户已变化", exact: true }),
  ).toBeVisible();
  await expect(page.locator("main")).not.toContainText("退出");
  await other.evaluate(async () => {
    const current = await window.logoutSessions.read();
    if (!current?.session) throw new Error("Missing fixture choice");
    await window.logoutSessions.clear({
      id: current.session.id,
      revision: current.revision,
    });
  });
  await page.bringToFront();
  await expect(
    page.getByRole("heading", { name: "请登录后继续", exact: true }),
  ).toBeVisible();
  await expect(page.locator("main")).not.toContainText("退出");
  expect(writes).toBe(1);
  expect(
    await page.evaluate(
      () => JSON.parse(sessionStorage.getItem("halo.logout")!).outcome,
    ),
  ).toBe("complete");
});
