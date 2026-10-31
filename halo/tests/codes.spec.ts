import { test, expect, type Page, type Route } from "@playwright/test";
import { readFile } from "node:fs/promises";

const widget = `
window.__widgets = [];
window.turnstile = {
  render(element, options) {
    const id = String(window.__widgets.length);
    const button = document.createElement("button");
    button.type = "button";
    button.textContent = "完成安全验证（测试替身）";
    button.onclick = () => options.callback(options.cData);
    element.append(button);
    window.__widgets.push({ options, element, button, removed: false, resets: 0 });
    return id;
  },
  reset(id) {
    const widget = window.__widgets[Number(id)];
    widget.resets++;
    widget.options["expired-callback"]();
  },
  remove(id) {
    const widget = window.__widgets[Number(id)];
    widget.removed = true;
    widget.element.replaceChildren();
  }
};`;

test.beforeEach(async ({ page }) => {
  await page.route(
    "https://challenges.cloudflare.com/turnstile/v0/api.js?render=explicit",
    (route) => route.fulfill({ contentType: "text/javascript", body: widget }),
  );
});

async function prepare(page: Page, mobile: string) {
  await page.setExtraHTTPHeaders({ "X-Halo-Fixture-Source": mobile.slice(-2) });
  await page.goto("/authentication/codes");
  await page.getByLabel("手机号", { exact: true }).fill(mobile);
  await page.getByRole("button", { name: "完成安全验证（测试替身）" }).click();
}

async function submit(page: Page) {
  await page.getByRole("button", { name: "获取验证码" }).click();
}

for (const delivery of ["sent", "unknown", "failed"] as const) {
  test(`terminal ${delivery} handoff hides login at its exact saved deadline`, async ({
    page,
  }) => {
    await page.clock.install();
    await page.goto("/authentication/codes");
    await page.evaluate((delivery) => {
      const now = Date.now();
      sessionStorage.setItem(
        "halo.c1",
        JSON.stringify({ version: 1, closed: true, delivery }),
      );
      sessionStorage.setItem(
        "halo.login.message",
        JSON.stringify({
          version: 1,
          operation: crypto.randomUUID(),
          mobile: "13900000000",
          message: {
            id: crypto.randomUUID(),
            delivery,
            expiresAt: new Date(now + 10_000).toISOString(),
          },
          createdAt: now,
          expiresAt: now + 10_000,
        }),
      );
    }, delivery);
    await page.reload();
    const action = page.getByRole("link", { name: "输入短信验证码" });
    if (delivery === "failed") await expect(action).toHaveCount(0);
    else await expect(action).toBeVisible();
    await page.clock.runFor(9000);
    if (delivery !== "failed") await expect(action).toBeVisible();
    await page.clock.runFor(1000);
    await expect(action).toHaveCount(0);
    await expect
      .poll(() =>
        page.evaluate(() => sessionStorage.getItem("halo.login.message")),
      )
      .toBeNull();
  });
}

async function rejectBeforeAdmission(
  route: Route,
  code: "invalid_input" | "permission_denied",
) {
  const headers = { ...route.request().headers() };
  if (code === "permission_denied") headers.origin = "https://invalid.example";
  else headers["content-length"] = "1";
  const response = await route.fetch({
    headers,
    ...(code === "invalid_input" ? { postData: "{" } : {}),
  });
  expect((await response.json()).code).toBe(code);
  await route.fulfill({ response });
}

test("every matched codes URL permits the verification widget", async ({
  page,
}) => {
  for (const path of [
    "/authentication/codes",
    "/authentication/codes/",
    "/AUTHENTICATION/CODES",
  ]) {
    const response = await page.goto(path);
    expect(response?.status()).toBe(200);
    const policy = response?.headers()["content-security-policy"];
    expect(policy).toContain("frame-src https://challenges.cloudflare.com");
    expect(
      policy
        ?.split("; ")
        .find((directive) => directive.startsWith("script-src ")),
    ).toContain("https://challenges.cloudflare.com");
    await expect(
      page.getByRole("button", { name: "完成安全验证（测试替身）" }),
    ).toBeVisible();
  }
});

for (const code of ["invalid_input", "permission_denied"] as const) {
  test(`first-attempt ${code} permits explicit correction after reload`, async ({
    page,
  }) => {
    let requests = 0;
    await page.route("**/api/v1/codes", async (route) => {
      requests++;
      await rejectBeforeAdmission(route, code);
    });
    await prepare(page, "13900000000");
    const original = await page.evaluate(
      () => (window as any).__widgets[0].options.cData,
    );
    await submit(page);
    const title =
      code === "invalid_input" ? "请检查手机号" : "暂时无法获取验证码";
    await expect(
      page.getByRole("heading", { name: title, exact: true }),
    ).toBeVisible();
    await page.reload();
    await page.getByRole("button", { name: "重新填写并验证" }).click();
    await expect(page.getByLabel("手机号", { exact: true })).toBeFocused();
    await expect(
      page.getByRole("button", { name: "完成安全验证（测试替身）" }),
    ).toBeVisible();
    expect(
      await page.evaluate(() => (window as any).__widgets[0].options.cData),
    ).not.toBe(original);
    expect(requests).toBe(1);
  });
}

for (const [index, code] of (
  [
    "invalid_input",
    "permission_denied",
    "authentication_invalid",
    "request_mismatch",
  ] as const
).entries()) {
  test(`a replay ${code} cannot imply that the original SMS was not sent`, async ({
    page,
  }) => {
    const requests: { operation: string; input: string }[] = [];
    await page.route("**/api/v1/codes", async (route) => {
      requests.push({
        operation: route.request().headers()["operation-id"],
        input: route.request().postData()!,
      });
      if (requests.length === 1) {
        const response = await route.fetch();
        expect(response.status()).toBe(200);
        await route.abort("connectionreset");
      } else if (code === "invalid_input" || code === "permission_denied") {
        await rejectBeforeAdmission(route, code);
      } else {
        const input = route.request().postDataJSON();
        const response = await route.fetch({
          postData: {
            ...input,
            ...(code === "authentication_invalid"
              ? { token: "different-recovery-proof" }
              : { mobile: "13900000000" }),
          },
        });
        expect((await response.json()).code).toBe(code);
        await route.fulfill({ response });
      }
    });
    await prepare(page, `139000000${String(index + 9).padStart(2, "0")}`);
    await submit(page);
    await expect(
      page.getByRole("heading", { name: "暂时无法确认发送结果" }),
    ).toBeVisible();
    await page.reload();
    await expect(
      page.getByRole("heading", { name: "暂时无法确认发送结果" }),
    ).toBeVisible();
    expect(requests).toHaveLength(1);
    await page
      .getByRole("button", { name: "重试获取验证码", exact: true })
      .click();
    const title = "暂时无法查看发送结果";
    await expect(page.getByRole("heading", { name: title })).toBeVisible();
    expect(requests).toHaveLength(2);
    expect(requests[1]).toEqual(requests[0]);
    await page.reload();
    await expect(page.getByRole("heading", { name: title })).toBeVisible();
    const saved = await page.evaluate(() =>
      JSON.parse(sessionStorage.getItem("halo.c1")!),
    );
    expect(saved.operation).toBe(requests[0].operation);
    expect(saved.input).toEqual(JSON.parse(requests[0].input));
    await expect(
      page.getByRole("button", {
        name: /重试获取验证码|继续获取验证码|重新填写|重新验证/,
      }),
    ).toHaveCount(0);
    expect(requests).toHaveLength(2);
  });
}

test("normal supplier acceptance, terminal reload and real quota refusal", async ({
  page,
  context,
}) => {
  await prepare(page, "13900000001");
  await submit(page);
  await expect(
    page.getByRole("heading", { name: "验证码已提交发送" }),
  ).toBeFocused();
  await expect(page.getByText("请查看手机短信。")).toBeVisible();
  await expect(
    page.getByText("提交发送不代表已送达。请勿向他人透露验证码。", { exact: true }),
  ).toBeVisible();
  expect(
    await page.evaluate(() => sessionStorage.getItem("halo.c1")),
  ).not.toContain("token");
  await page.screenshot({ path: "test-results/sent.png", fullPage: true });
  for (const width of [1440, 768, 390, 320]) {
    await page.setViewportSize({ width, height: 950 });
    const title = page.getByRole("heading", { name: "验证码已提交发送" });
    await expect(title).toHaveAttribute("data-result-title", "true");
    await expect(title).toHaveAttribute("tabindex", "-1");
    await expect(title).toBeFocused();
    await page.keyboard.press("Tab");
    await expect(
      page.getByRole("link", { name: "输入短信验证码", exact: true }),
    ).toBeFocused();
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    ).toBe(true);
    console.log(
      "Result layout",
      await page.locator(".result").evaluate((result) => ({
        viewport: innerWidth,
        width: result.getBoundingClientRect().width,
        title: result.querySelector("h1")!.textContent,
        iconHidden: result
          .querySelector(".result-icon")!
          .getAttribute("aria-hidden"),
        notice: result.querySelector(".notice")!.textContent,
        actions: result.querySelector(".result-actions")!.textContent,
        copy: result.querySelector(".result-copy")!.textContent,
        focused: document.activeElement!.textContent,
      })),
    );
    await page.screenshot({
      path: `test-results/sent-${width}.png`,
      fullPage: true,
    });
    await title.focus();
  }
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "验证码已提交发送" }),
  ).toBeVisible();
  const other = await context.newPage();
  await other.route(
    "https://challenges.cloudflare.com/turnstile/v0/api.js?render=explicit",
    (route) => route.fulfill({ contentType: "text/javascript", body: widget }),
  );
  await prepare(other, "13900000001");
  await submit(other);
  await expect(
    other.getByRole("heading", { name: "请稍后再试", exact: true }),
  ).toBeVisible();
  await expect(
    other.getByRole("button", { name: /秒后可重试/ }),
  ).toBeDisabled();
  await other.close();
});

test("lost response and refresh reuse the exact input and original proof", async ({
  page,
}) => {
  const requests: { operation: string; input: string }[] = [];
  await page.route("**/api/v1/codes", async (route) => {
    requests.push({
      operation: route.request().headers()["operation-id"],
      input: route.request().postData()!,
    });
    if (requests.length === 1) {
      const response = await route.fetch();
      expect(response.status()).toBe(200);
      await route.abort("connectionclosed");
    } else await route.continue();
  });
  await prepare(page, "13900000002");
  await submit(page);
  await expect(
    page.getByRole("heading", { name: "暂时无法确认发送结果" }),
  ).toBeVisible();
  await page.evaluate(() => {
    const widgets = (window as any).__widgets;
    widgets[0].options.callback("late-new-candidate");
    widgets[0].options["expired-callback"]();
  });
  const saved = await page.evaluate(() =>
    JSON.parse(sessionStorage.getItem("halo.c1")!),
  );
  expect(saved.input.token).toBe(requests[0].operation);
  await expect(
    page.getByText("请先查看手机短信，稍后可重试获取验证码。"),
  ).toBeVisible();
  await page.screenshot({ path: "test-results/pending.png", fullPage: true });
  const viewport = page.viewportSize()!;
  await page.setViewportSize({ width: 390, height: 950 });
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBe(true);
  await page.screenshot({
    path: "test-results/pending-narrow.png",
    fullPage: true,
  });
  await page.setViewportSize(viewport);
  await expect(
    page.getByRole("button", { name: "重试获取验证码", exact: true }),
  ).toBeEnabled();
  await page.route("**/build.json", (route) =>
    route.fulfill({
      contentType: "application/json",
      body: JSON.stringify({ buildId: "new-foreground-build" }),
    }),
  );
  const checkedAt = await page.evaluate(() => {
    if (document.hidden) throw new Error("The fixture page must be visible");
    const now = Date.now();
    document.dispatchEvent(new Event("visibilitychange"));
    return now;
  });
  await expect(page.getByRole("heading", { name: "请重新加载" })).toBeVisible();
  const postponed = await page.evaluate(
    () => JSON.parse(sessionStorage.getItem("halo.c1")!).retryAt,
  );
  expect(postponed).toBeGreaterThanOrEqual(checkedAt + 5000);
  await page.unroute("**/build.json");
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "暂时无法确认发送结果" }),
  ).toBeVisible();
  expect(requests.length).toBe(1);
  await expect(
    page.getByRole("button", { name: /秒后可重试/ }),
  ).toBeDisabled();
  await page.getByRole("button", { name: "重试获取验证码", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "验证码已提交发送" }),
  ).toBeVisible();
  expect(requests).toHaveLength(2);
  expect(requests[1]).toEqual(requests[0]);
});

test("PNVS refusal and unknown supplier outcome are distinct", async ({
  page,
}) => {
  await prepare(page, "13900000003");
  await submit(page);
  await expect(
    page.getByRole("heading", { name: "验证码未能发送" }),
  ).toBeVisible();
  await expect(page.getByRole("link", { name: "输入短信验证码" })).toHaveCount(
    0,
  );
  await expect(
    page.getByRole("button", { name: "重新获取验证码", exact: true }),
  ).toBeVisible();
  await page.screenshot({
    path: "test-results/sms-delivery-failed.png",
    fullPage: true,
  });
  let loginPosts = 0;
  page.on("request", (request) => {
    if (
      request.method() === "POST" &&
      new URL(request.url()).pathname === "/api/v1/sessions"
    )
      loginPosts++;
  });
  await page.goto("/authentication/login");
  await expect(
    page.getByRole("heading", { name: "验证码未能发送", exact: true }),
  ).toBeFocused();
  await expect(page.getByRole("textbox")).toHaveCount(0);
  const resend = page.getByRole("link", {
    name: "返回验证码页面",
    exact: true,
  });
  await page.keyboard.press("Tab");
  await expect(resend).toBeFocused();
  await page.screenshot({
    path: "test-results/login-delivery-failed.png",
    fullPage: true,
  });
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "验证码未能发送", exact: true }),
  ).toBeVisible();
  expect(loginPosts).toBe(0);
  await resend.click();
  await expect(
    page.getByRole("button", { name: "重新获取验证码", exact: true }),
  ).toBeVisible();
  await page.evaluate(() => {
    sessionStorage.clear();
    history.replaceState(null, "");
  });
  await prepare(page, "13900000004");
  await submit(page);
  await expect(
    page.getByRole("heading", { name: "暂时无法确认发送结果" }),
  ).toBeVisible();
  await page.getByRole("button", { name: "重试获取验证码", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "暂时无法确认发送结果" }),
  ).toBeVisible();
  await expect(page.getByRole("button", { name: /获取|重发/ })).toHaveCount(0);
});

for (const [index, fault] of ["media", "shape", "code"].entries()) {
  test(`untrusted ${fault} response never completes the operation`, async ({
    page,
  }) => {
    let requests = 0;
    await page.route("**/api/v1/codes", async (route) => {
      requests++;
      if (requests > 1) {
        await route.continue();
        return;
      }
      const response = await route.fetch();
      expect(response.status()).toBe(200);
      const json = await response.json();
      await route.fulfill({
        status: 200,
        contentType: fault === "media" ? "text/html" : "application/json",
        body: JSON.stringify(
          fault === "shape"
            ? { code: "ok" }
            : fault === "code"
              ? { ...json, code: "unknown_success" }
              : json,
        ),
      });
    });
    await prepare(page, `1390000000${index + 5}`);
    await submit(page);
    await expect(
      page.getByRole("heading", { name: "请重新加载" }),
    ).toBeVisible();
    await expect(
      page.getByRole("heading", { name: "验证码已提交发送" }),
    ).toHaveCount(0);
    await page.getByRole("button", { name: "重新加载" }).click();
    await expect(
      page.getByRole("heading", { name: "暂时无法确认发送结果" }),
    ).toBeVisible();
    expect(requests).toBe(1);
    await page
      .getByRole("button", { name: "重试获取验证码", exact: true })
      .click();
    await expect(
      page.getByRole("heading", { name: "验证码已提交发送" }),
    ).toBeVisible();
    expect(requests).toBe(2);
  });
}

test("a held send expires after full-document navigation home without another SMS", async ({
  page,
}) => {
  let release!: () => void;
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  let received!: () => void;
  const responseReceived = new Promise<void>((resolve) => {
    received = resolve;
  });
  let requests = 0;
  await page.route("**/api/v1/codes", async (route) => {
    requests++;
    if (requests > 1) {
      await route.continue();
      return;
    }
    const response = await route.fetch();
    expect(response.status()).toBe(200);
    received();
    await gate;
    await route.fulfill({ response }).catch(() => {});
  });
  await prepare(page, "13900000008");
  await submit(page);
  await responseReceived;
  const original = await page.evaluate(() =>
    JSON.parse(sessionStorage.getItem("halo.c1")!),
  );
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "暂时无法确认发送结果" }),
  ).toBeVisible();
  const recovered = await page.evaluate(() =>
    JSON.parse(sessionStorage.getItem("halo.c1")!),
  );
  expect({ ...recovered, retryAt: 0 }).toEqual({ ...original, retryAt: 0 });
  expect(recovered.retryAt).toBeGreaterThanOrEqual(original.retryAt);
  await page.clock.install({ time: new Date(original.createdAt + 1000) });
  await page.clock.pauseAt(new Date(original.createdAt + 2000));
  await page.evaluate(() => ((window as any).__c1Document = true));
  await page.getByRole("link", { name: "司籍处首页" }).click();
  await expect(page).toHaveURL(/\/$/);
  expect(
    await page.evaluate(() => (window as any).__c1Document),
  ).toBeUndefined();
  expect(
    await page.evaluate(
      () => JSON.parse(sessionStorage.getItem("halo.c1")!).input,
    ),
  ).toEqual(original.input);
  await page.clock.fastForward(30 * 60 * 1000);
  await expect
    .poll(() => page.evaluate(() => sessionStorage.getItem("halo.c1")))
    .toBe('{"version":1,"closed":true}');
  release();
  await page.getByRole("link", { name: "手机号登录" }).click();
  await expect(
    page.getByRole("heading", { name: "暂时无法查看发送结果" }),
  ).toBeVisible();
  await page.reload();
  await expect(page.getByText("请先查看手机短信。")).toBeVisible();
  await expect(
    page.getByRole("button", { name: /重试获取验证码|获取验证码/ }),
  ).toHaveCount(0);
  expect(requests).toBe(1);
});

for (const path of ["/", "/missing-c1-lifetime-page"]) {
  test(`document startup closes expired C1 material on ${path}`, async ({
    page,
  }) => {
    let requests = 0;
    page.on("request", (request) => {
      if (request.url().includes("/api/v1/")) requests++;
    });
    await page.addInitScript(() => {
      if (sessionStorage.getItem("lifetime-seeded")) return;
      sessionStorage.setItem("lifetime-seeded", "true");
      sessionStorage.setItem("unrelated", "keep-other-material");
      const now = Date.now();
      sessionStorage.setItem(
        "halo.c1",
        JSON.stringify({
          version: 1,
          operation: crypto.randomUUID(),
          input: { mobile: "13900000008", token: "expired-on-startup" },
          createdAt: now - 31 * 60 * 1000,
          expiresAt: now - 60 * 1000,
          retryAt: 0,
        }),
      );
    });
    const response = await page.goto(path);
    expect(response?.status()).toBe(path === "/" ? 200 : 404);
    await expect
      .poll(() => page.evaluate(() => sessionStorage.getItem("halo.c1")))
      .toBe('{"version":1,"closed":true}');
    expect(await page.evaluate(() => sessionStorage.getItem("unrelated"))).toBe(
      "keep-other-material",
    );
    await page.reload();
    expect(await page.evaluate(() => sessionStorage.getItem("halo.c1"))).toBe(
      '{"version":1,"closed":true}',
    );
    expect(requests).toBe(0);
  });
}

for (const event of ["focus", "pageshow", "visibilitychange"]) {
  test(`home ${event} retries expired-material cleanup after denied storage`, async ({
    page,
  }) => {
    let requests = 0;
    page.on("request", (request) => {
      if (request.url().includes("/api/v1/")) requests++;
    });
    await page.clock.install();
    await page.goto("/");
    await page.clock.pauseAt(new Date(Date.now() + 1000));
    await page.evaluate(() => {
      const now = Date.now();
      sessionStorage.setItem(
        "halo.c1",
        JSON.stringify({
          version: 1,
          operation: crypto.randomUUID(),
          input: { mobile: "13900000008", token: "resume-proof" },
          createdAt: now,
          expiresAt: now + 5000,
          retryAt: 0,
        }),
      );
      (window as any).__restoreStorage = () => {
        Storage.prototype.getItem = get;
        Storage.prototype.setItem = set;
      };
      const get = Storage.prototype.getItem;
      const set = Storage.prototype.setItem;
      Storage.prototype.getItem = () => {
        throw new Error("temporarily denied");
      };
      Storage.prototype.setItem = () => {
        throw new Error("temporarily denied");
      };
      // Storage failures must not interrupt the public page.
      window.dispatchEvent(new Event("focus"));
    });
    await page.clock.fastForward(5000);
    await expect(page.getByRole("link", { name: "手机号登录" })).toBeVisible();
    await page.evaluate(() => (window as any).__restoreStorage());
    expect(
      await page.evaluate(() => sessionStorage.getItem("halo.c1")),
    ).toContain("resume-proof");
    await page.evaluate((event) => {
      if (event === "visibilitychange")
        document.dispatchEvent(new Event(event));
      else window.dispatchEvent(new Event(event));
    }, event);
    // Timers remain paused: the resume event itself must perform this cleanup.
    expect(await page.evaluate(() => sessionStorage.getItem("halo.c1"))).toBe(
      '{"version":1,"closed":true}',
    );
    expect(requests).toBe(0);
  });
}

test("storage failure, expired or missing materials never initiate a send", async ({
  page,
}) => {
  let requests = 0;
  page.on("request", (request) => {
    if (request.url().endsWith("/api/v1/codes")) requests++;
  });
  await prepare(page, "13900000009");
  await page.evaluate(() => {
    Storage.prototype.setItem = () => {
      throw new DOMException("fixture quota", "QuotaExceededError");
    };
  });
  await submit(page);
  await expect(
    page.getByRole("heading", { name: "暂时无法获取验证码" }),
  ).toBeVisible();
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "获取短信验证码" }),
  ).toBeVisible();
  await page.evaluate(() => {
    const now = Date.now();
    history.replaceState({ haloC1: true }, "");
    sessionStorage.setItem(
      "halo.c1",
      JSON.stringify({
        version: 1,
        operation: crypto.randomUUID(),
        input: { mobile: "13900000009", token: "expired-fixture" },
        createdAt: now - 31 * 60 * 1000,
        expiresAt: now - 60 * 1000,
        retryAt: 0,
      }),
    );
  });
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "暂时无法查看发送结果" }),
  ).toBeVisible();
  await expect(page.getByText("请先查看手机短信。")).toBeVisible();
  await expect(page.getByText("请先查看手机短信。")).toBeVisible();
  await page.screenshot({ path: "test-results/expired.png", fullPage: true });
  expect(
    await page.evaluate(() => sessionStorage.getItem("halo.c1")),
  ).not.toContain("expired-fixture");
  await page.evaluate(() => sessionStorage.clear());
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "暂时无法查看发送结果" }),
  ).toBeVisible();
  await expect(page.getByText("请先查看手机短信。")).toBeVisible();
  await page.screenshot({ path: "test-results/missing.png", fullPage: true });
  await page.setViewportSize({ width: 390, height: 950 });
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBe(true);
  await page.screenshot({
    path: "test-results/missing-narrow.png",
    fullPage: true,
  });
  expect(requests).toBe(0);
});

test("failed supplier script stays in widget recovery without sending", async ({
  page,
}) => {
  let loads = 0;
  let writes = 0;
  await page.route(
    "https://challenges.cloudflare.com/turnstile/v0/api.js?render=explicit",
    async (route) => {
      loads++;
      if (loads === 1) await route.abort("failed");
      else
        await route.fulfill({ contentType: "text/javascript", body: widget });
    },
  );
  page.on("request", (request) => {
    if (request.url().endsWith("/api/v1/codes")) writes++;
  });
  await page.goto("/authentication/codes");
  await page.getByLabel("手机号", { exact: true }).fill("13900000000");
  await page.getByRole("button", { name: "重新加载安全验证" }).click();
  await page.getByRole("button", { name: "完成安全验证（测试替身）" }).click();
  await expect(page.getByRole("button", { name: "获取验证码" })).toBeEnabled();
  await expect(page.getByLabel("手机号", { exact: true })).toHaveValue(
    "13900000000",
  );
  expect(loads).toBe(2);
  expect(writes).toBe(0);
});

test("replacement verification refreshes settings and ignores the old callback", async ({
  page,
}) => {
  let reads = 0;
  await page.route("**/api/v1/settings", async (route) => {
    const response = await route.fetch();
    const body = await response.json();
    reads++;
    if (reads > 1)
      body.settings.challenge.turnstile.siteKey = "rotated-public-site-key";
    await route.fulfill({ response, json: body });
  });
  await prepare(page, "13900000010");
  const operation = await page.evaluate(
    () => (window as any).__widgets[0].options.cData,
  );
  await page.evaluate(() =>
    (window as any).__widgets[0].options.callback(crypto.randomUUID()),
  );
  await submit(page);
  await expect(
    page.getByRole("heading", { name: "安全验证未通过" }),
  ).toBeVisible();
  await page.getByRole("button", { name: "重新验证" }).click();
  await expect(page.getByLabel("手机号", { exact: true })).toBeFocused();
  await expect(
    page.getByRole("button", { name: "完成安全验证（测试替身）" }),
  ).toBeVisible();
  expect(
    await page.evaluate(() => (window as any).__widgets.at(-1).options.sitekey),
  ).toBe("rotated-public-site-key");
  expect(reads).toBe(2);
  await page.evaluate(() =>
    (window as any).__widgets[0].options.callback("late-old-candidate"),
  );
  await expect(page.getByRole("button", { name: "获取验证码" })).toBeDisabled();
  expect(
    await page.evaluate(() => (window as any).__widgets.at(-1).options.cData),
  ).not.toBe(operation);
  expect(await page.evaluate(() => (window as any).__widgets[0].removed)).toBe(
    true,
  );
});

test("desktop, intermediate and narrow keyboard layouts, with real CSP", async ({
  page,
}) => {
  const violations: string[] = [];
  await page.exposeFunction("recordViolation", (directive: string) =>
    violations.push(directive),
  );
  await page.addInitScript(() => {
    document.addEventListener("securitypolicyviolation", (event) =>
      (window as any).recordViolation(event.violatedDirective),
    );
  });
  await page.goto("/");
  await expect(
    page.getByRole("heading", { name: /个人账户与员工服务/ }),
  ).toBeVisible();
  await page.getByRole("link", { name: /手机号登录/ }).click();
  await expect(
    page.getByRole("heading", { name: "获取短信验证码" }),
  ).toBeVisible();
  for (const [width, name] of [
    [1440, "desktop"],
    [820, "intermediate"],
    [390, "narrow"],
  ] as const) {
    await page.setViewportSize({ width, height: 950 });
    const input = page.getByLabel("手机号", { exact: true });
    await input.fill(" 13900000000");
    await page
      .getByRole("button", { name: "完成安全验证（测试替身）" })
      .click();
    await page.getByRole("button", { name: "获取验证码" }).focus();
    await page.keyboard.press("Enter");
    await expect(input).toBeFocused();
    await expect(input).toHaveAttribute("aria-invalid", "true");
    await input.fill("");
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    ).toBe(true);
    await page.screenshot({
      path: `test-results/${name}-input.png`,
      fullPage: true,
    });
  }
  expect(violations).toEqual([]);
  await expect(page.locator("style")).toHaveCount(0);
  await expect(page.getByText(/微信|工牌|演示工具/)).toHaveCount(0);
});

test("Nginx routing, nonce, manifest, cache and Cookie isolation", async ({
  request,
}) => {
  const page = await request.get("/authentication/codes");
  const second = await request.get("/authentication/codes", {
    headers: { Cookie: "fictional-private-session=secret" },
  });
  const normalize = (html: string) =>
    html.replace(/nonce="[^"]*"/g, 'nonce="NORMALIZED"');
  expect(normalize(await page.text())).toBe(normalize(await second.text()));
  expect(page.headers()["content-security-policy"]).not.toBe(
    second.headers()["content-security-policy"],
  );
  const nonce = /'nonce-([^']+)'/.exec(
    page.headers()["content-security-policy"],
  )![1];
  expect(await page.text()).toContain(`nonce="${nonce}"`);
  expect(page.headers()["cache-control"]).toContain("no-store");
  expect(page.headers()["content-security-policy"]).toContain(
    "https://challenges.cloudflare.com",
  );
  const home = await request.get("/");
  expect(home.headers()["content-security-policy"]).toContain(
    "frame-src 'none'",
  );
  expect(home.headers()["content-security-policy"]).not.toContain(
    "https://challenges.cloudflare.com",
  );
  expect(await home.text()).toContain("个人账户与员工服务");
  const direct = await request.get(
    `${process.env.HALO_TEST_NODE}/authentication/codes`,
    { headers: { Cookie: "fixture=private" } },
  );
  expect(normalize(await direct.text())).toBe(normalize(await page.text()));
  for (const path of [
    "/absent",
    "/openapi/v1.json",
    "/health/ready",
    "/assets/unknown.js",
    "/assets/",
    "/.env",
  ]) {
    const response = await request.get(path);
    expect(response.status(), path).toBe(404);
    expect(response.headers()["cache-control"], path).toContain("no-store");
  }
  const api = await request.get("/api/v1/absent", {
    headers: { "Operation-Id": crypto.randomUUID() },
  });
  expect(api.status()).toBe(404);
  expect(api.headers()["content-type"]).toContain("application/json");
  const manifest = JSON.parse(await readFile("build/release.json", "utf8"));
  for (const resource of manifest.resources) {
    const response = await request.get(resource.path);
    expect(response.status()).toBe(200);
    expect(response.headers()["content-type"]).toBe(resource.mediaType);
    expect(response.headers()["cache-control"]).toContain("immutable");
    expect(response.headers()["x-content-type-options"]).toBe("nosniff");
  }
  const settings = await request.get("/api/v1/settings", {
    headers: { "Operation-Id": crypto.randomUUID() },
  });
  expect(await settings.json()).toEqual({
    code: "ok",
    settings: { challenge: { turnstile: { siteKey: "fixture-site-key" } } },
  });
  expect(settings.headers()["cache-control"]).toContain("no-store");
  const privateResource = await request.get("/api/__halo_cache_fixture");
  expect(privateResource.status()).toBe(200);
  expect(await privateResource.text()).toBe("fictional-private-bytes");
  expect(privateResource.headers()["cache-control"]).toBe(
    "private, max-age=60, immutable",
  );
});

test("reentered missing or corrupt material cannot turn into a new application on reload", async ({
  page,
}) => {
  let writes = 0;
  page.on("request", (request) => {
    if (request.url().endsWith("/api/v1/codes")) writes++;
  });
  await page.goto("/authentication/codes");
  await page.evaluate(() => {
    const now = Date.now();
    sessionStorage.setItem(
      "halo.c1",
      JSON.stringify({
        version: 1,
        operation: crypto.randomUUID(),
        input: { mobile: "13900000000", token: "fictional-original-proof" },
        createdAt: now,
        expiresAt: now + 30 * 60 * 1000,
        retryAt: 0,
        attempted: true,
      }),
    );
  });
  await page.goto("/");
  await page.goto("/authentication/codes");
  await expect(
    page.getByRole("heading", { name: "暂时无法确认发送结果" }),
  ).toBeVisible();
  expect(await page.evaluate(() => history.state?.haloC1)).toBe(true);
  await page.evaluate(() => sessionStorage.removeItem("halo.c1"));
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "暂时无法查看发送结果" }),
  ).toBeVisible();
  for (const bytes of [
    "{",
    '{"version":1,"input":{"token":"fictional-proof"}}',
  ]) {
    await page.evaluate((bytes) => {
      sessionStorage.setItem("halo.c1", bytes);
      history.replaceState({}, "");
    }, bytes);
    await page.reload();
    await expect(
      page.getByRole("heading", { name: "暂时无法查看发送结果" }),
    ).toBeVisible();
    expect(
      await page.evaluate(() => JSON.parse(sessionStorage.getItem("halo.c1")!)),
    ).toEqual({ version: 1, closed: true });
    await page.goto("/");
    await page.evaluate(() => sessionStorage.removeItem("halo.c1"));
    await page.goBack();
    await expect(
      page.getByRole("heading", { name: "暂时无法查看发送结果" }),
    ).toBeVisible();
    await page.reload();
    await expect(
      page.getByRole("heading", { name: "暂时无法查看发送结果" }),
    ).toBeVisible();
    await expect(page.getByRole("button", { name: "获取验证码" })).toHaveCount(
      0,
    );
  }
  expect(writes).toBe(0);
});

test("changed build stops new writes and preserves already saved material", async ({
  page,
}) => {
  let writes = 0;
  page.on("request", (request) => {
    if (request.url().endsWith("/api/v1/codes")) writes++;
  });
  await prepare(page, "13900000009");
  await page.route("**/build.json", (route) =>
    route.fulfill({
      contentType: "application/json",
      body: JSON.stringify({ buildId: "another-build" }),
    }),
  );
  await submit(page);
  await expect(page.getByRole("heading", { name: "请重新加载" })).toBeVisible();
  expect(writes).toBe(0);
  const saved = await page.evaluate(() =>
    JSON.parse(sessionStorage.getItem("halo.c1")!),
  );
  expect(saved.input.token).toBe(saved.operation);
});

test("an unsent application remains correctable after the required build reload", async ({
  page,
}) => {
  let writes = 0;
  await page.route("**/api/v1/codes", async (route) => {
    writes++;
    await rejectBeforeAdmission(route, "permission_denied");
  });
  await prepare(page, "13900000000");
  await page.route("**/build.json", (route) =>
    route.fulfill({
      contentType: "application/json",
      body: JSON.stringify({ buildId: "new-build" }),
    }),
  );
  await submit(page);
  await expect(page.getByRole("heading", { name: "请重新加载" })).toBeVisible();
  await page.unroute("**/build.json");
  await page.getByRole("button", { name: "重新加载" }).click();
  await expect(
    page.getByRole("heading", { name: "尚未发送验证码" }),
  ).toBeVisible();
  expect(writes).toBe(0);
  await expect(
    page.getByRole("button", { name: "重试获取验证码", exact: true }),
  ).toHaveCount(0);
  await page.screenshot({
    path: "test-results/unsubmitted.png",
    fullPage: true,
  });
  await page
    .getByRole("button", { name: "继续获取验证码", exact: true })
    .click();
  await expect(
    page.getByRole("button", { name: "重新填写并验证" }),
  ).toBeVisible();
  expect(writes).toBe(1);
});

for (const body of ["{", "null"]) {
  test(`malformed build metadata ${body} requires reload before any write`, async ({
    page,
  }) => {
    let writes = 0;
    page.on("request", (request) => {
      if (request.url().endsWith("/api/v1/codes")) writes++;
    });
    await prepare(page, "13900000000");
    await page.route("**/build.json", (route) =>
      route.fulfill({ contentType: "application/json", body }),
    );
    await submit(page);
    await expect(
      page.getByRole("heading", { name: "请重新加载" }),
    ).toBeVisible();
    await expect(
      page.getByRole("button", {
        name: /重试获取验证码|继续获取验证码/,
        exact: true,
      }),
    ).toHaveCount(0);
    const saved = await page.evaluate(() =>
      JSON.parse(sessionStorage.getItem("halo.c1")!),
    );
    expect(saved.attempted).toBe(false);
    expect(saved.input.token).toBe(saved.operation);
    expect(writes).toBe(0);
  });
}

test("missing bootstrap resources leave a reloadable shell and no write controls", async ({
  page,
}) => {
  await page.route("**/assets/**/*.js", (route) => route.abort("failed"));
  await page.goto("/authentication/codes");
  await expect(page.getByRole("link", { name: "重新加载" })).toBeVisible();
  await expect(page.getByRole("button", { name: "获取验证码" })).toHaveCount(0);
});

test("a hydration mismatch is retained before the form mounts", async ({
  page,
}) => {
  let writes = 0;
  page.on("request", (request) => {
    if (request.url().endsWith("/api/v1/codes")) writes++;
  });
  await page.route("**/authentication/codes", async (route) => {
    const response = await route.fetch();
    const original = await response.text();
    expect(original).toContain("正在打开验证码页面…");
    await route.fulfill({
      response,
      body: original.replace("正在打开验证码页面…", "过期的页面内容"),
    });
  });
  await page.goto("/authentication/codes");
  await expect(page.getByRole("heading", { name: "请重新加载" })).toBeVisible();
  await expect(page.getByRole("button", { name: "获取验证码" })).toHaveCount(0);
  expect(writes).toBe(0);
});

for (const resource of [
  "stylesheet",
  "bootstrap",
  "logo",
  "botanical",
] as const) {
  test(`an early ${resource} failure prevents write controls`, async ({
    page,
  }) => {
    let writes = 0;
    let failures = 0;
    page.on("request", (request) => {
      if (request.url().endsWith("/api/v1/codes")) writes++;
    });
    await page.route(
      {
        stylesheet: "**/assets/**/*.css",
        bootstrap: "**/assets/**/bootstrap-*.js",
        logo: "**/assets/**/logo-*.png",
        botanical: "**/assets/**/halo-botanical-*.svg",
      }[resource],
      (route) => {
        failures++;
        return route.abort("failed");
      },
    );
    await page.goto("/authentication/codes");
    await expect(
      page.getByRole("heading", { name: "请重新加载" }),
    ).toBeVisible();
    await expect(page.getByRole("button", { name: "获取验证码" })).toHaveCount(
      0,
    );
    expect(failures).toBeGreaterThan(0);
    expect(writes).toBe(0);
  });
}
