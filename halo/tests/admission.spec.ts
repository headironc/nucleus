import { test, expect, type Page } from "@playwright/test";
import { build } from "vite";
import tailwindcss from "@tailwindcss/vite";
import { resolve } from "node:path";

const origin = "http://admission.localhost";
const id = "00000000-0000-4000-8000-000000000001";
const target = "00000000-0000-4000-8000-000000000002";
const roleId = "00000000-0000-4000-8000-000000000003";
const member = {
  id: roleId,
  changeId: roleId,
  kind: "default",
  name: "普通成员",
  level: 1,
  permissions: [],
};
const admin = {
  id,
  changeId: id,
  kind: "full",
  name: "管理员",
  level: 1000,
  permissions: ["workforce.admission"],
};
const candidate = {
  code: "ok",
  identity: {
    accountId: target,
    personId: target,
    revisionId: target,
    name: "阿依古丽·阿不都热依木·买*",
    idNumber: { suffix: "002X" },
  },
  assignmentId: target,
  suspensionId: null,
  grant: null,
  count: 2,
  roles: [admin, member],
};
const resources = new Map<
  string,
  { body: string | Buffer; contentType: string }
>();
let html: string;
function account(selected: string) {
  return {
    code: "ok",
    account: {
      id: selected,
      personId: selected,
      revisionId: selected,
      name: selected === id ? "机构管理者" : "另一账户",
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
function completed(
  operation: string,
  input: { role: { id: string; changeId: string } },
) {
  const subject = { personId: target, revisionId: target, name: "阿*" };
  const operator = { personId: id, revisionId: id, name: "机*" };
  const grant = {
    id: target,
    subject,
    operator,
    operationId: operation,
    grantedAt: "2026-10-30T08:00:00Z",
    status: "active",
    sequence: 1,
  };
  return {
    code: "ok",
    grant,
    assignment: {
      id,
      grantId: target,
      subject,
      operator,
      role: input.role.id === id ? admin : member,
      sequence: 1,
      assignedAt: grant.grantedAt,
      operationId: operation,
    },
  };
}
test.beforeAll(async () => {
  const result = await build({
    configFile: false,
    publicDir: false,
    plugins: [tailwindcss()],
    logLevel: "error",
    define: { __BUILD_ID__: JSON.stringify("admission-fixture") },
    build: {
      write: false,
      emptyOutDir: false,
      cssCodeSplit: false,
      assetsInlineLimit: 0,
      rolldownOptions: {
        input: resolve("tests/fixtures/admission.tsx"),
        output: { entryFileNames: "fixture.js" },
      },
    },
  });
  if (Array.isArray(result) || !("output" in result))
    throw Error("Expected fixture build");
  const styles: string[] = [];
  for (const item of result.output) {
    const contentType =
      item.type === "chunk" || item.fileName.endsWith(".js")
        ? "text/javascript"
        : item.fileName.endsWith(".css")
          ? "text/css"
          : item.fileName.endsWith(".svg")
            ? "image/svg+xml"
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
async function choose(page: Page, selected = id) {
  await page.evaluate(async (selected) => {
    const sessions = window.admissionSessions;
    const attempt = await sessions.begin(crypto.randomUUID());
    await sessions.accept(attempt, {
      id: selected,
      expiresAt: "2035-01-01T00:00:00Z",
    });
  }, selected);
}
test.beforeEach(async ({ page }) => {
  await page.route(origin + "/**", async (route) => {
    const path = new URL(route.request().url()).pathname;
    if (path === "/build.json")
      return route.fulfill({ json: { buildId: "admission-fixture" } });
    if (path === "/api/v1/accounts/current")
      return route.fulfill({
        json: account(route.request().headers()["session-id"]),
      });
    if (path === "/api/v1/accounts/current/role")
      return route.fulfill({
        json: {
          code: "ok",
          role: { ...admin, permissions: ["institution.workforce"] },
        },
      });
    if (path === "/api/v1/accounts/current/corrections")
      return route.fulfill({ json: { code: "ok", count: 0, corrections: [] } });
    if (path === "/api/v1/workforce/grants/candidates")
      return route.fulfill({ json: candidate });
    if (path === "/api/v1/workforce/grants")
      return route.fulfill({
        status: 201,
        json: completed(
          route.request().headers()["operation-id"],
          route.request().postDataJSON(),
        ),
      });
    if (path === "/api/v1/workforce/current/grant")
      return route.fulfill({ status: 404, json: { code: "grant_absent" } });
    if (path.startsWith("/api/")) return route.abort();
    return route.fulfill(
      resources.get(path) ?? { body: html, contentType: "text/html" },
    );
  });
  await page.goto(origin + "/workforce/grants/new");
  await choose(page);
  await expect(page.getByLabel("同事的手机号")).toBeVisible();
});
async function find(page: Page) {
  await page.getByLabel("同事的手机号").fill("13800000002");
  await page.getByRole("button", { name: "查找同事", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "确认开通", exact: true }),
  ).toBeVisible();
}
async function confirm(page: Page) {
  await page.getByRole("button", { name: "确认开通", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "取消", exact: true }),
  ).toBeFocused();
  await page.getByRole("button", { name: "开通员工服务", exact: true }).click();
}
test("institution account entry, default role and keyboard confirmation", async ({
  page,
}) => {
  await page.goto(origin + "/account");
  await page.getByRole("link", { name: "开通员工服务", exact: true }).click();
  await find(page);
  await expect(
    page.getByRole("radio", { name: "普通成员 · 默认选择" }),
  ).toBeChecked();
  await expect(page.getByRole("radio", { name: /^管理员/ })).not.toBeChecked();
  await page.getByRole("button", { name: "确认开通", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "取消", exact: true }),
  ).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).not.toBeVisible();
  await expect(
    page.getByRole("button", { name: "确认开通", exact: true }),
  ).toBeFocused();
  await page.getByRole("radio", { name: /^管理员/ }).check();
  await page.getByRole("button", { name: "确认开通", exact: true }).click();
  await expect(
    page.getByRole("dialog").getByText("管理员", { exact: true }),
  ).toBeVisible();
  await page.keyboard.press("Tab");
  await expect(
    page.getByRole("button", { name: "开通员工服务", exact: true }),
  ).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(
    page.getByText("本次员工服务已开通", { exact: true }),
  ).toBeVisible();
  await page
    .getByRole("button", { name: "继续办理另一人", exact: true })
    .click();
  await expect(page.getByLabel("同事的手机号")).toHaveValue("");
});
test("lost response survives reload and replays the exact original without history reads", async ({
  page,
}) => {
  const calls: { operation: string; input: unknown }[] = [];
  const forbidden: string[] = [];
  page.on("request", (request) => {
    if (
      request.method() === "GET" &&
      new URL(request.url()).pathname.startsWith("/api/v1/workforce/grants")
    )
      forbidden.push(request.url());
  });
  await page.route("**/api/v1/workforce/grants", async (route) => {
    calls.push({
      operation: route.request().headers()["operation-id"],
      input: route.request().postDataJSON(),
    });
    if (calls.length === 1) return route.abort();
    return route.fulfill({
      status: 201,
      json: completed(calls[0].operation, route.request().postDataJSON()),
    });
  });
  await find(page);
  await confirm(page);
  await expect(
    page.getByRole("button", { name: /秒后可检查开通结果/ }),
  ).toBeDisabled();
  await page.reload();
  await expect(
    page.getByText("此前开通结果尚未确认。", { exact: false }),
  ).toBeVisible();
  expect(calls).toHaveLength(1);
  await expect(
    page.getByRole("button", { name: "检查开通结果", exact: true }),
  ).toBeEnabled({ timeout: 8000 });
  await page.getByRole("button", { name: "检查开通结果", exact: true }).click();
  await expect(
    page.getByText("本次员工服务已开通", { exact: true }),
  ).toBeVisible();
  expect(calls).toHaveLength(2);
  expect(calls[1]).toEqual(calls[0]);
  expect(forbidden).toEqual([]);
});
test("server pagination finds a last-page default and exposes every actual page", async ({
  page,
}) => {
  const pages: number[] = [];
  const roles = Array.from({ length: 200 }, (_, i) => ({
    ...admin,
    id: `00000000-0000-4000-8000-${String(i + 100).padStart(12, "0")}`,
    changeId: id,
    kind: "custom",
    level: 999 - i,
    name: `角色 ${i + 1}`,
  }));
  await page.route("**/api/v1/workforce/grants/candidates?*", async (route) => {
    const number = Number(
      new URL(route.request().url()).searchParams.get("page"),
    );
    pages.push(number);
    const all = [admin, ...roles, member];
    await route.fulfill({
      json: {
        ...candidate,
        count: all.length,
        roles: all.slice((number - 1) * 100, number * 100),
      },
    });
  });
  await find(page);
  expect(pages).toEqual([1, 3]);
  await expect(
    page.getByRole("radio", { name: "普通成员 · 默认选择" }),
  ).toBeChecked();
  await page.getByRole("button", { name: "下一页", exact: true }).click();
  await expect(page.getByText("第 2 / 3 页", { exact: false })).toBeVisible();
  await page.getByRole("button", { name: "下一页", exact: true }).click();
  await expect(page.getByText("第 3 / 3 页", { exact: false })).toBeVisible();
  expect(pages).toEqual([1, 3, 2, 3]);
});
test("denies wrong permission, self and same-rank targets without inventing an empty state", async ({
  page,
}) => {
  await page.route("**/api/v1/accounts/current/role", (route) =>
    route.fulfill({
      json: {
        code: "ok",
        role: { ...admin, permissions: ["institution.roles"] },
      },
    }),
  );
  await page.reload();
  await expect(
    page.getByText("当前账户没有开通员工服务的权限。", { exact: false }),
  ).toBeVisible();
  await expect(page.getByLabel("同事的手机号")).not.toBeVisible();
  await page.unroute("**/api/v1/accounts/current/role");
  await page.reload();
  await page.route("**/api/v1/workforce/grants/candidates?*", (route) =>
    route.fulfill({ status: 404, json: { code: "target_unavailable" } }),
  );
  await page.getByLabel("同事的手机号").fill("13800000002");
  await page.getByRole("button", { name: "查找同事", exact: true }).click();
  await expect(
    page.getByText("无法为该账户办理", { exact: false }),
  ).toBeVisible();
});
test("switching account closes confirmation and prevents a late candidate from exposing old data", async ({
  page,
}) => {
  await find(page);
  await page.getByRole("button", { name: "确认开通", exact: true }).click();
  await choose(page, target);
  await expect(page.getByRole("dialog")).not.toBeVisible();
  await expect(
    page.getByText(candidate.identity.name, { exact: true }),
  ).not.toBeVisible();
  await expect(page.getByLabel("同事的手机号")).toHaveValue("");
});
test("dependency and rate-limit failures stay failures and respect Retry-After", async ({
  page,
}) => {
  await page.route("**/api/v1/workforce/grants/candidates?*", (route) =>
    route.fulfill({
      status: 429,
      headers: { "Retry-After": "20" },
      json: { code: "rate_limited" },
    }),
  );
  await page.getByLabel("同事的手机号").fill("13800000002");
  await page.getByRole("button", { name: "查找同事", exact: true }).click();
  await expect(
    page.getByText("暂时无法查找同事和可选角色", { exact: false }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: /秒后可重新查找/ }),
  ).toBeDisabled();
});
for (const patch of [
  { suspensionId: target },
  { grant: { id: target, status: "active" } },
])
  test(`blocks ineligible candidate ${JSON.stringify(patch)}`, async ({
    page,
  }) => {
    await page.route("**/api/v1/workforce/grants/candidates?*", (route) =>
      route.fulfill({ json: { ...candidate, ...patch } }),
    );
    await find(page);
    await expect(
      page.getByRole("button", { name: "确认开通", exact: true }),
    ).toBeDisabled();
  });
for (const width of [1440, 768, 390, 320])
  for (const theme of ["light", "dark"] as const)
    test(`actual admission page ${width} ${theme}`, async ({ page }) => {
      await page.setViewportSize({ width, height: 1000 });
      await page.emulateMedia({ colorScheme: theme });
      await find(page);
      await expect(
        page.getByRole("radio", { name: "普通成员 · 默认选择" }),
      ).toBeChecked();
      expect(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= innerWidth,
        ),
      ).toBe(true);
      await page.screenshot({
        path: `test-results/admission-${width}-${theme}.png`,
        fullPage: true,
      });
      await page.getByRole("button", { name: "确认开通", exact: true }).click();
      await expect(
        page.getByRole("button", { name: "取消", exact: true }),
      ).toBeFocused();
      await expect(
        page.getByRole("link", { name: "跳到主要内容", exact: true }),
      ).not.toBeFocused();
      expect(
        await page
          .locator(".skip-link")
          .evaluate((element) => element.getBoundingClientRect().bottom <= 0),
      ).toBe(true);
      expect(
        await page
          .getByRole("dialog")
          .evaluate(
            (element) => element.getBoundingClientRect().width <= innerWidth,
          ),
      ).toBe(true);
      await page.screenshot({
        path: `test-results/admission-dialog-${width}-${theme}.png`,
        fullPage: false,
      });
    });
