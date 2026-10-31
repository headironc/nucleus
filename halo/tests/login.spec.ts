import { test, expect, type BrowserContext, type Page, type Request } from "@playwright/test";
import { readFile, rename, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { createHash, randomUUID } from "node:crypto";
import { widget } from "./widget";
import { ownHistory } from "./history";
import { ownWorkforce } from "./workforce";
import { ownNumbers } from "./numbers";
import { ownTermination } from "./termination";
import { admissionAndFirstNumber } from "./admission-number";
import { ownBadgeApplications } from "./badge-applications";
import { reviewBadgeApplications } from "./badge-reviews";
import { cancelBadgeApplications } from "./badge-cancellations";
import { ownMobileChange } from "./mobile-change";
import { cookieDigests, identityObservation, identityReads } from "./identity";
import { affiliationObservation } from "./affiliation";
import type { Account, Session } from "../app/protocol/session";

async function message(
  page: Page,
  mobile: string,
  bookkeepingFailure = false,
  resume: boolean | "all-logout-result" | "service-termination" | "mobile-change" = false,
) {
  await page.route(
    "https://challenges.cloudflare.com/turnstile/v0/api.js?render=explicit",
    (route) => route.fulfill({ contentType: "text/javascript", body: widget }),
  );
  await page.setExtraHTTPHeaders({ "X-Halo-Fixture-Source": mobile.slice(-2) });
  if (resume)
    await expect(page).toHaveURL(
      resume === "mobile-change"
        ? /\/authentication\/codes\?return=mobile-change$/
        : resume === "service-termination"
        ? /\/authentication\/codes\?return=service-termination$/
        : resume === "all-logout-result"
          ? /\/authentication\/codes\?return=all-logout-result$/
          : /\/authentication\/codes\?return=logout-result$/,
    );
  else await page.goto("/authentication/codes");
  await expect(page.getByLabel("手机号", { exact: true })).toBeVisible();
  expect(
    await page.evaluate(() => sessionStorage.getItem("halo.login.message")),
  ).toBeNull();
  await page.route(
    "**/api/v1/codes",
    async (route) => {
      const response = await route.fetch();
      const body = await response.json();
      expect(response.status()).toBe(200);
      expect(body.message.delivery).toBe("sent");
      body.message.delivery = "sending";
      if (bookkeepingFailure)
        await page.evaluate(() => {
          const set = Storage.prototype.setItem;
          Storage.prototype.setItem = function (key, bytes) {
            if (this === sessionStorage && key === "halo.c1") {
              Storage.prototype.setItem = set;
              throw new DOMException(
                "Fixture bookkeeping refusal",
                "QuotaExceededError",
              );
            }
            set.call(this, key, bytes);
          };
        });
      await route.fulfill({ response, body: JSON.stringify(body) });
    },
    { times: 1 },
  );
  await page.getByLabel("手机号", { exact: true }).fill(mobile);
  await page.getByRole("button", { name: "完成安全验证（测试替身）" }).click();
  await page.getByRole("button", { name: "获取验证码", exact: true }).click();
  await expect(
    page.getByRole("heading", {
      name: bookkeepingFailure ? "暂时无法查看发送结果" : "验证码正在发送",
    }),
  ).toBeVisible();
  if (bookkeepingFailure) {
    expect(
      await page.evaluate(() => sessionStorage.getItem("halo.c1")),
    ).toBeNull();
    await page.reload();
    await expect(
      page.getByRole("link", { name: "输入短信验证码" }),
    ).toBeVisible();
  } else
    expect(
      await page.evaluate(
        () => !!JSON.parse(sessionStorage.getItem("halo.c1")!).input.token,
      ),
    ).toBe(true);
  const id = await page.evaluate(
    () => JSON.parse(sessionStorage.getItem("halo.login.message")!).message.id,
  );
  const code = await readFile(
    join(process.env.HALO_TEST_MATERIAL!, `${id}.code`),
    "utf8",
  );
  expect(/^[0-9]{6}$/.test(code)).toBe(true);
  const [navigation] = await Promise.all([
    page.waitForResponse(
      (response) =>
        response.request().isNavigationRequest() &&
        new URL(response.url()).pathname === "/authentication/login",
    ),
    page.getByRole("link", { name: "输入短信验证码" }).click(),
  ]);
  expect(navigation.headers()["content-security-policy"]).toContain(
    "frame-src 'none'",
  );
  expect(navigation.headers()["content-security-policy"]).not.toContain(
    "challenges.cloudflare.com",
  );
  await expect(
    page.getByRole("textbox", { name: "短信验证码", exact: true }),
  ).toBeFocused();
  expect(
    await page.evaluate(() => sessionStorage.getItem("halo.c1")),
  ).toBeNull();
  return code;
}

async function paste(page: Page, index: number, value: string) {
  await page
    .locator(".code-slots input")
    .nth(index)
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

async function digits(page: Page) {
  return page
    .locator(".code-slots input")
    .evaluateAll((inputs) =>
      inputs.map((input) => (input as HTMLInputElement).value).join(""),
    );
}

async function failResource(page: Page) {
  await page.evaluate(
    () =>
      new Promise<void>((resolve) => {
        const source = document.querySelector<HTMLScriptElement>(
          'script[src*="/bootstrap-"]',
        )!.src;
        const script = document.createElement("script");
        script.src = new URL("missing-integrity.js", source).href;
        script.onerror = () => resolve();
        document.head.append(script);
      }),
  );
}

async function compose(
  page: Page,
  index: number,
  intermediate: string,
  final: string,
  whileComposing?: () => Promise<void>,
) {
  await page
    .locator(".code-slots input")
    .nth(index)
    .evaluate(
      (element, values) => {
        const input = element as HTMLInputElement;
        const set = Object.getOwnPropertyDescriptor(
          HTMLInputElement.prototype,
          "value",
        )!.set!;
        input.dispatchEvent(
          new CompositionEvent("compositionstart", { bubbles: true }),
        );
        set.call(input, values.intermediate);
        input.dispatchEvent(
          new InputEvent("input", {
            bubbles: true,
            inputType: "insertCompositionText",
            isComposing: true,
            data: values.intermediate,
          }),
        );
      },
      { intermediate },
    );
  await whileComposing?.();
  await page
    .locator(".code-slots input")
    .nth(index)
    .evaluate((element, value) => {
      const input = element as HTMLInputElement;
      Object.getOwnPropertyDescriptor(
        HTMLInputElement.prototype,
        "value",
      )!.set!.call(input, value);
      input.dispatchEvent(
        new CompositionEvent("compositionend", { bubbles: true, data: value }),
      );
    }, final);
}

async function choose(
  page: Page,
  session: { id: string; expiresAt: string },
  attempt?: { operation: string; revision: string },
) {
  // A second real account is test setup; this models a completed selection in another tab.
  await page.evaluate(
    async ({ session, revision, attempt }) => {
      await new Promise<void>((resolve, reject) => {
        const opening = indexedDB.open("halo.session", 1);
        opening.onerror = () => reject(opening.error);
        opening.onsuccess = () => {
          const database = opening.result;
          const tx = database.transaction("selection", "readwrite");
          tx.objectStore("selection").put(
            {
              version: 1,
              session,
              revision,
              attempt: attempt?.operation ?? null,
            },
            "current",
          );
          tx.oncomplete = () => {
            database.close();
            resolve();
          };
          tx.onabort = () => reject(tx.error);
        };
      });
      const channel = new BroadcastChannel("halo.session");
      channel.postMessage(null);
      channel.close();
    },
    { session, revision: attempt?.revision ?? randomUUID(), attempt },
  );
}

test("real self mobile change revokes both original sessions and reads its result after login", async ({
  page,
}) => {
  test.setTimeout(240_000);
  await ownMobileChange(page, message);
});

async function selectedSession(page: Page) {
  return page.evaluate(async () => {
    const database = await new Promise<IDBDatabase>((resolve, reject) => {
      const request = indexedDB.open("halo.session", 1);
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
    try {
      const stored = await new Promise<{
        session: { id: string; expiresAt: string } | null;
      } | null>((resolve, reject) => {
        const request = database
          .transaction("selection", "readonly")
          .objectStore("selection")
          .get("current");
        request.onsuccess = () => resolve(request.result ?? null);
        request.onerror = () => reject(request.error);
      });
      if (!stored?.session) throw Error("missing browser session selection");
      return stored.session;
    } finally {
      database.close();
    }
  });
}

async function registerRealAccount(
  page: Page,
  mobile: string,
  name: string,
  idNumber: string,
) {
  const code = await message(page, mobile);
  const sentAt = Date.now();
  await page
    .getByRole("textbox", { name: "短信验证码", exact: true })
    .fill(code);
  await page.getByRole("button", { name: "验证并继续", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "创建个人账户", exact: true }),
  ).toBeVisible();
  await page.getByLabel("姓名", { exact: true }).fill(name);
  await page.getByLabel("居民身份证号", { exact: true }).fill(idNumber);
  const currentRead = page.waitForResponse(
    (response) =>
      new URL(response.url()).pathname === "/api/v1/accounts/current" &&
      response.status() === 200,
  );
  await page.getByRole("button", { name: "创建账户", exact: true }).click();
  await expect(page).toHaveURL(/\/account$/);
  await expect(page.getByText(name, { exact: true })).toBeVisible();
  const account = (await (await currentRead).json()).account as Account;
  return { account, session: await selectedSession(page), sentAt };
}

async function prepareBadgeDirectory(
  page: Page,
  manager: Session,
  other: { account: Account; session: Session },
) {
  return page.evaluate(async ({ manager, other }) => {
    const request = async (
      session: string, path: string, input: unknown, method = "POST", status = 201,
    ) => {
      // Setup observes the real two-per-second authenticated-write budget.
      await new Promise((resolve) => setTimeout(resolve, 550));
      const response = await fetch(`/api/v1/workforce/${path}`, {
        method, credentials: "same-origin", redirect: "error",
        headers: {
          "Session-Id": session, "Operation-Id": crypto.randomUUID(),
          "Content-Type": "application/json",
        },
        body: JSON.stringify(input),
      });
      if (response.status !== status)
        throw new Error(`Badge setup ${path}: ${response.status}`);
      return response.json();
    };
    const { role } = await request(manager.id, "roles", {
      entry: "institution", name: "Badge fixture directory", level: 100,
      permissions: ["workforce.directory"],
    });
    const candidate = await request(manager.id, "grants/candidates", {
      entry: "institution", mobile: "13900000018",
    }, "POST", 200);
    if (candidate.identity.accountId !== other.account.id ||
        candidate.identity.personId !== other.account.personId ||
        candidate.identity.revisionId !== other.account.revisionId)
      throw new Error("Badge setup candidate differs from registered observer");
    const receipt = await request(manager.id, "grants", {
      entry: "institution",
      target: {
        accountId: candidate.identity.accountId, personId: candidate.identity.personId,
        revisionId: candidate.identity.revisionId, mobile: "13900000018",
        assignmentId: candidate.assignmentId, suspensionId: candidate.suspensionId,
        grant: candidate.grant,
      },
      role: { id: role.id, changeId: role.changeId },
    });
    if (receipt.grant.status !== "active")
      throw new Error("Badge observer was not admitted");
    const { item: group } = await request(other.session.id, "directories/groups", {
      name: "Badge fixture group", enabled: true, index: 0,
    });
    const { item: department } = await request(other.session.id, "directories/departments", {
      name: "Badge fixture department", enabled: true, index: 0,
      groupId: group.id, parentId: null,
    });
    for (const subtype of [100, 200, 300, 400, 500, 600, 700, 801, 802, 803, 804, 901, 902, 903, 904, 1000]) {
      await request(other.session.id, `rules/${subtype}`, {
        departmentId: department.id, position: subtype !== 902,
      }, "PUT");
    }
    return { role, department };
  }, { manager, other });
}

test("damaged closed login records discard secrets without starting another login", async ({
  page,
}) => {
  const posts: string[] = [];
  page.on("request", (request) => {
    if (request.method() === "POST") posts.push(request.url());
  });
  await page.goto("/");
  await page.evaluate(() => {
    sessionStorage.setItem(
      "halo.r1",
      JSON.stringify({
        version: 1,
        kind: "closed",
        reason: "session",
        input: { secret: "stale-secret", credential: { code: "000001" } },
        registration: { token: "stale-token" },
      }),
    );
  });
  const expected = { version: 1, kind: "closed", reason: "missing" };
  await page.goto("/authentication/login");
  for (let reload = 0; reload < 2; reload += 1) {
    await expect(
      page.getByRole("heading", { name: "暂时无法确认此前的登录结果" }),
    ).toBeVisible();
    expect(
      await page.evaluate(() => JSON.parse(sessionStorage.getItem("halo.r1")!)),
    ).toEqual(expected);
    expect(posts).toEqual([]);
    if (reload === 0) await page.reload();
  }
  for (const width of [1440, 768, 320]) {
    await page.setViewportSize({ width, height: 900 });
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    ).toBe(true);
    await page.screenshot({
      path: `test-results/login-status-unavailable-${width}.png`,
      fullPage: true,
    });
  }
  const check = page.getByRole("link", { name: "检查登录状态", exact: true });
  await expect(
    page.getByRole("heading", { name: "暂时无法确认此前的登录结果" }),
  ).toBeFocused();
  await page.keyboard.press("Tab");
  await expect(check).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(
    page.getByRole("heading", { name: "请使用手机号登录" }),
  ).toBeVisible();
  expect(posts).toEqual([]);
});

test("real SMS login, original recovery, cookie-backed identity and cross-tab concealment", async ({
  page,
  context,
  browser,
}) => {
  // Cancellation adds a real same-mobile registration after its 60 s cooldown.
  test.setTimeout(450_000);
  const resourceDiagnostics: {
    path: string;
    status?: number;
    failure?: boolean;
  }[] = [];
  page.on("response", (response) => {
    const path = new URL(response.url()).pathname;
    if (path === "/build.json" || path.startsWith("/assets/"))
      resourceDiagnostics.push({ path, status: response.status() });
  });
  page.on("requestfailed", (request) => {
    const path = new URL(request.url()).pathname;
    if (path === "/build.json" || path.startsWith("/assets/"))
      resourceDiagnostics.push({ path, failure: true });
  });
  await context.route(
    "https://challenges.cloudflare.com/turnstile/v0/api.js?render=explicit",
    (route) => route.fulfill({ contentType: "text/javascript", body: widget }),
  );
  async function openLogin() {
    const opening = context.waitForEvent("page");
    const link = page.getByRole("link", { name: "手机号登录", exact: true });
    await expect(link).toHaveAccessibleDescription(
      "登录页面将在新标签页打开。",
    );
    await link.focus();
    await page.keyboard.press("Enter");
    const login = await opening;
    await expect(login.getByLabel("手机号", { exact: true })).toBeVisible();
    expect(
      await login.evaluate(() => ({
        opener: window.opener === null,
        original: sessionStorage.getItem("halo.r1"),
        message: sessionStorage.getItem("halo.login.message"),
        send: sessionStorage.getItem("halo.c1"),
      })),
    ).toEqual({ opener: true, original: null, message: null, send: null });
    return login;
  }
  await page.goto("/account");
  await expect(
    page.getByRole("heading", { name: "请使用手机号登录" }),
  ).toBeVisible();
  await page.evaluate(async () => {
    const database = await new Promise<IDBDatabase>((resolve, reject) => {
      const request = indexedDB.open("halo.session", 1);
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
    await new Promise<void>((resolve, reject) => {
      const transaction = database.transaction("selection", "readwrite");
      transaction
        .objectStore("selection")
        .put({ version: 99, session: "damaged" }, "current");
      transaction.oncomplete = () => resolve();
      transaction.onabort = () => reject(transaction.error);
    });
    database.close();
  });
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "登录信息暂时无法使用" }),
  ).toBeVisible();
  await page.getByRole("button", { name: "重新检查", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "登录信息暂时无法使用" }),
  ).toBeVisible();
  for (const width of [1440, 768, 320]) {
    await page.setViewportSize({ width, height: 900 });
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    ).toBe(true);
    await page.screenshot({
      path: `test-results/current-storage-${width}.png`,
      fullPage: true,
    });
  }
  const freshLogin = await openLogin();
  await freshLogin.close();
  const correct = await message(page, "13900000000", true);
  const firstSendFinishedAt = Date.now();
  const wrong = correct.slice(0, 5) + ((Number(correct[5]) + 1) % 10);
  await page.getByRole("button", { name: "验证并继续" }).click();
  await expect(page.getByRole("alert")).toContainText("6 位短信验证码");
  await expect(page.locator(".code-slots input").first()).toBeFocused();
  await page.keyboard.type("01");
  await expect(page.locator(".code-slots input").nth(2)).toBeFocused();
  await page.keyboard.press("Backspace");
  await expect.poll(() => digits(page)).toBe("0");
  await expect(page.locator(".code-slots input").nth(1)).toBeFocused();
  await page.keyboard.press("ArrowLeft");
  await expect(page.locator(".code-slots input").first()).toBeFocused();
  await page.keyboard.press("ArrowRight");
  await expect(page.locator(".code-slots input").nth(1)).toBeFocused();
  await paste(page, 0, "012345");
  let premature = 0;
  const observe = (request: Request) => {
    if (
      request.method() === "POST" &&
      new URL(request.url()).pathname === "/api/v1/sessions"
    )
      premature++;
  };
  page.on("request", observe);
  await compose(page, 0, "98", "98中", async () => {
    await page.getByRole("button", { name: "验证并继续" }).click();
    await expect(
      page.getByRole("heading", { name: "手机号登录", exact: true }),
    ).toBeVisible();
  });
  await expect(page.getByRole("alert")).toContainText("6 位数字");
  expect(await digits(page)).toBe("012345");
  await compose(page, 2, "65", "654321");
  await expect.poll(() => digits(page)).toBe("654321");
  const hidden = page.locator("#login-code-hidden-input");
  await expect(hidden).toHaveAttribute("autocomplete", "off");
  await hidden.evaluate((element) => {
    const input = element as HTMLInputElement;
    Object.getOwnPropertyDescriptor(
      HTMLInputElement.prototype,
      "value",
    )!.set!.call(input, "01 2345");
    input.dispatchEvent(
      new InputEvent("input", {
        bubbles: true,
        inputType: "insertReplacementText",
        data: "01 2345",
      }),
    );
  });
  await expect(page.getByRole("alert")).toContainText("6 位数字");
  expect(await digits(page)).toBe("654321");
  for (const width of [1440, 768, 320]) {
    await page.setViewportSize({ width, height: 900 });
    await paste(page, 5, "001234");
    await expect.poll(() => digits(page)).toBe("001234");
    await paste(page, 4, "12345");
    await expect(page.getByRole("alert")).toContainText("6 位数字");
    expect(await digits(page)).toBe("001234");
    for (const invalid of ["12345678", "12 345", "12345\n", "１２３４５６"]) {
      await paste(page, 0, invalid);
      expect(await digits(page)).toBe("001234");
    }
    await paste(page, 2, "99");
    await expect.poll(() => digits(page)).toBe("009934");
    const slots = page.locator(".code-slots input");
    expect(await slots.count()).toBe(6);
    await slots.nth(3).fill("012345");
    await expect.poll(() => digits(page)).toBe("012345");
    await slots.first().fill("12345678");
    await expect(page.getByRole("alert")).toContainText("6 位数字");
    expect(await digits(page)).toBe("012345");
    await paste(page, 0, "012345");
    await expect(page.getByRole("alert")).toBeEmpty();
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    ).toBe(true);
    await page.screenshot({
      path: `test-results/login-code-${width}.png`,
      fullPage: true,
    });
  }
  await page.setViewportSize({ width: 1440, height: 1000 });
  expect(premature).toBe(0);
  page.off("request", observe);
  await page
    .getByRole("textbox", { name: "短信验证码", exact: true })
    .fill(wrong);
  await page.getByRole("button", { name: "验证并继续" }).click();
  await expect(page.getByRole("alert")).toContainText(
    "请检查本次短信中的验证码",
  );
  await expect(
    page.getByRole("textbox", { name: "短信验证码", exact: true }),
  ).toBeFocused();
  for (const slot of await page.locator(".code-slots input").all())
    await expect(slot).toHaveAttribute("aria-invalid", "true");

  const requests: { operation: string; digest: string }[] = [];
  let original: { id: string; expiresAt: string } | undefined;
  let wireExpiry = 0;
  await page.route("**/api/v1/sessions", async (route) => {
    requests.push({
      operation: route.request().headers()["operation-id"],
      digest: createHash("sha256")
        .update(route.request().postData()!)
        .digest("hex"),
    });
    const response = await route.fetch();
    expect(response.status()).toBe(200);
    const session = (await response.json()).session;
    const header = response.headers()["set-cookie"] ?? "";
    const expires = /(?:^|;\s*)expires=([^;]+)/i.exec(header)?.[1];
    wireExpiry = Date.parse(expires ?? "") / 1000;
    expect(wireExpiry).toBe(
      Math.floor(Date.parse(session.expiresAt) / 1000) +
        Number(/\.[0-9]*[1-9][0-9]*Z$/.test(session.expiresAt)),
    );
    expect(/(?:^|;\s*)(max-age|domain)=/i.test(header)).toBe(false);
    if (!original) {
      original = session;
      await route.abort("connectionclosed");
    } else {
      expect(session).toEqual(original);
      await route.fulfill({
        response,
        ...(requests.length === 2 ? { body: '{"code":"ok"}' } : {}),
      });
    }
  });
  await page
    .getByRole("textbox", { name: "短信验证码", exact: true })
    .fill(correct);
  await page.route("**/build.json", async (route) => {
    const unsent = await page.evaluate(() => {
      const value = JSON.parse(sessionStorage.getItem("halo.r1") ?? "null");
      return value?.kind === "login" && value.sent === false;
    });
    if (unsent) await route.abort("connectionclosed");
    else await route.continue();
  });
  await page.getByRole("button", { name: "验证并继续" }).click();
  await expect(
    page.getByRole("heading", { name: "尚未提交登录", exact: true }),
  ).toBeFocused();
  expect(requests).toHaveLength(0);
  const unsent = await page.evaluate(() =>
    JSON.parse(sessionStorage.getItem("halo.r1")!),
  );
  expect(unsent.sent).toBe(false);
  await page.unroute("**/build.json");
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "尚未提交登录", exact: true }),
  ).toBeVisible();
  expect(requests).toHaveLength(0);
  for (const width of [1440, 768, 320]) {
    await page.setViewportSize({ width, height: 900 });
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    ).toBe(true);
    await page.screenshot({
      path: `test-results/login-unsent-${width}.png`,
      fullPage: true,
    });
  }
  await page.getByRole("button", { name: "继续登录", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "暂时无法确认是否已登录" }),
  ).toBeVisible();
  expect(requests[0]).toEqual({
    operation: unsent.attempt.operation,
    digest: createHash("sha256")
      .update(JSON.stringify(unsent.input))
      .digest("hex"),
  });
  for (const width of [1440, 768, 320]) {
    await page.setViewportSize({ width, height: 900 });
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    ).toBe(true);
    await page.screenshot({
      path: `test-results/login-pending-${width}.png`,
      fullPage: true,
    });
  }
  await page.setViewportSize({ width: 1440, height: 1000 });
  const pending = await page.evaluate(() =>
    JSON.parse(sessionStorage.getItem("halo.r1")!),
  );
  const stored = await page.evaluate(() => sessionStorage.getItem("halo.r1"));
  const newWrites: string[] = [];
  const observeNew = (request: Request) => {
    if (request.method() === "POST") newWrites.push(request.url());
  };
  context.on("request", observeNew);
  await page.goto("/account");
  await expect(
    page.getByRole("heading", { name: "请使用手机号登录" }),
  ).toBeVisible();
  const separateLogin = await openLogin();
  await expect(
    separateLogin.getByRole("button", { name: "获取验证码", exact: true }),
  ).toBeVisible();
  expect(await page.evaluate(() => sessionStorage.getItem("halo.r1"))).toBe(
    stored,
  );
  expect(newWrites).toEqual([]);
  expect(requests).toHaveLength(1);
  await separateLogin.close();
  context.off("request", observeNew);
  await page.goto("/authentication/login");
  await expect(
    page.getByRole("heading", { name: "暂时无法确认是否已登录" }),
  ).toBeVisible();
  await failResource(page);
  await expect(page.getByRole("heading", { name: "请重新加载" })).toBeVisible();
  const retained = await page.evaluate(() =>
    JSON.parse(sessionStorage.getItem("halo.r1")!),
  );
  expect(retained).toEqual({ ...pending, retryAt: retained.retryAt });
  expect(retained.retryAt).toBeGreaterThanOrEqual(pending.retryAt);
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect(page.getByRole("heading", { name: "请重新加载" })).toBeVisible();
  expect(requests).toHaveLength(1);
  const cookieName = `__Host-nucleus-session-${original!.id}`;
  // Model loss of the original Set-Cookie as well as its JSON response, precisely.
  await context.clearCookies({ name: cookieName });
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "暂时无法确认是否已登录" }),
  ).toBeVisible();
  expect(requests).toHaveLength(1);
  await page.getByRole("button", { name: "查看登录结果", exact: true }).click();
  await expect(page.getByRole("heading", { name: "请重新加载" })).toBeVisible();
  const untrusted = await page.evaluate(() =>
    sessionStorage.getItem("halo.r1"),
  );
  await page.evaluate(() => {
    window.dispatchEvent(new Event("focus"));
    window.dispatchEvent(new Event("pageshow"));
    document.dispatchEvent(new Event("visibilitychange"));
  });
  await expect(page.getByRole("heading", { name: "请重新加载" })).toBeVisible();
  await expect(
    page.getByRole("button", { name: "查看登录结果", exact: true }),
  ).toHaveCount(0);
  expect(await page.evaluate(() => sessionStorage.getItem("halo.r1"))).toBe(
    untrusted,
  );
  expect(requests).toHaveLength(2);
  await page.screenshot({
    path: "test-results/login-untrusted-reload.png",
    fullPage: true,
  });
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "暂时无法确认是否已登录" }),
  ).toBeVisible();
  expect(requests).toHaveLength(2);
  const loginCurrent = page.waitForResponse(
    (response) =>
      new URL(response.url()).pathname === "/api/v1/accounts/current" &&
      response.status() === 200,
  );
  await page.getByRole("button", { name: "查看登录结果", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "个人账户", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText("Fixture Operator", { exact: true }),
  ).toBeVisible();
  // The post-login page keeps the login document's policy, so it must already
  // permit the configured media origin.
  const avatar = page.locator('.avatar-slot[data-state="ready"]');
  await expect(avatar).toBeVisible();
  expect(
    await avatar
      .locator("img")
      .evaluate((node) => (node as HTMLImageElement).naturalWidth),
  ).toBe(512);
  await expect(avatar.locator("img")).toHaveAttribute(
    "src",
    ((await (await loginCurrent).json()) as { account: Account }).account.avatar
      .href,
  );
  expect(requests).toHaveLength(3);
  expect(requests[1]).toEqual(requests[0]);
  expect(requests[2]).toEqual(requests[0]);
  const cookie = (await context.cookies()).find(
    (cookie) => cookie.name === cookieName,
  )!;
  expect(cookie).toMatchObject({
    secure: true,
    httpOnly: true,
    sameSite: "Lax",
    path: "/",
  });
  // Chromium adjusts cookie expiry using the server's second-precision Date.
  // The exact absolute server contract is checked on both Set-Cookie headers.
  expect(Math.abs(cookie.expires - wireExpiry)).toBeLessThan(2);
  expect(await page.evaluate(() => document.cookie)).not.toContain(cookieName);
  await test.step("known success retries local storage without another login", async () => {
    await page.evaluate((proof) => {
      sessionStorage.setItem("halo.r1", JSON.stringify(proof));
      sessionStorage.removeItem(`halo.r1.session.${proof.attempt.operation}`);
    }, pending);
    await choose(page, original!, pending.attempt);
    await page.goto("/authentication/login");
    try {
      await expect(
        page.getByRole("heading", { name: "暂时无法确认是否已登录" }),
      ).toBeVisible();
    } catch (error) {
      await writeFile(
        "test-results/login-resource-diagnostics.json",
        JSON.stringify(
          {
            resources: resourceDiagnostics,
            page: await page.evaluate(() => ({
              reloadRequired: window.__halo?.reloadRequired,
              hidden: document.hidden,
              path: location.pathname,
            })),
          },
          null,
          2,
        ),
      );
      throw error;
    }
    await page.evaluate(() => {
      const json = Response.prototype.json;
      const write = Storage.prototype.setItem;
      Response.prototype.json = async function () {
        const value = await json.call(this);
        if (
          new URL(this.url).pathname === "/api/v1/sessions" &&
          this.status === 200 &&
          value.code === "ok" &&
          value.session
        ) {
          Response.prototype.json = json;
          Storage.prototype.setItem = function (key, bytes) {
            if (this === sessionStorage && key.startsWith("halo.r1"))
              throw new DOMException(
                "Fixture storage refusal",
                "QuotaExceededError",
              );
            write.call(this, key, bytes);
          };
          Reflect.set(window, "restoreLoginStorage", () => {
            Storage.prototype.setItem = write;
          });
        }
        return value;
      };
    });
    await page
      .getByRole("button", { name: "查看登录结果", exact: true })
      .click();
    await expect(
      page.getByRole("heading", { name: "暂时无法打开个人账户", exact: true }),
    ).toBeFocused();
    expect(
      await page.evaluate(() => sessionStorage.getItem("halo.r1")),
    ).toBeNull();
    await expect(page.getByText("请重试打开个人账户。")).toBeVisible();
    await expect(
      page.getByRole("button", { name: "重新加载", exact: true }),
    ).toHaveCount(0);
    const retry = page.getByRole("button", {
      name: "重试打开个人账户",
      exact: true,
    });
    await page.keyboard.press("Tab");
    await expect(retry).toBeFocused();
    await page.keyboard.press("Enter");
    await expect(
      page.getByRole("heading", { name: "暂时无法打开个人账户", exact: true }),
    ).toBeFocused();
    expect(requests).toHaveLength(4);
    expect(requests[3]).toEqual(requests[0]);
    await page.screenshot({
      path: "test-results/login-known-success-storage.png",
      fullPage: true,
    });
    await page.evaluate(() => Reflect.get(window, "restoreLoginStorage")());
    await retry.click();
    await expect(
      page.getByRole("heading", { name: "个人账户", exact: true }),
    ).toBeVisible();
    expect(requests).toHaveLength(4);
  });
  await test.step("saved success retries matching handoff cleanup after reload", async () => {
    await page.evaluate((pending) => {
      const now = Date.now();
      const completed = JSON.parse(
        sessionStorage.getItem(`halo.r1.session.${pending.attempt.operation}`)!,
      );
      completed.cleanup = {
        message: pending.input.credential.id,
        send: pending.send,
      };
      sessionStorage.setItem("halo.r1", JSON.stringify(completed));
      sessionStorage.setItem(
        "halo.login.message",
        JSON.stringify({
          version: 1,
          operation: pending.send,
          mobile: pending.input.credential.mobile,
          message: {
            id: pending.input.credential.id,
            delivery: "sent",
            expiresAt: new Date(now + 60_000).toISOString(),
          },
          createdAt: now,
          expiresAt: now + 30_000,
        }),
      );
      sessionStorage.setItem("fixture.cleanup.block", "true");
    }, pending);
    await page.addInitScript(() => {
      const remove = Storage.prototype.removeItem;
      Storage.prototype.removeItem = function (key) {
        if (
          this === sessionStorage &&
          key === "halo.login.message" &&
          sessionStorage.getItem("fixture.cleanup.block")
        )
          throw new DOMException(
            "Fixture removal refusal",
            "InvalidStateError",
          );
        remove.call(this, key);
      };
    });
    await page.goto("/authentication/login");
    await expect(
      page.getByRole("heading", { name: "暂时无法打开个人账户", exact: true }),
    ).toBeVisible();
    await page.reload();
    await expect(
      page.getByRole("heading", { name: "暂时无法打开个人账户", exact: true }),
    ).toBeVisible();
    expect(
      await page.evaluate(() => sessionStorage.getItem("halo.login.message")),
    ).not.toBeNull();
    await page.evaluate(() =>
      sessionStorage.removeItem("fixture.cleanup.block"),
    );
    await page
      .getByRole("button", { name: "重试打开个人账户", exact: true })
      .click();
    await expect(
      page.getByRole("heading", { name: "个人账户", exact: true }),
    ).toBeVisible();
    expect(
      await page.evaluate(() => sessionStorage.getItem("halo.login.message")),
    ).toBeNull();
    expect(
      await page.evaluate(
        () => JSON.parse(sessionStorage.getItem("halo.r1")!).message,
      ),
    ).toBeUndefined();
    expect(requests).toHaveLength(4);
  });
  await test.step("a transient fixed-record failure remains restorable with corrupt handoff cleanup", async () => {
    await page.evaluate((proof) => {
      sessionStorage.setItem("halo.r1", JSON.stringify(proof));
      sessionStorage.removeItem(`halo.r1.session.${proof.attempt.operation}`);
    }, pending);
    await choose(page, original!, pending.attempt);
    await page.goto("/authentication/login");
    await expect(
      page.getByRole("heading", { name: "暂时无法确认是否已登录" }),
    ).toBeVisible();
    await page.evaluate(() => {
      const json = Response.prototype.json;
      Response.prototype.json = async function () {
        const value = await json.call(this);
        if (
          new URL(this.url).pathname === "/api/v1/sessions" &&
          this.status === 200 &&
          value.session
        ) {
          Response.prototype.json = json;
          sessionStorage.setItem("halo.login.message", "{");
          sessionStorage.setItem("fixture.cleanup.block", "true");
          const write = Storage.prototype.setItem;
          Storage.prototype.setItem = function (key, bytes) {
            if (this === sessionStorage && key === "halo.r1") {
              Storage.prototype.setItem = write;
              throw new DOMException(
                "Fixture transient replacement refusal",
                "QuotaExceededError",
              );
            }
            write.call(this, key, bytes);
          };
        }
        return value;
      };
    });
    await page
      .getByRole("button", { name: "查看登录结果", exact: true })
      .click();
    await expect(
      page.getByRole("heading", { name: "暂时无法打开个人账户", exact: true }),
    ).toBeVisible();
    expect(
      await page.evaluate(() => JSON.parse(sessionStorage.getItem("halo.r1")!)),
    ).toEqual({
      version: 1,
      kind: "session",
      attempt: pending.attempt,
      session: original,
      cleanup: { message: pending.input.credential.id, send: pending.send },
    });
    await page.reload();
    await expect(
      page.getByRole("heading", { name: "暂时无法打开个人账户", exact: true }),
    ).toBeVisible();
    expect(
      await page.evaluate(() => sessionStorage.getItem("halo.login.message")),
    ).toBe("{");
    await page.evaluate(() =>
      sessionStorage.removeItem("fixture.cleanup.block"),
    );
    await page
      .getByRole("button", { name: "重试打开个人账户", exact: true })
      .click();
    await expect(
      page.getByRole("heading", { name: "个人账户", exact: true }),
    ).toBeVisible();
    expect(
      await page.evaluate(() => sessionStorage.getItem("halo.login.message")),
    ).toBeNull();
    expect(requests).toHaveLength(5);
    expect(requests[4]).toEqual(requests[0]);
  });
  await test.step("a superseded login keeps its exact successful session", async () => {
    // Recreate the original recoverable state, then replay the same real POST.
    // No new SMS, login operation or server Session is introduced.
    await page.evaluate((proof) => {
      sessionStorage.setItem("halo.r1", JSON.stringify(proof));
      sessionStorage.removeItem(`halo.r1.session.${proof.attempt.operation}`);
    }, pending);
    await choose(page, original!, pending.attempt);
    await page.goto("/authentication/login");
    await expect(
      page.getByRole("heading", { name: "暂时无法确认是否已登录" }),
    ).toBeVisible();
    await page.evaluate(() => {
      const json = Response.prototype.json;
      Response.prototype.json = async function () {
        const value = await json.call(this);
        if (
          new URL(this.url).pathname === "/api/v1/sessions" &&
          this.status === 200 &&
          value.code === "ok" &&
          value.session
        ) {
          Response.prototype.json = json;
          Reflect.set(window, "haloLoginRead", value.session);
          // Hold processing after the real body and cookie have arrived.
          await new Promise<void>((resolve) => {
            Reflect.set(window, "releaseHaloLogin", resolve);
          });
        }
        return value;
      };
    });
    await page
      .getByRole("button", { name: "查看登录结果", exact: true })
      .click();
    await page.waitForFunction(() => Reflect.has(window, "haloLoginRead"));
    expect(
      await page.evaluate(() => Reflect.get(window, "haloLoginRead")),
    ).toEqual(original);
    expect(
      (await context.cookies()).find((value) => value.name === cookieName),
    ).toBeDefined();
    const contender = await context.newPage();
    await contender.goto("/");
    const newer = { operation: randomUUID(), revision: randomUUID() };
    await choose(contender, original!, newer);
    await expect(
      page.getByRole("heading", { name: "登录状态有变化", exact: true }),
    ).toBeVisible();
    await page.evaluate(() => Reflect.get(window, "releaseHaloLogin")());
    const receipt = {
      version: 1,
      kind: "session",
      attempt: pending.attempt,
      session: original,
    };
    await expect
      .poll(() =>
        page.evaluate(
          (operation) =>
            JSON.parse(
              sessionStorage.getItem(`halo.r1.session.${operation}`) ?? "null",
            ),
          pending.attempt.operation,
        ),
      )
      .toEqual(receipt);
    expect(
      await page.evaluate(() => JSON.parse(sessionStorage.getItem("halo.r1")!)),
    ).toEqual(receipt);
    await expect(
      page.getByRole("heading", { name: "登录状态有变化", exact: true }),
    ).toBeVisible();
    await page.reload();
    await expect(
      page.getByRole("heading", { name: "登录状态有变化", exact: true }),
    ).toBeVisible();
    const selection = await contender.evaluate(
      () =>
        new Promise<Record<string, unknown>>((resolve, reject) => {
          const opening = indexedDB.open("halo.session", 1);
          opening.onerror = () => reject(opening.error);
          opening.onsuccess = () => {
            const database = opening.result;
            const read = database
              .transaction("selection", "readonly")
              .objectStore("selection")
              .get("current");
            read.onsuccess = () => {
              database.close();
              resolve(read.result);
            };
            read.onerror = () => reject(read.error);
          };
        }),
    );
    expect(selection).toEqual({
      version: 1,
      attempt: newer.operation,
      revision: newer.revision,
      session: original,
    });
    expect(requests).toHaveLength(6);
    expect(requests[5]).toEqual(requests[0]);
    await page.screenshot({
      path: "test-results/login-superseded-success.png",
      fullPage: true,
    });
    await choose(contender, original!);
    await expect(
      page.getByRole("heading", { name: "个人账户", exact: true }),
    ).toBeVisible();
    expect(requests).toHaveLength(6);
    await contender.close();
  });
  // Replaying Set-Cookie can change Chromium's Date-based fractional adjustment.
  // A subsequent page reopen must preserve the expiry from that final delivery.
  const restoredCookieExpiry = (await context.cookies()).find(
    (value) => value.name === cookieName,
  )!.expires;
  expect(Math.abs(restoredCookieExpiry - wireExpiry)).toBeLessThan(2);
  const tabRecords = await page.evaluate(() =>
    Object.fromEntries(
      Object.keys(sessionStorage).map((key) => [
        key,
        sessionStorage.getItem(key)!,
      ]),
    ),
  );
  expect(tabRecords).not.toHaveProperty("halo.c1");
  expect(tabRecords).not.toHaveProperty("halo.login.message");
  expect(
    Object.values(tabRecords).some((value) =>
      /"secret"|"credential"|"token"/.test(value),
    ),
  ).toBe(false);
  for (const path of ["/account", "/authentication/login"]) {
    const shell = await context.request.get(path);
    expect(shell.status()).toBe(200);
    expect(
      new Set(
        shell
          .headers()
          ["cache-control"].split(",")
          .map((value) => value.trim()),
      ),
    ).toEqual(new Set(["no-store"]));
    const html = await shell.text();
    expect(html).not.toContain("Fixture Operator");
    expect(html).not.toContain(original!.id);
    expect(html).toContain('content="noindex"');
    expect(shell.headers()["content-security-policy"]).toContain(
      "frame-src 'none'",
    );
  }
  await writeFile(
    join(process.env.HALO_TEST_MATERIAL!, "login.json"),
    JSON.stringify({ operation: requests[0].operation, session: original }),
  );
  for (const width of [1440, 768, 320]) {
    await page.setViewportSize({ width, height: 1000 });
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    ).toBe(true);
    await page.screenshot({
      path: `test-results/current-${width}.png`,
      fullPage: true,
    });
  }

  // A new tab and a reopened page use the persisted selection and original cookie.
  const reopened = await context.newPage();
  await reopened.goto("/account");
  await expect(
    reopened.getByText("Fixture Operator", { exact: true }),
  ).toBeVisible();
  expect(
    (await context.cookies()).find((value) => value.name === cookieName)!
      .expires,
  ).toBe(restoredCookieExpiry);
  await reopened.close();

  // The same real new number now completes R2 through the production page.
  const other = await context.newPage();
  const newCode = await message(other, "13900000013");
  let authenticationOperation: string | undefined;
  other.on("request", (request) => {
    if (new URL(request.url()).pathname === "/api/v1/sessions")
      authenticationOperation = request.headers()["operation-id"];
  });
  await other
    .getByRole("textbox", { name: "短信验证码", exact: true })
    .fill(newCode);
  await other.getByRole("button", { name: "验证并继续" }).click();
  await expect(
    other.getByRole("heading", { name: "创建个人账户" }),
  ).toBeVisible();
  for (const width of [320, 390, 768, 1440]) {
    await other.setViewportSize({ width, height: 1000 });
    expect(
      await other.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    ).toBe(true);
    await other.screenshot({
      path: `test-results/registration-${width}.png`,
      fullPage: true,
    });
  }
  await other.getByLabel("姓名", { exact: true }).fill("Second Fixture");
  await other
    .getByLabel("居民身份证号", { exact: true })
    .fill("00000020000229007x");
  expect(
    await other.evaluate(() => sessionStorage.getItem("halo.r2")),
  ).toBeNull();
  const registrations: { operation: string; input: unknown }[] = [];
  const statuses: number[] = [];
  let secondSession: { id: string; expiresAt: string } | undefined;
  await other.route("**/api/v1/registrations", async (route) => {
    const request = route.request();
    registrations.push({
      operation: request.headers()["operation-id"],
      input: request.postDataJSON(),
    });
    const response = await route.fetch();
    statuses.push(response.status());
    const result = await response.json();
    if (registrations.length === 1) {
      expect(response.status()).toBe(201);
      secondSession = result.session;
      await route.abort("failed");
    } else {
      expect(response.status()).toBe(200);
      expect(result.session).toEqual(secondSession);
      await route.fulfill({ response });
    }
  });
  await other.getByRole("button", { name: "创建账户" }).click();
  await expect(
    other.getByRole("heading", { name: "暂时无法确认注册结果" }),
  ).toBeVisible();
  await other.reload();
  await expect(
    other.getByRole("heading", { name: "暂时无法确认注册结果" }),
  ).toBeVisible();
  expect(registrations).toHaveLength(1);
  const savedRegistration = await other.evaluate(() =>
    JSON.parse(sessionStorage.getItem("halo.r2")!),
  );
  expect(savedRegistration.input.name).toBe("Second Fixture");
  expect(savedRegistration.input.idNumber).toBe("00000020000229007X");
  expect(savedRegistration.operation).not.toBe(authenticationOperation);
  expect(
    await other.evaluate(() => sessionStorage.getItem("halo.r1")),
  ).not.toContain('"token"');
  await expect(
    other.getByRole("button", { name: "查看注册结果" }),
  ).toBeEnabled();
  const currentResponse = other.waitForResponse(
    (response) =>
      new URL(response.url()).pathname === "/api/v1/accounts/current" &&
      response.status() === 200,
  );
  await other.getByRole("button", { name: "查看注册结果" }).click();
  const registeredAccount = (await (await currentResponse).json()).account;
  await expect(
    other.getByText("Second Fixture", { exact: true }),
  ).toBeVisible();
  // Registration also navigates client-side with the document that started it,
  // so the avatar must load under that policy without a reload.
  const registeredAvatar = other.locator('.avatar-slot[data-state="ready"]');
  await expect(registeredAvatar).toBeVisible();
  await expect
    .poll(() =>
      registeredAvatar
        .locator("img")
        .evaluate((node) => (node as HTMLImageElement).naturalWidth),
    )
    .toBe(512);
  await expect(registeredAvatar.locator("img")).toHaveAttribute(
    "src",
    registeredAccount.avatar.href,
  );
  expect(registrations).toHaveLength(2);
  expect(registrations[1]).toEqual(registrations[0]);
  const r2Cookie = (await context.cookies()).find(
    (cookie) => cookie.name === `__Host-nucleus-session-${secondSession!.id}`,
  )!;
  expect(r2Cookie).toMatchObject({
    secure: true,
    httpOnly: true,
    sameSite: "Lax",
    path: "/",
  });
  expect(
    Math.abs(
      r2Cookie.expires - Math.ceil(Date.parse(secondSession!.expiresAt) / 1000),
    ),
  ).toBeLessThan(2);
  const registrationRecords = await other.evaluate(() =>
    Object.values(sessionStorage).join("\n"),
  );
  expect(registrationRecords).not.toMatch(
    /"token"|"secret"|"credential"|"idNumber"/,
  );
  expect(registrationRecords).not.toContain("Second Fixture");
  await writeFile(
    join(process.env.HALO_TEST_MATERIAL!, "r2.json"),
    JSON.stringify({
      operation: registrations[0].operation,
      authenticationOperation,
      account: {
        id: registeredAccount.id,
        personId: registeredAccount.personId,
      },
      session: secondSession,
      statuses,
      requestCount: registrations.length,
    }),
  );
  await other.unroute("**/api/v1/registrations");
  await identityReads(
    other,
    registeredAccount.personId,
    "original",
    "Second Fixture",
    "007X",
  );
  await ownHistory(other, original!.id, registeredAccount.personId);

  await test.step("real first affiliation fill, dropped success and read-only state refresh", async () => {
    const person = registeredAccount.personId;
    let writes = 0;
    let originalOperation = "";
    let originalBody = "";
    await other.route(
      "**/api/v1/accounts/current/affiliation",
      async (route) => {
        if (route.request().method() !== "PUT") return route.continue();
        writes++;
        expect(writes).toBe(1);
        const operation = route.request().headers()["operation-id"]!;
        const body = route.request().postData()!;
        if (writes === 1) {
          originalOperation = operation;
          originalBody = body;
          const response = await route.fetch();
          expect(response.status()).toBe(200);
          expect(await response.json()).toEqual({ code: "ok" });
          // The backend committed. Prove that original fact before any refresh,
          // then drop the response so the browser observes an unknown outcome.
          await affiliationObservation(person, "dropped", "before", operation);
          await route.abort("connectionclosed");
          return;
        }
      },
    );
    await other.goto("/account/affiliation");
    await expect(
      other.getByRole("heading", { name: "尚未填写政治面貌" }),
    ).toBeVisible();
    await affiliationObservation(person, "original", "before");
    await other.getByRole("button", { name: "填写政治面貌" }).click();
    await expect(other.locator('input[name="affiliation"]')).toHaveCount(13);
    for (const name of [
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
    ])
      await expect(other.getByRole("radio", { name })).toBeVisible();
    await other.getByRole("radio", { name: "民革会员" }).check();
    await other.getByRole("button", { name: "继续" }).click();
    await expect(
      other.getByRole("heading", { name: "确认填写政治面貌" }),
    ).toBeVisible();
    await other.getByRole("button", { name: "确认填写" }).click();
    await expect(
      other.getByRole("heading", { name: "未能确认填写结果" }),
    ).toBeVisible();
    await expect(
      other.getByRole("button", { name: "重试本次填写" }),
    ).toHaveCount(0);
    expect(originalOperation).toMatch(
      /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i,
    );
    expect(JSON.parse(originalBody)).toEqual({ affiliation: "04" });

    await other
      .getByRole("button", { name: "刷新", exact: true })
      .click();
    await expect(
      other.getByRole("heading", { name: "当前政治面貌" }),
    ).toBeVisible();
    await expect(
      other.getByRole("heading", { name: "未能确认填写结果" }),
    ).toHaveCount(0);
    await expect(
      other.getByRole("heading", { name: "政治面貌已填写" }),
    ).toHaveCount(0);
    expect(writes).toBe(1);
    await affiliationObservation(person, "dropped", "after", originalOperation);
    await other.unroute("**/api/v1/accounts/current/affiliation");

    await affiliationObservation(person, "read", "before");
    const currentResponse = other.waitForResponse(
      (response) =>
        new URL(response.url()).pathname ===
          "/api/v1/accounts/current/affiliation" &&
        response.request().method() === "GET" &&
        response.status() === 200,
    );
    await other.reload();
    const current = await (await currentResponse).json();
    expect(current).toMatchObject({
      code: "ok",
      affiliation: { value: "04" },
    });
    expect(Number.isFinite(Date.parse(current.affiliation.createdAt))).toBe(
      true,
    );
    await expect(other.locator(".record-panel")).toContainText("民革会员");
    await affiliationObservation(person, "read", "after");

    await affiliationObservation(person, "conflict", "before");
    const conflict = await other.evaluate(async (session) => {
      const response = await fetch("/api/v1/accounts/current/affiliation", {
        method: "PUT",
        headers: {
          "Operation-Id": crypto.randomUUID(),
          "Session-Id": session,
          "Content-Type": "application/json",
        },
        body: JSON.stringify({ affiliation: "04" }),
      });
      return {
        status: response.status,
        body: await response.json(),
        setCookie: response.headers.get("set-cookie"),
      };
    }, secondSession!.id);
    expect(conflict).toEqual({
      status: 409,
      body: { code: "state_conflict" },
      setCookie: null,
    });
    await affiliationObservation(person, "conflict", "after");
  });

  // Preserve the original current-account race's starting selection.
  await choose(other, original!);

  await page.bringToFront();
  await page.goto("/account");
  await expect(
    page.getByText("Fixture Operator", { exact: true }),
  ).toBeVisible();
  let release!: () => void;
  let captured!: () => void;
  const waiting = new Promise<void>((resolve) => {
    captured = resolve;
  });
  const unblock = new Promise<void>((resolve) => {
    release = resolve;
  });
  let delayed = false;
  await page.route("**/api/v1/accounts/current", async (route) => {
    if (!delayed) {
      delayed = true;
      // APIRequestContext relays this same-origin browser read without fetch
      // metadata; supply the matching Origin required by the real boundary.
      const response = await route.fetch({
        headers: {
          ...(await route.request().allHeaders()),
          Origin: new URL(page.url()).origin,
        },
      });
      expect(response.status()).toBe(200);
      captured();
      await unblock;
      await route.fulfill({ response }).catch(() => {});
    } else await route.continue();
  });
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await waiting;
  await expect(page.getByText("Fixture Operator", { exact: true })).toHaveCount(
    0,
  );
  await choose(other, secondSession!);
  await expect(page.locator(".current-summary").getByText("Second Fixture", { exact: true })).toBeVisible();
  release();
  await expect(page.getByText("Fixture Operator", { exact: true })).toHaveCount(
    0,
  );
  await page.unroute("**/api/v1/accounts/current");
  await page.setViewportSize({ width: 320, height: 800 });
  await page.screenshot({
    path: "test-results/current-narrow.png",
    fullPage: true,
  });

  await page.route("**/api/v1/accounts/current", (route) =>
    route.abort("failed"),
  );
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect(
    page.getByRole("heading", { name: "个人资料暂时无法加载" }),
  ).toBeVisible();
  await expect(page.getByText("Second Fixture", { exact: true })).toHaveCount(
    0,
  );
  await page.unroute("**/api/v1/accounts/current");
  await page.getByRole("button", { name: "重新检查", exact: true }).click();
  await expect(page.locator(".current-summary").getByText("Second Fixture", { exact: true })).toBeVisible();

  let accountReads = 0;
  const observeAccount = (request: Request) => {
    if (new URL(request.url()).pathname === "/api/v1/accounts/current")
      accountReads++;
  };
  page.on("request", observeAccount);
  for (const invalid of ["status", "code", "media", "body"] as const) {
    await test.step(`an untrusted account ${invalid} requires a new document`, async () => {
      await page.route("**/api/v1/accounts/current", async (route) => {
        const response = await route.fetch({
          headers: {
            ...(await route.request().allHeaders()),
            Origin: new URL(page.url()).origin,
          },
        });
        const body = await response.json();
        expect(body.code).toBe("ok");
        await route.fulfill({
          response,
          status: invalid === "status" ? 202 : response.status(),
          contentType: invalid === "media" ? "text/plain" : "application/json",
          body: JSON.stringify(
            invalid === "code"
              ? { ...body, code: "unknown" }
              : invalid === "body"
                ? { code: "ok", account: null }
                : body,
          ),
        });
      });
      await page.evaluate(() => window.dispatchEvent(new Event("focus")));
      await expect(
        page.getByRole("heading", { name: "请重新加载", exact: true }),
      ).toBeVisible();
      await expect(
        page.getByText("Second Fixture", { exact: true }),
      ).toHaveCount(0);
      const observed = accountReads;
      await page.evaluate(() => {
        window.dispatchEvent(new Event("focus"));
        window.dispatchEvent(new Event("pageshow"));
      });
      await expect(
        page.getByRole("button", { name: "重新检查", exact: true }),
      ).toHaveCount(0);
      expect(accountReads).toBe(observed);
      await page.unroute("**/api/v1/accounts/current");
      await page
        .getByRole("button", { name: "重新加载", exact: true })
        .click();
      await expect(
        page.locator(".current-summary").getByText("Second Fixture", { exact: true }),
      ).toBeVisible();
    });
  }
  for (const failure of ["changed", "unavailable"] as const) {
    await test.step(`a ${failure} foreground build blocks private reads`, async () => {
      await page.route("**/build.json", (route) =>
        failure === "changed"
          ? route.fulfill({ json: { buildId: "a-different-build" } })
          : route.abort("failed"),
      );
      const observed = accountReads;
      await page.evaluate(() => window.dispatchEvent(new Event("focus")));
      await expect(
        page.getByRole("heading", { name: "请重新加载", exact: true }),
      ).toBeVisible();
      await expect(
        page.getByText("Second Fixture", { exact: true }),
      ).toHaveCount(0);
      await page.evaluate(() => window.dispatchEvent(new Event("pageshow")));
      expect(accountReads).toBe(observed);
      await page.screenshot({
        path: `test-results/current-build-${failure}.png`,
        fullPage: true,
      });
      await page.unroute("**/build.json");
      await page
        .getByRole("button", { name: "重新加载", exact: true })
        .click();
      await expect(
        page.locator(".current-summary").getByText("Second Fixture", { exact: true }),
      ).toBeVisible();
    });
  }
  page.off("request", observeAccount);

  await context.setOffline(true);
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect(
    page.getByRole("heading", { name: "请重新加载", exact: true }),
  ).toBeVisible();
  await expect(page.getByText("Second Fixture", { exact: true })).toHaveCount(
    0,
  );
  await context.setOffline(false);
  await page.getByRole("button", { name: "重新加载", exact: true }).click();
  await expect(page.locator(".current-summary").getByText("Second Fixture", { exact: true })).toBeVisible();

  await failResource(page);
  await expect(
    page.getByRole("heading", { name: "请重新加载", exact: true }),
  ).toBeVisible();
  await expect(page.getByText("Second Fixture", { exact: true })).toHaveCount(
    0,
  );
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect(
    page.getByRole("heading", { name: "请重新加载", exact: true }),
  ).toBeVisible();
  await page.getByRole("button", { name: "重新加载", exact: true }).click();
  await expect(page.locator(".current-summary").getByText("Second Fixture", { exact: true })).toBeVisible();

  const replacement =
    await test.step("recover the one original logout through a real same-account login", async () => {
      await choose(other, original!);
      await page.bringToFront();
      await page.goto("/account");
      await expect(
        page.getByText("Fixture Operator", { exact: true }),
      ).toBeVisible();
      const current = await context.request.get("/api/v1/accounts/current", {
        headers: {
          "Session-Id": original!.id,
          "Operation-Id": randomUUID(),
          Origin: new URL(page.url()).origin,
        },
      });
      expect(current.status()).toBe(200);
      const owner = (await current.json()).account;
      const writes: { operation: string; session: string }[] = [];
      let revokedAt: string | undefined;
      await page.route("**/api/v1/sessions/current", async (route) => {
        const request = route.request();
        expect(request.method()).toBe("DELETE");
        expect(request.postData()).toBeNull();
        writes.push({
          operation: request.headers()["operation-id"],
          session: request.headers()["session-id"],
        });
        const response = await route.fetch();
        expect(response.status()).toBe(200);
        const body = await response.json();
        expect(body.completion).toMatchObject({
          id: writes[0].operation,
          count: 1,
        });
        revokedAt = body.completion.revokedAt;
        const cookie = response.headers()["set-cookie"] ?? "";
        expect(cookie.startsWith(`${cookieName}=;`)).toBe(true);
        expect(/(?:^|;\s*)(max-age|domain)=/i.test(cookie)).toBe(false);
        const expires = /(?:^|;\s*)expires=([^;]+)/i.exec(cookie)?.[1];
        expect(Date.parse(expires ?? "")).toBeLessThan(Date.now());
        // The backend committed and sent its exact cookie cleanup, but this page loses the receipt.
        await route.abort("connectionclosed");
      });
      const reads: { operation: string; session: string; query: string }[] = [];
      await page.route("**/api/v1/revocations?*", async (route) => {
        const request = route.request();
        reads.push({
          operation: new URL(request.url()).searchParams.get("operation_id")!,
          session: request.headers()["session-id"],
          query: request.headers()["operation-id"],
        });
        // APIRequestContext forwarding does not preserve browser fetch metadata.
        const response = await route.fetch({
          headers: {
            ...(await request.allHeaders()),
            Origin: new URL(page.url()).origin,
          },
        });
        expect(response.status()).toBe(200);
        expect(response.headers()["set-cookie"]).toBeUndefined();
        expect(await response.json()).toEqual({
          code: "ok",
          count: 1,
          revocations: [{ id: original!.id, revokedAt }],
          original: { scope: "current", initiatorId: original!.id },
        });
        await route.fulfill({ response });
      });
      await page
        .getByRole("link", { name: "退出登录", exact: true })
        .click();
      await expect(
        page.getByRole("heading", { name: "退出登录？", exact: true }),
      ).toBeVisible();
      expect(writes).toEqual([]);
      await page
        .getByRole("button", { name: "退出登录", exact: true })
        .click();
      await expect(
        page.getByRole("heading", { name: "请重新登录", exact: true }),
      ).toBeVisible();
      await expect(
        page.getByText("退出结果尚未确认。请使用原账户的手机号登录，继续查看退出结果。", { exact: false }),
      ).toBeVisible();
      expect(writes).toHaveLength(1);
      expect(writes[0].session).toBe(original!.id);
      expect(reads).toEqual([]);
      expect(
        (await context.cookies()).some((cookie) => cookie.name === cookieName),
      ).toBe(false);
      const retainedCookie = (await context.cookies()).find(
        (cookie) => cookie.name === r2Cookie.name,
      );
      expect(
        createHash("sha256")
          .update(JSON.stringify(retainedCookie))
          .digest("hex"),
      ).toBe(
        createHash("sha256").update(JSON.stringify(r2Cookie)).digest("hex"),
      );
      const foreign = await context.request.get(
        `/api/v1/revocations?operation_id=${writes[0].operation}`,
        {
          headers: {
            "Operation-Id": randomUUID(),
            "Session-Id": secondSession!.id,
            Origin: new URL(page.url()).origin,
          },
        },
      );
      expect(foreign.status()).toBe(200);
      expect(await foreign.json()).toEqual({
        code: "ok",
        count: 0,
        revocations: [],
        original: null,
      });
      expect(foreign.headers()["set-cookie"]).toBeUndefined();
      const loginLink = page.getByRole("link", {
        name: "手机号登录",
        exact: true,
      });
      await expect(loginLink).toHaveAttribute(
        "href",
        "/authentication/codes?return=logout-result",
      );
      await loginLink.click();
      await expect(page.getByRole("dialog")).toBeVisible();
      await page.getByRole("button", { name: "仍然离开", exact: true }).click();
      // This is the real per-mobile send interval, not synchronization by sleeping.
      const remaining = firstSendFinishedAt + 61_000 - Date.now();
      if (remaining > 0) await page.waitForTimeout(remaining);
      await page.unroute("**/api/v1/sessions");
      let fresh: { id: string; expiresAt: string } | undefined;
      let authenticationOperation: string | undefined;
      await page.route(
        "**/api/v1/sessions",
        async (route) => {
          authenticationOperation = route.request().headers()["operation-id"];
          const response = await route.fetch();
          expect(response.status()).toBe(200);
          fresh = (await response.json()).session;
          await route.fulfill({ response });
        },
        { times: 1 },
      );
      const code = await message(page, "13900000000", false, true);
      await expect(page).toHaveURL(
        /\/authentication\/login\?return=logout-result$/,
      );
      await page
        .getByRole("textbox", { name: "短信验证码", exact: true })
        .fill(code);
      await page.getByRole("button", { name: "验证并继续" }).click();
      await expect(page).toHaveURL(/\/sessions\/logout\?return=logout-result$/);
      await expect(
        page.getByRole("heading", { name: "退出结果尚不确定", exact: true }),
      ).toBeVisible();
      expect(fresh?.id).toBeTruthy();
      expect(fresh!.id).not.toBe(original!.id);
      expect(reads).toEqual([]);
      await page.reload();
      await expect(
        page.getByRole("heading", { name: "退出结果尚不确定", exact: true }),
      ).toBeVisible();
      expect(reads).toEqual([]);
      const freshCookie = (await context.cookies()).find(
        (cookie) => cookie.name === `__Host-nucleus-session-${fresh!.id}`,
      )!;
      expect(Boolean(freshCookie)).toBe(true);
      await page
        .getByRole("button", { name: "查看退出结果", exact: true })
        .click();
      await expect(
        page.getByRole("heading", { name: "此前的退出已完成", exact: true }),
      ).toBeVisible();
      await expect(
        page.getByText("请打开个人账户，查看当前登录状态。", { exact: false }),
      ).toBeVisible();
      expect(reads).toHaveLength(1);
      expect(reads[0]).toMatchObject({
        operation: writes[0].operation,
        session: fresh!.id,
      });
      expect(reads[0].query).not.toBe(writes[0].operation);
      expect(writes).toHaveLength(1);
      const cookies = await context.cookies();
      for (const preserved of [freshCookie, r2Cookie]) {
        const actual = cookies.find((cookie) => cookie.name === preserved.name);
        expect(
          createHash("sha256").update(JSON.stringify(actual)).digest("hex"),
        ).toBe(
          createHash("sha256").update(JSON.stringify(preserved)).digest("hex"),
        );
      }
      await writeFile(
        join(process.env.HALO_TEST_MATERIAL!, "logout.json"),
        JSON.stringify({
          operation: writes[0].operation,
          original: original!.id,
          account: { id: owner.id, personId: owner.personId },
          replacement: fresh!.id,
          authenticationOperation,
          untouched: secondSession!.id,
          revokedAt,
          writes: writes.length,
          reads: reads.length,
        }),
      );
      await page.unroute("**/api/v1/sessions/current");
      await page.unroute("**/api/v1/revocations?*");
      await choose(other, secondSession!);
      await page.goto("/account");
      await expect(
        page.locator(".current-summary").getByText("Second Fixture", { exact: true }),
      ).toBeVisible();
      return fresh!;
    });

  // Finish both account controllers before removing the shared cookie. A
  // concurrent 401 in the other tab may otherwise clear the selection first,
  // making this page correctly anonymous instead of the clearing page.
  await Promise.all([page.goto("/"), other.goto("/")]);
  await context.clearCookies({
    name: `__Host-nucleus-session-${secondSession!.id}`,
  });
  const missingCookie = page.waitForResponse(
    (response) =>
      new URL(response.url()).pathname === "/api/v1/accounts/current" &&
      response.request().headers()["session-id"] === secondSession!.id &&
      response.status() === 401,
  );
  await page.goto("/account");
  expect(await (await missingCookie).json()).toEqual({
    code: "authentication_invalid",
  });
  await expect(
    page.getByRole("heading", { name: "请重新登录", exact: true }),
  ).toBeVisible();
  await expect(page.getByText("Second Fixture", { exact: true })).toHaveCount(
    0,
  );
  expect(
    (await context.cookies()).some(
      (value) => value.name === `__Host-nucleus-session-${replacement.id}`,
    ),
  ).toBe(true);
  await page.screenshot({
    path: "test-results/current-expired.png",
    fullPage: true,
  });
  // Restore the actual cookie removed by the preceding missing-cookie case.
  // The following cases still authenticate against the real application.
  await context.addCookies([r2Cookie]);
  let lastSend = Date.now();
  await test.step("a transport-delayed read proves the real avatar link expiry and explicit recovery", async () => {
    const expiryContext = await browser.newContext({ ignoreHTTPSErrors: true });
    try {
      await expiryContext.addCookies([r2Cookie]);
      const expiryPage = await expiryContext.newPage();
      const cdp = await expiryContext.newCDPSession(expiryPage);
      // Force the delayed expiry to be a network request even if an earlier
      // read issued the same signed link within the same second.
      await cdp.send("Network.setCacheDisabled", { cacheDisabled: true });
      const mutations: string[] = [];
      expiryPage.on("request", (request) => {
        const path = new URL(request.url()).pathname;
        if (
          ["/api/v1/codes", "/api/v1/sessions", "/api/v1/registrations"].includes(
            path,
          )
        )
          mutations.push(path);
      });
      await expiryPage.goto(`${process.env.HALO_TEST_ORIGIN!}/account`);
      await expect(expiryPage.getByRole("heading", { name: "请使用手机号登录" })).toBeVisible();
      await choose(expiryPage, secondSession!);
      await expect(
        expiryPage.locator('.avatar-slot[data-state="ready"]'),
      ).toBeVisible();
      const selection = () =>
        expiryPage.evaluate(
          () =>
            new Promise<string | null>((resolve, reject) => {
              const opening = indexedDB.open("halo.session", 1);
              opening.onerror = () => reject(opening.error);
              opening.onsuccess = () => {
                const database = opening.result;
                const request = database
                  .transaction("selection")
                  .objectStore("selection")
                  .get("current");
                request.onerror = () => reject(request.error);
                request.onsuccess = () => {
                  database.close();
                  resolve(request.result?.session?.id ?? null);
                };
              };
            }),
        );
      await identityObservation(registeredAccount.personId, "ordinary", "before");
      const cookiesBefore = await cookieDigests(expiryPage);
      expect(await selection()).toBe(secondSession!.id);
      const mediaLog = process.env.HALO_TEST_MEDIA_LOG!;
      const mediaBefore = (await readFile(mediaLog, "utf8")).split("\n").filter(Boolean).length;

      let captured: Account | undefined;
      let currentDelay = 0;
      await expiryPage.route(
        "**/api/v1/accounts/current",
        async (route) => {
          const started = Date.now();
          // Preserve the browser request and the fixture's same-origin proof.
          const response = await route.fetch({
            headers: {
              ...route.request().headers(),
              Origin: new URL(expiryPage.url()).origin,
            },
          });
          expect(response.status()).toBe(200);
          const body = (await response.json()) as { account: Account };
          captured = body.account;
          const remaining = 30_000 - (Date.now() - started);
          if (remaining > 0)
            await new Promise((resolve) => setTimeout(resolve, remaining));
          currentDelay = Date.now() - started;
          // Deliver this backend success unchanged; only transport was delayed.
          await route.fulfill({ response });
        },
        { times: 1 },
      );
      let imageDelay = 0;
      let releasedAt = 0;
      await expiryPage.route(
        "**/api/v1/avatars/**",
        async (route) => {
          expect(captured).toBeTruthy();
          expect(route.request().url() === captured!.avatar.href).toBe(true);
          const started = Date.now();
          const hold = Date.parse(captured!.avatar.expiresAt) + 1_500 - started;
          expect(hold).toBeGreaterThan(0);
          expect(hold).toBeLessThan(35_000);
          await new Promise((resolve) => setTimeout(resolve, hold));
          imageDelay = Date.now() - started;
          releasedAt = Date.now();
          // Continue the original request to the actual media/backend route.
          await route.continue();
        },
        { times: 1 },
      );
      // Chromium's opaque-response blocking hides cross-origin JSON error
      // responses from Playwright's response event. Bind the native failure
      // to the original request and inspect the real upstream observation.
      const failedImage = expiryPage.waitForEvent("requestfailed", {
        predicate: (request) => request.url() === captured?.avatar.href,
        timeout: 70_000,
      });
      const [, failed] = await Promise.all([
        expiryPage.reload({ waitUntil: "domcontentloaded" }),
        failedImage,
      ]);
      expect(failed.failure()).not.toBeNull();
      let expired: Record<string, unknown> | undefined;
      await expect.poll(async () => {
        const entries = (await readFile(mediaLog, "utf8")).split("\n").filter(Boolean)
          .slice(mediaBefore).map((line) => JSON.parse(line) as Record<string, unknown>);
        expired = entries.find((entry) => entry.status === 403 &&
          entry.path === new URL(captured!.avatar.href).pathname);
        return expired !== undefined;
      }).toBe(true);
      expect(expired).toMatchObject({
        status: 403,
        code: "avatar_link_invalid",
        cacheControl: "no-store",
        contentType: "application/json",
        nosniff: "nosniff",
        authorization: false,
        sessionId: false,
        operationId: false,
        setCookie: false,
      });
      expect(currentDelay).toBeLessThan(35_000);
      expect(imageDelay).toBeLessThan(35_000);
      expect(releasedAt).toBeGreaterThan(Date.parse(captured!.avatar.expiresAt));
      await expect(
        expiryPage.locator('.avatar-slot[data-state="failed"]'),
      ).toBeVisible();
      await expect(expiryPage.getByText("头像暂时无法显示。")).toBeVisible();
      await expect(
        expiryPage.locator('.avatar-slot[data-state="failed"] img'),
      ).toHaveCount(0);
      expect(await expiryPage.evaluate(() => document.body.textContent)).not.toContain(
        "avatar_link_invalid",
      );

      // Remove both one-shot delays before the user-explicit whole refresh.
      await expiryPage.unroute("**/api/v1/accounts/current");
      await expiryPage.unroute("**/api/v1/avatars/**");
      const freshCurrent = expiryPage.waitForResponse(
        (response) =>
          new URL(response.url()).pathname === "/api/v1/accounts/current" &&
          response.status() === 200,
      );
      const freshImage = expiryPage.waitForResponse(
        (response) =>
          new URL(response.url()).pathname.startsWith("/api/v1/avatars/") &&
          response.status() === 200,
      );
      await expiryPage
        .getByRole("button", { name: "重新加载头像", exact: true })
        .click();
      const current = await freshCurrent;
      const image = await freshImage;
      expect(current.headers()["cache-control"]).toBe("no-store");
      expect(current.headers()["set-cookie"]).toBeUndefined();
      const fresh = (await current.json()) as { account: Account };
      expect(fresh.account).toMatchObject({
        id: captured!.id,
        personId: captured!.personId,
        revisionId: captured!.revisionId,
        name: captured!.name,
        idNumber: captured!.idNumber,
        avatar: {
          revision: captured!.avatar.revision,
          mediaType: "image/webp",
          width: 512,
          height: 512,
        },
      });
      expect(fresh.account.avatar.href !== captured!.avatar.href).toBe(true);
      expect(image.headers()["content-type"]).toBe("image/webp");
      expect(image.headers()["x-content-type-options"]).toBe("nosniff");
      expect(image.headers()["set-cookie"]).toBeUndefined();
      const lifetime = /^private, max-age=(\d+), immutable$/.exec(
        image.headers()["cache-control"] ?? "",
      );
      expect(lifetime).not.toBeNull();
      expect(Number(lifetime?.[1])).toBeLessThanOrEqual(60);
      const ready = expiryPage.locator('.avatar-slot[data-state="ready"]');
      await expect(ready).toBeVisible();
      await expect(ready.locator("img")).toHaveAttribute(
        "src",
        fresh.account.avatar.href,
      );
      expect(
        await ready
          .locator("img")
          .evaluate((node) => (node as HTMLImageElement).naturalWidth),
      ).toBe(512);
      expect(await cookieDigests(expiryPage)).toEqual(cookiesBefore);
      expect(await selection()).toBe(secondSession!.id);
      expect(mutations).toEqual([]);
      await identityObservation(registeredAccount.personId, "ordinary", "after");
      await expiryPage.screenshot({
        path: "test-results/avatar-expiry-recovered.png",
        fullPage: true,
      });
      await writeFile(
        join(process.env.HALO_TEST_MATERIAL!, "avatar-expiry.json"),
        JSON.stringify({
          status: expired!.status,
          code: expired!.code,
          cacheControl: expired!.cacheControl,
          contentType: expired!.contentType,
          nosniff: expired!.nosniff,
          expiresAt: captured!.avatar.expiresAt,
          currentDelayMs: currentDelay,
          imageDelayMs: imageDelay,
          releasedAfterExpiry: releasedAt > Date.parse(captured!.avatar.expiresAt),
          freshStatus: image.status(),
          freshCacheControl: image.headers()["cache-control"],
        }),
      );
    } finally {
      await expiryContext.close();
    }
  });
  await choose(page, replacement);
  await test.step("institution admission and first number through the real pages", async () => {
    const colleague = await registerRealAccount(
      page,
      "13900000017",
      "Admission Colleague",
      "000000200002298880",
    );
    await admissionAndFirstNumber(page, replacement, colleague, (session) =>
      choose(page, session),
    );
    const other = await registerRealAccount(page, "13900000018", "Badge Observer", "000000200002298899");
    const fixture = await prepareBadgeDirectory(page, replacement, other);
    const submitted = await ownBadgeApplications(page, replacement, colleague, other, (session) => choose(page, session));
    const role = await reviewBadgeApplications(page, {
      manager: replacement, owner: colleague, reviewer: other, fixture, submitted,
      select: (session) => choose(page, session),
    });
    await cancelBadgeApplications(page, {
      manager: replacement, owner: colleague, reviewer: other, role, submitted,
      select: (session) => choose(page, session),
    });
  });
  await ownWorkforce(page, replacement.id, secondSession!.id);
  const recoverTermination = await ownTermination(
    page,
    replacement.id,
    secondSession!.id,
    () => choose(page, secondSession!),
    () => choose(page, replacement),
  );
  await ownNumbers(page, replacement.id, secondSession!.id, () =>
    choose(page, secondSession!),
  );
  await test.step("self cancellation and a lost success response", async () => {
    const cancellationPage = await context.newPage();
    try {
      await ownCancellation(cancellationPage, context, replacement.id);
    } finally {
      await cancellationPage.close();
    }
  });
  await choose(page, replacement);
  await page.goto("/account");
  async function loginAgain(resume: boolean | "service-termination" = false) {
    const remaining = lastSend + 61_000 - Date.now();
    if (remaining > 0) await page.waitForTimeout(remaining);
    let session: { id: string; expiresAt: string } | undefined;
    let operation: string | undefined;
    await page.route(
      "**/api/v1/sessions",
      async (route) => {
        expect(route.request().method()).toBe("POST");
        operation = route.request().headers()["operation-id"];
        const response = await route.fetch();
        expect(response.status()).toBe(200);
        session = (await response.json()).session;
        await route.fulfill({ response });
      },
      { times: 1 },
    );
    if (resume === "service-termination")
      await page.goto("/authentication/codes?return=service-termination");
    const code = await message(
      page,
      "13900000000",
      false,
      resume === "service-termination"
        ? "service-termination"
        : resume
          ? "all-logout-result"
          : false,
    );
    lastSend = Date.now();
    await page
      .getByRole("textbox", { name: "短信验证码", exact: true })
      .fill(code);
    await page.getByRole("button", { name: "验证并继续", exact: true }).click();
    await expect(page).toHaveURL(
      resume === "service-termination"
        ? /\/workforce\/termination\?return=service-termination$/
        : resume
          ? /\/sessions\/logout-all\?return=all-logout-result$/
          : /\/account$/,
    );
    expect(session?.id).toBeTruthy();
    return { session: session!, operation: operation! };
  }
  await test.step("all logout freezes two real sessions and a later login reads that original set", async () => {
    const before = await loginAgain("service-termination");
    await recoverTermination(before.session.id);
    expect(before.session.id).not.toBe(replacement.id);
    await choose(other, replacement);
    await page.bringToFront();
    await page.goto("/account");
    await expect(
      page.getByText("Fixture Operator", { exact: true }),
    ).toBeVisible();
    const accountResponse = await context.request.get(
      "/api/v1/accounts/current",
      {
        headers: {
          "Session-Id": replacement.id,
          "Operation-Id": randomUUID(),
          Origin: new URL(page.url()).origin,
        },
      },
    );
    expect(accountResponse.status()).toBe(200);
    const account = (await accountResponse.json()).account;
    const beforeCookie = (await context.cookies()).find(
      (cookie) => cookie.name === `__Host-nucleus-session-${before.session.id}`,
    )!;
    let operation = "";
    let revokedAt = "";
    let writes = 0;
    let committed!: () => void;
    const commit = new Promise<void>((resolve) => {
      committed = resolve;
    });
    let release!: () => void;
    const released = new Promise<void>((resolve) => {
      release = resolve;
    });
    await page.route(
      "**/api/v1/sessions",
      async (route) => {
        const request = route.request();
        expect(request.method()).toBe("DELETE");
        expect(request.postData()).toBeNull();
        expect(request.headers()["session-id"]).toBe(replacement.id);
        operation = request.headers()["operation-id"];
        writes++;
        const response = await route.fetch();
        expect(response.status()).toBe(200);
        const body = await response.json();
        expect(body.completion).toMatchObject({ id: operation, count: 2 });
        revokedAt = body.completion.revokedAt;
        const cookie = response.headers()["set-cookie"] ?? "";
        expect(
          cookie.startsWith(`__Host-nucleus-session-${replacement.id}=;`),
        ).toBe(true);
        expect(/(?:^|;\s*)(max-age|domain)=/i.test(cookie)).toBe(false);
        expect(
          Date.parse(/(?:^|;\s*)expires=([^;]+)/i.exec(cookie)?.[1] ?? ""),
        ).toBeLessThan(Date.now());
        committed();
        await released;
        await route.abort("connectionclosed");
      },
      { times: 1 },
    );
    await page.getByRole("link", { name: "退出全部登录", exact: true }).click();
    await page
      .getByRole("button", { name: "退出全部登录", exact: true })
      .click();
    await expect(page.getByRole("dialog")).toBeVisible();
    expect(writes).toBe(0);
    await page
      .getByRole("button", { name: "全部退出", exact: true })
      .click();
    await commit;
    await page.getByRole("link", { name: "返回首页", exact: true }).click();
    await page.getByRole("button", { name: "仍然离开", exact: true }).click();
    await expect(page).toHaveURL(/\/$/);
    release();
    expect(
      await page.evaluate(
        () => JSON.parse(sessionStorage.getItem("halo.logout-all")!).binding,
      ),
    ).toBe("pending");
    await page.goto("/sessions/logout-all");
    await expect(
      page.getByRole("heading", { name: "请重新登录", exact: true }),
    ).toBeVisible();
    expect(writes).toBe(1);
    const cookies = await context.cookies();
    expect(
      cookies.some(
        (cookie) => cookie.name === `__Host-nucleus-session-${replacement.id}`,
      ),
    ).toBe(false);
    for (const retained of [beforeCookie, r2Cookie]) {
      expect(cookies.find((cookie) => cookie.name === retained.name)).toEqual(
        retained,
      );
    }
    const invalid = await context.request.get("/api/v1/accounts/current", {
      headers: {
        "Session-Id": before.session.id,
        "Operation-Id": randomUUID(),
        Origin: new URL(page.url()).origin,
      },
    });
    expect(invalid.status()).toBe(401);
    const foreign = await context.request.get(
      `/api/v1/revocations?operation_id=${operation}`,
      {
        headers: {
          "Session-Id": secondSession!.id,
          "Operation-Id": randomUUID(),
          Origin: new URL(page.url()).origin,
        },
      },
    );
    expect(foreign.status()).toBe(200);
    expect(await foreign.json()).toEqual({
      code: "ok",
      count: 0,
      revocations: [],
      original: null,
    });
    expect(foreign.headers()["set-cookie"]).toBeUndefined();
    const link = page.getByRole("link", { name: "手机号登录", exact: true });
    await expect(link).toHaveAttribute(
      "href",
      "/authentication/codes?return=all-logout-result",
    );
    await link.click();
    await page.getByRole("button", { name: "仍然离开", exact: true }).click();
    const later = await loginAgain(true);
    const expected = [replacement.id, before.session.id]
      .sort()
      .map((id) => ({ id, revokedAt }));
    let reads = 0;
    await page.route("**/api/v1/revocations?*", async (route) => {
      const request = route.request();
      expect(new URL(request.url()).searchParams.get("operation_id")).toBe(
        operation,
      );
      expect(request.headers()["operation-id"]).not.toBe(operation);
      expect(request.headers()["session-id"]).toBe(later.session.id);
      reads++;
      const response = await route.fetch({
        headers: {
          ...(await request.allHeaders()),
          Origin: new URL(page.url()).origin,
        },
      });
      expect(response.status()).toBe(200);
      expect(response.headers()["set-cookie"]).toBeUndefined();
      expect(await response.json()).toEqual({
        code: "ok",
        count: 2,
        revocations: expected,
        original: { scope: "all", initiatorId: replacement.id },
      });
      await route.fulfill({ response });
    });
    await page.reload();
    await expect(
      page.getByRole("button", { name: "查看退出结果", exact: true }),
    ).toBeVisible();
    expect(reads).toBe(0);
    const laterCookie = (await context.cookies()).find(
      (cookie) => cookie.name === `__Host-nucleus-session-${later.session.id}`,
    )!;
    await page
      .getByRole("button", { name: "查看退出结果", exact: true })
      .click();
    await expect(
      page.getByRole("heading", { name: "此前的退出已完成", exact: true }),
    ).toBeVisible();
    expect(reads).toBe(1);
    expect(writes).toBe(1);
    expect(
      (await context.cookies()).find(
        (cookie) => cookie.name === laterCookie.name,
      ),
    ).toEqual(laterCookie);
    await writeFile(
      join(process.env.HALO_TEST_MATERIAL!, "logout-all.json"),
      JSON.stringify({
        operation,
        original: replacement.id,
        account: { id: account.id, personId: account.personId },
        sessions: expected.map((value) => value.id),
        revokedAt,
        replacement: later.session.id,
        authenticationOperation: later.operation,
        interruptedBeforeClassification: true,
        writes,
        reads,
      }),
    );
    await page.screenshot({
      path: "test-results/all-logout-recovered.png",
      fullPage: true,
    });
    await page.unroute("**/api/v1/revocations?*");
    await choose(other, secondSession!);
    const single = await context.newPage();
    await single.goto("/account");
    await expect(
      single.locator(".current-summary").getByText("Second Fixture", { exact: true }),
    ).toBeVisible();
    let singleOperation = "";
    let singleTime = "";
    let singleWrites = 0;
    await single.route(
      "**/api/v1/sessions",
      async (route) => {
        expect(route.request().method()).toBe("DELETE");
        expect(route.request().postData()).toBeNull();
        expect(route.request().headers()["session-id"]).toBe(secondSession!.id);
        singleOperation = route.request().headers()["operation-id"];
        singleWrites++;
        const response = await route.fetch();
        expect(response.status()).toBe(200);
        const body = await response.json();
        expect(body.completion).toMatchObject({
          id: singleOperation,
          count: 1,
        });
        singleTime = body.completion.revokedAt;
        await single.evaluate(() => {
          const set = Storage.prototype.setItem;
          Storage.prototype.setItem = function (key, value) {
            if (this === sessionStorage && key === "halo.logout-all")
              throw new DOMException(
                "Fixture terminal save refusal",
                "QuotaExceededError",
              );
            return set.call(this, key, value);
          };
        });
        await route.fulfill({ response });
      },
      { times: 1 },
    );
    await single
      .getByRole("link", { name: "退出全部登录", exact: true })
      .click();
    await single
      .getByRole("button", { name: "退出全部登录", exact: true })
      .click();
    await single
      .getByRole("button", { name: "全部退出", exact: true })
      .click();
    await expect(
      single.getByRole("heading", { name: "登录信息暂时无法使用", exact: true }),
    ).toBeVisible();
    expect(
      await single.evaluate(
        () => JSON.parse(sessionStorage.getItem("halo.logout-all")!).binding,
      ),
    ).toBe("pending");
    await single.reload();
    await expect(
      single.getByRole("heading", { name: "请重新登录", exact: true }),
    ).toBeVisible();
    await single.getByRole("link", { name: "手机号登录", exact: true }).click();
    await single.getByRole("button", { name: "仍然离开", exact: true }).click();
    let singleReplacement: { id: string; expiresAt: string } | undefined;
    let singleAuthentication = "";
    await single.route(
      "**/api/v1/sessions",
      async (route) => {
        expect(route.request().method()).toBe("POST");
        singleAuthentication = route.request().headers()["operation-id"];
        const response = await route.fetch();
        expect(response.status()).toBe(200);
        singleReplacement = (await response.json()).session;
        await route.fulfill({ response });
      },
      { times: 1 },
    );
    const singleCode = await message(
      single,
      "13900000013",
      false,
      "all-logout-result",
    );
    await single
      .getByRole("textbox", { name: "短信验证码", exact: true })
      .fill(singleCode);
    await single
      .getByRole("button", { name: "验证并继续", exact: true })
      .click();
    await expect(single).toHaveURL(
      /\/sessions\/logout-all\?return=all-logout-result$/,
    );
    await single.reload();
    const singleCookie = (await context.cookies()).find(
      (cookie) =>
        cookie.name === `__Host-nucleus-session-${singleReplacement!.id}`,
    )!;
    expect(singleCookie).toBeTruthy();
    let singleReads = 0;
    await single.route("**/api/v1/revocations?*", async (route) => {
      expect(
        new URL(route.request().url()).searchParams.get("operation_id"),
      ).toBe(singleOperation);
      expect(route.request().headers()["operation-id"]).not.toBe(
        singleOperation,
      );
      expect(route.request().headers()["session-id"]).toBe(
        singleReplacement!.id,
      );
      singleReads++;
      const response = await route.fetch({
        headers: {
          ...(await route.request().allHeaders()),
          Origin: new URL(single.url()).origin,
        },
      });
      expect(response.status()).toBe(200);
      expect(response.headers()["set-cookie"]).toBeUndefined();
      expect(await response.json()).toEqual({
        code: "ok",
        original: { scope: "all", initiatorId: secondSession!.id },
        count: 1,
        revocations: [{ id: secondSession!.id, revokedAt: singleTime }],
      });
      await route.fulfill({ response });
    });
    expect(singleReads).toBe(0);
    await single
      .getByRole("button", { name: "查看退出结果", exact: true })
      .click();
    await expect(
      single.getByRole("heading", { name: "此前的退出已完成", exact: true }),
    ).toBeVisible();
    expect(singleReads).toBe(1);
    expect(
      (await context.cookies()).find(
        (cookie) => cookie.name === singleCookie.name,
      ),
    ).toEqual(singleCookie);
    expect(singleWrites).toBe(1);
    expect(
      (await context.cookies()).some((cookie) => cookie.name === r2Cookie.name),
    ).toBe(false);
    expect(
      (await context.cookies()).find(
        (cookie) => cookie.name === laterCookie.name,
      ),
    ).toEqual(laterCookie);
    await writeFile(
      join(process.env.HALO_TEST_MATERIAL!, "logout-all-single.json"),
      JSON.stringify({
        operation: singleOperation,
        original: secondSession!.id,
        account: {
          id: registeredAccount.id,
          personId: registeredAccount.personId,
        },
        sessions: [secondSession!.id],
        revokedAt: singleTime,
        replacement: singleReplacement!.id,
        authenticationOperation: singleAuthentication,
        classificationSaveFailed: true,
        reads: singleReads,
        writes: singleWrites,
      }),
    );
    await single.close();
  });
  await other.close();
});

async function ownCancellation(page: Page, context: BrowserContext, manager: string) {
  const successName = "Cancellation Success";
  const lostName = "Cancellation Unknown";
  const replacementName = "Cancellation Renewal";
  const successMobile = "13900000015";
  const lostMobile = "13900000016";
  const admit = async (target: Page, mobile: string) => {
    const grant = await target.evaluate(async ({ manager, mobile }) => {
      const post = async (path: string, body: unknown) => {
        const response = await fetch(path, {
          method: "POST",
          credentials: "same-origin",
          headers: { "Operation-Id": crypto.randomUUID(), "Session-Id": manager, "Content-Type": "application/json" },
          body: JSON.stringify(body),
        });
        const value = await response.json();
        if (!response.ok) throw Error(`${path}: ${response.status} ${value.code}`);
        return value;
      };
      const candidate = await post("/api/v1/workforce/grants/candidates", { entry: "institution", mobile });
      const role = candidate.roles.find((value: { kind: string }) => value.kind === "default");
      if (!role) throw Error("Missing ordinary Workforce role");
      const receipt = await post("/api/v1/workforce/grants", {
        entry: "institution",
        target: {
          accountId: candidate.identity.accountId,
          personId: candidate.identity.personId,
          revisionId: candidate.identity.revisionId,
          mobile,
          assignmentId: candidate.assignmentId,
          suspensionId: candidate.suspensionId,
          grant: candidate.grant,
        },
        role: { id: role.id, changeId: role.changeId },
      });
      return receipt.grant.id as string;
    }, { manager, mobile });
    const selection = await selectedSession(target);
    const response = await context.request.get("/api/v1/workforce/current/grant", {
      headers: { "Session-Id": selection.id, "Operation-Id": randomUUID(), Origin: new URL(target.url()).origin },
    });
    expect(response.status()).toBe(200);
    expect((await response.json()).grant).toMatchObject({ id: grant, status: "active" });
    return grant;
  };
  const observe = async (phase: string, value: unknown) => {
    const path = join(
      process.env.HALO_TEST_MATERIAL!,
      `cancellation-${phase}`,
    );
    await writeFile(`${path}.pending`, JSON.stringify(value));
    await rename(`${path}.pending`, `${path}.request`);
    await expect
      .poll(() => readFile(`${path}.ack`, "utf8").catch(() => ""), {
        timeout: 10_000,
      })
      .toBe("ok");
    await writeFile(
      `test-results/cancellation-${phase}.json`,
      JSON.stringify({ request: value, acknowledgement: "ok" }),
    );
  };
  const openEntry = async (target: Page, name: string) => {
    await target.goto("/account");
    await expect(target.getByText(name, { exact: true })).toBeVisible();
    await target
      .getByRole("link", { name: "注销账户", exact: true })
      .click();
    await expect(target).toHaveURL(/\/account\/cancellation$/);
    await expect(
      target.getByRole("heading", { level: 1, name: "注销账户", exact: true }),
    ).toBeVisible();
    await expect(target.getByText(name, { exact: true })).toBeVisible();
    await expect(
      target.getByRole("heading", { name: "这不是退出登录", exact: true }),
    ).toBeVisible();
  };
  const confirmOpen = async (target: Page) => {
    const trigger = target.getByRole("button", {
      name: "注销账户",
      exact: true,
    });
    await trigger.click();
    await expect(target.getByRole("dialog")).toBeVisible();
    await expect(
      target.getByRole("button", { name: "暂不注销", exact: true }),
    ).toBeFocused();
    await target.keyboard.press("Escape");
    await expect(target.getByRole("dialog")).toHaveCount(0);
    await expect(trigger).toBeFocused();
    await trigger.click();
    await expect(target.getByRole("dialog")).toBeVisible();
  };
  const material = async (target: Page) =>
    target.evaluate(() =>
      JSON.parse(sessionStorage.getItem("halo.account-cancellation")!),
    );

  const success = await registerRealAccount(
    page,
    successMobile,
    successName,
    "00000020000229015X",
  );
  const successGrant = await admit(page, successMobile);
  await openEntry(page, successName);
  await confirmOpen(page);
  let successPosts = 0;
  let successOperation = "";
  let successResult: { id: string; cancelledAt: string } | undefined;
  await page.route(
    "**/api/v1/accounts/current/cancellations",
    async (route) => {
      const request = route.request();
      expect(request.method()).toBe("POST");
      successPosts += 1;
      expect(successPosts).toBe(1);
      successOperation = request.headers()["operation-id"];
      expect(successOperation).toMatch(/^[0-9a-f-]{36}$/i);
      expect(request.headers()["session-id"]).toBe(success.session.id);
      expect(request.headers()["origin"]).toBe(new URL(page.url()).origin);
      expect(request.headers()["content-type"]).toBe("application/json");
      expect(request.headers()["accept"]).toBe("application/json");
      expect(request.postDataJSON()).toEqual({
        accountId: success.account.id,
        personId: success.account.personId,
        revisionId: success.account.revisionId,
      });
      const response = await route.fetch({
        maxRedirects: 0,
        maxRetries: 0,
      });
      expect(response.status()).toBe(200);
      const body = await response.json();
      expect(body.code).toBe("ok");
      const cancellation = body.cancellation as {
        id: string;
        cancelledAt: string;
      };
      expect(Number.isFinite(Date.parse(cancellation.cancelledAt))).toBe(true);
      successResult = cancellation;
      const cookie = response.headers()["set-cookie"] ?? "";
      expect(
        cookie.startsWith(
          `__Host-nucleus-session-${success.session.id}=;`,
        ),
      ).toBe(true);
      await observe("success-after", {
        operation: successOperation,
        account: success.account.id,
        person: success.account.personId,
        revision: success.account.revisionId,
        session: success.session.id,
        grant: successGrant,
        cancellation: cancellation.id,
        cancelledAt: cancellation.cancelledAt,
        posts: successPosts,
      });
      await route.fulfill({ response });
    },
  );
  await page.getByRole("button", { name: "确认注销", exact: true }).click();
  await expect(
    page.getByRole("heading", { level: 1, name: "账户已注销", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText("全部登录和员工服务已结束，原账户无法恢复。", {
      exact: true,
    }),
  ).toBeVisible();
  expect(successPosts).toBe(1);
  expect(
    (await context.cookies()).some(
      (cookie) =>
        cookie.name === `__Host-nucleus-session-${success.session.id}`,
    ),
  ).toBe(false);
  const successMaterial = await material(page);
  expect(Object.keys(successMaterial).sort()).toEqual(
    [
      "version",
      "operation",
      "target",
      "selection",
      "phase",
      "outcome",
      "notice",
      "retryAt",
      "result",
      "hidden",
      "clearedRevision",
    ].sort(),
  );
  expect(successMaterial).toMatchObject({
    version: 1,
    operation: successOperation,
    target: {
      accountId: success.account.id,
      personId: success.account.personId,
      revisionId: success.account.revisionId,
    },
    selection: { id: success.session.id },
    phase: "possibly-sent",
    outcome: "complete",
    notice: "pending",
    retryAt: 0,
    hidden: false,
  });
  expect(successMaterial.result).toEqual({
    id: successResult!.id,
    cancelledAt: successResult!.cancelledAt,
  });
  expect(typeof successMaterial.clearedRevision).toBe("string");
  const successSaved = JSON.stringify(successMaterial);
  expect(successSaved).not.toContain(successName);
  expect(successSaved).not.toContain(successMobile);
  expect(successSaved).not.toContain("20000229015X");
  await page.reload();
  await expect(
    page.getByRole("heading", { level: 1, name: "账户已注销", exact: true }),
  ).toBeVisible();
  expect(successPosts).toBe(1);

  const lostPage = await context.newPage();
  const lost = await registerRealAccount(
    lostPage,
    lostMobile,
    lostName,
    "000000200002290168",
  );
  const lostGrant = await admit(lostPage, lostMobile);
  await openEntry(lostPage, lostName);
  await confirmOpen(lostPage);
  let lostPosts = 0;
  let lostOperation = "";
  let lostResult: { id: string; cancelledAt: string } | undefined;
  await lostPage.route(
    "**/api/v1/accounts/current/cancellations",
    async (route) => {
      const request = route.request();
      expect(request.method()).toBe("POST");
      lostPosts += 1;
      expect(lostPosts).toBe(1);
      lostOperation = request.headers()["operation-id"];
      expect(request.headers()["session-id"]).toBe(lost.session.id);
      expect(request.headers()["origin"]).toBe(
        new URL(lostPage.url()).origin,
      );
      expect(request.postDataJSON()).toEqual({
        accountId: lost.account.id,
        personId: lost.account.personId,
        revisionId: lost.account.revisionId,
      });
      const response = await route.fetch({
        maxRedirects: 0,
        maxRetries: 0,
      });
      expect(response.status()).toBe(200);
      const body = await response.json();
      expect(body.code).toBe("ok");
      const cancellation = body.cancellation as {
        id: string;
        cancelledAt: string;
      };
      lostResult = cancellation;
      // The real transaction committed before this acknowledgement. The
      // browser response is then dropped, so the page cannot use that fact.
      await observe("lost-after", {
        operation: lostOperation,
        account: lost.account.id,
        person: lost.account.personId,
        revision: lost.account.revisionId,
        session: lost.session.id,
        grant: lostGrant,
        cancellation: cancellation.id,
        cancelledAt: cancellation.cancelledAt,
        posts: lostPosts,
      });
      await route.abort("connectionclosed");
    },
  );
  await lostPage.getByRole("button", { name: "确认注销", exact: true }).click();
  await expect(
    lostPage.getByRole("heading", {
      level: 1,
      name: "注销结果尚不确定",
      exact: true,
    }),
  ).toBeVisible();
  await expect(
    lostPage.getByText("暂时不要重复提交，可先检查登录状态。", { exact: true }),
  ).toBeVisible();
  expect(lostPosts).toBe(1);
  const lostMaterial = await material(lostPage);
  expect(lostMaterial).toMatchObject({
    operation: lostOperation,
    phase: "possibly-sent",
    outcome: "pending",
    notice: "unknown",
    result: null,
    hidden: false,
    clearedRevision: null,
  });
  let currentReads = 0;
  const observeCurrent = (request: Request) => {
    if (
      request.method() === "GET" &&
      new URL(request.url()).pathname === "/api/v1/accounts/current"
    )
      currentReads += 1;
  };
  lostPage.on("request", observeCurrent);
  await lostPage
    .getByRole("button", { name: "检查登录状态", exact: true })
    .click();
  await expect(
    lostPage.getByRole("heading", {
      level: 2,
      name: "请重新登录",
      exact: true,
    }),
  ).toBeVisible();
  expect(currentReads).toBe(1);
  expect(lostPosts).toBe(1);
  lostPage.off("request", observeCurrent);
  const hiddenMaterial = await material(lostPage);
  expect(hiddenMaterial).toMatchObject({
    operation: lostOperation,
    outcome: "pending",
    hidden: false,
    result: null,
  });
  expect(await lostPage.getByText("账户已注销", { exact: true }).count()).toBe(
    0,
  );
  const loginLink = lostPage.getByRole("link", {
    name: "手机号登录",
    exact: true,
  });
  await expect(loginLink).toBeVisible();
  await loginLink.click();
  await expect(
    lostPage.getByRole("heading", { name: "注销结果尚未确认", exact: true }),
  ).toBeVisible();
  await lostPage.getByRole("button", { name: "离开页面", exact: true }).click();
  await expect(lostPage).toHaveURL(/\/authentication\/codes$/);
  const remaining = lost.sentAt + 61_000 - Date.now();
  if (remaining > 0) await lostPage.waitForTimeout(remaining);
  const replacement = await registerRealAccount(
    lostPage,
    lostMobile,
    replacementName,
    "000000200002290176",
  );
  expect(replacement.account.id).not.toBe(lost.account.id);
  expect(replacement.account.personId).not.toBe(lost.account.personId);
  await lostPage.goto("/account/cancellation");
  await expect(
    lostPage.getByRole("heading", {
      level: 1,
      name: "账户状态",
      exact: true,
    }),
  ).toBeVisible();
  expect(
    await lostPage.getByText("账户已注销", { exact: true }).count(),
  ).toBe(0);
  expect(lostPosts).toBe(1);
  const laterMaterial = await material(lostPage);
  expect(laterMaterial).toMatchObject({
    operation: lostOperation,
    outcome: "pending",
    hidden: true,
    result: null,
  });
  await observe("lost-later", {
    operation: lostOperation,
    account: lost.account.id,
    person: lost.account.personId,
    revision: lost.account.revisionId,
    session: lost.session.id,
    grant: lostGrant,
    cancellation: lostResult!.id,
    cancelledAt: lostResult!.cancelledAt,
    posts: lostPosts,
    replacement: {
      account: replacement.account.id,
      person: replacement.account.personId,
    },
    sameMobile: true,
  });
  await lostPage.setViewportSize({ width: 390, height: 900 });
  await lostPage.screenshot({
    path: "test-results/cancellation-unknown-390.png",
    fullPage: true,
  });
  await lostPage.close();
}

for (const denied of ["sessionStorage", "BroadcastChannel"] as const) {
  test(`${denied} refusal leaves a controlled login state without a request`, async ({
    page,
  }) => {
    const errors: string[] = [];
    const writes: string[] = [];
    page.on("pageerror", (error) => errors.push(error.message));
    page.on("request", (request) => {
      if (request.method() === "POST") writes.push(request.url());
    });
    await page.addInitScript((name) => {
      Object.defineProperty(window, name, {
        configurable: true,
        get() {
          throw new DOMException("Fixture browser refusal", "SecurityError");
        },
      });
    }, denied);
    await page.goto("/authentication/login");
    await expect(
      page.getByRole("heading", { name: "登录信息暂时无法使用" }),
    ).toBeVisible();
    await expect(page.getByRole("button", { name: "重新加载" })).toBeVisible();
    expect(errors).toEqual([]);
    expect(writes).toEqual([]);
    if (denied === "BroadcastChannel") {
      await page.goto("/account");
      await expect(
        page.getByRole("heading", { name: "登录信息暂时无法使用" }),
      ).toBeVisible();
      expect(errors).toEqual([]);
    }
  });
}

for (const path of ["/authentication/login", "/account"]) {
  test(`an early stylesheet failure stops ${path} before private work`, async ({
    page,
  }) => {
    const business: string[] = [];
    page.on("request", (request) => {
      if (/\/api\/v1\/(?:sessions|accounts)/.test(request.url()))
        business.push(request.url());
    });
    await page.route("**/assets/**/*.css", (route) => route.abort("failed"));
    await page.goto(path);
    await expect(
      page.getByRole("heading", {
        name: "请重新加载",
        exact: true,
      }),
    ).toBeVisible();
    expect(business).toEqual([]);
  });
}
