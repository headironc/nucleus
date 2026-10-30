import {
  test,
  expect,
  type BrowserContext,
  type Page,
} from "@playwright/test";
import { build } from "vite";
import tailwindcss from "@tailwindcss/vite";
import { resolve } from "node:path";
import { readFile } from "node:fs/promises";

// Real page, protocol, controller and browser storage. HTTP is controlled only
// for exact states and interleavings; tests/http/website.rs owns real backend
// and independent database acceptance.
const origin = "http://affiliation.localhost";
const id = "56ad73b5-d5b9-4cb7-aa22-7c446b97247e";
const other = "0d3d3fb2-e285-4123-b2be-bee3fe77cd02";
const session = { id, expiresAt: "2035-01-01T00:00:00Z" };
const otherSession = { id: other, expiresAt: "2035-01-01T00:00:00Z" };
const names = [
  "中共党员",
  "中共预备党员",
  "共青团员",
  "民革会员",
  "民盟盟员",
  "民建会员",
  "民进会员",
  "农工党党员",
  "致公党党员",
  "九三学社社员",
  "台盟盟员",
  "无党派民主人士",
  "群众",
];
const codes = names.map((_, index) => String(index + 1).padStart(2, "0"));
const affiliations = codes.map((value, index) => ({
  value,
  name: names[index],
}));
const record = {
  value: "04",
  createdAt: "2026-10-25T03:04:05+08:00",
};
const longName = "阿卜杜热合曼·阿不都热西提";
const accountFor = (sessionId: string, name: string) => ({
  id: sessionId,
  personId: sessionId,
  revisionId: sessionId,
  name,
  idNumber: { suffix: "002X" },
  binding: null,
  avatar: {
    revision: sessionId,
    href: `https://fixture.invalid/api/v1/avatars/${"a".repeat(64)}.webp?key_id=7&expires_at=2051222400&signature=${Buffer.alloc(32, 255).toString("base64url")}`,
    expiresAt: session.expiresAt,
    mediaType: "image/webp",
    width: 512,
    height: 512,
  },
});

type Reply = {
  status?: number;
  body: unknown;
  contentType?: string;
  headers?: Record<string, string>;
};
type ReadHandler = (sessionId: string) => Reply | Promise<Reply>;
type PutHandler = (
  body: unknown,
  meta: { operation: string; sessionId: string },
) => Reply | Promise<Reply>;
type Control = {
  accountName: string;
  reads: number;
  readSessions: string[];
  puts: { operation: string; sessionId: string; body: unknown }[];
  read: ReadHandler;
  put: PutHandler;
};

function control(overrides: Partial<Control> = {}): Control {
  return {
    accountName: "测试人员",
    reads: 0,
    readSessions: [],
    puts: [],
    read: () => ({ status: 404, body: { code: "affiliation_unset" } }),
    put: () => ({ status: 200, body: { code: "ok" } }),
    ...overrides,
  };
}

const resources = new Map<
  string,
  { body: string | Buffer; contentType: string }
>();
let html: string;
let avatar: Buffer;

test.beforeAll(async () => {
  avatar = await readFile(resolve("../assets/avatars/0-cat.webp"));
  const output = await build({
    configFile: false,
    publicDir: false,
    plugins: [tailwindcss()],
    logLevel: "error",
    define: { __BUILD_ID__: JSON.stringify("affiliation-fixture") },
    build: {
      write: false,
      emptyOutDir: false,
      cssCodeSplit: false,
      assetsInlineLimit: 0,
      rolldownOptions: {
        input: resolve("tests/fixtures/affiliation.tsx"),
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

function fulfill(route: Parameters<Parameters<BrowserContext["route"]>[1]>[0], reply: Reply) {
  return route.fulfill({
    status: reply.status ?? 200,
    headers: {
      "content-type": reply.contentType ?? "application/json",
      ...reply.headers,
    },
    body:
      typeof reply.body === "string"
        ? reply.body
        : JSON.stringify(reply.body),
  });
}

async function install(context: BrowserContext, state: Control) {
  await context.route(`${origin}/**`, async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    try {
      if (url.pathname === "/build.json") {
        await route.fulfill({ json: { buildId: "affiliation-fixture" } });
        return;
      }
      if (url.pathname === "/api/v1/accounts/current") {
        const selected = request.headers()["session-id"] ?? "";
        await fulfill(route, {
          status: 200,
          body: {
            code: "ok",
            account: accountFor(
              selected,
              selected === other ? "另一位用户" : state.accountName,
            ),
          },
        });
        return;
      }
      if (url.pathname === "/api/v1/accounts/current/corrections") {
        await fulfill(route, {
          status: 200,
          body: { code: "ok", count: 0, corrections: [] },
        });
        return;
      }
      if (url.pathname === "/api/v1/affiliations") {
        await fulfill(route, {
          status: 200,
          body: { code: "ok", affiliations },
        });
        return;
      }
      if (url.pathname === "/api/v1/accounts/current/affiliation") {
        const selected = request.headers()["session-id"] ?? "";
        if (request.method() === "GET") {
          state.reads++;
          state.readSessions.push(selected);
          const reply = await state.read(selected);
          await fulfill(route, reply);
          return;
        }
        if (request.method() === "PUT") {
          const body: unknown = JSON.parse(request.postData() ?? "null");
          const operation = request.headers()["operation-id"] ?? "";
          state.puts.push({ operation, sessionId: selected, body });
          const reply = await state.put(body, { operation, sessionId: selected });
          await fulfill(route, reply);
          return;
        }
      }
      if (url.pathname.startsWith("/api/")) {
        await route.abort();
        return;
      }
      await route.fulfill(
        resources.get(url.pathname) ?? { body: html, contentType: "text/html" },
      );
    } catch {
      await route.abort().catch(() => {});
    }
  });
}

async function choose(page: Page, selected = session) {
  await page.evaluate(async (value) => {
    const attempt = await window.accountSessions.begin(crypto.randomUUID());
    if (!(await window.accountSessions.accept(attempt, value)))
      throw Error("superseded selection");
  }, selected);
}

async function start(
  page: Page,
  context: BrowserContext,
  state: Control,
  target = "/account/affiliation",
) {
  await install(context, state);
  await page.goto(origin);
  await page.waitForFunction(() => !!window.accountSessions);
  await choose(page);
  await page.goto(origin + target);
}

async function openForm(page: Page) {
  await expect(
    page.getByRole("heading", { name: "尚未填写政治面貌" }),
  ).toBeVisible();
  await page.getByRole("button", { name: "填写政治面貌" }).click();
  await expect(page.getByRole("heading", { name: "填写政治面貌" })).toBeVisible();
  await expect(page.locator('input[name="affiliation"]')).toHaveCount(13);
  await page.getByRole("radio", { name: "民革会员" }).check();
  await page.getByRole("button", { name: "继续" }).click();
  await expect(
    page.getByRole("heading", { name: "确认填写政治面貌" }),
  ).toBeVisible();
}

function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

async function sensitiveStorage(page: Page) {
  return page.evaluate(() =>
    JSON.stringify({ local: localStorage, session: sessionStorage }),
  );
}

test.beforeEach(async ({ context }) => {
  await context.route("https://fixture.invalid/**", (route) =>
    route.fulfill({ contentType: "image/webp", body: avatar }),
  );
});

test("account entry, exact first fill, success and independent current read", async ({
  page,
  context,
}) => {
  const state = control();
  await start(page, context, state, "/account");
  await expect(
    page.getByRole("link", { name: "政治面貌", exact: true }),
  ).toBeVisible();
  await expect(page.locator(".account-affiliation .field-hint")).toHaveText(
    "进入本人政治面貌页面。未填写时可自愿首次填写；已有值只读。",
  );
  await page.getByRole("link", { name: "政治面貌", exact: true }).click();
  await expect(page).toHaveURL(/\/account\/affiliation$/);
  await expect(page.locator("#page-title")).toHaveText("政治面貌");
  await expect(page.locator("[data-focus]")).toBeFocused();

  await openForm(page);
  await expect(
    page.getByText("提交后，你不能自行修改、撤回或清空。"),
  ).toBeVisible();
  await expect(page.locator(".confirm-choice")).toHaveText("04 民革会员");
  await page.screenshot({ path: "test-results/affiliation-confirm-desktop.png", fullPage: true });

  const gate = deferred();
  state.put = async () => {
    await gate.promise;
    return { status: 200, body: { code: "ok" } };
  };
  await page.getByRole("button", { name: "确认填写" }).click();
  await expect.poll(() => state.puts.length).toBe(1);
  expect(state.puts[0].body).toEqual({ affiliation: "04" });
  expect(state.puts[0].sessionId).toBe(id);
  expect(state.puts[0].operation).toMatch(
    /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i,
  );
  const marker = await page.evaluate(() =>
    sessionStorage.getItem("halo:affiliation"),
  );
  expect(JSON.parse(marker!)).toEqual({ version: 1, session: id });
  expect(marker).not.toContain("04");
  expect(marker).not.toContain("民革");
  expect(await sensitiveStorage(page)).not.toContain("民革会员");
  gate.resolve();
  await expect(
    page.getByRole("heading", { name: "政治面貌已填写" }),
  ).toBeVisible();
  await expect(page.locator("[data-focus]")).toBeFocused();
  await expect
    .poll(() =>
      page.evaluate(() => sessionStorage.getItem("halo:affiliation")),
    )
    .toBeNull();
  expect(await sensitiveStorage(page)).not.toContain("民革会员");
  expect(await sensitiveStorage(page)).not.toContain('"affiliation":"04"');
  expect(new URL(page.url()).search).toBe("");

  state.read = () => ({
    status: 200,
    body: { code: "ok", affiliation: record },
  });
  await page.getByRole("button", { name: "查看当前政治面貌" }).click();
  const current = page.locator(".current-panel");
  await expect(
    current.getByRole("heading", { name: "当前政治面貌" }),
  ).toBeVisible();
  await expect(current).toContainText("04");
  await expect(current).toContainText("民革会员");
  await expect(current).toContainText("2026年10月25日 03:04");
  await page.screenshot({ path: "test-results/affiliation-success-current.png", fullPage: true });
  await expect(
    page.getByRole("heading", { name: "政治面貌已填写" }),
  ).toBeVisible();
});

test("leave confirmation distinguishes unsent choice from a sent request and restores focus", async ({
  page,
  context,
}) => {
  const state = control();
  await start(page, context, state);
  await openForm(page);
  const account = page.getByRole("link", { name: "← 返回个人账户" });
  await account.click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible();
  await expect(dialog.getByRole("heading")).toHaveText("放弃当前选择？");
  await expect(dialog).toContainText("当前选择尚未提交。离开后需要重新选择。");
  await expect(
    dialog.getByRole("button", { name: "留在此页" }),
  ).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(dialog).not.toBeVisible();
  await expect(account).toBeFocused();

  const gate = deferred();
  state.put = async () => {
    await gate.promise;
    return { status: 200, body: { code: "ok" } };
  };
  await page.getByRole("button", { name: "确认填写" }).click();
  await expect.poll(() => state.puts.length).toBe(1);
  const home = page.getByRole("link", { name: "返回首页 ↗" });
  await home.click();
  await expect(dialog.getByRole("heading")).toHaveText(
    "离开前，请留意填写结果",
  );
  await expect(dialog).toContainText("离开不会取消已经发出的填写。");
  await expect(
    dialog.getByRole("button", { name: "留在此页" }),
  ).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(dialog).not.toBeVisible();
  await expect(home).toBeFocused();
  gate.resolve();
  await expect(
    page.getByRole("heading", { name: "政治面貌已填写" }),
  ).toBeVisible();
});

for (const failure of ["disconnected", "unavailable"] as const) {
  test(`an existing record with ${failure} names never claims unset`, async ({ page, context }) => {
    const state = control({
      read: () => ({ body: { code: "ok", affiliation: record } }),
    });
    await install(context, state);
    await context.route(`${origin}/api/v1/affiliations`, async (route) => {
      if (failure === "disconnected") return route.abort("connectionclosed");
      return fulfill(route, {
        status: 503,
        body: { code: "dependency_unavailable" },
        headers: { "Retry-After": "12" },
      });
    });
    await page.goto(origin);
    await page.waitForFunction(() => !!window.accountSessions);
    await choose(page);
    await page.goto(origin + "/account/affiliation");
    await expect(page.getByRole("heading", { name: "当前政治面貌暂时无法显示" })).toBeVisible();
    await expect(page.locator("main")).not.toContainText("尚未填写");
    await expect(page.getByRole("button", { name: /秒后重新读取/ })).toBeDisabled();
    await expect(page.getByRole("button", { name: "填写政治面貌", exact: true })).toHaveCount(0);
    expect(state.puts).toHaveLength(0);
  });
}

for (const currentFailure of ["503", "disconnected"] as const) {
  test(`initial ${currentFailure} read preserves the parallel dictionary cooldown`, async ({ page, context }) => {
    await page.clock.install();
    const state = control({ read: () => ({ status: 503, body: { code: "dependency_unavailable" } }) });
    await install(context, state);
    let dictionaryReads = 0;
    let available = false;
    await context.route(`${origin}/api/v1/affiliations`, async (route) => {
      dictionaryReads++;
      await fulfill(route, available
        ? { body: { code: "ok", affiliations } }
        : { status: 429, body: { code: "rate_limited" }, headers: { "Retry-After": "60" } });
    });
    if (currentFailure === "disconnected") {
      await context.route(`${origin}/api/v1/accounts/current/affiliation`, async (route) => {
        state.reads++;
        if (available) await fulfill(route, { status: 404, body: { code: "affiliation_unset" } });
        else await route.abort("connectionclosed");
      });
    }
    await page.goto(origin);
    await page.waitForFunction(() => !!window.accountSessions);
    await choose(page);
    await page.goto(origin + "/account/affiliation");
    await expect(page.getByRole("heading", { name: "暂时无法读取当前政治面貌" })).toBeVisible();
    const retry = page.getByRole("button", { name: /重新读取/ });
    await page.clock.fastForward(59_000);
    await expect(retry).toBeDisabled();
    expect(state.reads).toBe(1);
    expect(dictionaryReads).toBe(1);
    available = true;
    state.read = () => ({ status: 404, body: { code: "affiliation_unset" } });
    await page.clock.fastForward(2_000);
    await retry.click();
    await expect(page.getByRole("heading", { name: "尚未填写政治面貌" })).toBeVisible();
    expect(state.reads).toBe(2);
    expect(dictionaryReads).toBe(2);
    expect(state.puts).toHaveLength(0);
  });
}

test("native reload warns about a draft and explicit page departure needs only one confirmation", async ({ page, context }) => {
  const state = control();
  await start(page, context, state);
  await openForm(page);
  const dialog = page.waitForEvent("dialog");
  await page.evaluate(() => {
    setTimeout(() => location.reload(), 0);
  });
  const warning = await dialog;
  expect(warning.type()).toBe("beforeunload");
  await warning.dismiss();
  await expect(page.getByRole("heading", { name: "确认填写政治面貌" })).toBeVisible();
  expect(state.puts).toHaveLength(0);
  let redundantWarning = false;
  page.on("dialog", async (prompt) => {
    redundantWarning = true;
    await prompt.dismiss();
  });
  await page.getByRole("link", { name: "返回首页 ↗" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "离开此页", exact: true }).click();
  await expect(page.getByText("Public fixture", { exact: true })).toBeVisible();
  expect(redundantWarning).toBe(false);
});

const rejections = [
  {
    name: "rate limit",
    status: 429,
    code: "rate_limited",
    heading: "请稍后再试",
    retry: true,
  },
  {
    name: "invalid input",
    status: 400,
    code: "invalid_input",
    heading: "请重新核对所选内容",
    retry: false,
  },
  {
    name: "state conflict",
    status: 409,
    code: "state_conflict",
    heading: "当前已有政治面貌记录",
    retry: false,
  },
  {
    name: "request mismatch",
    status: 409,
    code: "request_mismatch",
    heading: "本次填写无法继续",
    retry: false,
  },
  {
    name: "dependency unavailable",
    status: 503,
    code: "dependency_unavailable",
    heading: "暂时无法完成填写",
    retry: true,
  },
  {
    name: "unknown outcome",
    status: 500,
    code: "outcome_unknown",
    heading: "暂时无法确认填写结果",
    retry: true,
  },
] as const;

for (const rejection of rejections) {
  test(`first send rejection: ${rejection.name}`, async ({ page, context }) => {
    const state = control({
      put: () => {
        const reply: Reply = {
          status: rejection.status,
          body: { code: rejection.code },
        };
        if (rejection.status === 429) reply.headers = { "Retry-After": "0" };
        return reply;
      },
    });
    await start(page, context, state);
    await openForm(page);
    await page.getByRole("button", { name: "确认填写" }).click();
    await expect(
      page.getByRole("heading", { name: rejection.heading }),
    ).toBeVisible();
    await expect(page.locator('input[name="affiliation"]')).toHaveCount(0);
    if (rejection.code === "state_conflict") {
      state.read = () => ({
        status: 200,
        body: { code: "ok", affiliation: record },
      });
      await page.getByRole("button", { name: "查看当前政治面貌" }).click();
      await expect(
        page.getByRole("heading", { name: "当前政治面貌" }),
      ).toBeVisible();
      await expect(page.locator(".record-panel")).toContainText("民革会员");
    }
    if (rejection.code === "outcome_unknown") {
      await expect(
        page.getByText(
          "本次填写可能已经完成。离开本页不会取消已经发出的填写。",
        ),
      ).toBeVisible();
      await expect(page.locator(".retry-note")).toContainText(
        "重试本次填写会再次提交上面的选项，可能完成本次填写。请不要另选一项。",
      );
    }
    if (rejection.code === "rate_limited") {
      await expect(
        page.getByRole("button", { name: /秒后可重新提交/ }),
      ).toBeVisible();
      await page.waitForTimeout(5_100);
      state.put = () => ({ status: 200, body: { code: "ok" } });
      await page.getByRole("button", { name: "重试本次填写" }).click();
      await expect(
        page.getByRole("heading", { name: "政治面貌已填写" }),
      ).toBeVisible();
      expect(state.puts).toHaveLength(2);
      expect(state.puts[1].operation).toBe(state.puts[0].operation);
      expect(state.puts[1].body).toEqual(state.puts[0].body);
      expect(state.puts[1].sessionId).toBe(state.puts[0].sessionId);
    }
  });
}

for (const status of [401, 403] as const) {
  test(`first send rejection ${status} conceals without an application code`, async ({
    page,
    context,
  }) => {
    const state = control({
      put: () => ({
        status,
        body: {
          code:
            status === 401 ? "authentication_invalid" : "permission_denied",
        },
      }),
    });
    await start(page, context, state);
    await openForm(page);
    await page.getByRole("button", { name: "确认填写" }).click();
    await expect(
      page.getByRole("heading", {
        name: status === 401 ? "请重新登录" : "暂时无法继续访问",
      }),
    ).toBeVisible();
    await expect(page.locator("body")).not.toContainText(
      status === 401 ? "authentication_invalid" : "permission_denied",
    );
  });
}

test("unknown original retries with the same operation while current reads stay independent", async ({
  page,
  context,
}) => {
  const state = control({
    put: () => ({ status: 500, body: { code: "outcome_unknown" } }),
  });
  await start(page, context, state);
  await openForm(page);
  await page.getByRole("button", { name: "确认填写" }).click();
  await expect(
    page.getByRole("heading", { name: "暂时无法确认填写结果" }),
  ).toBeVisible();
  const marker = await page.evaluate(() =>
    sessionStorage.getItem("halo:affiliation"),
  );
  expect(JSON.parse(marker!)).toEqual({ version: 1, session: id });

  state.read = () => ({ status: 404, body: { code: "affiliation_unset" } });
  await page.getByRole("button", { name: "查看当前政治面貌" }).click();
  await expect(
    page.locator(".current-panel").getByRole("heading", {
      name: "当前尚未填写",
    }),
  ).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "暂时无法确认填写结果" }),
  ).toBeVisible();
  await expect(page.locator('input[name="affiliation"]')).toHaveCount(0);
  await expect(page.locator(".current-panel")).toContainText(
    "不能证明原填写未完成",
  );
  expect(await sensitiveStorage(page)).not.toContain("民革会员");

  state.put = () => ({ status: 200, body: { code: "ok" } });
  await page.getByRole("button", { name: "重试本次填写" }).click();
  await expect(
    page.getByRole("heading", { name: "政治面貌已填写" }),
  ).toBeVisible();
  expect(state.puts).toHaveLength(2);
  expect(state.puts[1].operation).toBe(state.puts[0].operation);
  expect(state.puts[1].body).toEqual({ affiliation: "04" });
  await expect
    .poll(() =>
      page.evaluate(() => sessionStorage.getItem("halo:affiliation")),
    )
    .toBeNull();
});

for (const response of [
  { code: "request_mismatch", status: 409, heading: "本次重试无法继续" },
  { code: "state_conflict", status: 409, heading: "当前已有政治面貌记录" },
  { code: "invalid_input", status: 400, heading: "本次重试未被受理" },
]) {
  test(`terminal retry ${response.code} displays independent loading, failure and record`, async ({ page, context }) => {
    await page.clock.install();
    const state = control({ put: () => ({ status: 500, body: { code: "outcome_unknown" } }) });
    await start(page, context, state);
    await openForm(page);
    await page.getByRole("button", { name: "确认填写" }).click();
    await expect(page.getByRole("heading", { name: "暂时无法确认填写结果" })).toBeVisible();
    await page.clock.fastForward(5_001);
    state.put = () => ({ status: response.status, body: { code: response.code } });
    await page.getByRole("button", { name: "重试本次填写" }).click();
    await expect(page.getByRole("heading", { name: response.heading })).toBeVisible();
    const gate = deferred();
    state.read = async () => {
      await gate.promise;
      return { status: 429, body: { code: "rate_limited" }, headers: { "Retry-After": "60" } };
    };
    const action = page.getByRole("button", { name: "查看当前政治面貌", exact: true });
    await action.click();
    await expect(page.locator(".current-panel").getByRole("heading", { name: "正在读取当前政治面貌" })).toBeVisible();
    await expect(action).toBeDisabled();
    gate.resolve();
    await expect(page.locator(".current-panel").getByRole("heading", { name: "暂时无法读取当前政治面貌" })).toBeVisible();
    await expect(action).toBeDisabled();
    await page.clock.fastForward(59_000);
    await expect(action).toBeDisabled();
    expect(state.reads).toBe(2);
    state.read = () => ({ body: { code: "ok", affiliation: record } });
    await page.clock.fastForward(2_000);
    await expect(action).toBeEnabled();
    await action.click();
    await expect(page.locator(".current-panel")).toContainText("民革会员");
    await expect(page.locator(".current-panel")).toContainText("原结果仍未确认");
    await expect(page.getByRole("heading", { name: response.heading })).toBeVisible();
    expect(state.reads).toBe(3);
    expect(state.puts).toHaveLength(2);
    expect(state.puts[1]).toEqual(state.puts[0]);
  });
}

for (const marker of ["present", "denied"] as const) {
  test(`independent read with ${marker} marker keeps its loading and cooldown disabled`, async ({ page, context }) => {
    await page.clock.install();
    await page.addInitScript(({ marker, id }) => {
      if (marker === "present") {
        sessionStorage.setItem("halo:affiliation", JSON.stringify({ version: 1, session: id }));
      } else {
        const original = Storage.prototype.getItem;
        Storage.prototype.getItem = function (key: string) {
          if (key === "halo:affiliation") throw new DOMException("Unavailable", "SecurityError");
          return original.call(this, key);
        };
      }
    }, { marker, id });
    const gate = deferred();
    const state = control({ read: async () => {
      await gate.promise;
      return { status: 429, body: { code: "rate_limited" }, headers: { "Retry-After": "60" } };
    } });
    await start(page, context, state);
    const action = page.getByRole("button", { name: "查看当前政治面貌", exact: true });
    await action.click();
    await expect(action).toBeDisabled();
    await expect(page.locator(".current-panel")).toContainText("正在读取当前政治面貌");
    gate.resolve();
    await expect(page.locator(".current-panel")).toContainText("暂时无法读取当前政治面貌");
    await page.clock.fastForward(59_000);
    await expect(action).toBeDisabled();
    expect(state.reads).toBe(1);
    state.read = () => ({ body: { code: "ok", affiliation: record } });
    await page.clock.fastForward(2_000);
    await action.click();
    await expect(page.locator(".current-panel")).toContainText("民革会员");
    expect(state.reads).toBe(2);
    expect(state.puts).toHaveLength(0);
  });
}

for (const kind of ["present", "corrupt"] as const) {
  test(`a ${kind} marker with missing original input never opens a fresh form`, async ({
    page,
    context,
  }) => {
    const state = control();
    await install(context, state);
    await page.goto(origin);
    await page.waitForFunction(() => !!window.accountSessions);
    await choose(page);
    await page.evaluate(
      ({ kind, id }) =>
        sessionStorage.setItem(
          "halo:affiliation",
          kind === "present"
            ? JSON.stringify({ version: 1, session: id })
            : "not-json",
        ),
      { kind, id },
    );
    await page.goto(origin + "/account/affiliation");
    await expect(
      page.getByRole("heading", { name: "暂时无法确认填写结果" }),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: "重试本次填写" }),
    ).toHaveCount(0);
    await expect(page.locator('input[name="affiliation"]')).toHaveCount(0);
    expect(state.reads).toBe(0);

    state.read = () => ({
      status: 200,
      body: { code: "ok", affiliation: record },
    });
    await page.getByRole("button", { name: "查看当前政治面貌" }).click();
    await expect(page.locator(".current-panel")).toContainText("民革会员");
    await expect(
      page.getByRole("heading", { name: "暂时无法确认填写结果" }),
    ).toBeVisible();
  });
}

test("a failed later current read cannot downgrade a known success", async ({
  page,
  context,
}) => {
  const state = control();
  await start(page, context, state);
  await openForm(page);
  await page.getByRole("button", { name: "确认填写" }).click();
  await expect(
    page.getByRole("heading", { name: "政治面貌已填写" }),
  ).toBeVisible();
  state.read = () => ({
    status: 503,
    body: { code: "dependency_unavailable" },
  });
  await page.getByRole("button", { name: "查看当前政治面貌" }).click();
  await expect(
    page.locator(".current-panel").getByRole("heading", {
      name: "暂时无法读取当前政治面貌",
    }),
  ).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "政治面貌已填写" }),
  ).toBeVisible();
  await expect(page.locator(".current-panel")).toContainText(
    "已确认的填写结果仍然有效",
  );
});

test("a late initial read cannot cross a pagehide or a parallel account selection", async ({
  page,
  context,
}) => {
  const first = deferred();
  let reads = 0;
  const state = control({
    read: async () => {
      reads++;
      if (reads === 1) {
        await first.promise;
        return { status: 200, body: { code: "ok", affiliation: record } };
      }
      return { status: 404, body: { code: "affiliation_unset" } };
    },
  });
  await start(page, context, state);
  await expect(
    page.getByRole("heading", { name: "正在读取当前政治面貌" }),
  ).toBeVisible();
  await page.evaluate(() => window.dispatchEvent(new Event("pagehide")));
  await expect(
    page.getByRole("heading", { name: "正在读取当前账户" }),
  ).toBeVisible();
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect(
    page.getByRole("heading", { name: "尚未填写政治面貌" }),
  ).toBeVisible();
  first.resolve();
  await page.waitForTimeout(100);
  await expect(
    page.getByRole("heading", { name: "尚未填写政治面貌" }),
  ).toBeVisible();
  await expect(page.locator("body")).not.toContainText("民革会员");

  await page.evaluate(async (selected) => {
    const attempt = await window.accountSessions.begin(crypto.randomUUID());
    if (!(await window.accountSessions.accept(attempt, selected)))
      throw Error("superseded selection");
  }, otherSession);
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect(page.getByText("另一位用户")).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "尚未填写政治面貌" }),
  ).toBeVisible();
  expect(state.readSessions.at(-1)).toBe(other);
});

test("a late write and a missing tab marker cannot reopen a submitted history entry", async ({
  page,
  context,
}) => {
  const gate = deferred();
  const state = control({
    put: async () => {
      await gate.promise;
      return { status: 200, body: { code: "ok" } };
    },
  });
  await start(page, context, state);
  await openForm(page);
  await page.getByRole("button", { name: "确认填写" }).click();
  await expect.poll(() => state.puts.length).toBe(1);
  await page.evaluate(() => sessionStorage.removeItem("halo:affiliation"));
  await page.evaluate(() => window.dispatchEvent(new Event("pagehide")));
  await expect(
    page.getByRole("heading", { name: "正在读取当前账户" }),
  ).toBeVisible();
  gate.resolve();
  await page.waitForTimeout(100);
  await expect(
    page.getByRole("heading", { name: "正在读取当前账户" }),
  ).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "政治面貌已填写" }),
  ).toHaveCount(0);
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect(
    page.getByRole("heading", { name: "暂时无法确认填写结果" }),
  ).toBeVisible();
  await expect(page.locator('input[name="affiliation"]')).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "重试本次填写" }),
  ).toHaveCount(0);
  page.once("dialog", async (dialog) => {
    expect(dialog.type()).toBe("beforeunload");
    await dialog.accept();
  });
  await page.reload();
  await expect(page.getByRole("heading", { name: "暂时无法确认填写结果" })).toBeVisible();
  await page.getByRole("button", { name: "查看当前政治面貌" }).click();
  await expect(page.locator(".current-panel")).toContainText("未填写");
  await expect(page.locator('input[name="affiliation"]')).toHaveCount(0);
  expect(state.puts).toHaveLength(1);
});

test("form layout, keyboard and focus remain usable at all target widths and schemes", async ({
  page,
  context,
}) => {
  const state = control({ accountName: longName });
  await start(page, context, state);
  await openForm(page);
  await expect(page.locator(".person-context strong")).toHaveText(longName);
  await expect(
    page.getByRole("heading", { name: "确认填写政治面貌" }),
  ).toBeVisible();
  await page.getByRole("button", { name: "返回选择" }).click();
  await expect(page.getByRole("radio", { name: "民革会员" })).toBeChecked();
  for (const width of [320, 390, 768, 1440]) {
    for (const scheme of ["light", "dark"] as const) {
      await page.setViewportSize({ width, height: 900 });
      await page.emulateMedia({ colorScheme: scheme });
      await expect(page.locator(".affiliation-option")).toHaveCount(13);
      expect(
        await page.evaluate(() => document.documentElement.scrollWidth),
      ).toBeLessThanOrEqual(width + 1);
      const box = await page.locator(".affiliation-option").first().boundingBox();
      expect(box?.height).toBeGreaterThanOrEqual(44);
      await page.getByRole("heading", { name: "填写政治面貌", exact: true }).focus();
      await page.screenshot({
        path: `test-results/affiliation-form-${width}-${scheme}.png`,
        fullPage: true,
      });
    }
  }

  await page.setViewportSize({ width: 390, height: 900 });
  await page.emulateMedia({ colorScheme: "light" });
  for (const name of names) {
    await page.getByRole("radio", { name, exact: true }).check();
    await expect(page.getByRole("radio", { name, exact: true })).toBeChecked();
  }
  const keyboardChoice = page.getByRole("radio", { name: "民盟盟员" });
  await keyboardChoice.focus();
  await page.keyboard.press("Space");
  await expect(keyboardChoice).toBeChecked();
  await expect(page.locator(".selection-status")).toContainText("民盟盟员");
  await page.getByRole("button", { name: "继续" }).focus();
  await page.keyboard.press("Enter");
  await expect(
    page.getByRole("heading", { name: "确认填写政治面貌" }),
  ).toBeVisible();
  await expect(page.locator(".confirm-choice")).toHaveText("05 民盟盟员");
  await page.screenshot({ path: "test-results/affiliation-confirm-mobile.png", fullPage: true });
});
