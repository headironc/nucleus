import { expect, test, type Page } from "@playwright/test";
import { randomUUID } from "node:crypto";
import { readFile, rename, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { cookieDigests } from "./identity";
import { assertDocumentPolicy } from "./policy";

type Authentication = (
  page: Page,
  mobile: string,
  bookkeepingFailure?: boolean,
  resume?: boolean | "mobile-change",
) => Promise<string>;

const previousMobile = "13900000040";
const nextMobile = "13900000041";

async function code(page: Page, value: string) {
  await page
    .getByRole("textbox", { name: "短信验证码", exact: true })
    .fill(value);
}

async function cooldown(page: Page, sentAt: number) {
  // Respect the real shared per-mobile SMS interval, not a UI synchronization delay.
  const remaining = sentAt + 61_000 - Date.now();
  if (remaining > 0) await page.waitForTimeout(remaining);
}

async function screenshot(page: Page, name: string) {
  const path = test.info().outputPath(`${name}.png`);
  await page.screenshot({ path, fullPage: true });
  await test.info().attach(name, { path, contentType: "image/png" });
}

/** Real browser, Cookie, HTTP and supplier flow with independent database observations. */
export async function ownMobileChange(
  page: Page,
  authenticate: Authentication,
) {
  const context = page.context();
  const material = process.env.HALO_TEST_MATERIAL!;
  const firstCode = await authenticate(page, previousMobile);
  let sentAt = Date.now();
  await code(page, firstCode);
  await page.getByRole("button", { name: "验证并继续" }).click();
  await expect(
    page.getByRole("heading", { name: "创建个人账户" }),
  ).toBeVisible();
  await page.getByLabel("姓名", { exact: true }).fill("Mobile Fixture");
  await page.getByLabel("居民身份证号", { exact: true }).fill("000000200002294003");
  const registration = page.waitForResponse(
    (response) =>
      new URL(response.url()).pathname === "/api/v1/registrations" &&
      response.status() === 201,
  );
  await page.getByRole("button", { name: "创建账户" }).click();
  const first = (await (await registration).json()).session;
  await expect(page).toHaveURL(/\/account$/);
  await expect(page.getByText("Mobile Fixture", { exact: true })).toBeVisible();

  await cooldown(page, sentAt);
  const secondCode = await authenticate(page, previousMobile);
  sentAt = Date.now();
  const secondLogin = page.waitForResponse(
    (response) =>
      new URL(response.url()).pathname === "/api/v1/sessions" &&
      response.status() === 200,
  );
  await code(page, secondCode);
  await page.getByRole("button", { name: "验证并继续" }).click();
  const second = (await (await secondLogin).json()).session;
  expect(second.id).not.toBe(first.id);
  await expect(page).toHaveURL(/\/account$/);
  await expect(page.getByText("Mobile Fixture", { exact: true })).toBeVisible();
  const current = await context.request.get("/api/v1/accounts/current", {
    headers: {
      "Operation-Id": randomUUID(),
      "Session-Id": second.id,
      Origin: new URL(page.url()).origin,
    },
  });
  expect(current.status()).toBe(200);
  const account = (await current.json()).account;
  const originals: string[] = [first.id, second.id].sort();
  const cookies = await cookieDigests(page);

  const writes: {
    path: string;
    operation: string;
    session: string;
    body: string | null;
  }[] = [];
  page.on("request", (request) => {
    const path = new URL(request.url()).pathname;
    if (
      (request.method() === "POST" || request.method() === "PUT") &&
      (path === "/api/v1/accounts/current/codes" ||
        path === "/api/v1/accounts/current/mobile" ||
        path.startsWith("/api/v1/mobile-changes"))
    ) {
      writes.push({
        path,
        operation: request.headers()["operation-id"],
        session: request.headers()["session-id"],
        body: request.postData(),
      });
    }
  });
  const navigation = await page.goto("/account/mobile");
  assertDocumentPolicy(navigation!.headers(), true);
  await expect(page.getByLabel("当前手机号", { exact: true })).toBeVisible();
  await page.getByLabel("当前手机号", { exact: true }).fill(previousMobile);
  await page.getByRole("button", { name: "完成安全验证（测试替身）" }).click();
  const limitedResponse = page.waitForResponse(
    (response) =>
      new URL(response.url()).pathname === "/api/v1/accounts/current/codes",
  );
  await page.getByRole("button", { name: "获取验证码" }).click();
  expect((await limitedResponse).status()).toBe(429);
  await expect(
    page.getByRole("heading", { name: "请稍后重试" }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "完成安全验证（测试替身）" }),
  ).toHaveCount(0);
  expect(await cookieDigests(page)).toEqual(cookies);
  await cooldown(page, sentAt);
  const currentCodeResponse = page.waitForResponse(
    (response) =>
      new URL(response.url()).pathname === "/api/v1/accounts/current/codes",
  );
  await page
    .getByRole("button", { name: "重试获取当前手机号验证码", exact: true })
    .click();
  const currentCode = await currentCodeResponse;
  expect(currentCode.status()).toBe(200);
  expect(writes).toHaveLength(2);
  expect(writes[1]).toEqual(writes[0]);
  const originalMessage = (await currentCode.json()).message;
  expect(originalMessage.delivery).toBe("sent");
  await code(
    page,
    await readFile(join(material, `${originalMessage.id}.code`), "utf8"),
  );
  const establishing = page.waitForResponse(
    (response) => new URL(response.url()).pathname === "/api/v1/mobile-changes",
  );
  await page.getByRole("button", { name: "验证并继续" }).click();
  const established = await establishing;
  expect(established.status()).toBe(201);
  const transfer = (await established.json()).transfer;
  await page.getByLabel("新手机号", { exact: true }).fill(nextMobile);
  await page.getByRole("button", { name: "完成安全验证（测试替身）" }).click();
  const bindingResponse = page.waitForResponse(
    (response) =>
      new URL(response.url()).pathname ===
      `/api/v1/mobile-changes/${transfer.id}/codes`,
  );
  await page.getByRole("button", { name: "获取新手机号验证码" }).click();
  const binding = await bindingResponse;
  expect(binding.status()).toBe(200);
  const newMessage = (await binding.json()).message;
  expect(newMessage.delivery).toBe("sent");
  sentAt = Date.now();
  await code(
    page,
    await readFile(join(material, `${newMessage.id}.code`), "utf8"),
  );
  await page.getByRole("button", { name: "继续", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "确认更换手机号", exact: true }),
  ).toBeVisible();

  let operation: string | null = null;
  let completedAt: string | null = null;
  let later: string | null = null;
  async function observe(phase: string) {
    const path = join(material, `mobile-${phase}`);
    await writeFile(
      `${path}.pending`,
      JSON.stringify({
        account: account.id,
        person: account.personId,
        transfer: transfer.id,
        operation,
        message: newMessage.id,
        session: second.id,
        originals,
        completedAt,
        later,
      }),
    );
    await rename(`${path}.pending`, `${path}.request`);
    await expect
      .poll(() => readFile(`${path}.ack`, "utf8").catch(() => ""), {
        timeout: 10_000,
      })
      .toBe("ok");
  }
  await page.route("**/api/v1/accounts/current/mobile", async (route) => {
    const request = route.request();
    expect(request.method()).toBe("PUT");
    const input = request.postDataJSON();
    expect(Object.keys(input).sort()).toEqual(["code", "messageId", "mobile"]);
    expect(input.messageId).toBe(newMessage.id);
    expect(input.mobile).toBe(nextMobile);
    expect(input.code).toMatch(/^[0-9]{6}$/);
    operation = request.headers()["operation-id"];
    await observe("before");
    const response = await route.fetch({ maxRedirects: 0, maxRetries: 0 });
    expect(response.status()).toBe(200);
    expect(response.headers()["set-cookie"]).toBeUndefined();
    const result = await response.json();
    expect(result).toMatchObject({
      code: "ok",
      completion: { id: transfer.id },
    });
    completedAt = result.completion.completedAt;
    await observe("after");
    await route.abort("failed");
  });
  await page
    .getByRole("button", { name: "确认更换手机号", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "更换结果尚不确定", exact: true }),
  ).toBeVisible();
  expect(writes).toHaveLength(5);
  expect(new Set(writes.map((write) => write.operation)).size).toBe(4);
  expect(writes.every((write) => write.session === second.id)).toBe(true);
  expect(await cookieDigests(page)).toEqual(cookies);
  for (const session of originals) {
    const response = await context.request.get("/api/v1/accounts/current", {
      headers: {
        "Operation-Id": randomUUID(),
        "Session-Id": session,
        Origin: new URL(page.url()).origin,
      },
    });
    expect(response.status()).toBe(401);
    expect(response.headers()["set-cookie"]).toBeUndefined();
  }
  await screenshot(page, "mobile-real-unknown");
  await page.getByRole("button", { name: "查看更换结果", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "请重新登录", exact: true }),
  ).toBeVisible();
  await page
    .locator('a[href="/authentication/codes?return=mobile-change"]')
    .first()
    .click();
  await cooldown(page, sentAt);
  const loginCode = await authenticate(
    page,
    nextMobile,
    false,
    "mobile-change",
  );
  const loginResponse = page.waitForResponse(
    (response) =>
      new URL(response.url()).pathname === "/api/v1/sessions" &&
      response.status() === 200,
  );
  const returnedDocument = page.waitForResponse(
    (response) =>
      response.request().isNavigationRequest() &&
      new URL(response.url()).pathname === "/account/mobile",
  );
  await code(page, loginCode);
  await page.getByRole("button", { name: "验证并继续" }).click();
  later = (await (await loginResponse).json()).session.id;
  expect(originals).not.toContain(later);
  assertDocumentPolicy((await returnedDocument).headers(), true);
  await expect(page).toHaveURL(/\/account\/mobile\?return=mobile-change$/);
  await expect(
    page.getByRole("heading", { name: "查看更换结果", exact: true }),
  ).toBeVisible();
  const laterCookies = await cookieDigests(page);
  let reads = 0;
  await page.route(`**/api/v1/mobile-changes/${transfer.id}`, async (route) => {
    reads++;
    expect(route.request().method()).toBe("GET");
    expect(route.request().postData()).toBeNull();
    expect(new URL(route.request().url()).search).toBe("");
    expect(route.request().headers()["session-id"]).toBe(later);
    await observe("query-before");
    // APIRequestContext does not preserve the browser's fetch metadata.
    const response = await route.fetch({
      headers: {
        ...(await route.request().allHeaders()),
        Origin: new URL(page.url()).origin,
      },
      maxRedirects: 0,
      maxRetries: 0,
    });
    expect(response.status()).toBe(200);
    expect(await response.json()).toEqual({
      code: "ok",
      transfer: { status: "completed", completedAt },
    });
    expect(response.headers()["set-cookie"]).toBeUndefined();
    await observe("query-after");
    await route.fulfill({ response });
  });
  await page.getByRole("button", { name: "查看更换结果", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "此前的手机号更换已完成", exact: true }),
  ).toBeVisible();
  expect(reads).toBe(1);
  expect(writes).toHaveLength(5);
  expect(await cookieDigests(page)).toEqual(laterCookies);
  const retained = await page.evaluate(() =>
    JSON.parse(sessionStorage.getItem("halo.mobile-change")!),
  );
  expect(retained.active).toBeNull();
  expect(retained.known).toContainEqual({
    account: account.id,
    person: account.personId,
    transfer: transfer.id,
    completedAt,
  });
  await screenshot(page, "mobile-real-completed");
}
