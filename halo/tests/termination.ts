import { expect, type Page } from "@playwright/test";
import { readFile, rename, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { cookieDigests } from "./identity";
import { assertDocumentPolicy } from "./policy";

/** A committed original survives rejoining, another account and a real new login. */
export async function ownTermination(
  page: Page,
  owner: string,
  manager: string,
  selectOther: () => Promise<void>,
  selectOwner: () => Promise<void>,
) {
  await selectOwner();
  await page.setExtraHTTPHeaders({ "X-Halo-Fixture-Source": "13" });
  const cookies = await cookieDigests(page);
  const currentRead = page.waitForResponse(
    (response) =>
      new URL(response.url()).pathname === "/api/v1/accounts/current" &&
      response.status() === 200,
  );
  const grantRead = page.waitForResponse(
    (response) =>
      new URL(response.url()).pathname === "/api/v1/workforce/current/grant" &&
      response.status() === 200,
  );
  await page.goto("/workforce");
  const account = (await (await currentRead).json()).account;
  const grantBody = await (await grantRead).json();
  const grant = grantBody.grant.id as string;
  expect(grantBody.grant.status).toBe("active");
  expect(grantBody.grant.subject.personId).toBe(account.personId);
  await page.getByRole("link", { name: "结束员工服务", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "结束员工服务", exact: true }),
  ).toBeFocused();
  await page.getByRole("button", { name: "结束员工服务", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "暂不结束", exact: true }),
  ).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).toHaveCount(0);

  const calls: {
    operation: string;
    session: string;
    path: string;
    body: string | null;
  }[] = [];
  let terminatedAt: string | null = null;
  let replacement: string | null = null;
  const observe = async (
    phase: "original" | "replay",
    position: "before" | "after",
  ) => {
    const call = calls.at(-1)!;
    const path = join(
      process.env.HALO_TEST_MATERIAL!,
      `termination-${phase}-${position}`,
    );
    await writeFile(
      `${path}.pending`,
      JSON.stringify({
        operation: call.operation,
        grant,
        person: account.personId,
        account: account.id,
        session: call.session,
        replacement,
        terminatedAt,
      }),
    );
    await rename(`${path}.pending`, `${path}.request`);
    await expect
      .poll(() => readFile(`${path}.ack`, "utf8").catch(() => ""), {
        timeout: 10_000,
      })
      .toBe("ok");
  };
  const pattern = `**/api/v1/workforce/grants/${grant}/terminations`;
  await page.route(pattern, async (route) => {
    const request = route.request();
    expect(request.method()).toBe("POST");
    expect(request.postData()).toBeNull();
    expect(request.headers()["content-type"]).toBeUndefined();
    const phase = calls.length === 0 ? "original" : "replay";
    calls.push({
      operation: request.headers()["operation-id"],
      session: request.headers()["session-id"],
      path: new URL(request.url()).pathname,
      body: request.postData(),
    });
    expect(calls.length).toBeLessThanOrEqual(2);
    await observe(phase, "before");
    const response = await route.fetch({ maxRedirects: 0, maxRetries: 0 });
    expect(response.status()).toBe(201);
    expect(response.headers()["set-cookie"]).toBeUndefined();
    const value = await response.json();
    expect(value.code).toBe("ok");
    expect(value.termination).toMatchObject({
      id: grant,
      mode: "self",
      operationId: calls[0].operation,
      subject: { personId: account.personId },
      operator: { personId: account.personId },
    });
    if (phase === "original") terminatedAt = value.termination.terminatedAt;
    else expect(value.termination.terminatedAt).toBe(terminatedAt);
    await observe(phase, "after");
    if (phase === "original") await route.abort("connectionclosed");
    else await route.fulfill({ response });
  });
  await page.getByRole("button", { name: "结束员工服务", exact: true }).click();
  await page.getByRole("button", { name: "确认结束", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "结束结果尚不确定", exact: true }),
  ).toBeFocused();
  expect(calls).toHaveLength(1);
  expect(calls[0]).toMatchObject({
    path: `/api/v1/workforce/grants/${grant}/terminations`,
    session: owner,
    body: null,
  });
  expect(calls[0].operation).toMatch(
    /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
  );
  expect(await cookieDigests(page)).toEqual(cookies);
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "结束结果尚不确定", exact: true }),
  ).toBeVisible();
  expect(calls).toHaveLength(1);

  // The existing full Workforce manager performs the same legal admission as
  // the preceding Workforce fixture. Direct self admission is prohibited.
  replacement = await page.evaluate(
    async ({ manager }) => {
      const post = async (path: string, body: unknown) => {
        const response = await fetch(path, {
          method: "POST",
          credentials: "same-origin",
          headers: {
            "Operation-Id": crypto.randomUUID(),
            "Session-Id": manager,
            "Content-Type": "application/json",
          },
          body: JSON.stringify(body),
        });
        const value = await response.json();
        if (!response.ok)
          throw Error(`Fixture admission ${response.status} ${value.code}`);
        return value;
      };
      const mobile = "13900000000";
      const candidate = await post("/api/v1/workforce/grants/candidates", {
        entry: "domain",
        mobile,
      });
      const role = candidate.roles.find(
        (value: { kind: string }) => value.kind === "default",
      );
      if (!role) throw Error("Missing eligible default role");
      const receipt = await post("/api/v1/workforce/grants", {
        entry: "domain",
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
    },
    { manager },
  );
  expect(replacement).not.toBe(grant);
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect(
    page.getByRole("heading", { name: "结束结果尚不确定", exact: true }),
  ).toBeVisible();
  expect(calls).toHaveLength(1);
  await page.screenshot({
    path: "test-results/termination-real-pending.png",
    fullPage: true,
  });
  await page.getByRole("link", { name: "打开个人账户", exact: true }).click();
  await page.getByRole("button", { name: "仍然离开", exact: true }).click();
  await expect(page).toHaveURL(/\/account$/);
  await selectOther();
  await page.goto("/workforce/termination");
  await expect(
    page.getByRole("heading", { name: "当前账户已变化", exact: true }),
  ).toBeVisible();
  await expect(page.locator("main")).not.toContainText(account.name);
  expect(calls).toHaveLength(1);
  await selectOwner();
  await page.goto("/account");
  await page.setExtraHTTPHeaders({ "X-Halo-Fixture-Source": "00" });

  return async (session: string) => {
    expect(session).not.toBe(owner);
    await expect(page).toHaveURL(
      /\/workforce\/termination\?return=service-termination$/,
    );
    await expect(
      page.getByRole("heading", {
        name: "结束结果尚不确定",
        exact: true,
      }),
    ).toBeVisible();
    expect(calls).toHaveLength(1);
    const before = await cookieDigests(page);
    await expect(
      page.getByRole("button", { name: "查看结束结果", exact: true }),
    ).toBeEnabled();
    await page.getByRole("button", { name: "查看结束结果", exact: true }).click();
    await expect(
      page.getByRole("heading", { name: "此前的员工服务已结束", exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("heading", {
        name: "当前员工服务已重新开通",
        exact: true,
      }),
    ).toBeVisible();
    expect(calls).toHaveLength(2);
    expect(calls[1]).toMatchObject({
      operation: calls[0].operation,
      path: calls[0].path,
      session,
      body: null,
    });
    expect(await cookieDigests(page)).toEqual(before);
    const saved = await page.evaluate(() =>
      JSON.parse(sessionStorage.getItem("halo.workforce-termination")!),
    );
    expect(saved).toMatchObject({
      known: [{
        operation: calls[0].operation,
        grant,
        person: account.personId,
        terminatedAt,
      }],
      active: null,
    });
    await page.screenshot({
      path: "test-results/termination-real-recovered.png",
      fullPage: true,
    });
    const shell = await page.request.get("/workforce/termination");
    expect(shell.status()).toBe(200);
    assertDocumentPolicy(shell.headers(), false);
    expect(await shell.text()).not.toContain(account.name);
    await page.unroute(pattern);
    await page.goto("/account");
  };
}
