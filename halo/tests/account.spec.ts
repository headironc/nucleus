import { test, expect, type Page } from "@playwright/test";
import { build } from "vite";
import tailwindcss from "@tailwindcss/vite";
import { resolve } from "node:path";
import { readFile } from "node:fs/promises";

// Real page, protocol and browser storage; controlled HTTP is used only for
// visual states and exact interleavings. website.rs owns real Cookie/backend evidence.
const origin = "http://account.localhost";
const id = "56ad73b5-d5b9-4cb7-aa22-7c446b97247e";
const other = "0d3d3fb2-e285-4123-b2be-bee3fe77cd02";
const session = { id, expiresAt: "2035-01-01T00:00:00Z" };
const longName = "阿依古丽·阿不都热依木·买买提明";
const account = {
  id,
  personId: id,
  revisionId: id,
  name: longName,
  idNumber: { suffix: "002X" },
  binding: null,
  avatar: {
    revision: id,
    href: `https://fixture.invalid/api/v1/avatars/${"a".repeat(64)}.webp?key_id=7&expires_at=2051222400&signature=${Buffer.alloc(32, 255).toString("base64url")}`,
    expiresAt: session.expiresAt,
    mediaType: "image/webp",
    width: 512,
    height: 512,
  },
};
const records = Array.from({ length: 5 }, (_, index) => ({
  id: `00000000-0000-4000-8000-00000000000${index}`,
  before: { name: `${longName}前${index}`, idNumber: { suffix: "0007" } },
  after: { name: `${longName}后${index}`, idNumber: { suffix: "002X" } },
  correctedAt: `2026-10-${23 - index}T01:25:00Z`,
}));
const resources = new Map<
  string,
  { body: string | Buffer; contentType: string }
>();
let html: string;
let avatarBytes: Buffer;
test.beforeAll(async () => {
  avatarBytes = await readFile(resolve("../assets/avatars/0-cat.webp"));
  const output = await build({
    configFile: false,
    publicDir: false,
    plugins: [tailwindcss()],
    logLevel: "error",
    define: { __BUILD_ID__: JSON.stringify("history-fixture") },
    build: {
      write: false,
      emptyOutDir: false,
      cssCodeSplit: false,
      assetsInlineLimit: 0,
      rolldownOptions: {
        input: resolve("tests/fixtures/account.tsx"),
        output: { entryFileNames: "fixture.js" },
      },
    },
  });
  if (Array.isArray(output) || !("output" in output))
    throw Error("Expected one build");
  const styles: string[] = [];
  for (const item of output.output) {
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

async function choose(page: Page, selected = session) {
  await page.evaluate(async (selected) => {
    const attempt = await window.accountSessions.begin(crypto.randomUUID());
    if (!(await window.accountSessions.accept(attempt, selected)))
      throw Error("superseded selection");
  }, selected);
}
function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

async function prepareScreenshot(page: Page) {
  await page.evaluate(async () => {
    scrollTo(0, 0);
    await document.fonts.ready;
    await new Promise<void>((resolve) =>
      requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
    );
  });
}

test.beforeEach(async ({ context, page }) => {
  await page.route("https://fixture.invalid/**", (route) =>
    route.fulfill({ contentType: "image/webp", body: avatarBytes }),
  );
  await context.route(`${origin}/**`, async (route) => {
    const url = new URL(route.request().url());
    if (url.pathname === "/build.json") {
      await route.fulfill({ json: { buildId: "history-fixture" } });
      return;
    }
    if (url.pathname === "/api/v1/accounts/current") {
      const selected = route.request().headers()["session-id"];
      await route.fulfill({
        json: {
          code: "ok",
          account:
            selected === other
              ? { ...account, id: other, personId: other, name: "另一位用户" }
              : account,
        },
      });
      return;
    }
    if (url.pathname === "/api/v1/accounts/current/corrections") {
      const page = Number(url.searchParams.get("page"));
      const size = Number(url.searchParams.get("size"));
      const own = route.request().headers()["session-id"] === id;
      await route.fulfill({
        json: {
          code: "ok",
          count: own ? records.length : 0,
          corrections: own ? records.slice((page - 1) * size, page * size) : [],
        },
      });
      return;
    }
    if (url.pathname.startsWith("/api/")) {
      await route.abort();
      return;
    }
    await route.fulfill(
      resources.get(url.pathname) ?? { body: html, contentType: "text/html" },
    );
  });
  await page.goto(origin);
  await page.waitForFunction(() => !!window.accountSessions);
  await choose(page);
  await page.goto(origin + "/account");
  await expect(page.locator(".correction-list li")).toHaveCount(5);
});

test("own history reads, pagination, refresh, accessibility and responsive long names", async ({
  page,
}, info) => {
  const writes: string[] = [];
  page.on("request", (request) => {
    if (!["GET", "HEAD"].includes(request.method())) writes.push(request.url());
  });
  await expect(
    page.getByRole("link", { name: "退出登录", exact: true }),
  ).toHaveAttribute("href", "/sessions/logout");
  await expect(
    page.getByRole("link", { name: "退出全部登录", exact: true }),
  ).toHaveAttribute("href", "/sessions/logout-all");
  await expect(
    page.getByRole("link", { name: "注销账户", exact: true }),
  ).toHaveAttribute("href", "/account/cancellation");
  await page.getByLabel("每页").selectOption("2");
  await expect(page.locator(".correction-list li")).toHaveCount(2);
  await expect(page.locator(".correction-list li").first()).toContainText(
    records[0].before.name,
  );
  await expect(page.locator(".correction-list li").first()).toContainText(
    "2026年10月23日 09:25",
  );
  await expect(page.getByRole("button", { name: "上一页" })).toBeDisabled();
  await page.getByRole("button", { name: "下一页" }).click();
  await expect(page.locator(".correction-list li").first()).toContainText(
    records[2].before.name,
  );
  await page.getByRole("button", { name: "下一页" }).click();
  await expect(page.locator(".correction-list li")).toHaveCount(1);
  await expect(page.getByRole("button", { name: "下一页" })).toBeDisabled();
  await page.getByRole("button", { name: "刷新记录" }).click();
  await expect(page.locator(".correction-list li").first()).toContainText(
    records[0].before.name,
  );
  await expect(page.locator("#history-title")).toBeFocused();
  await page.keyboard.press("Tab");
  await expect(page.getByRole("button", { name: "刷新记录" })).toBeFocused();
  for (const width of [1440, 768, 390, 320]) {
    for (const colorScheme of ["light", "dark"] as const) {
      await page.setViewportSize({ width, height: 1000 });
      await page.emulateMedia({ colorScheme });
      expect(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= innerWidth,
        ),
      ).toBe(true);
      await prepareScreenshot(page);
      await page.screenshot({
        path: info.outputPath(`history-${width}-${colorScheme}.png`),
        fullPage: true,
      });
    }
  }
  expect(writes).toEqual([]);
  const storage = await page.evaluate(async () => ({
    local: { ...localStorage },
    session: { ...sessionStorage },
    selection: await window.accountSessions.read(),
  }));
  expect(JSON.stringify(storage)).not.toContain(longName);
  expect(JSON.stringify(storage)).not.toContain(records[0].id);
});

test("management permission loading retains trusted facts and waits before showing the entry", async ({
  page,
}, info) => {
  await page.setViewportSize({ width: 320, height: 1000 });
  await page.emulateMedia({ colorScheme: "dark" });
  const entered = deferred();
  const release = deferred();
  await page.route("**/api/v1/accounts/current/role", async (route) => {
    entered.resolve();
    await release.promise;
    await route
      .fulfill({
        json: {
          code: "ok",
          role: {
            id,
            changeId: id,
            kind: "full",
            name: "管理员",
            level: 1000,
            permissions: ["institution.workforce"],
          },
        },
      })
      .catch(() => {});
  });
  try {
    await page.reload();
    await entered.promise;
    await expect(page.locator(".current-summary")).toContainText(longName);
    await expect(
      page.getByText("正在检查管理权限…", { exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("link", { name: "为同事开通员工服务", exact: true }),
    ).toHaveCount(0);
    await prepareScreenshot(page);
    await page.screenshot({
      path: info.outputPath("management-permission-loading.png"),
      fullPage: true,
    });
  } finally {
    release.resolve();
  }
  await expect(
    page.getByRole("link", { name: "为同事开通员工服务", exact: true }),
  ).toHaveAttribute("href", "/workforce/grants/new");
  await expect(
    page.getByText("正在检查管理权限…", { exact: true }),
  ).toHaveCount(0);
});
test("empty history and count-preserving out-of-range pages are distinct", async ({
  page,
}, info) => {
  await page.getByLabel("每页").selectOption("2");
  await expect(page.locator(".correction-list li")).toHaveCount(2);
  await page.route("**/api/v1/accounts/current/corrections?*", (route) =>
    route.fulfill({ json: { code: "ok", count: 1, corrections: [] } }),
  );
  await page.getByRole("button", { name: "下一页" }).click();
  await expect(page.getByText("找不到这一页")).toBeVisible();
  await expect(page.getByText("共 1 条记录", { exact: true })).toBeVisible();
  await prepareScreenshot(page);
  await page.screenshot({
    path: info.outputPath("out-of-range.png"),
    fullPage: true,
  });
  await page.unroute("**/api/v1/accounts/current/corrections?*");
  await page.getByRole("button", { name: "返回第一页" }).click();
  await expect(page.locator(".correction-list li")).toHaveCount(2);
  await page.route("**/api/v1/accounts/current/corrections?*", (route) =>
    route.fulfill({ json: { code: "ok", count: 0, corrections: [] } }),
  );
  await page.getByRole("button", { name: "刷新记录" }).click();
  await expect(
    page.getByText("暂无实名更正记录", { exact: true }),
  ).toBeVisible();
  await prepareScreenshot(page);
  await page.screenshot({ path: info.outputPath("empty.png"), fullPage: true });
});

for (const status of [429, 503])
  test(`history ${status} preserves trustworthy Current and observes Retry-After`, async ({
    page,
  }, info) => {
    let calls = 0;
    await page.route(
      "**/api/v1/accounts/current/corrections?*",
      async (route) => {
        calls++;
        await route.fulfill({
          status,
          headers: { "Retry-After": "60" },
          json: {
            code: status === 429 ? "rate_limited" : "dependency_unavailable",
          },
        });
      },
    );
    await page.getByRole("button", { name: "刷新记录" }).click();
    await expect(
      page.getByRole("heading", {
        name: "实名更正记录暂时无法加载",
      }),
    ).toBeVisible();
    await expect(page.locator(".current-summary")).toContainText(longName);
    await expect(page.locator(".correction-list")).toHaveCount(0);
    await expect(page.getByRole("button", { name: "刷新记录" })).toBeDisabled();
    await page.evaluate(() => window.dispatchEvent(new Event("focus")));
    await expect(
      page.getByRole("button", { name: /秒后可重新加载/ }),
    ).toBeDisabled();
    expect(calls).toBe(1);
    await prepareScreenshot(page);
    await page.screenshot({
      path: info.outputPath(`failure-${status}.png`),
      fullPage: true,
    });
  });

for (const status of [401, 403])
  test(`history ${status} conceals private data and preserves exact selection rules`, async ({
    page,
  }) => {
    await page.route("**/api/v1/accounts/current/corrections?*", (route) =>
      route.fulfill({
        status,
        json: {
          code: status === 401 ? "authentication_invalid" : "permission_denied",
        },
      }),
    );
    await page.getByRole("button", { name: "刷新记录" }).click();
    await expect(
      page.getByRole("heading", {
        name: status === 401 ? "请重新登录" : "目前无法查看个人账户",
      }),
    ).toBeVisible();
    await expect(
      page.locator(".current-summary, .correction-panel"),
    ).toHaveCount(0);
    expect(
      (await page.evaluate(() => window.accountSessions.read()))?.session?.id ??
        null,
    ).toBe(status === 401 ? null : id);
    if (status === 401)
      await expect(
        page.getByRole("link", { name: "手机号登录" }),
      ).toHaveAttribute("href", "/authentication/codes");
  });

test("late history 401 cannot clear another tab's new selection", async ({
  page,
  context,
}) => {
  const started = deferred();
  const release = deferred();
  await page.route(
    "**/api/v1/accounts/current/corrections?*",
    async (route) => {
      if (route.request().headers()["session-id"] !== id) {
        await route.fallback();
        return;
      }
      started.resolve();
      await release.promise;
      await route
        .fulfill({ status: 401, json: { code: "authentication_invalid" } })
        .catch(() => {});
    },
  );
  await page.getByRole("button", { name: "刷新记录" }).click();
  await started.promise;
  const tab = await context.newPage();
  await tab.goto(origin);
  await tab.waitForFunction(() => !!window.accountSessions);
  await choose(tab, { ...session, id: other });
  await expect(page.locator(".current-summary")).toContainText("另一位用户");
  await expect(
    page.getByText("暂无实名更正记录", { exact: true }),
  ).toBeVisible();
  release.resolve();
  await expect
    .poll(
      async () =>
        (await page.evaluate(() => window.accountSessions.read()))?.session?.id,
    )
    .toBe(other);
  await expect(page.locator(".correction-list")).toHaveCount(0);
  await tab.close();
});

test("current revision recheck conceals history until the new identity is reliable", async ({
  page,
}) => {
  const started = deferred();
  const release = deferred();
  await page.route("**/api/v1/accounts/current", async (route) => {
    started.resolve();
    await release.promise;
    await route.fulfill({
      json: {
        code: "ok",
        account: { ...account, revisionId: other, name: "更正后的姓名" },
      },
    });
  });
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await started.promise;
  await expect(page.locator(".current-summary, .correction-panel")).toHaveCount(
    0,
  );
  release.resolve();
  await expect(page.locator(".current-summary")).toContainText("更正后的姓名");
  await expect(page.locator(".correction-list li")).toHaveCount(5);
});

test("pagehide, real navigation/back and background recovery discard private memory", async ({
  page,
  context,
}) => {
  await page.evaluate(() =>
    window.dispatchEvent(
      new PageTransitionEvent("pagehide", { persisted: true }),
    ),
  );
  await expect(page.locator(".current-summary, .correction-panel")).toHaveCount(
    0,
  );
  await page.evaluate(() =>
    window.dispatchEvent(
      new PageTransitionEvent("pageshow", { persisted: true }),
    ),
  );
  await expect(page.locator(".correction-list li")).toHaveCount(5);
  await page
    .getByRole("link", { name: "返回首页", exact: true })
    .last()
    .click();
  await expect(page.locator(".correction-panel")).toHaveCount(0);
  await page.goBack();
  await expect(page.locator(".correction-list li")).toHaveCount(5);
  const cdp = await context.newCDPSession(page);
  await cdp.send("Page.setWebLifecycleState", { state: "frozen" });
  await cdp.send("Page.setWebLifecycleState", { state: "active" });
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect(page.locator(".correction-list li")).toHaveCount(5);
  await cdp.detach();
});

test("a document first mounted in the background waits for visibility before reading private data", async ({
  page,
}) => {
  await page.addInitScript(() => {
    let hidden = true;
    Object.defineProperty(document, "hidden", {
      get: () => hidden,
      configurable: true,
    });
    Reflect.set(window, "showAccountFixture", () => {
      hidden = false;
      document.dispatchEvent(new Event("visibilitychange"));
    });
  });
  const reads: string[] = [];
  page.on("request", (request) => {
    if (new URL(request.url()).pathname.startsWith("/api/v1/accounts/current"))
      reads.push(request.url());
  });
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "正在打开个人账户…" }),
  ).toBeVisible();
  await expect(page.locator(".current-summary, .correction-panel")).toHaveCount(
    0,
  );
  expect(reads).toEqual([]);
  await page.evaluate(() => Reflect.get(window, "showAccountFixture")());
  await expect(page.locator(".correction-list li")).toHaveCount(5);
  await expect
    .poll(() => reads.map((url) => new URL(url).pathname).sort())
    .toEqual([
      "/api/v1/accounts/current",
      "/api/v1/accounts/current/corrections",
      "/api/v1/accounts/current/role",
    ]);
});

test("storage failure and untrusted history close private access", async ({
  page,
}) => {
  await page.evaluate(() => {
    IDBDatabase.prototype.transaction = () => {
      throw Error("blocked fixture");
    };
  });
  await page.getByRole("button", { name: "刷新记录" }).click();
  await expect(
    page.getByRole("heading", { name: "登录信息暂时无法使用" }),
  ).toBeVisible();
  await expect(page.locator(".current-summary, .correction-panel")).toHaveCount(
    0,
  );
  await page.reload();
  await expect(page.locator(".correction-list li")).toHaveCount(5);
  await page.route("**/api/v1/accounts/current/corrections?*", (route) =>
    route.fulfill({
      json: { code: "ok", count: Number.MAX_SAFE_INTEGER + 1, corrections: [] },
    }),
  );
  await page.getByRole("button", { name: "刷新记录" }).click();
  await expect(page.getByRole("heading", { name: "请重新加载" })).toBeVisible();
  await expect(page.locator(".current-summary, .correction-panel")).toHaveCount(
    0,
  );
});
