import { expect, test, type Page, type Route } from "@playwright/test";
import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import type { Account, Session } from "../app/protocol/session";
import type { AdmissionInput, Receipt } from "../app/protocol/admission";
import type { AllocationInput, FixedNumber } from "../app/protocol/number";
import { cookieDigests } from "./identity";
import { assertDocumentPolicy } from "./policy";

type Phase = "original" | "replay" | "current";
type Position = "before" | "after";
export type AdmissionObservation = {
  operation: string;
  input: AdmissionInput;
  operator: { accountId: string; personId: string };
  session: string;
  receipt: { grantId: string; assignmentId: string; grantedAt: string } | null;
};
export type AllocationObservation = {
  operation: string;
  input: AllocationInput;
  accountId: string;
  personId: string;
  session: string;
  number: FixedNumber | null;
};

/** Each acknowledgement comes from the independent Rust database observer. */
async function observe(
  stage: "admission" | "allocation",
  phase: Phase,
  position: Position,
  value: AdmissionObservation | AllocationObservation,
) {
  const path = join(
    process.env.HALO_TEST_MATERIAL!,
    `${stage}-${phase}-${position}`,
  );
  await writeFile(`${path}.pending`, JSON.stringify(value));
  await rename(`${path}.pending`, `${path}.request`);
  await expect
    .poll(() => readFile(`${path}.ack`, "utf8").catch(() => ""), {
      timeout: 10_000,
      message: `Independent ${stage} ${phase} ${position} observation`,
    })
    .toBe("ok");
}

async function picture(page: Page, name: string, dialog = false) {
  const path = process.env.HALO_TEST_EVIDENCE_DIR
    ? join(
        process.env.HALO_TEST_EVIDENCE_DIR,
        "admission-number",
        `${name}.png`,
      )
    : test.info().outputPath("admission-number", `${name}.png`);
  await mkdir(dirname(path), { recursive: true });
  await page.screenshot({ path, fullPage: !dialog });
  await test.info().attach(name, { path, contentType: "image/png" });
}

function responseFor(page: Page, path: string, session: string) {
  return page.waitForResponse(
    (response) =>
      new URL(response.url()).pathname === path &&
      response.request().headers()["session-id"] === session,
  );
}

/** Both business writes originate in the real pages; only identity preparation is external. */
export async function admissionAndFirstNumber(
  page: Page,
  manager: Session,
  colleague: { account: Account; session: Session },
  select: (session: Session) => Promise<void>,
) {
  const viewport = page.viewportSize();
  const mobile = "13900000017";
  const admissionCalls: {
    operation: string;
    session: string;
    input: AdmissionInput;
  }[] = [];
  const allocationCalls: {
    operation: string;
    session: string;
    input: AllocationInput;
  }[] = [];
  let admission: Receipt | null = null;
  let fixed: FixedNumber | null = null;
  let admissionOperation = "";
  let allocationOperation = "";
  let admissionInput: AdmissionInput;
  let operator: Account;
  const allocationInput: AllocationInput = { subtype: 902 };

  const admissionObservation = (phase: Phase, position: Position) =>
    observe("admission", phase, position, {
      operation: admissionOperation,
      input: admissionInput,
      operator: { accountId: operator.id, personId: operator.personId },
      session: phase === "current" ? colleague.session.id : manager.id,
      receipt: admission
        ? {
            grantId: admission.grant.id,
            assignmentId: admission.assignment.id,
            grantedAt: admission.grant.grantedAt,
          }
        : null,
    });
  const allocationObservation = (phase: Phase, position: Position) =>
    observe("allocation", phase, position, {
      operation: allocationOperation,
      input: allocationInput,
      accountId: colleague.account.id,
      personId: colleague.account.personId,
      session: colleague.session.id,
      number: fixed,
    });

  const captureAdmission = async (route: Route) => {
    const request = route.request();
    expect(request.method()).toBe("POST");
    expect(request.postDataJSON()).toEqual(admissionInput);
    expect(request.headers()["session-id"]).toBe(manager.id);
    expect(request.headers()["origin"]).toBe(new URL(page.url()).origin);
    expect(request.headers()["content-type"]).toBe("application/json");
    const original = admissionCalls.length === 0;
    admissionOperation ||= request.headers()["operation-id"];
    expect(request.headers()["operation-id"]).toBe(admissionOperation);
    expect(admissionOperation).toMatch(/^[0-9a-f-]{36}$/i);
    admissionCalls.push({
      operation: admissionOperation,
      session: manager.id,
      input: request.postDataJSON(),
    });
    expect(admissionCalls.length).toBeLessThanOrEqual(2);
    await admissionObservation(original ? "original" : "replay", "before");
    const response = await route.fetch({ maxRedirects: 0, maxRetries: 0 });
    expect(response.status()).toBe(201);
    expect(response.headers()["cache-control"]).toBe("no-store");
    expect(response.headers()["set-cookie"]).toBeUndefined();
    const body = (await response.json()) as Receipt;
    expect(body.code).toBe("ok");
    expect(body.grant.operationId).toBe(admissionOperation);
    expect(body.grant.subject.personId).toBe(colleague.account.personId);
    expect(body.grant.subject.revisionId).toBe(colleague.account.revisionId);
    expect(body.grant.operator.personId).toBe(operator.personId);
    expect(body.grant.status).toBe("active");
    expect(body.assignment.operationId).toBe(admissionOperation);
    expect(body.assignment.grantId).toBe(body.grant.id);
    expect(body.assignment.role).toMatchObject({
      kind: "default",
      ...admissionInput.role,
    });
    if (admission) expect(body).toEqual(admission);
    else admission = body;
    await admissionObservation(original ? "original" : "replay", "after");
    // The observer confirms the committed facts before the client loses its receipt.
    if (original) await route.abort();
    else await route.fulfill({ response });
  };
  const captureAllocation = async (route: Route) => {
    const request = route.request();
    expect(request.method()).toBe("POST");
    expect(request.postDataJSON()).toEqual(allocationInput);
    expect(request.headers()["session-id"]).toBe(colleague.session.id);
    expect(request.headers()["origin"]).toBe(new URL(page.url()).origin);
    expect(request.headers()["content-type"]).toBe("application/json");
    const original = allocationCalls.length === 0;
    allocationOperation ||= request.headers()["operation-id"];
    expect(request.headers()["operation-id"]).toBe(allocationOperation);
    expect(allocationOperation).toMatch(/^[0-9a-f-]{36}$/i);
    allocationCalls.push({
      operation: allocationOperation,
      session: colleague.session.id,
      input: request.postDataJSON(),
    });
    expect(allocationCalls.length).toBeLessThanOrEqual(2);
    await allocationObservation(original ? "original" : "replay", "before");
    const response = await route.fetch({ maxRedirects: 0, maxRetries: 0 });
    expect(response.status()).toBe(201);
    expect(response.headers()["cache-control"]).toBe("no-store");
    expect(response.headers()["set-cookie"]).toBeUndefined();
    const body = (await response.json()) as {
      code: string;
      number: FixedNumber;
    };
    expect(body.code).toBe("ok");
    expect(body.number.subtype).toBe(allocationInput.subtype);
    if (fixed) expect(body.number).toEqual(fixed);
    else fixed = body.number;
    await allocationObservation(original ? "original" : "replay", "after");
    if (original) await route.abort();
    else await route.fulfill({ response });
  };

  await select(manager);
  await page.setExtraHTTPHeaders({ "X-Halo-Fixture-Source": "17" });
  await page.setViewportSize({ width: 1440, height: 1000 });
  try {
    const current = responseFor(page, "/api/v1/accounts/current", manager.id);
    await page.goto("/account");
    operator = (await (await current).json()).account as Account;
    const prerequisites = await page.evaluate(
      async ({ manager, colleague }) => {
        const get = async (session: string, path: string) => {
          const response = await fetch(path, {
            headers: {
              "Session-Id": session,
              "Operation-Id": crypto.randomUUID(),
            },
            credentials: "same-origin",
            cache: "no-store",
            redirect: "error",
          });
          return { status: response.status, body: await response.json() };
        };
        return {
          role: await get(manager, "/api/v1/accounts/current/role"),
          managerGrant: await get(manager, "/api/v1/workforce/current/grant"),
          colleagueNumber: await get(
            colleague,
            "/api/v1/workforce/current/numbers",
          ),
        };
      },
      { manager: manager.id, colleague: colleague.session.id },
    );
    expect(prerequisites.role.status).toBe(200);
    expect(prerequisites.role.body.role).toMatchObject({
      kind: "full",
      level: 1000,
    });
    expect(prerequisites.role.body.role.permissions).toContain(
      "institution.workforce",
    );
    expect(prerequisites.managerGrant).toEqual({
      status: 404,
      body: { code: "grant_absent" },
    });
    expect(prerequisites.colleagueNumber).toEqual({
      status: 200,
      body: { code: "ok", number: null },
    });
    const cookies = await cookieDigests(page);

    await page.getByRole("link", { name: "为同事开通员工服务", exact: true }).click();
    const candidateRead = responseFor(
      page,
      "/api/v1/workforce/grants/candidates",
      manager.id,
    );
    await page.getByLabel("同事的手机号").fill(mobile);
    await page.getByRole("button", { name: "查找同事", exact: true }).click();
    const response = await candidateRead;
    expect(response.status()).toBe(200);
    const candidate = await response.json();
    expect(candidate.identity).toMatchObject({
      accountId: colleague.account.id,
      personId: colleague.account.personId,
      revisionId: colleague.account.revisionId,
      idNumber: colleague.account.idNumber,
    });
    expect(candidate.identity.name).not.toBe(colleague.account.name);
    expect(candidate.suspensionId).toBeNull();
    expect(candidate.grant).toBeNull();
    const role = candidate.roles.find(
      (role: { kind: string }) => role.kind === "default",
    );
    expect(role).toBeDefined();
    await expect(
      page.getByRole("radio", { name: "普通成员 · 默认选择" }),
    ).toBeChecked();
    admissionInput = {
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
    };
    await page.route("**/api/v1/workforce/grants", captureAdmission);
    await page.getByRole("button", { name: "确认开通", exact: true }).click();
    await expect(
      page.getByRole("button", { name: "取消", exact: true }),
    ).toBeFocused();
    await page.keyboard.press("Escape");
    await expect(page.getByRole("dialog")).not.toBeVisible();
    await expect(
      page.getByRole("button", { name: "确认开通", exact: true }),
    ).toBeFocused();
    await page.getByRole("button", { name: "确认开通", exact: true }).click();
    await picture(page, "admission-confirm", true);
    await page
      .getByRole("button", { name: "开通员工服务", exact: true })
      .click();
    await expect(
      page.getByText("暂时无法确认开通结果。", { exact: false }),
    ).toBeVisible();
    expect(admissionCalls).toHaveLength(1);
    await picture(page, "admission-unknown");
    const savedAdmission = await page.evaluate(
      (account) =>
        JSON.parse(
          sessionStorage.getItem(`halo.workforce-admission:${account}`)!,
        ),
      operator.id,
    );
    expect(savedAdmission).toMatchObject({
      operation: admissionOperation,
      input: admissionInput,
      account: operator.id,
      operator: operator.personId,
    });
    await page.reload();
    await expect(
      page.getByRole("button", { name: "检查开通结果", exact: true }),
    ).toBeEnabled({ timeout: 10_000 });
    expect(admissionCalls).toHaveLength(1);
    await page
      .getByRole("button", { name: "检查开通结果", exact: true })
      .click();
    await expect(
      page.getByText("本次员工服务已开通", { exact: true }),
    ).toBeVisible();
    expect(admissionCalls).toHaveLength(2);
    expect(admissionCalls[1]).toEqual(admissionCalls[0]);
    await picture(page, "admission-recovered");
    expect(await cookieDigests(page)).toEqual(cookies);

    await select(colleague.session);
    await admissionObservation("current", "before");
    const grantRead = responseFor(
      page,
      "/api/v1/workforce/current/grant",
      colleague.session.id,
    );
    const document = await page.goto("/workforce");
    assertDocumentPolicy(document!.headers(), false);
    const grant = (await (await grantRead).json()).grant;
    expect(grant).toMatchObject({ id: admission!.grant.id, status: "active" });
    await expect(
      page.getByRole("heading", { name: "已加入员工服务", exact: true }),
    ).toBeVisible();
    const refreshedGrant = responseFor(
      page,
      "/api/v1/workforce/current/grant",
      colleague.session.id,
    );
    await page
      .getByRole("button", { name: "刷新服务状态", exact: true })
      .click();
    expect((await (await refreshedGrant).json()).grant).toMatchObject({
      id: admission!.grant.id,
      status: "active",
    });
    await expect(
      page.getByRole("heading", { name: "已加入员工服务", exact: true }),
    ).toBeVisible();
    await admissionObservation("current", "after");
    await picture(page, "admission-self-active");

    // The admission observer compares all number facts. Finish it before allocating.
    await page.getByRole("link", { name: "申请工号", exact: true }).click();
    await expect(
      page.getByRole("heading", { name: "领取我的工号", exact: true }),
    ).toBeVisible();
    await expect(page.getByLabel("人员类别", { exact: true })).toHaveValue("");
    await expect(
      page.getByRole("button", { name: "领取工号", exact: true }),
    ).toBeDisabled();
    await page.route("**/api/v1/workforce/numbers", captureAllocation);
    await page
      .getByLabel("人员类别", { exact: true })
      .selectOption(String(allocationInput.subtype));
    await page.getByRole("button", { name: "领取工号", exact: true }).click();
    await expect(
      page.getByRole("button", { name: "暂不领取", exact: true }),
    ).toBeFocused();
    await picture(page, "allocation-confirm", true);
    await page.getByRole("button", { name: "确认领取", exact: true }).click();
    await expect(
      page.getByRole("heading", { name: "领取结果待确认", exact: true }),
    ).toBeVisible();
    expect(allocationCalls).toHaveLength(1);
    await picture(page, "allocation-unknown");
    const savedAllocation = await page.evaluate(() =>
      JSON.parse(sessionStorage.getItem("halo.number-allocation")!),
    );
    expect(savedAllocation).toMatchObject({
      operation: allocationOperation,
      input: allocationInput,
      account: colleague.account.id,
      person: colleague.account.personId,
      outcome: "pending",
      number: null,
    });
    await page.reload();
    await expect(
      page.getByRole("button", { name: "查看本次领取结果", exact: true }),
    ).toBeEnabled({ timeout: 10_000 });
    expect(allocationCalls).toHaveLength(1);
    await page
      .getByRole("button", { name: "查看本次领取结果", exact: true })
      .click();
    await expect(
      page.getByRole("heading", { name: "工号已领取", exact: true }),
    ).toBeVisible();
    expect(allocationCalls).toHaveLength(2);
    expect(allocationCalls[1]).toEqual(allocationCalls[0]);
    const number = fixed!;
    // This expectation follows the public number display contract, independently of summarize().
    const display = `9${String(number.year % 100).padStart(2, "0")}${String(number.serial).padStart(2, "0")}`;
    await expect(page.locator(".employee-number")).toHaveText(display);
    await expect(page.getByText("实习", { exact: true })).toBeVisible();
    await picture(page, "allocation-recovered");

    await allocationObservation("current", "before");
    const numberRead = responseFor(
      page,
      "/api/v1/workforce/current/numbers",
      colleague.session.id,
    );
    await page.goto("/workforce");
    expect((await (await numberRead).json()).number).toEqual(number);
    await expect(page.getByText(display, { exact: true })).toBeVisible();
    const refreshedNumber = responseFor(
      page,
      "/api/v1/workforce/current/numbers",
      colleague.session.id,
    );
    await page.getByRole("button", { name: "刷新工号", exact: true }).click();
    expect((await (await refreshedNumber).json()).number).toEqual(number);
    await expect(page.getByText(display, { exact: true })).toBeVisible();
    expect(allocationCalls).toHaveLength(2);
    await allocationObservation("current", "after");
    expect(await cookieDigests(page)).toEqual(cookies);
    await writeFile(
      join(process.env.HALO_TEST_MATERIAL!, "admission-number.json"),
      JSON.stringify({
        admissionCalls,
        allocationCalls,
        grantId: admission!.grant.id,
        assignmentId: admission!.assignment.id,
        number,
      }),
    );
  } finally {
    await page.unroute("**/api/v1/workforce/grants", captureAdmission);
    await page.unroute("**/api/v1/workforce/numbers", captureAllocation);
    await select(manager);
    await page.setExtraHTTPHeaders({ "X-Halo-Fixture-Source": "00" });
    if (viewport) await page.setViewportSize(viewport);
  }
}
