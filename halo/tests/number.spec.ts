import { test, expect, type BrowserContext, type Page } from "@playwright/test";
import { build } from "vite";
import tailwindcss from "@tailwindcss/vite";
import { resolve } from "node:path";

// Real page and browser storage; controlled HTTP does not prove backend effects.
const origin = "http://number.localhost";
const id = "56ad73b5-d5b9-4cb7-aa22-7c446b97247e";
const other = "0d3d3fb2-e285-4123-b2be-bee3fe77cd02";
const identity = { personId: id, revisionId: id, name: "测试同事" };
const number = { id: other, subtype: 902, year: 2026, serial: 5 };
const account = {
  id,
  ...identity,
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
};
const resources = new Map<
  string,
  { body: string | Buffer; contentType: string }
>();
let html = "";
test.beforeAll(async () => {
  const output = await build({
    configFile: false,
    publicDir: false,
    plugins: [tailwindcss()],
    logLevel: "error",
    define: { __BUILD_ID__: JSON.stringify("number-fixture") },
    build: {
      write: false,
      emptyOutDir: false,
      cssCodeSplit: false,
      assetsInlineLimit: 0,
      rolldownOptions: {
        input: resolve("tests/fixtures/number.tsx"),
        output: { entryFileNames: "fixture.js" },
      },
    },
  });
  if (Array.isArray(output) || !("output" in output))
    throw Error("Expected one build");
  const styles: string[] = [];
  for (const item of output.output) {
    const contentType =
      item.type === "chunk" || item.fileName.endsWith(".js")
        ? "text/javascript"
        : item.fileName.endsWith(".css")
          ? "text/css"
          : "image/png";
    resources.set("/" + item.fileName, {
      body: item.type === "chunk" ? item.code : Buffer.from(item.source),
      contentType,
    });
    if (contentType === "text/css")
      styles.push(`<link rel="stylesheet" href="/${item.fileName}">`);
  }
  html = `<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><link rel="icon" href="data:,">${styles.join("")}</head><body><div id="fixture"></div><script type="module" src="/fixture.js"></script></body></html>`;
});
type Reply = {
  status: number;
  body: unknown;
  headers?: Record<string, string>;
};
function control() {
  return {
    posts: [] as { operation: string; session: string; input: unknown }[],
    status: "active",
    currentNumber: null as typeof number | null,
    post: async (): Promise<Reply | null> => ({
      status: 201,
      body: { code: "ok", number },
    }),
  };
}
async function install(
  context: BrowserContext,
  state: ReturnType<typeof control>,
) {
  await context.route(`${origin}/**`, async (route) => {
    const request = route.request();
    const path = new URL(request.url()).pathname;
    if (path === "/build.json")
      return route.fulfill({ json: { buildId: "number-fixture" } });
    if (path === "/api/v1/accounts/current") {
      const selected = request.headers()["session-id"];
      return route.fulfill({
        json: {
          code: "ok",
          account:
            selected === other
              ? { ...account, id: other, personId: other, name: "另一位同事" }
              : account,
        },
      });
    }
    if (path === "/api/v1/workforce/current/grant")
      return route.fulfill({
        json: {
          code: "ok",
          grant: {
            id: other,
            subject: identity,
            operator: identity,
            grantedAt: "2026-10-01T00:00:00Z",
            sequence: 1,
            operationId: id,
            status: state.status,
          },
        },
      });
    if (path === "/api/v1/workforce/current/numbers")
      return route.fulfill({
        json: { code: "ok", number: state.currentNumber },
      });
    if (path === "/api/v1/workforce/numbers" && request.method() === "POST") {
      state.posts.push({
        operation: request.headers()["operation-id"],
        session: request.headers()["session-id"],
        input: request.postDataJSON(),
      });
      const reply = await state.post();
      return reply
        ? route.fulfill({
            status: reply.status,
            json: reply.body,
            headers: reply.headers,
          })
        : route.abort();
    }
    if (path.startsWith("/api/")) return route.abort();
    return route.fulfill(
      resources.get(path) ?? { body: html, contentType: "text/html" },
    );
  });
}
async function choose(page: Page, selected = id) {
  await page.evaluate(async (selected) => {
    const attempt = await window.accountSessions.begin(crypto.randomUUID());
    if (
      !(await window.accountSessions.accept(attempt, {
        id: selected,
        expiresAt: "2035-01-01T00:00:00Z",
      }))
    )
      throw Error("selection changed");
  }, selected);
}
async function start(page: Page, context: BrowserContext, state = control()) {
  await install(context, state);
  await page.goto(origin);
  await page.waitForFunction(() => !!window.accountSessions);
  await choose(page);
  await page.goto(origin + "/workforce/number");
  return state;
}
async function capture(page: Page, prefix: string) {
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
        path: test.info().outputPath(`${prefix}-${width}-${colorScheme}.png`),
        fullPage: true,
      });
    }
  }
}
async function confirm(page: Page, beforeSubmit?: () => Promise<void>) {
  await expect(page.getByRole("heading", { name: "申请工号" })).toBeVisible();
  await page.getByLabel("人员类别", { exact: true }).selectOption("902");
  await page.getByRole("button", { name: "下一步", exact: true }).click();
  await expect(page.getByRole("dialog")).toBeVisible();
  await expect(page.getByRole("button", { name: "返回修改" })).toBeFocused();
  await beforeSubmit?.();
  await page.getByRole("button", { name: "确认申请", exact: true }).click();
}
test("no preselection, explicit confirmation and complete fixed number", async ({
  page,
  context,
}) => {
  const state = control();
  let release!: () => void;
  const waiting = new Promise<void>((done) => {
    release = done;
  });
  state.post = async () => {
    await waiting;
    return { status: 201, body: { code: "ok", number } };
  };
  await start(page, context, state);
  await expect(page.getByRole("heading", { name: "申请工号" })).toBeVisible();
  await expect(page.getByLabel("人员类别", { exact: true })).toHaveValue("");
  await expect(page.getByRole("option")).toHaveCount(17);
  await expect(
    page.getByRole("button", { name: "下一步", exact: true }),
  ).toBeDisabled();
  await confirm(page, async () => {
    expect(state.posts).toHaveLength(0);
    await expect(page.getByRole("dialog")).toContainText(
      "工号分配后，不能自行更改人员类别。",
    );
    await capture(page, "confirmation");
  });
  try {
    await expect(
      page.getByRole("heading", { name: "正在办理工号申请", exact: true }),
    ).toBeVisible();
    await capture(page, "busy");
  } finally {
    release();
  }
  await expect(
    page.getByRole("heading", { name: "工号已分配", exact: true }),
  ).toBeVisible();
  await expect(page.getByText("92605", { exact: true })).toBeVisible();
  expect(state.posts).toHaveLength(1);
  expect(state.posts[0].input).toEqual({ subtype: 902 });
  await capture(page, "complete");
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "工号已分配", exact: true }),
  ).toBeVisible();
  expect(state.posts).toHaveLength(1);
});
test("confirmed receipt is shown immediately when persistence fails", async ({
  page,
  context,
}) => {
  await page.addInitScript(() => {
    const original = Storage.prototype.setItem;
    Storage.prototype.setItem = function (key, value) {
      if (
        key === "halo.number-allocation" &&
        JSON.parse(value).outcome === "complete"
      )
        throw new DOMException("Storage unavailable", "QuotaExceededError");
      return original.call(this, key, value);
    };
  });
  const state = await start(page, context);
  await confirm(page);
  await expect(
    page.getByRole("heading", { name: "工号已分配", exact: true }),
  ).toBeVisible();
  await expect(page.getByText("92605", { exact: true })).toBeVisible();
  await expect(page.getByRole("alert")).toHaveText(
    "本页未能保存申请结果。请记下工号，或返回员工服务查看。",
  );
  await expect(
    page.getByRole("button", { name: "查看申请结果", exact: true }),
  ).toHaveCount(0);
  expect(state.posts).toHaveLength(1);
  await capture(page, "complete-unsaved");
});

test("unsaved pending material is never described as a confirmed allocation", async ({
  page,
  context,
}) => {
  await page.addInitScript(() => {
    const original = Storage.prototype.setItem;
    Storage.prototype.setItem = function (key, value) {
      if (key === "halo.number-allocation")
        throw new DOMException("Storage unavailable", "QuotaExceededError");
      return original.call(this, key, value);
    };
  });
  const state = await start(page, context);
  await confirm(page);
  await expect(
    page.getByRole("heading", { name: "申请信息暂时无法使用", exact: true }),
  ).toBeVisible();
  expect(state.posts).toHaveLength(0);
  await capture(page, "storage");
  await page.getByRole("button", { name: "重新检查", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "申请结果尚不确定", exact: true }),
  ).toBeVisible();
  await expect(page.getByText("工号已分配", { exact: false })).toHaveCount(0);
  await expect(page.getByText("92605", { exact: true })).toHaveCount(0);
  expect(state.posts).toHaveLength(0);
});

test("untrusted response requires reload even if its storage readback fails", async ({
  page,
  context,
}) => {
  await page.addInitScript(() => {
    const original = Storage.prototype.getItem;
    let failed = false;
    Storage.prototype.getItem = function (key) {
      const value = original.call(this, key);
      if (
        key === "halo.number-allocation" &&
        value &&
        JSON.parse(value).code === "untrusted" &&
        !failed
      ) {
        failed = true;
        throw new DOMException(
          "Storage temporarily unavailable",
          "SecurityError",
        );
      }
      return value;
    };
  });
  const state = control();
  state.post = async () => ({
    status: 201,
    body: { code: "ok", number: null },
  });
  await start(page, context, state);
  await confirm(page);
  await expect(
    page.getByRole("heading", { name: "请重新加载", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "重新检查", exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "查看申请结果", exact: true }),
  ).toHaveCount(0);
  expect(state.posts).toHaveLength(1);
  await capture(page, "reload");
});

test("lost response reload replays only the same explicit original", async ({
  page,
  context,
}) => {
  await page.clock.setFixedTime(Date.now());
  const state = control();
  let lost = true;
  state.post = async () => {
    if (lost) {
      lost = false;
      return null;
    }
    return { status: 201, body: { code: "ok", number } };
  };
  await start(page, context, state);
  await confirm(page);
  await expect(
    page.getByRole("heading", { name: "申请结果尚不确定" }),
  ).toBeVisible();
  await capture(page, "unknown");
  state.currentNumber = { ...number, serial: 6 };
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "申请结果尚不确定" }),
  ).toBeVisible();
  expect(state.posts).toHaveLength(1);
  await page.clock.setFixedTime(
    (await page.evaluate(() => Date.now())) + 6_000,
  );
  await page.getByRole("button", { name: "查看申请结果" }).click();
  await expect(
    page.getByRole("heading", { name: "工号已分配", exact: true }),
  ).toBeVisible();
  expect(state.posts).toHaveLength(2);
  expect(state.posts[1]).toEqual(state.posts[0]);
  await expect(
    page.getByRole("region", { name: "本次分配的工号" }),
  ).toContainText("92605");
  await expect(page.getByText("92606", { exact: true })).toHaveCount(0);
});
test("mismatch remains closed after reload", async ({ page, context }) => {
  const state = control();
  state.post = async () => ({
    status: 409,
    body: { code: "request_mismatch" },
  });
  await start(page, context, state);
  await confirm(page);
  await expect(
    page.getByRole("heading", { name: "无法继续提交本次申请" }),
  ).toBeVisible();
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "无法继续提交本次申请" }),
  ).toBeVisible();
  await expect(page.getByRole("button", { name: "查看申请结果" })).toHaveCount(
    0,
  );
  expect(state.posts).toHaveLength(1);
  await capture(page, "mismatch");
});
test("account switch conceals original category and result", async ({
  page,
  context,
}) => {
  const state = control();
  state.post = async () => null;
  await start(page, context, state);
  await confirm(page);
  await expect(
    page.getByRole("heading", { name: "申请结果尚不确定" }),
  ).toBeVisible();
  await choose(page, other);
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "当前账户已变化" }),
  ).toBeVisible();
  await expect(page.getByText("测试同事", { exact: true })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "查看申请结果" })).toHaveCount(
    0,
  );
  expect(state.posts).toHaveLength(1);
  await capture(page, "other-account");
});
test("paused service cannot open a first allocation form", async ({
  page,
  context,
}) => {
  const state = control();
  state.status = "suspended";
  await start(page, context, state);
  await expect(
    page.getByRole("heading", { name: "暂时无法申请工号" }),
  ).toBeVisible();
  await expect(page.getByLabel("人员类别", { exact: true })).toHaveCount(0);
  expect(state.posts).toHaveLength(0);
  await capture(page, "ineligible");
});

test("current number closes the application form without an allocation", async ({
  page,
  context,
}) => {
  const state = control();
  state.currentNumber = number;
  await start(page, context, state);
  await expect(
    page.getByRole("heading", { name: "当前已有工号", exact: true }),
  ).toBeVisible();
  await expect(page.getByLabel("人员类别", { exact: true })).toHaveCount(0);
  expect(state.posts).toHaveLength(0);
  await capture(page, "present");
});

test("a previous allocation refusal does not claim a current number exists", async ({
  page,
  context,
}) => {
  const state = control();
  state.post = async () => ({ status: 409, body: { code: "number_present" } });
  await start(page, context, state);
  await confirm(page);
  await expect(
    page.getByRole("heading", { name: "本次申请未完成", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText(
      "你已取得过工号，不能再次申请。可返回员工服务查看当前工号。",
      { exact: true },
    ),
  ).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "当前已有工号", exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "查看申请结果", exact: true }),
  ).toHaveCount(0);
  expect(state.posts).toHaveLength(1);
  await capture(page, "previous-allocation");
});

test("pool exhaustion remains an original refusal and reselecting needs a new confirmation", async ({
  page,
  context,
}) => {
  const state = control();
  state.post = async () =>
    state.posts.length === 1
      ? { status: 409, body: { code: "number_pool_exhausted" } }
      : {
          status: 201,
          body: { code: "ok", number: { ...number, subtype: 901 } },
        };
  await start(page, context, state);
  await confirm(page);
  await expect(
    page.getByRole("heading", { name: "本次申请未完成", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText(
      "本次申请时，所选类别没有可分配的工号。只有人员类别选错时，才重新选择。",
      { exact: true },
    ),
  ).toBeVisible();
  expect(state.posts).toHaveLength(1);
  await capture(page, "pool-exhausted");
  await page.getByRole("button", { name: "重新选择类别", exact: true }).click();
  await expect(page.getByLabel("人员类别", { exact: true })).toHaveValue("");
  expect(state.posts).toHaveLength(1);
  await page.getByLabel("人员类别", { exact: true }).selectOption("901");
  await page.getByRole("button", { name: "下一步", exact: true }).click();
  await expect(page.getByRole("dialog")).toBeVisible();
  expect(state.posts).toHaveLength(1);
  await page.getByRole("button", { name: "确认申请", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "工号已分配", exact: true }),
  ).toBeVisible();
  expect(state.posts).toHaveLength(2);
  expect(state.posts[1].operation).not.toBe(state.posts[0].operation);
  expect(state.posts[1].input).toEqual({ subtype: 901 });
  await expect(page.getByText("见习", { exact: true })).toBeVisible();
});

for (const [status, code] of [
  [403, "permission_denied"],
  [429, "rate_limited"],
  [503, "dependency_unavailable"],
] as const)
  test(`allocation ${status} preserves the uncertain original instead of reopening the form`, async ({
    page,
    context,
  }) => {
    await page.clock.setFixedTime(Date.now());
    const state = control();
    state.post = async () => ({
      status,
      body: { code },
      headers: { "Retry-After": "60" },
    });
    await start(page, context, state);
    await confirm(page);
    const title = status === 403 ? "暂时无法确认申请结果" : "申请结果尚不确定";
    await expect(
      page.getByRole("heading", { name: title, exact: true }),
    ).toBeVisible();
    await expect(page.getByLabel("人员类别", { exact: true })).toHaveCount(0);
    await expect(
      page.getByRole("button", { name: /秒后可查看结果/ }),
    ).toBeDisabled();
    await capture(page, `pending-${status}`);
    await page.reload();
    await expect(
      page.getByRole("heading", { name: title, exact: true }),
    ).toBeVisible();
    expect(state.posts).toHaveLength(1);
  });

test("missing original material never starts a replacement allocation", async ({
  page,
  context,
}) => {
  const state = control();
  state.post = async () => null;
  await start(page, context, state);
  await confirm(page);
  await expect(
    page.getByRole("heading", { name: "申请结果尚不确定", exact: true }),
  ).toBeVisible();
  await page.evaluate(() =>
    sessionStorage.removeItem("halo.number-allocation"),
  );
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "无法查看申请结果", exact: true }),
  ).toBeVisible();
  await expect(page.getByLabel("人员类别", { exact: true })).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "查看申请结果", exact: true }),
  ).toHaveCount(0);
  expect(state.posts).toHaveLength(1);
  await capture(page, "missing");
});

test("initial service read failure remains retryable without an allocation", async ({
  page,
  context,
}) => {
  await page.clock.setFixedTime(Date.now());
  await page.route("**/api/v1/workforce/current/grant", (route) =>
    route.fulfill({ status: 503, json: { code: "dependency_unavailable" } }),
  );
  const state = await start(page, context);
  await expect(
    page.getByRole("heading", { name: "工号信息暂时无法加载", exact: true }),
  ).toBeVisible();
  await expect(page.getByLabel("人员类别", { exact: true })).toHaveCount(0);
  expect(state.posts).toHaveLength(0);
  await capture(page, "initial-unavailable");
  await page.unroute("**/api/v1/workforce/current/grant");
  await page.clock.setFixedTime(
    (await page.evaluate(() => Date.now())) + 6_000,
  );
  await expect(
    page.getByRole("button", { name: "重新加载", exact: true }),
  ).toBeEnabled();
  await page.getByRole("button", { name: "重新加载", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "申请工号", exact: true }),
  ).toBeVisible();
  expect(state.posts).toHaveLength(0);
});

for (const width of [320, 390, 768, 1440])
  for (const colorScheme of ["light", "dark"] as const) {
    test(`layout ${width} ${colorScheme}`, async ({ page, context }) => {
      await page.setViewportSize({ width, height: 900 });
      await page.emulateMedia({ colorScheme });
      await start(page, context);
      await expect(
        page.getByRole("heading", { name: "申请工号" }),
      ).toBeVisible();
      expect(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= window.innerWidth,
        ),
      ).toBe(true);
      await page.screenshot({
        path: test.info().outputPath(`number-${width}-${colorScheme}.png`),
        fullPage: true,
      });
      await page.getByLabel("人员类别", { exact: true }).selectOption("902");
      await page.getByRole("button", { name: "下一步", exact: true }).click();
      await expect(page.getByRole("dialog")).toBeVisible();
      await page.screenshot({
        path: test
          .info()
          .outputPath(`number-dialog-${width}-${colorScheme}.png`),
      });
    });
  }
