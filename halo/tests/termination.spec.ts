import { test, expect, type Page, type Route } from "@playwright/test";
import { build } from "vite";
import tailwindcss from "@tailwindcss/vite";
import { resolve } from "node:path";

// Controlled response fixture: real page, controllers, decoder and storage.
// Backend transaction, Grant state and cookie effects are covered by the real
// combination acceptance, not by these responses.
const origin = "http://termination.localhost";
const sessionA = "019354fd-933c-7e6a-8912-345678901234";
const sessionB = "019354fd-933c-7e6a-8912-345678901235";
const accountA = {
  account: "019354fd-933c-7e6a-8912-345678901236",
  person: "018f3c1a-0000-7000-8000-000000000001",
  name: "Fixture Operator",
};
const accountB = {
  account: "019354fd-933c-7e6a-8912-345678901237",
  person: "018f3c1a-0000-7000-8000-000000000009",
  name: "Other Fixture",
};
const grantA = "018f3c1a-0000-7000-8000-000000000002";
const grantB = "018f3c1a-0000-7000-8000-000000000003";

type Person = typeof accountA;
type Call = {
  operation: string;
  session: string;
  grant: string;
  body: string | null;
};
let sessions: Record<string, Person>;
let service: { status: "active" | "suspended" | "terminated"; grant: string };
let calls: Call[];
let terminatedAt: string;
let terminate: (route: Route, call: Call) => Promise<void>;
const resources = new Map<
  string,
  { body: string | Buffer; contentType: string }
>();
let html: string;

function accountResponse(person: Person) {
  return {
    code: "ok",
    account: {
      id: person.account,
      personId: person.person,
      revisionId: person.person,
      name: person.name,
      idNumber: { suffix: "002X" },
      binding: null,
      avatar: {
        revision: person.account,
        href: `https://fixture.invalid/api/v1/avatars/${"a".repeat(64)}.webp?key_id=7&expires_at=2051222400&signature=${Buffer.alloc(32, 255).toString("base64url")}`,
        expiresAt: "2035-01-01T00:00:00Z",
        mediaType: "image/webp",
        width: 512,
        height: 512,
      },
    },
  };
}

function grantResponse(person: Person) {
  const identity = {
    personId: person.person,
    revisionId: person.person,
    name: "示*",
  };
  return {
    code: "ok",
    grant: {
      id: service.grant,
      subject: identity,
      operator: identity,
      grantedAt: "2026-10-23T00:00:00Z",
      sequence: 1,
      operationId: grantA,
      status: service.status,
    },
  };
}

function terminationResponse(call: Call, person: Person) {
  const identity = {
    personId: person.person,
    revisionId: person.person,
    name: "示*",
  };
  return {
    code: "ok",
    termination: {
      id: call.grant,
      subject: identity,
      operator: identity,
      mode: "self",
      terminatedAt,
      sequence: 2,
      operationId: call.operation,
    },
  };
}

test.beforeAll(async () => {
  const result = await build({
    configFile: false,
    publicDir: false,
    plugins: [tailwindcss()],
    logLevel: "error",
    define: { __BUILD_ID__: JSON.stringify("termination-fixture") },
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

async function choose(page: Page, session: string, person: Person) {
  sessions[session] = person;
  await page.evaluate(async (value) => {
    const sessions = window.workforceSessions;
    const attempt = await sessions.begin(crypto.randomUUID());
    if (
      !(await sessions.accept(attempt, {
        id: value,
        expiresAt: "2035-01-01T00:00:00Z",
      }))
    )
      throw Error("selection changed");
  }, session);
}

test.beforeEach(async ({ page }) => {
  sessions = {};
  calls = [];
  terminatedAt = "2026-10-24T09:20:00Z";
  service = { status: "active", grant: grantA };
  terminate = async (route, call) => {
    service = { status: "terminated", grant: call.grant };
    await route.fulfill({
      status: 201,
      json: terminationResponse(call, sessions[call.session] ?? accountA),
    });
  };
  await page.route(origin + "/**", async (route) => {
    const url = new URL(route.request().url());
    const path = url.pathname;
    if (path === "/build.json") {
      await route.fulfill({ json: { buildId: "termination-fixture" } });
      return;
    }
    if (path === "/api/v1/accounts/current") {
      const person = sessions[route.request().headers()["session-id"]];
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
    if (path === "/api/v1/workforce/current/grant") {
      const person = sessions[route.request().headers()["session-id"]];
      await route.fulfill({ json: grantResponse(person ?? accountA) });
      return;
    }
    const match = /^\/api\/v1\/workforce\/grants\/([^/]+)\/terminations$/.exec(
      path,
    );
    if (match) {
      const request = route.request();
      const call: Call = {
        operation: request.headers()["operation-id"],
        session: request.headers()["session-id"],
        grant: match[1],
        body: request.postData(),
      };
      calls.push(call);
      await terminate(route, call);
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
  await choose(page, sessionA, accountA);
});

async function openEntry(page: Page) {
  await page.goto(origin + "/workforce");
  await expect(
    page.getByRole("heading", { name: "已加入员工服务" }),
  ).toBeVisible();
  await page.getByRole("link", { name: "结束员工服务", exact: true }).click();
  await expect(page).toHaveURL(origin + "/workforce/termination");
  await expect(
    page.getByRole("heading", { name: "结束员工服务", exact: true }),
  ).toBeFocused();
}

async function confirm(page: Page) {
  await page.getByRole("button", { name: "结束员工服务", exact: true }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible();
  await expect(
    page.getByRole("button", { name: "暂不结束", exact: true }),
  ).toBeFocused();
  await page.getByRole("button", { name: "确认结束", exact: true }).click();
}

const material = (page: Page) =>
  page.evaluate(() =>
    JSON.parse(sessionStorage.getItem("halo.workforce-termination") ?? "null"),
  );

test("entry, confirmation focus, empty-body binding, current read and persistence", async ({
  page,
}, info) => {
  await openEntry(page);
  await expect(
    page.getByText(accountA.name, { exact: true }),
  ).toBeVisible();
  await expect(page.getByText("结束后会发生什么")).toBeVisible();
  await expect(
    page
      .getByRole("list")
      .getByText("结束后，将无法申请工号或继续办理工牌。", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("list").getByText("个人账户和已有工号保留。", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "已有申请按当时状态办理" }),
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
      await page.screenshot({
        path: info.outputPath(`termination-entry-${colorScheme}-${width}.png`),
        fullPage: true,
      });
    }
  }
  const trigger = page.getByRole("button", {
    name: "结束员工服务",
    exact: true,
  });
  await trigger.focus();
  await page.keyboard.press("Enter");
  await expect(page.getByRole("dialog")).toBeVisible();
  await expect(
    page.getByRole("button", { name: "暂不结束", exact: true }),
  ).toBeFocused();
  await page.screenshot({
    path: info.outputPath("termination-confirm.png"),
    fullPage: true,
  });
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(trigger).toBeFocused();
  expect(calls).toEqual([]);

  await confirm(page);
  await expect(
    page.getByRole("heading", {
      name: "此前的员工服务已结束",
      exact: true,
    }),
  ).toBeFocused();
  await expect(page.getByText("结束时间：", { exact: false })).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "当前员工服务已结束" }),
  ).toBeVisible();
  expect(await page.evaluate(() => document.body.innerText)).not.toContain(
    "结束结果尚不确定",
  );
  expect(calls).toHaveLength(1);
  expect(calls[0]).toMatchObject({ grant: grantA, session: sessionA });
  expect(calls[0].operation).toMatch(
    /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
  );
  expect(calls[0].body).toBeNull();
  const saved = await material(page);
  expect(saved).toMatchObject({
    version: 1,
    active: null,
    known: [{
      operation: calls[0].operation,
      account: accountA.account,
      person: accountA.person,
      grant: grantA,
      terminatedAt,
    }],
  });
  const stored = await page.evaluate(() =>
    JSON.stringify({ ...sessionStorage }),
  );
  expect(stored).not.toContain(accountA.name);
  for (const colorScheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme });
    for (const width of [1440, 768, 390, 320]) {
      await page.setViewportSize({ width, height: 1000 });
      expect(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= innerWidth,
        ),
      ).toBe(true);
      await page.screenshot({
        path: info.outputPath(
          `termination-complete-${colorScheme}-${width}.png`,
        ),
        fullPage: true,
      });
    }
  }
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "此前的员工服务已结束" }),
  ).toBeVisible();
  expect(calls).toHaveLength(1);
});

for (const failure of ["throw", "silent"] as const) {
  test(`a pre-send ${failure} save sends no request and survives reload`, async ({
    page,
  }, info) => {
    await openEntry(page);
    await page.evaluate((failure) => {
      const set = Storage.prototype.setItem;
      Storage.prototype.setItem = function (key, value) {
        if (this === sessionStorage && key === "halo.workforce-termination") {
          if (failure === "throw")
            throw new DOMException("Fixture quota", "QuotaExceededError");
          return;
        }
        set.call(this, key, value);
      };
    }, failure);
    await confirm(page);
    await expect(
      page.getByRole("heading", {
        name: "本次结束申请尚未提交",
        exact: true,
      }),
    ).toBeFocused();
    expect(calls).toEqual([]);
    expect(await material(page)).toBeNull();
    await page.setViewportSize({ width: 320, height: 1000 });
    await page.emulateMedia({ colorScheme: "dark" });
    await page.screenshot({
      path: info.outputPath("unsubmitted-320-dark.png"),
      fullPage: true,
    });
    await page.reload();
    await expect(
      page.getByRole("heading", { name: "结束员工服务", exact: true }),
    ).toBeVisible();
    expect(calls).toEqual([]);
  });
}

test("a first 429 is definitely not accepted and returns to confirmation after the wait", async ({
  page,
}) => {
  await page.clock.install();
  terminate = async (route) =>
    route.fulfill({
      status: 429,
      headers: { "Retry-After": "60" },
      json: { code: "rate_limited" },
    });
  await openEntry(page);
  await confirm(page);
  await expect(
    page.getByRole("heading", {
      name: "本次结束申请未完成",
      exact: true,
    }),
  ).toBeFocused();
  const back = page.getByRole("button", { name: /秒后可返回确认页面/ });
  await expect(back).toBeDisabled();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  expect(calls).toHaveLength(1);
  await page.clock.fastForward(61_001);
  await expect(
    page.getByRole("button", { name: "返回确认页面", exact: true }),
  ).toBeEnabled();
  await page
    .getByRole("button", { name: "返回确认页面", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "结束员工服务", exact: true }),
  ).toBeVisible();
  terminate = async (route, call) => {
    service = { status: "terminated", grant: call.grant };
    await route.fulfill({
      status: 201,
      json: terminationResponse(call, accountA),
    });
  };
  await confirm(page);
  await expect(
    page.getByRole("heading", { name: "此前的员工服务已结束" }),
  ).toBeVisible();
  expect(calls).toHaveLength(2);
  expect(calls[1].operation).not.toBe(calls[0].operation);
  expect(calls[1].grant).toBe(grantA);
});

test("an unknown original replays explicitly with the same operation, target and empty body", async ({
  page,
}, info) => {
  let count = 0;
  terminate = async (route) => {
    count++;
    if (count === 1) await route.abort("connectionclosed");
    else {
      service = { status: "terminated", grant: grantA };
      await route.fulfill({
        status: 201,
        json: terminationResponse(calls[0], accountA),
      });
    }
  };
  await openEntry(page);
  await confirm(page);
  await expect(
    page.getByRole("heading", {
      name: "结束结果尚不确定",
      exact: true,
    }),
  ).toBeFocused();
  await expect(
    page.getByRole("button", { name: /秒后可查看结束结果/ }),
  ).toBeDisabled();
  await page.setViewportSize({ width: 320, height: 1000 });
  await page.emulateMedia({ colorScheme: "dark" });
  await page.screenshot({
    path: info.outputPath("unknown-320-dark.png"),
    fullPage: true,
  });
  await page.getByRole("link", { name: "打开个人账户", exact: true }).click();
  await expect(page.getByRole("dialog", { name: "结束结果仍未确认" })).toBeVisible();
  await page.screenshot({
    path: info.outputPath("leave-unknown-320-dark.png"),
    fullPage: true,
  });
  await page.getByRole("button", { name: "留在此页", exact: true }).click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  expect(calls).toHaveLength(1);
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "结束结果尚不确定" }),
  ).toBeVisible();
  expect(calls).toHaveLength(1);
  await page.waitForTimeout(5_100);
  await page.getByRole("button", { name: "查看结束结果", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "此前的员工服务已结束" }),
  ).toBeVisible();
  expect(calls).toHaveLength(2);
  expect(calls[1]).toMatchObject({
    operation: calls[0].operation,
    grant: calls[0].grant,
    session: sessionA,
    body: null,
  });
});

test("request_mismatch blocks further submission across reloads", async ({
  page,
}) => {
  let count = 0;
  terminate = async (route) => {
    count++;
    if (count === 1) await route.abort("connectionclosed");
    else
      await route.fulfill({
        status: 409,
        json: { code: "request_mismatch" },
      });
  };
  await openEntry(page);
  await confirm(page);
  await page.waitForTimeout(5_100);
  await page.getByRole("button", { name: "查看结束结果", exact: true }).click();
  await expect(
    page.getByRole("heading", {
      name: "结束结果尚不确定",
      exact: true,
    }),
  ).toBeFocused();
  await expect(page.locator("main")).toContainText("原结束结果仍无法确认");
  expect(await material(page)).toMatchObject({
    active: { outcome: "pending", notice: "mismatch" },
  });
  expect(calls).toHaveLength(2);
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "结束结果尚不确定" }),
  ).toBeVisible();
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect(
    page.getByRole("heading", { name: "结束结果尚不确定" }),
  ).toBeVisible();
  expect(calls).toHaveLength(2);
});

test("an expired original login keeps the unknown and offers the fixed return", async ({
  page,
}) => {
  terminate = async (route) => route.abort("connectionclosed");
  await openEntry(page);
  await confirm(page);
  await expect(
    page.getByRole("heading", { name: "结束结果尚不确定" }),
  ).toBeVisible();
  delete sessions[sessionA];
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect(
    page.getByRole("heading", { name: "请登录后继续", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("link", { name: "手机号登录", exact: true }),
  ).toHaveAttribute("href", "/authentication/codes?return=service-termination");
  expect(await material(page)).toMatchObject({
    active: { outcome: "pending", sent: true },
  });
  expect(calls).toHaveLength(1);
});

test("a same-account new session replays; another account conceals the original", async ({
  page,
  context,
}, info) => {
  terminate = async (route) => route.abort("connectionclosed");
  await openEntry(page);
  await confirm(page);
  await expect(
    page.getByRole("heading", { name: "结束结果尚不确定" }),
  ).toBeVisible();
  const tab = await context.newPage();
  await tab.route(origin + "/**", async (route) => {
    const path = new URL(route.request().url()).pathname;
    if (path === "/build.json") {
      await route.fulfill({ json: { buildId: "termination-fixture" } });
      return;
    }
    await route.fulfill(
      resources.get(path) ?? { body: html, contentType: "text/html" },
    );
  });
  await tab.goto(origin);
  await tab.waitForFunction(() => !!window.workforceSessions);
  await choose(tab, sessionB, accountA);
  await page.bringToFront();
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await page.waitForTimeout(5_100);
  await page.getByRole("button", { name: "查看结束结果", exact: true }).click();
  await expect
    .poll(() => calls.length)
    .toBe(2);
  expect(calls[1]).toMatchObject({
    operation: calls[0].operation,
    grant: grantA,
    session: sessionB,
    body: null,
  });
  await expect(
    page.getByRole("heading", { name: "结束结果尚不确定" }),
  ).toBeVisible();
  expect(await material(page)).toMatchObject({
    active: {
      operation: calls[0].operation,
      grant: grantA,
      outcome: "pending",
      notice: "unavailable",
    },
  });

  await choose(tab, sessionB, accountB);
  await page.bringToFront();
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect(
    page.getByRole("heading", { name: "当前账户已变化", exact: true }),
  ).toBeVisible();
  await expect(page.locator("main")).not.toContainText(accountA.name);
  expect(calls).toHaveLength(2);
  await page.setViewportSize({ width: 320, height: 1000 });
  await page.emulateMedia({ colorScheme: "light" });
  await page.screenshot({
    path: info.outputPath("changed-account-320-light.png"),
    fullPage: true,
  });
  await tab.close();
});

test("lifecycle hiding rejects a late completion and revives the unknown original", async ({
  page,
}) => {
  let release!: () => void;
  const held = new Promise<void>((done) => {
    release = done;
  });
  let entered!: () => void;
  const waiting = new Promise<void>((done) => {
    entered = done;
  });
  let delivered!: () => void;
  const finished = new Promise<void>((done) => {
    delivered = done;
  });
  terminate = async (route, call) => {
    entered();
    await held;
    service = { status: "terminated", grant: call.grant };
    await route.fulfill({
      status: 201,
      json: terminationResponse(call, accountA),
    });
    delivered();
  };
  await openEntry(page);
  await confirm(page);
  await waiting;
  expect(
    await page.evaluate(() => {
      window.dispatchEvent(new Event("pagehide"));
      return document.querySelector("main")?.textContent?.includes("Fixture");
    }),
  ).toBe(false);
  release();
  await finished;
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect(
    page.getByRole("heading", { name: "结束结果尚不确定" }),
  ).toBeVisible();
  expect(await material(page)).toMatchObject({
    known: [],
    active: { outcome: "pending", sent: true },
  });
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect(
    page.getByRole("heading", { name: "结束结果尚不确定" }),
  ).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "当前员工服务已结束" }),
  ).toBeVisible();
  expect(calls).toHaveLength(1);
  const original = calls[0];
  await expect(page.getByRole("button", { name: "查看结束结果", exact: true })).toBeEnabled();
  await page.getByRole("button", { name: "查看结束结果", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "此前的员工服务已结束" }),
  ).toBeVisible();
  expect(calls).toHaveLength(2);
  expect(calls[1]).toEqual(original);
});

test("a terminal save failure keeps the seen success and never fabricates a saved one", async ({
  page,
}, info) => {
  await openEntry(page);
  await page.evaluate(() => {
    const set = Storage.prototype.setItem;
    Storage.prototype.setItem = function (key, value) {
      if (
        this === sessionStorage &&
        key === "halo.workforce-termination" &&
        String(value).includes("terminatedAt")
      )
        throw new DOMException("Fixture terminal quota", "QuotaExceededError");
      set.call(this, key, value);
    };
  });
  await confirm(page);
  await expect(
    page.getByRole("heading", { name: "此前的员工服务已结束" }),
  ).toBeVisible();
  await expect(page.getByText("本页未能保存结束结果")).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "当前员工服务已结束" }),
  ).toBeVisible();
  expect(await material(page)).toMatchObject({
    known: [],
    active: { outcome: "pending", sent: true },
  });
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect(
    page.getByRole("heading", { name: "此前的员工服务已结束" }),
  ).toBeVisible();
  await expect(page.getByText("本页未能保存结束结果")).toBeVisible();
  expect(calls).toHaveLength(1);
  await page.setViewportSize({ width: 320, height: 1000 });
  await page.emulateMedia({ colorScheme: "dark" });
  await page.screenshot({
    path: info.outputPath("complete-unsaved-320-dark.png"),
    fullPage: true,
  });
});

test("a corrupt record shows no result and offers only the current service", async ({
  page,
}) => {
  await page.evaluate(() =>
    sessionStorage.setItem("halo.workforce-termination", "{"),
  );
  await page.goto(origin + "/workforce/termination");
  await expect(
    page.getByRole("heading", {
      name: "结束结果尚不确定",
      exact: true,
    }),
  ).toBeVisible();
  await expect(page.locator("main")).toContainText("本页无法查看原结束结果");
  await expect(
    page.getByRole("link", { name: "查看当前员工服务" }),
  ).toBeVisible();
  expect(calls).toEqual([]);
});

test("a rejoined Grant is read independently and only an explicit new confirmation ends it", async ({
  page,
}, info) => {
  await openEntry(page);
  await confirm(page);
  await expect(
    page.getByRole("heading", { name: "此前的员工服务已结束" }),
  ).toBeVisible();
  service = { status: "active", grant: grantB };
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect(
    page.getByRole("heading", { name: "当前员工服务已重新开通" }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "结束当前员工服务", exact: true }),
  ).toBeVisible();
  expect(calls).toHaveLength(1);
  await page.setViewportSize({ width: 320, height: 1000 });
  await page.emulateMedia({ colorScheme: "dark" });
  await page.screenshot({
    path: info.outputPath("complete-rejoined-320-dark.png"),
    fullPage: true,
  });
  await page
    .getByRole("button", { name: "结束当前员工服务", exact: true })
    .click();
  await expect(page.getByRole("dialog")).toBeVisible();
  await expect(
    page.getByRole("button", { name: "暂不结束", exact: true }),
  ).toBeFocused();
  await page.getByRole("button", { name: "确认结束", exact: true }).click();
  await expect
    .poll(() => calls.length)
    .toBe(2);
  expect(calls[1].grant).toBe(grantB);
  expect(calls[1].operation).not.toBe(calls[0].operation);
  expect(calls[1].body).toBeNull();
  await expect(
    page.getByRole("heading", { name: "此前的员工服务已结束" }),
  ).toBeVisible();
  expect(await material(page)).toMatchObject({
    known: [{ grant: grantB, operation: calls[1].operation }],
    active: null,
  });
});

for (const path of ["/api/v1/accounts/current", "/api/v1/workforce/current/grant"]) {
  test(`an untrusted ${path} read exposes a working reload action for the pending original`, async ({ page }) => {
    terminate = async (route) => route.abort("connectionclosed");
    await openEntry(page);
    await confirm(page);
    await expect(page.getByRole("heading", { name: "结束结果尚不确定" })).toBeVisible();
    const untrusted = async (route: Route) => route.fulfill({ json: { code: "ok" } });
    await page.route(origin + path, untrusted);
    await page.evaluate(() => window.dispatchEvent(new Event("focus")));
    const reload = page.getByRole("button", { name: "重新加载", exact: true });
    await expect(reload).toBeVisible();
    expect(calls).toHaveLength(1);
    await page.unroute(origin + path, untrusted);
    page.on("dialog", (dialog) => dialog.accept());
    await reload.click();
    await expect(page.getByRole("heading", { name: "结束结果尚不确定" })).toBeVisible();
    expect(calls).toHaveLength(1);
    expect(await material(page)).toMatchObject({ active: { operation: calls[0].operation, sent: true } });
  });
}

test("completed results remain account-scoped while another account ends its own service", async ({ page }) => {
  await openEntry(page);
  await confirm(page);
  await expect(page.getByRole("heading", { name: "此前的员工服务已结束" })).toBeVisible();
  service = { status: "suspended", grant: grantB };
  await choose(page, sessionB, accountB);
  await page.goto(origin + "/workforce/termination");
  await expect(page.getByRole("heading", { name: "结束员工服务", exact: true })).toBeVisible();
  await expect(page.locator("main")).not.toContainText(accountA.name);
  expect(calls).toHaveLength(1);
  await confirm(page);
  await expect(page.getByRole("heading", { name: "此前的员工服务已结束" })).toBeVisible();
  expect(calls).toHaveLength(2);
  expect(calls[1]).toMatchObject({ grant: grantB, session: sessionB, body: null });
  expect(calls[1].operation).not.toBe(calls[0].operation);
  service = { status: "terminated", grant: grantA };
  await choose(page, sessionA, accountA);
  await page.reload();
  await expect(page.getByRole("heading", { name: "此前的员工服务已结束" })).toBeVisible();
  await expect(page.locator("main")).not.toContainText(accountB.name);
  expect(await material(page)).toMatchObject({ known: [
    { account: accountA.account, grant: grantA, operation: calls[0].operation },
    { account: accountB.account, grant: grantB, operation: calls[1].operation },
  ], active: null });
  expect(calls).toHaveLength(2);
});

test("another account can confirm its own termination after a first-attempt 429", async ({ page }) => {
  terminate = async (route) => route.fulfill({
    status: 429, headers: { "Retry-After": "60" }, json: { code: "rate_limited" },
  });
  await openEntry(page);
  await confirm(page);
  await expect.poll(async () => (await material(page))?.active?.outcome).toBe("rejected");
  const rejected = await material(page);
  service = { status: "suspended", grant: grantB };
  await choose(page, sessionB, accountB);
  await page.reload();
  await expect(page.getByRole("heading", { name: "结束员工服务", exact: true })).toBeVisible();
  await expect(page.locator("main")).not.toContainText(accountA.name);
  expect(await material(page)).toEqual(rejected);
  expect(calls).toHaveLength(1);
  terminate = async (route) => route.fulfill({ status: 201, json: terminationResponse(calls[1], accountB) });
  await confirm(page);
  await expect(page.getByRole("heading", { name: "此前的员工服务已结束" })).toBeVisible();
  expect(calls).toHaveLength(2);
  expect(calls[1]).toMatchObject({ grant: grantB, session: sessionB, body: null });
  expect(calls[1].operation).not.toBe(calls[0].operation);
});

test("a definite conflict offers confirmation for a later Grant without submitting on refresh", async ({ page }) => {
  terminate = async (route) => route.fulfill({ status: 409, json: { code: "state_conflict" } });
  await openEntry(page);
  await confirm(page);
  await expect(page.getByRole("heading", { name: "本次结束申请未完成" })).toBeVisible();
  await expect(page.getByRole("button", { name: "返回确认页面", exact: true })).toHaveCount(0);
  const rejected = await material(page);
  service = { status: "suspended", grant: grantB };
  await page.reload();
  await expect(page.locator("main")).toContainText("当前已有新的员工服务");
  expect(await material(page)).toEqual(rejected);
  expect(calls).toHaveLength(1);
  await page.getByRole("button", { name: "返回确认页面", exact: true }).click();
  await expect(page.getByRole("heading", { name: "结束员工服务", exact: true })).toBeVisible();
  expect(calls).toHaveLength(1);
  terminate = async (route, call) => route.fulfill({ status: 201, json: terminationResponse(call, accountA) });
  await confirm(page);
  await expect(page.getByRole("heading", { name: "此前的员工服务已结束" })).toBeVisible();
  expect(calls).toHaveLength(2);
  expect(calls[1]).toMatchObject({ grant: grantB, session: sessionA, body: null });
  expect(calls[1].operation).not.toBe(calls[0].operation);
});

test("a completed original survives a failed current read and refreshes without another write", async ({
  page,
}, info) => {
  await page.clock.install();
  await openEntry(page);
  await confirm(page);
  await expect(
    page.getByRole("heading", { name: "此前的员工服务已结束" }),
  ).toBeVisible();
  const unavailable = async (route: Route) =>
    route.fulfill({ status: 503, json: { code: "dependency_unavailable" } });
  await page.route(origin + "/api/v1/workforce/current/grant", unavailable);
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect(
    page.getByText("当前状态暂时无法加载，请重试。", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "此前的员工服务已结束" }),
  ).toBeVisible();
  await expect(page.getByText("结束时间：", { exact: false })).toBeVisible();
  await expect(
    page.getByRole("button", { name: "结束当前员工服务", exact: true }),
  ).toHaveCount(0);
  const current = page
    .locator(".termination-panel")
    .getByRole("button", { name: /秒后可刷新状态/ });
  await expect(current).toBeDisabled();
  expect(calls).toHaveLength(1);
  for (const colorScheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme });
    await page.setViewportSize({
      width: colorScheme === "light" ? 1440 : 320,
      height: 1000,
    });
    await page.screenshot({
      path: info.outputPath(`complete-current-unavailable-${colorScheme}.png`),
      fullPage: true,
    });
  }
  await page.unroute(origin + "/api/v1/workforce/current/grant", unavailable);
  await page.clock.fastForward(5_100);
  await page
    .locator(".termination-panel")
    .getByRole("button", { name: "刷新当前状态", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "当前员工服务已结束" }),
  ).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "此前的员工服务已结束" }),
  ).toBeVisible();
  expect(calls).toHaveLength(1);
});

test("a denied original remains unknown without offering another result check or termination", async ({
  page,
}, info) => {
  terminate = async (route) =>
    route.fulfill({ status: 403, json: { code: "permission_denied" } });
  await openEntry(page);
  await confirm(page);
  await expect(
    page.getByRole("heading", { name: "目前无法查看员工服务" }),
  ).toBeVisible();
  await expect(page.locator("main")).not.toContainText(accountA.name);
  const original = calls[0];
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "结束结果尚不确定" }),
  ).toBeVisible();
  await expect(page.locator("main")).toContainText(
    "目前无法查看结果。可先返回个人账户",
  );
  await expect(
    page.getByRole("button", { name: "查看结束结果", exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "结束员工服务", exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "返回确认页面", exact: true }),
  ).toHaveCount(0);
  await page.setViewportSize({ width: 320, height: 1000 });
  await page.emulateMedia({ colorScheme: "dark" });
  await page.screenshot({
    path: info.outputPath("denied-unknown-320-dark.png"),
    fullPage: true,
  });
  expect(await material(page)).toMatchObject({
    active: {
      operation: original.operation,
      grant: grantA,
      sent: true,
      outcome: "pending",
      notice: "denied",
    },
  });
  expect(calls).toEqual([original]);
});

test("lost pending material stays unknown through explicit recovery and reload", async ({ page }) => {
  await page.clock.install();
  terminate = async (route) => route.abort("connectionclosed");
  await openEntry(page);
  await confirm(page);
  await expect(page.getByRole("heading", { name: "结束结果尚不确定" })).toBeVisible();
  await page.evaluate(() => sessionStorage.removeItem("halo.workforce-termination"));
  await page.clock.fastForward(5_100);
  await page.getByRole("button", { name: "查看结束结果", exact: true }).click();
  await expect(page.getByRole("heading", { name: "结束结果尚不确定" })).toBeVisible();
  await expect(page.getByRole("button", { name: "结束员工服务", exact: true })).toHaveCount(0);
  expect(await material(page)).toBe("missing");
  await page.reload();
  await expect(page.getByRole("heading", { name: "结束结果尚不确定" })).toBeVisible();
  expect(calls).toHaveLength(1);
});
