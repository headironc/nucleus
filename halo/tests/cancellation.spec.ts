import { test, expect, type Page, type Route } from "@playwright/test";
import { build } from "vite";
import tailwindcss from "@tailwindcss/vite";
import { resolve } from "node:path";

// The real route, controller, protocol and browser storage run against
// controlled HTTP. website.rs owns the real Cookie/Nginx/backend evidence.
const origin = "http://cancellation.localhost";
const id = "56ad73b5-d5b9-4cb7-aa22-7c446b97247e";
const other = "0d3d3fb2-e285-4123-b2be-bee3fe77cd02";
const cancellationId = "f86f7fe2-305e-443c-a1c1-8f10f37ebfc1";
const cancelledAt = "2026-10-25T02:30:00Z";
const longName = "阿依古丽·阿不都热依木·买买提明";
const session = { id, expiresAt: "2035-01-01T00:00:00Z" };
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
const success = {
  code: "ok",
  cancellation: { id: cancellationId, cancelledAt },
};
const resources = new Map<
  string,
  { body: string | Buffer; contentType: string }
>();
let html: string;

test.beforeAll(async () => {
  const output = await build({
    configFile: false,
    publicDir: false,
    plugins: [tailwindcss()],
    logLevel: "error",
    define: { __BUILD_ID__: JSON.stringify("cancellation-fixture") },
    build: {
      write: false,
      emptyOutDir: false,
      cssCodeSplit: false,
      assetsInlineLimit: 0,
      rolldownOptions: {
        input: resolve("tests/fixtures/cancellation.tsx"),
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

async function choose(
  page: Page,
  selected: { id: string; expiresAt: string } = session,
) {
  return page.evaluate(async (selected) => {
    const attempt = await window.cancellationSessions.begin(
      crypto.randomUUID(),
    );
    const accepted = await window.cancellationSessions.accept(
      attempt,
      selected,
    );
    if (!accepted) throw Error("superseded selection");
    return accepted;
  }, selected);
}

function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

async function openEntry(page: Page) {
  await page.goto(origin);
  await page.waitForFunction(() => !!window.cancellationSessions);
  await choose(page);
  await page.goto(origin + "/account/cancellation");
  await expect(
    page.getByRole("heading", { name: longName, exact: true }),
  ).toBeVisible();
}

async function confirm(page: Page) {
  await page.getByRole("button", { name: "注销账户", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "确认注销本人账户？" });
  await expect(dialog).toBeVisible();
  await dialog.getByRole("button", { name: "确认注销" }).click();
}

function countPosts(page: Page) {
  const sent: { headers: Record<string, string>; body: unknown }[] = [];
  page.on("request", (request) => {
    if (
      request.method() === "POST" &&
      new URL(request.url()).pathname ===
        "/api/v1/accounts/current/cancellations"
    )
      sent.push({
        headers: request.headers(),
        body: request.postDataJSON(),
      });
  });
  return sent;
}

async function capture(
  page: Page,
  output: (name: string) => string,
  name: string,
) {
  for (const width of [1440, 768, 390, 320]) {
    for (const colorScheme of ["light", "dark"] as const) {
      await page.setViewportSize({ width, height: 1000 });
      await page.emulateMedia({ colorScheme });
      expect(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= innerWidth,
        ),
      ).toBe(true);
      await page.screenshot({
        path: output(`${name}-${width}-${colorScheme}.png`),
        fullPage: true,
      });
    }
  }
}

test.beforeEach(async ({ context, page }) => {
  await context.route(`${origin}/**`, async (route) => {
    const url = new URL(route.request().url());
    if (url.pathname === "/build.json") {
      await route.fulfill({ json: { buildId: "cancellation-fixture" } });
      return;
    }
    if (url.pathname === "/api/v1/accounts/current") {
      const selected = route.request().headers()["session-id"];
      await route.fulfill({
        json: {
          code: "ok",
          account:
            selected === other
              ? {
                  ...account,
                  id: other,
                  personId: other,
                  name: "另一位用户",
                }
              : account,
        },
      });
      return;
    }
    if (url.pathname === "/api/v1/accounts/current/cancellations") {
      await route.fulfill({ json: success });
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
});

test("account entry leads to a trusted identity and explicit confirmation", async ({
  page,
}, info) => {
  await openEntry(page);
  await expect(
    page.getByText("本人账户 · 身份证尾号", { exact: false }),
  ).toContainText("002X");
  await expect(
    page.getByText("账户不能恢复，当前及所有设备的登录都会结束。"),
  ).toBeVisible();
  await expect(page.getByText("这不是退出登录", { exact: true })).toBeVisible();
  await expect(
    page.getByRole("link", { name: "返回个人账户", exact: true }),
  ).toHaveAttribute("href", "/account");
  await expect(
    page.getByRole("link", { name: "注销账户", exact: true }),
  ).toHaveCount(0);
  await capture(page, (name) => info.outputPath(name), "entry");
  await page.getByRole("button", { name: "注销账户", exact: true }).click();
  await expect(page.getByRole("button", { name: "暂不注销", exact: true })).toBeFocused();
  await capture(page, (name) => info.outputPath(name), "confirmation");
});

test("one exact request produces success, an own-clear refresh and durable private-data limits", async ({
  page,
}, info) => {
  const selection = await (async () => {
    await page.goto(origin);
    await page.waitForFunction(() => !!window.cancellationSessions);
    const chosen = await choose(page);
    await page.goto(origin + "/account/cancellation");
    await expect(
      page.getByRole("heading", { name: longName, exact: true }),
    ).toBeVisible();
    return chosen;
  })();
  const sent = countPosts(page);
  await confirm(page);
  await expect(
    page.getByRole("heading", { name: "账户已注销", level: 1 }),
  ).toBeVisible();
  await expect(
    page.getByText("本次注销已经生效。", { exact: true }),
  ).toBeVisible();
  await expect(page.locator(".cancellation-time time")).toHaveAttribute(
    "datetime",
    cancelledAt,
  );
  expect(sent).toHaveLength(1);
  expect(sent[0].body).toEqual({
    accountId: id,
    personId: id,
    revisionId: id,
  });
  expect(Object.keys(sent[0].body as object).sort()).toEqual([
    "accountId",
    "personId",
    "revisionId",
  ]);
  expect(sent[0].headers["operation-id"]).toMatch(
    /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i,
  );
  expect(sent[0].headers["session-id"]).toBe(id);
  await capture(page, (name) => info.outputPath(name), "success");

  const stored = await page.evaluate(async () => {
    const raw = sessionStorage.getItem("halo.account-cancellation");
    const material = raw ? JSON.parse(raw) : null;
    return {
      raw,
      material,
      selection: await window.cancellationSessions.read(),
      marker: history.state?.haloCancellation ?? null,
    };
  });
  expect(Object.keys(stored.material).sort()).toEqual([
    "clearedRevision",
    "hidden",
    "notice",
    "operation",
    "outcome",
    "phase",
    "result",
    "retryAt",
    "selection",
    "target",
    "version",
  ]);
  expect(stored.material.target).toEqual({
    accountId: id,
    personId: id,
    revisionId: id,
  });
  expect(stored.material.selection).toEqual(selection);
  expect(stored.selection).not.toBeNull();
  expect(stored.material.clearedRevision).toBe(stored.selection!.revision);
  expect(stored.selection).toMatchObject({ session: null, attempt: null });
  expect(stored.marker).toBe("submitted");
  expect(stored.raw).not.toContain(longName);
  expect(stored.raw).not.toContain("002X");
  expect(stored.raw).not.toContain("fixture.invalid");
  expect(stored.raw).not.toContain("avatar");
  expect(stored.raw).not.toContain("cookie");
  expect(stored.raw).not.toContain("token");

  const hrefs = await page
    .locator("a[href]")
    .evaluateAll((links) => links.map((link) => link.getAttribute("href")));
  expect(hrefs).not.toContain("/account");
  expect(hrefs.some((href) => href?.startsWith("/sessions"))).toBe(false);
  expect(hrefs.some((href) => href?.startsWith("/workforce"))).toBe(false);

  await page.reload();
  await expect(
    page.getByRole("heading", { name: "账户已注销", level: 1 }),
  ).toBeVisible();
  expect(sent).toHaveLength(1);

  await page.evaluate(async (session) => {
    const attempt = await window.cancellationSessions.begin(
      crypto.randomUUID(),
    );
    await window.cancellationSessions.accept(attempt, session);
  }, session);
  await expect(
    page.getByRole("heading", { name: "账户状态", level: 1 }),
  ).toBeVisible();
  await expect(page.getByRole("heading", { name: "账户已注销" })).toHaveCount(
    0,
  );
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "账户状态", level: 1 }),
  ).toBeVisible();
  await expect(page.getByRole("heading", { name: "账户已注销" })).toHaveCount(
    0,
  );
  expect(sent).toHaveLength(1);
});

test("a lost response stays unknown across reload and checking login never resubmits", async ({
  page,
}, info) => {
  await openEntry(page);
  const sent = countPosts(page);
  await page.route(`${origin}/api/v1/accounts/current/cancellations`, (route) =>
    route.abort("failed"),
  );
  await confirm(page);
  await expect(
    page.getByRole("heading", { name: "暂时无法确认注销结果" }),
  ).toBeVisible();
  await expect(
    page.getByText("页面不会自动重复提交，也不提供注销结果查询。"),
  ).toBeVisible();
  await expect(
    page.getByText("登录失效不代表注销成功", { exact: true }),
  ).toBeVisible();
  await capture(page, (name) => info.outputPath(name), "unknown");
  let hrefs = await page
    .locator("a[href]")
    .evaluateAll((links) => links.map((link) => link.getAttribute("href")));
  expect(hrefs).not.toContain("/account");
  expect(hrefs.some((href) => href?.startsWith("/sessions"))).toBe(false);
  expect(hrefs).toContain("/");
  expect(sent).toHaveLength(1);

  await page.reload();
  await expect(
    page.getByRole("heading", { name: "暂时无法确认注销结果" }),
  ).toBeVisible();
  expect(sent).toHaveLength(1);
  await expect(
    page.getByRole("button", { name: "检查登录状态" }),
  ).toBeVisible();
  await expect(page.getByRole("button", { name: "确认注销" })).toHaveCount(0);

  await page.getByRole("button", { name: "检查登录状态" }).click();
  await expect(
    page.getByRole("heading", { name: "当前登录仍有效" }),
  ).toBeVisible();
  await expect(page.locator(".current-account")).toContainText(longName);
  await expect(
    page.getByText("不能单独确认注销结果", { exact: true }),
  ).toBeVisible();
  expect(sent).toHaveLength(1);

  await page.route(`${origin}/api/v1/accounts/current`, (route) =>
    route.fulfill({
      status: 401,
      json: { code: "authentication_invalid" },
    }),
  );
  await page.getByRole("button", { name: "重新检查登录状态" }).click();
  await expect(
    page.getByRole("heading", { name: "当前登录已无法使用" }),
  ).toBeVisible();
  await expect(
    page.getByRole("link", { name: "手机号登录", exact: true }),
  ).toHaveAttribute("href", "/authentication/codes");
  await expect(page.getByRole("heading", { name: "账户已注销" })).toHaveCount(
    0,
  );
  expect(sent).toHaveLength(1);
});

const unknownResponses: {
  name: string;
  fulfill: (route: Route) => Promise<void>;
}[] = [
  {
    name: "500",
    fulfill: (route) =>
      route.fulfill({ status: 500, json: { code: "internal_error" } }),
  },
  {
    name: "untrusted success",
    fulfill: (route) =>
      route.fulfill({ json: { code: "ok", cancellation: { id } } }),
  },
  {
    name: "request mismatch",
    fulfill: (route) =>
      route.fulfill({ status: 409, json: { code: "request_mismatch" } }),
  },
];
for (const { name, fulfill } of unknownResponses) {
  test(`${name} stays an unknown outcome without a retry`, async ({ page }) => {
    await openEntry(page);
    const sent = countPosts(page);
    await page.route(
      `${origin}/api/v1/accounts/current/cancellations`,
      fulfill,
    );
    await confirm(page);
    await expect(
      page.getByRole("heading", { name: "暂时无法确认注销结果" }),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: "检查登录状态" }),
    ).toBeVisible();
    await expect(page.getByRole("button", { name: "确认注销" })).toHaveCount(0);
    expect(sent).toHaveLength(1);
    if (name === "request mismatch")
      await expect(
        page.getByText(
          "本次提交与已保存的信息不一致，当前无法确认账户是否已注销。",
        ),
      ).toBeVisible();
  });
}

const refusals: {
  name: string;
  status: number;
  code: string;
  heading: string;
  retry?: boolean;
  login?: boolean;
  restart?: string;
}[] = [
  {
    name: "rate limited",
    status: 429,
    code: "rate_limited",
    heading: "本次注销尚未受理",
    retry: true,
  },
  {
    name: "invalid input",
    status: 422,
    code: "invalid_input",
    heading: "本次注销未能办理",
    restart: "重新查看账户资料",
  },
  {
    name: "authentication",
    status: 401,
    code: "authentication_invalid",
    heading: "当前登录已失效",
    login: true,
  },
  {
    name: "forbidden",
    status: 403,
    code: "permission_denied",
    heading: "本次注销未能办理",
  },
  {
    name: "target changed",
    status: 409,
    code: "target_changed",
    heading: "账户资料已变化",
    restart: "重新查看账户资料",
  },
  {
    name: "dependency",
    status: 503,
    code: "dependency_unavailable",
    heading: "本次注销没有完成",
    restart: "重新查看账户资料",
  },
];
for (const refusal of refusals) {
  test(`${refusal.name} is a definite refusal with its own recovery`, async ({
    page,
  }, info) => {
    await openEntry(page);
    const sent = countPosts(page);
    await page.route(
      `${origin}/api/v1/accounts/current/cancellations`,
      (route) =>
        route.fulfill({
          status: refusal.status,
          headers: refusal.retry ? { "Retry-After": "60" } : {},
          json: { code: refusal.code },
        }),
    );
    await confirm(page);
    await expect(
      page.getByRole("heading", { name: refusal.heading }),
    ).toBeVisible();
    await expect(page.getByRole("heading", { name: "账户已注销" })).toHaveCount(
      0,
    );
    expect(sent).toHaveLength(1);
    if (refusal.retry) {
      await expect(
        page.getByRole("button", { name: /秒后可返回确认页面/ }),
      ).toBeDisabled();
      await capture(page, (name) => info.outputPath(name), "refused-429");
    }
    if (refusal.login)
      await expect(
        page.getByRole("link", { name: "手机号登录", exact: true }),
      ).toHaveAttribute("href", "/authentication/codes");
    if (refusal.restart) {
      await page.getByRole("button", { name: refusal.restart }).click();
      await expect(
        page.getByRole("heading", { name: longName, exact: true }),
      ).toBeVisible();
      await expect(
        page.getByRole("button", { name: "注销账户", exact: true }),
      ).toBeVisible();
    }
    expect(sent).toHaveLength(1);
    const selection = await page.evaluate(() =>
      window.cancellationSessions.read(),
    );
    if (refusal.status === 401) expect(selection?.session).toBeNull();
    else expect(selection?.session?.id).toBe(id);
  });
}

test("a storage readback or history failure sends zero requests", async ({
  page,
}) => {
  await openEntry(page);
  const sent = countPosts(page);
  await page.evaluate(() => {
    const getItem = Storage.prototype.getItem;
    Storage.prototype.getItem = function (key) {
      const value = getItem.call(this, key);
      if (
        Reflect.get(window, "tamperCancellationReadback") === true &&
        key === "halo.account-cancellation"
      )
        return "{}";
      return value;
    };
    Reflect.set(window, "tamperCancellationReadback", true);
  });
  await confirm(page);
  await expect(
    page.getByRole("heading", { level: 1, name: "本次尚未提交注销", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText("注销申请没有发出。", { exact: false }),
  ).toBeVisible();
  expect(sent).toEqual([]);
  await expect(page.getByRole("button", { name: "确认注销" })).toHaveCount(0);
});

test("a history marker failure sends zero requests", async ({ page }) => {
  await openEntry(page);
  const sent = countPosts(page);
  await page.evaluate(() => {
    history.replaceState = () => {
      throw new DOMException("Fixture history refusal", "SecurityError");
    };
  });
  await confirm(page);
  await expect(
    page.getByRole("heading", { level: 1, name: "本次尚未提交注销", exact: true }),
  ).toBeVisible();
  expect(sent).toEqual([]);
});

test("submitting disables duplicates and a bounded timeout becomes unknown", async ({
  page,
}) => {
  await openEntry(page);
  const sent = countPosts(page);
  const started = deferred();
  const release = deferred();
  await page.route(
    `${origin}/api/v1/accounts/current/cancellations`,
    async (route) => {
      started.resolve();
      await release.promise;
      await route.fulfill({ json: success }).catch(() => {});
    },
  );
  await page.clock.install();
  await confirm(page);
  await started.promise;
  await expect(
    page.getByRole("heading", { name: "正在提交注销申请" }),
  ).toBeVisible();
  const busy = page.getByRole("button", { name: "正在注销" });
  await expect(busy).toBeDisabled();
  await busy.click({ force: true });
  expect(sent).toHaveLength(1);
  await page.clock.fastForward(35_001);
  await expect(
    page.getByRole("heading", { name: "暂时无法确认注销结果" }),
  ).toBeVisible();
  release.resolve();
  await expect(
    page.getByRole("heading", { name: "暂时无法确认注销结果" }),
  ).toBeVisible();
  expect(sent).toHaveLength(1);
});

test("a real pagehide closes private dialogs before any asynchronous check", async ({
  page,
}) => {
  await openEntry(page);
  await page.getByRole("button", { name: "注销账户", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "确认注销本人账户？" });
  await expect(dialog).toBeVisible();
  const concealed = await page.evaluate(() => {
    window.dispatchEvent(new Event("pagehide"));
    return document.body.textContent;
  });
  expect(concealed).not.toContain(longName);
  await expect(dialog).toBeHidden();
  await expect(page.locator(".identity-block")).toHaveCount(0);
  await expect(
    page.getByRole("heading", { name: "账户状态", level: 1 }),
  ).toBeVisible();
  await page.evaluate(() => window.dispatchEvent(new Event("pageshow")));
  await expect(
    page.getByRole("heading", { name: longName, exact: true }),
  ).toBeVisible();
});

test("a document mounted in the background performs no current read until visible", async ({
  page,
}) => {
  await openEntry(page);
  const reads: string[] = [];
  page.on("request", (request) => {
    if (new URL(request.url()).pathname === "/api/v1/accounts/current")
      reads.push(request.url());
  });
  await page.addInitScript(() => {
    let hidden = true;
    Object.defineProperty(document, "hidden", {
      get: () => hidden,
      configurable: true,
    });
    Reflect.set(window, "showCancellationFixture", () => {
      hidden = false;
      document.dispatchEvent(new Event("visibilitychange"));
    });
  });
  reads.length = 0;
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "正在检查当前账户" }),
  ).toBeVisible();
  expect(reads).toEqual([]);
  await page.evaluate(() => Reflect.get(window, "showCancellationFixture")());
  await expect(
    page.getByRole("heading", { name: longName, exact: true }),
  ).toBeVisible();
  expect(reads).toHaveLength(1);
});

test("Escape, close and distinct leave warnings restore focus without submitting", async ({
  page,
}) => {
  await openEntry(page);
  const trigger = page.getByRole("button", {
    name: "注销账户",
    exact: true,
  });
  await trigger.click();
  const confirmDialog = page.getByRole("dialog", {
    name: "确认注销本人账户？",
  });
  await expect(confirmDialog).toBeVisible();
  await expect(
    confirmDialog.getByRole("button", { name: "暂不注销" }),
  ).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(confirmDialog).toBeHidden();
  await expect(trigger).toBeFocused();

  const accountLink = page.getByRole("link", {
    name: "返回个人账户",
    exact: true,
  });
  await accountLink.click();
  const leave = page.getByRole("dialog", { name: "尚未提交注销" });
  await expect(leave).toBeVisible();
  await expect(leave.getByRole("button", { name: "留在此页" })).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(leave).toBeHidden();
  await expect(accountLink).toBeFocused();
  await expect(page).toHaveURL(origin + "/account/cancellation");

  await page.route(`${origin}/api/v1/accounts/current/cancellations`, (route) =>
    route.abort("failed"),
  );
  await confirm(page);
  await expect(
    page.getByRole("heading", { name: "暂时无法确认注销结果" }),
  ).toBeVisible();
  const home = page.getByRole("link", { name: "返回首页", exact: true }).last();
  await home.click();
  const pending = page.getByRole("dialog", {
    name: "离开前请留意注销结果",
  });
  await expect(pending).toBeVisible();
  await expect(pending).toContainText(
    "本次注销结果仍未确定。离开不会取消已经发出的申请。",
  );
  await pending.getByRole("button", { name: "留在此页" }).click();
  await expect(pending).toBeHidden();
  await expect(page).toHaveURL(origin + "/account/cancellation");
  await home.click();
  await pending.getByRole("button", { name: "离开页面" }).click();
  await expect(page).toHaveURL(origin + "/");
});

test("a new session or an empty selection permanently hides an unknown result", async ({
  page,
}) => {
  await openEntry(page);
  const sent = countPosts(page);
  await page.route(`${origin}/api/v1/accounts/current/cancellations`, (route) =>
    route.abort("failed"),
  );
  await confirm(page);
  await expect(
    page.getByRole("heading", { name: "暂时无法确认注销结果" }),
  ).toBeVisible();
  await page.evaluate(async (session) => {
    const attempt = await window.cancellationSessions.begin(
      crypto.randomUUID(),
    );
    await window.cancellationSessions.accept(attempt, session);
  }, session);
  await expect(
    page.getByRole("heading", { name: "账户状态", level: 1 }),
  ).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "暂时无法确认注销结果" }),
  ).toHaveCount(0);
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "账户状态", level: 1 }),
  ).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "暂时无法确认注销结果" }),
  ).toHaveCount(0);
  expect(sent).toHaveLength(1);
});

test("a late success cannot clear or reveal another login", async ({
  page,
}) => {
  await openEntry(page);
  const sent = countPosts(page);
  const started = deferred();
  const release = deferred();
  await page.route(
    `${origin}/api/v1/accounts/current/cancellations`,
    async (route) => {
      started.resolve();
      await release.promise;
      await route.fulfill({ json: success }).catch(() => {});
    },
  );
  await confirm(page);
  await started.promise;
  await expect(
    page.getByRole("heading", { name: "正在提交注销申请" }),
  ).toBeVisible();
  await page.evaluate(
    async (session) => {
      const attempt = await window.cancellationSessions.begin(
        crypto.randomUUID(),
      );
      await window.cancellationSessions.accept(attempt, session);
    },
    { ...session, id: other },
  );
  await expect(
    page.getByRole("heading", { name: "账户状态", level: 1 }),
  ).toBeVisible();
  release.resolve();
  await expect(
    page.getByRole("heading", { name: "正在提交注销申请" }),
  ).toHaveCount(0);
  await expect(page.getByRole("heading", { name: "账户已注销" })).toHaveCount(
    0,
  );
  await expect(
    page.getByRole("heading", { name: "暂时无法确认注销结果" }),
  ).toHaveCount(0);
  const selected = await page.evaluate(() =>
    window.cancellationSessions.read(),
  );
  expect(selected?.session?.id).toBe(other);
  expect(sent).toHaveLength(1);
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "账户状态", level: 1 }),
  ).toBeVisible();
  await expect(page.getByRole("heading", { name: "账户已注销" })).toHaveCount(
    0,
  );
});

test("an external empty selection hides an unknown result without reviving it", async ({
  page,
}) => {
  await openEntry(page);
  const sent = countPosts(page);
  await page.route(`${origin}/api/v1/accounts/current/cancellations`, (route) =>
    route.abort("failed"),
  );
  await confirm(page);
  await expect(
    page.getByRole("heading", { name: "暂时无法确认注销结果" }),
  ).toBeVisible();
  await page.evaluate(async () => {
    const selected = await window.cancellationSessions.read();
    await window.cancellationSessions.release({
      id: selected!.session!.id,
      revision: selected!.revision,
    });
  });
  await expect(
    page.getByRole("heading", { name: "账户状态", level: 1 }),
  ).toBeVisible();
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "账户状态", level: 1 }),
  ).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "暂时无法确认注销结果" }),
  ).toHaveCount(0);
  await expect(page.getByRole("heading", { name: "需要登录" })).toHaveCount(0);
  expect(sent).toHaveLength(1);
});
