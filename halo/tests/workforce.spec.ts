import { test, expect, type Page } from "@playwright/test";
import { build } from "vite";
import tailwindcss from "@tailwindcss/vite";
import { resolve } from "node:path";

// Controlled response fixture: real page, controllers, decoder and browser storage.
// Cookie and database behavior are covered by the production-build integration.
const origin = "http://workforce.localhost";
const id = "00000000-0000-4000-8000-000000000001";
const other = "00000000-0000-4000-8000-000000000002";
const name = "阿依古丽·阿不都热依木·买买提明";
const resources = new Map<
  string,
  { body: string | Buffer; contentType: string }
>();
let html: string;
function accountResponse(
  selected = id,
  revision = id,
  displayName = selected === id ? name : "另一位用户",
) {
  return {
    code: "ok",
    account: {
      id: selected,
      personId: selected,
      revisionId: revision,
      name: displayName,
      idNumber: { suffix: "002X" },
      binding: null,
      avatar: {
        revision: id,
        href: `https://fixture.invalid/api/v1/avatars/${"a".repeat(64)}.webp?key_id=7&expires_at=2051222400&signature=${Buffer.alloc(32, 255).toString("base64url")}`,
        expiresAt: "2035-01-01T00:00:00Z",
        mediaType: "image/webp",
        width: 512,
        height: 512,
      },
    },
  };
}
function grant(status = "active") {
  const identity = { personId: id, revisionId: other, name: "示*" };
  return {
    code: "ok",
    grant: {
      id: other,
      subject: identity,
      operator: identity,
      grantedAt: "2026-10-23T00:00:00Z",
      sequence: 1,
      operationId: other,
      status,
    },
  };
}
test.beforeAll(async () => {
  const result = await build({
    configFile: false,
    publicDir: false,
    plugins: [tailwindcss()],
    logLevel: "error",
    define: { __BUILD_ID__: JSON.stringify("workforce-fixture") },
    build: {
      write: false,
      emptyOutDir: false,
      cssCodeSplit: false,
      assetsInlineLimit: 0,
      rolldownOptions: {
        input: resolve("tests/fixtures/workforce.tsx"),
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
async function choose(page: Page, selected = id) {
  await page.evaluate(async (id) => {
    const sessions = window.workforceSessions;
    const attempt = await sessions.begin(crypto.randomUUID());
    if (
      !(await sessions.accept(attempt, {
        id,
        expiresAt: "2035-01-01T00:00:00Z",
      }))
    )
      throw Error("selection changed");
  }, selected);
}
test.beforeEach(async ({ page }) => {
  await page.route(origin + "/**", async (route) => {
    const path = new URL(route.request().url()).pathname;
    if (path === "/build.json") {
      await route.fulfill({ json: { buildId: "workforce-fixture" } });
      return;
    }
    if (path === "/api/v1/accounts/current") {
      const selected = route.request().headers()["session-id"];
      await route.fulfill({ json: accountResponse(selected) });
      return;
    }
    if (path === "/api/v1/workforce/current/grant") {
      await route.fulfill({ json: grant() });
      return;
    }
    if (path === "/api/v1/workforce/current/numbers") {
      await route.fulfill({
        json: {
          code: "ok",
          number: { id, subtype: 902, year: 2026, serial: 5 },
        },
      });
      return;
    }
    if (path === "/api/v1/accounts/current/corrections") {
      await route.fulfill({ json: { code: "ok", count: 0, corrections: [] } });
      return;
    }
    if (path.startsWith("/api/")) {
      await route.abort();
      return;
    }
    await route.fulfill(
      resources.get(path) ?? { body: html, contentType: "text/html" },
    );
  });
  await page.goto(origin);
  await page.waitForFunction(() => !!window.workforceSessions);
  await choose(page);
});
test("account entry, four states, keyboard, responsive long name and read-only memory", async ({
  page,
}, info) => {
  const writes: string[] = [];
  page.on("request", (request) => {
    if (!["GET", "HEAD"].includes(request.method())) writes.push(request.url());
  });
  await page.route("https://fixture.invalid/**", (route) => route.abort());
  await page.goto(origin + "/account");
  await page.getByRole("link", { name: "查看员工服务" }).click();
  await expect(
    page.getByRole("heading", { name: "员工服务已开通" }),
  ).toBeVisible();
  await expect(page.getByText(name, { exact: true })).toBeVisible();
  for (const [status, title] of [
    ["absent", "员工服务未开通"],
    ["active", "员工服务已开通"],
    ["suspended", "员工服务已暂停"],
    ["terminated", "员工服务已结束"],
  ]) {
    await page.route("**/api/v1/workforce/current/grant", (route) =>
      route.fulfill(
        status === "absent"
          ? { status: 404, json: { code: "grant_absent" } }
          : { json: grant(status) },
      ),
    );
    await page.getByRole("button", { name: "刷新服务状态" }).click();
    await expect(page.getByRole("heading", { name: title })).toBeFocused();
    if (status === "active") {
      await expect(page.getByText("92605", { exact: true })).toBeVisible();
      for (const width of [1440, 768, 390, 320])
        for (const colorScheme of ["light", "dark"] as const) {
          await page.setViewportSize({ width, height: 1000 });
          await page.emulateMedia({ colorScheme });
          expect(
            await page.evaluate(
              () => document.documentElement.scrollWidth <= innerWidth,
            ),
          ).toBe(true);
          await page.screenshot({
            path: info.outputPath(`workforce-${width}-${colorScheme}.png`),
            fullPage: true,
          });
        }
      await expect(
        page.getByRole("region", { name: "我的工号" }),
      ).toContainText("实习");
      await expect(
        page.getByRole("button", { name: "刷新工号", exact: true }),
      ).toBeEnabled();
    }
  }
  await page.keyboard.press("Tab");
  await expect(
    page.getByRole("button", { name: "刷新服务状态" }),
  ).toBeFocused();
  expect(writes).toEqual([]);
  const stored = await page.evaluate(async () =>
    JSON.stringify({
      local: { ...localStorage },
      session: { ...sessionStorage },
      selection: await window.workforceSessions.read(),
    }),
  );
  expect(stored).not.toContain(name);
  expect(stored).not.toContain("terminated");
  expect(stored).not.toContain("92605");
  await page.getByRole("link", { name: "返回个人账户" }).click();
  await expect(page.getByRole("heading", { name: "当前已登录" })).toBeVisible();
  await page.goBack();
  await expect(
    page.getByRole("heading", { name: "员工服务已结束" }),
  ).toBeVisible();
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "员工服务已结束" }),
  ).toBeVisible();
});
test("ordinary failure and rate limit preserve independent Current, never absence", async ({
  page,
}) => {
  await page.route("**/api/v1/workforce/current/grant", (route) =>
    route.fulfill({
      status: 429,
      headers: { "Retry-After": "60" },
      json: { code: "rate_limited" },
    }),
  );
  await page.goto(origin + "/workforce");
  await expect(
    page.getByRole("heading", { name: "暂时无法处理，请稍后再试" }),
  ).toBeVisible();
  await expect(page.getByText(name, { exact: true })).toBeVisible();
  await expect(
    page.getByRole("button", { name: /秒后可重新加载/ }),
  ).toBeDisabled();
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect(
    page.getByRole("heading", { name: "暂时无法处理，请稍后再试" }),
  ).toBeVisible();
  await page.route("**/api/v1/workforce/current/grant", (route) =>
    route.fulfill({ status: 503, json: { code: "dependency_unavailable" } }),
  );
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "暂时无法加载员工服务" }),
  ).toBeVisible();
  await expect(page.getByText(name, { exact: true })).toBeVisible();
  await expect(page.getByText("员工服务未开通", { exact: true })).toHaveCount(
    0,
  );
});
for (const status of [401, 403])
  test(`trusted ${status} conceals all restricted content`, async ({
    page,
  }) => {
    await page.route("**/api/v1/workforce/current/grant", (route) =>
      route.fulfill({
        status,
        json: {
          code: status === 401 ? "authentication_invalid" : "permission_denied",
        },
      }),
    );
    await page.goto(origin + "/workforce");
    await expect(
      page.getByRole("heading", {
        name: status === 401 ? "请重新登录" : "无法查看这项内容",
      }),
    ).toBeVisible();
    await expect(page.getByText(name, { exact: true })).toHaveCount(0);
    expect(
      await page.evaluate(
        async () =>
          (await window.workforceSessions.read())?.session?.id ?? null,
      ),
    ).toBe(status === 401 ? null : id);
  });
test("untrusted response requires reload and conceals Current", async ({
  page,
}) => {
  await page.route("**/api/v1/workforce/current/grant", (route) =>
    route.fulfill({ status: 404, json: { code: "target_unavailable" } }),
  );
  await page.goto(origin + "/workforce");
  await expect(
    page.getByRole("heading", { name: "请重新加载页面" }),
  ).toBeVisible();
  await expect(page.getByText(name, { exact: true })).toHaveCount(0);
});
test("lifecycle hides synchronously and another tab supersedes old status", async ({
  context,
  page,
}) => {
  await page.goto(origin + "/workforce");
  await expect(
    page.getByRole("heading", { name: "员工服务已开通" }),
  ).toBeVisible();
  let release!: () => void;
  const blocked = new Promise<void>((done) => {
    release = done;
  });
  let entered!: () => void;
  const waiting = new Promise<void>((done) => {
    entered = done;
  });
  await page.route("**/api/v1/workforce/current/grant", async (route) => {
    entered();
    await blocked;
    await route.fulfill({ json: grant("terminated") });
  });
  await page.getByRole("button", { name: "刷新服务状态" }).click();
  await waiting;
  expect(
    await page.evaluate(() => {
      window.dispatchEvent(new Event("pagehide"));
      return document.querySelector("main")?.textContent?.includes("阿依古丽");
    }),
  ).toBe(false);
  const tab = await context.newPage();
  await tab.route(origin + "/**", (route) =>
    route.fulfill(
      resources.get(new URL(route.request().url()).pathname) ?? {
        body: html,
        contentType: "text/html",
      },
    ),
  );
  await tab.goto(origin);
  await tab.waitForFunction(() => !!window.workforceSessions);
  await choose(tab, other);
  release();
  await page.route("**/api/v1/workforce/current/grant", (route) =>
    route.fulfill({ status: 404, json: { code: "grant_absent" } }),
  );
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect(page.getByText("另一位用户", { exact: true })).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "员工服务未开通" }),
  ).toBeVisible();
  await expect(page.getByText(name, { exact: true })).toHaveCount(0);
  await tab.close();
});

test("number refresh is independent and null never guesses a reason", async ({
  page,
}) => {
  const reads: string[] = [];
  page.on("request", (request) => {
    if (request.url().includes("/api/"))
      reads.push(new URL(request.url()).pathname);
  });
  await page.goto(origin + "/workforce");
  await expect(page.getByText("92605", { exact: true })).toBeVisible();
  await page.route("**/api/v1/workforce/current/numbers", (route) =>
    route.fulfill({ json: { code: "ok", number: null } }),
  );
  reads.length = 0;
  await page.getByRole("button", { name: "刷新工号", exact: true }).focus();
  await page.keyboard.press("Enter");
  await expect(page.getByText("当前没有工号。", { exact: true })).toBeVisible();
  expect(reads).toEqual(["/api/v1/workforce/current/numbers"]);
  await expect(page.getByText(name, { exact: true })).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "员工服务已开通" }),
  ).toBeVisible();
  expect(await page.locator("main").innerText()).not.toMatch(
    /从未|已被撤销|申请工号|查看工号|工牌/,
  );
});

for (const failure of ["503", "offline", "limited"])
  test(`number ${failure} preserves independent identity and service`, async ({
    page,
  }) => {
    await page.clock.install();
    let count = 0;
    await page.route("**/api/v1/workforce/current/numbers", (route) => {
      count++;
      return failure === "offline"
        ? route.abort()
        : route.fulfill({
            status: failure === "limited" ? 429 : 503,
            headers: { "Retry-After": "61" },
            json: {
              code:
                failure === "limited"
                  ? "rate_limited"
                  : "dependency_unavailable",
            },
          });
    });
    await page.goto(origin + "/workforce");
    await expect(
      page.getByText(
        failure === "limited"
          ? "暂时无法处理，请稍后再试。"
          : "暂时无法读取工号，请稍后重试。",
        { exact: true },
      ),
    ).toBeVisible();
    await expect(page.getByText(name, { exact: true })).toBeVisible();
    await expect(
      page.getByRole("heading", { name: "员工服务已开通" }),
    ).toBeVisible();
    await expect(page.getByText("当前没有工号。", { exact: true })).toHaveCount(
      0,
    );
    if (failure === "limited") {
      await expect(
        page.getByRole("button", { name: /秒后可刷新工号/ }),
      ).toBeDisabled();
      await page.evaluate(() => window.dispatchEvent(new Event("focus")));
      await expect(
        page.getByRole("button", { name: /秒后可刷新工号/ }),
      ).toBeDisabled();
      expect(count).toBe(1);
      await page.clock.fastForward(61_001);
      await expect(
        page.getByRole("button", { name: "刷新工号", exact: true }),
      ).toBeEnabled();
    }
  });

for (const bad of [
  { code: "unknown", number: null },
  { code: "ok", number: { id, subtype: 805, year: 2026, serial: 5 } },
  { code: "ok", number: { id, subtype: 902, year: 0, serial: 5 } },
  { code: "ok" },
])
  test(`invalid number response conceals rather than fabricating a number: ${JSON.stringify(bad)}`, async ({
    page,
  }) => {
    await page.route("**/api/v1/workforce/current/numbers", (route) =>
      route.fulfill({ json: bad }),
    );
    await page.goto(origin + "/workforce");
    await expect(
      page.getByRole("heading", { name: "请重新加载页面" }),
    ).toBeVisible();
    await expect(page.getByText(name, { exact: true })).toHaveCount(0);
    await expect(page.getByRole("region", { name: "我的工号" })).toHaveCount(0);
  });

for (const status of [401, 403])
  test(`number ${status} conceals and conditionally clears selection`, async ({
    page,
  }) => {
    await page.route("**/api/v1/workforce/current/numbers", (route) =>
      route.fulfill({
        status,
        json: {
          code: status === 401 ? "authentication_invalid" : "permission_denied",
        },
      }),
    );
    await page.goto(origin + "/workforce");
    await expect(
      page.getByRole("heading", {
        name: status === 401 ? "请重新登录" : "无法查看这项内容",
      }),
    ).toBeVisible();
    await expect(page.getByText(name, { exact: true })).toHaveCount(0);
    expect(
      await page.evaluate(
        async () =>
          (await window.workforceSessions.read())?.session?.id ?? null,
      ),
    ).toBe(status === 401 ? null : id);
  });

test("number timeout and late response cannot restore the card", async ({
  page,
}) => {
  await page.clock.install();
  let release!: () => void;
  const blocked = new Promise<void>((done) => {
    release = done;
  });
  let entered!: () => void;
  const waiting = new Promise<void>((done) => {
    entered = done;
  });
  await page.route("**/api/v1/workforce/current/numbers", async (route) => {
    entered();
    await blocked;
    await route.fulfill({
      json: { code: "ok", number: { id, subtype: 902, year: 2026, serial: 5 } },
    });
  });
  await page.goto(origin + "/workforce");
  await waiting;
  await expect(
    page.getByText("正在读取工号，请稍候。", { exact: true }),
  ).toBeVisible();
  await page.clock.fastForward(35_001);
  await expect(
    page.getByText("暂时无法读取工号，请稍后重试。", { exact: true }),
  ).toBeVisible();
  release();
  await expect(page.getByText("92605", { exact: true })).toHaveCount(0);
});

test("service refresh and lifecycle hide numbers synchronously, including late replies", async ({
  page,
}) => {
  await page.goto(origin + "/workforce");
  await expect(page.getByText("92605", { exact: true })).toBeVisible();
  for (const event of ["pagehide", "visibilitychange"]) {
    expect(
      await page.evaluate((event) => {
        const target = event === "visibilitychange" ? document : window;
        target.dispatchEvent(new Event(event));
        return document.querySelector("main")?.textContent?.includes("92605");
      }, event),
    ).toBe(false);
    await page.evaluate(() => window.dispatchEvent(new Event("focus")));
    await expect(page.getByText("92605", { exact: true })).toBeVisible();
  }
  let release!: () => void;
  const blocked = new Promise<void>((done) => {
    release = done;
  });
  let entered!: () => void;
  const waiting = new Promise<void>((done) => {
    entered = done;
  });
  await page.route("**/api/v1/workforce/current/numbers", async (route) => {
    entered();
    await blocked;
    await route.fulfill({
      json: {
        code: "ok",
        number: { id, subtype: 1000, year: 2026, serial: 1 },
      },
    });
  });
  await page.getByRole("button", { name: "刷新工号", exact: true }).click();
  await waiting;
  await page.route("**/api/v1/workforce/current/grant", (route) =>
    route.fulfill({ json: grant("terminated") }),
  );
  await page.getByRole("button", { name: "刷新服务状态", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "员工服务已结束" }),
  ).toBeVisible();
  release();
  await expect(page.getByText("102601", { exact: true })).toHaveCount(0);
  await expect(page.getByRole("region", { name: "我的工号" })).toHaveCount(0);
});

for (const change of ["account", "selection revision", "real-name revision"])
  test(`late number cannot replace a new ${change}`, async ({ page }) => {
    await page.goto(origin + "/workforce");
    await expect(page.getByText("92605", { exact: true })).toBeVisible();
    let release!: () => void;
    const blocked = new Promise<void>((done) => {
      release = done;
    });
    let entered!: () => void;
    const waiting = new Promise<void>((done) => {
      entered = done;
    });
    let completed = false;
    let reads = 0;
    await page.route("**/api/v1/workforce/current/numbers", async (route) => {
      if (++reads === 1) {
        entered();
        await blocked;
        await route.fulfill({
          json: {
            code: "ok",
            number: { id, subtype: 902, year: 2026, serial: 5 },
          },
        });
        completed = true;
      } else
        await route.fulfill({
          json: {
            code: "ok",
            number: { id: other, subtype: 1000, year: 2026, serial: 1 },
          },
        });
    });
    await page.getByRole("button", { name: "刷新工号", exact: true }).click();
    await waiting;
    if (change === "real-name revision") {
      await page.route("**/api/v1/accounts/current", (route) =>
        route.fulfill({ json: accountResponse(id, other, "实名已更正") }),
      );
      await page.evaluate(() => window.dispatchEvent(new Event("focus")));
    } else {
      await choose(page, change === "account" ? other : id);
      await page.evaluate(() => window.dispatchEvent(new Event("focus")));
    }
    await expect(page.getByText("102601", { exact: true })).toBeVisible();
    release();
    await expect.poll(() => completed).toBe(true);
    await expect(page.getByText("102601", { exact: true })).toBeVisible();
    await expect(page.getByText("92605", { exact: true })).toHaveCount(0);
  });
