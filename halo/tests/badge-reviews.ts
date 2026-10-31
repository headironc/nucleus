import {
  expect,
  test,
  type Page,
  type Request,
  type Response,
  type Route,
} from "@playwright/test";
import { randomUUID } from "node:crypto";
import {
  appendFile,
  mkdir,
  readFile,
  rename,
  writeFile,
} from "node:fs/promises";
import { dirname, join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import type { components } from "../app/protocol/generated/types";
import type { Account, Session } from "../app/protocol/session";
import type {
  Application,
  ApplicationDetail,
  ApplicationList,
  ManagedApplicationDetail,
  ManagedApplicationList,
  ReviewInput,
} from "../app/protocol/applications";
import { registry } from "../app/protocol/response";
import { cookieDigests } from "./identity";
import { assertDocumentPolicy } from "./policy";
import { trusted, wire, type ownBadgeApplications } from "./badge-applications";

type Actor = { account: Account; session: Session };
type Phase =
  | "denied"
  | "unavailable"
  | "rejected"
  | "fixed"
  | "approved"
  | "refresh"
  | "revoked"
  | "restricted";
type Department = {
  id: string;
  operationId: string;
  name: string;
  enabled: boolean;
  index: number;
  groupId: string;
  parentId: string | null;
};
type Context = {
  manager: Session;
  owner: Actor;
  reviewer: Actor;
  fixture: { role: components["schemas"]["role.Role"]; department: Department };
  submitted: Awaited<ReturnType<typeof ownBadgeApplications>>;
  select: (session: Session) => Promise<void>;
};
type Observation = {
  accountId: string;
  personId: string;
  session: string;
  applicationIds: string[];
  targetId: string;
  resourceId: string;
  roleId: string;
  originalGrantId: string;
  ownerGrantId: string;
  operation: string | null;
  input: ReviewInput | null;
  status: number | null;
  application: Application | null;
  ownRead: {
    caller: { accountId: string; personId: string; session: string };
    response: ApplicationList;
  } | null;
};

export async function reviewBadgeApplications(page: Page, context: Context) {
  const { manager, owner, reviewer, fixture, submitted, select } = context;
  const material = process.env.HALO_TEST_MATERIAL;
  if (!material)
    throw new Error("Badge reviews require supervised observation material");
  const evidence = join(
    process.env.HALO_TEST_EVIDENCE_DIR ?? material,
    "badge-review-reads.jsonl",
  );
  const record = (stage: string, facts: Record<string, unknown> = {}) =>
    appendFile(
      evidence,
      `${JSON.stringify({ stage, at: new Date().toISOString(), ...facts })}\n`,
    );
  const viewport = page.viewportSize();
  const theme = await page.evaluate(() =>
    matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light",
  );
  await page.setViewportSize({ width: 1440, height: 1000 });
  await page.emulateMedia({ colorScheme: "light" });
  const cookies = await cookieDigests(page);
  const records = submitted.applications;
  const rejected = records.find((item) => item.kind === "duplication")!;
  const approved = records.find((item) => item.kind === "replacement")!;
  const pending = records.find((item) => item.kind === "issuance")!;
  expect(records).toHaveLength(4);
  expect(records.every((item) => item.status === "submitted")).toBe(true);
  expect(rejected && approved && pending).toBeTruthy();
  let role = fixture.role;
  let ownerGrantId = submitted.grantId;
  let nextSend = performance.now() + 2000;
  let cadence = Promise.resolve();
  const writes: {
    operation: string;
    target: string;
    input: ReviewInput;
    status: number;
  }[] = [];
  let attempt:
    | {
        phase: "rejected" | "approved";
        target: Application;
        lost: boolean;
        handled: boolean;
      }
    | undefined;

  const source = (session: string) =>
    session === manager.id ? "00" : session === owner.session.id ? "17" : "18";
  const selectActor = async (actor: Session) => {
    await page.setExtraHTTPHeaders({
      "X-Halo-Fixture-Source": source(actor.id),
    });
    await select(actor);
  };
  const raw = async (
    session: string,
    path: string,
    method = "GET",
    input?: unknown,
    operation = randomUUID(),
  ) => {
    await page.setExtraHTTPHeaders({
      "X-Halo-Fixture-Source": source(session),
    });
    const target = new URL(path, page.url());
    const events: Record<string, unknown>[] = [];
    const note = (stage: string, facts: Record<string, unknown> = {}) =>
      events.push({ stage, at: new Date().toISOString(), ...facts });
    const relevant = (request: Request) =>
      new URL(request.url()).pathname === target.pathname &&
      new URL(request.url()).search === target.search &&
      request.method() === method;
    const matches = (request: Request) => ({
      sessionMatches: request.headers()["session-id"] === session,
      operationMatches: request.headers()["operation-id"] === operation,
    });
    const requested = (request: Request) => {
      if (relevant(request)) note("request", matches(request));
    };
    const responded = (response: Response) => {
      if (relevant(response.request()))
        note("response", {
          status: response.status(),
          ...matches(response.request()),
        });
    };
    const failed = (request: Request) => {
      if (relevant(request))
        note("request-failed", {
          ...matches(request),
          failure: request.failure()?.errorText ?? null,
        });
    };
    page.on("request", requested);
    page.on("response", responded);
    page.on("requestfailed", failed);
    await record("raw-start", { method, path, operation });
    const response = page.waitForResponse(
      (value) => {
        const request = value.request();
        const matched = matches(request);
        return (
          relevant(request) &&
          matched.sessionMatches &&
          matched.operationMatches
        );
      },
      { timeout: 35_000 },
    );
    const consumed = page.evaluate(
      async ({ session, path, method, input, operation }) => {
        const headers: Record<string, string> = {
          "Session-Id": session,
          "Operation-Id": operation,
        };
        if (input !== undefined) headers["Content-Type"] = "application/json";
        const response = await fetch(path, {
          method,
          headers,
          credentials: "same-origin",
          redirect: "error",
          cache: "no-store",
          signal: AbortSignal.timeout(35_000),
          ...(input === undefined ? {} : { body: JSON.stringify(input) }),
        });
        // Finish the browser body before Playwright inspects its retained bytes.
        await response.arrayBuffer();
      },
      { session, path, method, input, operation },
    );
    let responseMatched = false,
      bodyConsumed = false;
    try {
      const [received] = await Promise.all([
        response.then((value) => {
          responseMatched = true;
          note("response-matched", { status: value.status() });
          return value;
        }),
        consumed.then(() => {
          bodyConsumed = true;
          note("browser-body-consumed");
        }),
      ]);
      return received;
    } finally {
      page.off("request", requested);
      page.off("response", responded);
      page.off("requestfailed", failed);
      note("wait-finished", { responseMatched, bodyConsumed });
      await appendFile(
        evidence,
        events
          .map((event) => JSON.stringify({ method, path, operation, ...event }))
          .join("\n") + "\n",
      );
      await record("raw-finished", {
        method,
        path,
        operation,
        responseMatched,
        bodyConsumed,
      });
    }
  };
  const setup = async (
    session: string,
    path: string,
    input: unknown,
    method: string,
    status: number,
  ) => {
    await delay(550);
    const response = await wire(await raw(session, path, method, input));
    expect(response.status, `Review setup ${path}`).toBe(status);
    expect(response.cacheControl).toBe("no-store");
    expect(response.setCookie).toBe(false);
    return JSON.parse(response.text);
  };
  const configureApproval = async (enabled: boolean) => {
    const value = await setup(
      manager.id,
      `/api/v1/workforce/roles/${role.id}`,
      {
        entry: "institution",
        changeId: role.changeId,
        name: role.name,
        level: role.level,
        permissions: enabled
          ? ["workforce.directory", "workforce.approval"]
          : ["workforce.directory"],
      },
      "PUT",
      200,
    );
    expect(value.role.id).toBe(role.id);
    expect(value.role.changeId).not.toBe(role.changeId);
    role = value.role;
  };
  const observe = async (
    phase: Phase,
    position: "before" | "after",
    target: Application,
    value: Partial<Observation> = {},
  ) => {
    const actor = phase === "restricted" ? owner : reviewer;
    const request: Observation = {
      accountId: actor.account.id,
      personId: actor.account.personId,
      session: actor.session.id,
      applicationIds: records.map((item) => item.id),
      targetId: target.id,
      resourceId: target.photo.resourceId,
      roleId: role.id,
      originalGrantId: submitted.grantId,
      ownerGrantId,
      operation: null,
      input: null,
      status: null,
      application: null,
      ownRead: null,
      ...value,
    };
    const path = join(material, `badge-review-${phase}-${position}`);
    await writeFile(`${path}.pending`, JSON.stringify(request));
    await rename(`${path}.pending`, `${path}.request`);
    await expect
      .poll(() => readFile(`${path}.ack`, "utf8").catch(() => ""), {
        timeout: 10_000,
        message: `Independent badge review ${phase}/${position}`,
      })
      .toBe("ok");
  };
  const picture = async (name: string) => {
    const path = process.env.HALO_TEST_EVIDENCE_DIR
      ? join(process.env.HALO_TEST_EVIDENCE_DIR, "badge-reviews", `${name}.png`)
      : test.info().outputPath("badge-reviews", `${name}.png`);
    await mkdir(dirname(path), { recursive: true });
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    ).toBe(true);
    await page.screenshot({ path, fullPage: true });
    await test.info().attach(name, { path, contentType: "image/png" });
  };
  const readDetail = async (target: Application) => {
    const value = await trusted<ManagedApplicationDetail>(
      await wire(
        await raw(
          reviewer.session.id,
          `/api/v1/workforce/applications/${target.id}`,
        ),
      ),
      "GET /api/v1/workforce/applications/{id}",
      200,
    );
    expect(value.application.id).toBe(target.id);
    expect(value.application.snapshot).toEqual(target.snapshot);
    expect(value.application.photo).toEqual(target.photo);
    const serialized = JSON.stringify(value);
    for (const hidden of [
      "13900000017",
      "13900000018",
      "000000200002298880",
      "000000200002298899",
    ])
      expect(serialized.includes(hidden)).toBe(false);
    return value;
  };
  const open = async (target: Application) => {
    await selectActor(reviewer.session);
    const document = await page.goto(`/workforce/reviews/${target.id}`);
    assertDocumentPolicy(document!.headers(), false);
    await expect(
      page.getByRole("heading", { name: "申请内容", exact: true }),
    ).toBeVisible();
  };
  const reviewRoute = async (route: Route) => {
    if (route.request().method() !== "POST") return route.continue();
    const request = route.request();
    expect(request.headers()["session-id"]).toBe(reviewer.session.id);
    const operation = request.headers()["operation-id"];
    expect(operation).toBeTruthy();
    const input = request.postDataJSON() as ReviewInput;
    expect(registry.workforceApplicationReview_input(input)).toBe(true);
    const target = new URL(request.url()).pathname.split("/").at(-2)!;
    const current = attempt;
    if (current) {
      expect(current.handled).toBe(false);
      current.handled = true;
      expect(target).toBe(current.target.id);
      await observe(current.phase, "before", current.target, {
        operation,
        input,
      });
    }
    const ready = cadence.then(async () => {
      while (performance.now() < nextSend)
        await delay(Math.ceil(nextSend - performance.now()));
      nextSend = performance.now() + 500;
    });
    cadence = ready;
    await ready;
    const response = await route.fetch({
      maxRetries: 0,
      maxRedirects: 0,
      timeout: 35_000,
    });
    writes.push({ operation, target, input, status: response.status() });
    if (current) {
      const receipt = await trusted<{ application: Application }>(
        await wire(response),
        "POST /api/v1/workforce/applications/{id}/reviews",
        200,
      );
      expect(receipt.application.status).toBe(
        current.phase === "approved" ? "approved" : "rejected",
      );
      expect(receipt.application.snapshot).toEqual(current.target.snapshot);
      expect(receipt.application.photo).toEqual(current.target.photo);
      if (receipt.application.status !== "submitted")
        expect(receipt.application.reviewerId).toBe(reviewer.account.personId);
      await observe(current.phase, "after", current.target, {
        operation,
        input,
        status: 200,
        application: receipt.application,
      });
      if (current.lost) return route.abort("connectionclosed");
    }
    await route.fulfill({ response });
  };
  const routePattern = /\/api\/v1\/workforce\/applications\/[^/]+\/reviews$/;
  const rejectedWrite = async (
    phase: "unavailable" | "revoked",
    target: Application,
    status: number,
    code: string,
  ) => {
    const operation = randomUUID(),
      input: ReviewInput = { decision: "approve" };
    await observe(phase, "before", target, { operation, input });
    const result = await trusted<{ code: string }>(
      await wire(
        await raw(
          reviewer.session.id,
          `/api/v1/workforce/applications/${target.id}/reviews`,
          "POST",
          input,
          operation,
        ),
      ),
      "POST /api/v1/workforce/applications/{id}/reviews",
      status,
    );
    expect(result.code).toBe(code);
    return { operation, input, status };
  };
  const resourceDenied = async (session: string, target: Application) => {
    const response = await wire(
      await raw(
        session,
        `/api/v1/workforce/application-resources/${target.photo.resourceId}`,
      ),
    );
    const result = await trusted<{ code: string }>(
      response,
      "GET /api/v1/workforce/application-resources/{id}",
      404,
    );
    expect(result.code).toBe("target_unavailable");
    return response.status;
  };
  const readPhoto = async (target: Application) => {
    const response = await raw(
      reviewer.session.id,
      `/api/v1/workforce/application-resources/${target.photo.resourceId}`,
    );
    expect(response.status()).toBe(200);
    expect(response.headers()["content-type"]).toBe(target.photo.mediaType);
    expect(response.headers()["cache-control"]).toBe("no-store");
    expect(response.headers()["set-cookie"]).toBeUndefined();
    const bytes = await response.body();
    expect(bytes.length).toBeGreaterThan(0);
    return bytes;
  };
  await page.route(routePattern, reviewRoute);
  try {
    await selectActor(reviewer.session);
    await observe("denied", "before", rejected);
    const document = await page.goto("/workforce/reviews");
    assertDocumentPolicy(document!.headers(), false);
    await expect(
      page.getByText("目前无法查看工牌审核。", { exact: true }),
    ).toBeVisible();
    await record("denied-ui-visible");
    const denied = await wire(
      await raw(reviewer.session.id, "/api/v1/workforce/applications"),
    );
    await trusted(denied, "GET /api/v1/workforce/applications", 403);
    await record("denied-management-verified", { status: denied.status });
    const resourceStatus = await resourceDenied(reviewer.session.id, rejected);
    await record("denied-resource-verified", { status: resourceStatus });
    await picture("denied-management");
    await observe("denied", "after", rejected);
    await record("denied-after-observed");

    await configureApproval(true);
    await open(rejected);
    await expect(
      page.getByRole("button", { name: "批准申请", exact: true }),
    ).toBeDisabled();
    await expect(
      page.getByRole("button", { name: "驳回申请", exact: true }),
    ).toBeEnabled();
    const unavailable = await rejectedWrite(
      "unavailable",
      rejected,
      409,
      "applicant_unavailable",
    );
    const inactive = await readDetail(rejected);
    expect(inactive.current.accountActive).toBe(true);
    expect(inactive.current.participationActive).toBe(false);
    const originalPhoto = await readPhoto(rejected);
    await picture("inactive-applicant");
    await observe("unavailable", "after", rejected, unavailable);

    const reason =
      "  照片边缘未完整保留，请重新确认构图并补充清晰照片。此次申请保留原记录，审核意见可在个人申请中查看。  ";
    attempt = {
      phase: "rejected",
      target: rejected,
      lost: false,
      handled: false,
    };
    await page.getByRole("button", { name: "驳回申请", exact: true }).click();
    await page.getByRole("textbox", { name: "驳回理由" }).fill(reason);
    await picture("reject-confirmation");
    await page.getByRole("button", { name: "确认驳回", exact: true }).click();
    await expect(
      page.getByRole("heading", { name: "未通过原因", exact: true }),
    ).toBeVisible();
    await expect(page.locator(".review-reason")).toContainText(reason.trim());
    expect(attempt.handled).toBe(true);
    attempt = undefined;
    await picture("rejected-result");

    const candidate = await setup(
      manager.id,
      "/api/v1/workforce/grants/candidates",
      {
        entry: "institution",
        mobile: "13900000017",
      },
      "POST",
      200,
    );
    expect(candidate.identity).toMatchObject({
      accountId: owner.account.id,
      personId: owner.account.personId,
      revisionId: owner.account.revisionId,
    });
    const ordinary = candidate.roles.find(
      (item: { kind: string }) => item.kind === "default",
    );
    expect(ordinary).toBeTruthy();
    const admitted = await setup(
      manager.id,
      "/api/v1/workforce/grants",
      {
        entry: "institution",
        target: {
          accountId: candidate.identity.accountId,
          personId: candidate.identity.personId,
          revisionId: candidate.identity.revisionId,
          mobile: "13900000017",
          assignmentId: candidate.assignmentId,
          suspensionId: candidate.suspensionId,
          grant: candidate.grant,
        },
        role: { id: ordinary.id, changeId: ordinary.changeId },
      },
      "POST",
      201,
    );
    ownerGrantId = admitted.grant.id;
    expect(ownerGrantId).not.toBe(submitted.grantId);
    expect(admitted.grant.status).toBe("active");
    const department = fixture.department;
    const expected = {
      id: department.id,
      operationId: department.operationId,
      name: department.name,
      enabled: department.enabled,
      index: department.index,
      groupId: department.groupId,
      parentId: department.parentId,
    };
    await setup(
      reviewer.session.id,
      `/api/v1/workforce/directories/departments/${department.id}`,
      {
        expected,
        replacement: {
          ...expected,
          name: "Changed fixture department",
          enabled: false,
        },
      },
      "PUT",
      200,
    );

    await observe("fixed", "before", approved);
    let failed = false;
    const failPhoto = async (route: Route) => {
      if (failed) return route.continue();
      failed = true;
      await route.abort("connectionclosed");
    };
    const photoPath = `**/api/v1/workforce/application-resources/${approved.photo.resourceId}`;
    await page.route(photoPath, failPhoto);
    await open(approved);
    await expect(
      page.getByText("工牌预览暂时无法显示，请重新加载。", { exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: "批准申请", exact: true }),
    ).toBeDisabled();
    await expect(
      page.getByRole("button", { name: "驳回申请", exact: true }),
    ).toBeEnabled();
    await expect(page.locator(".review-current")).toContainText(
      "Changed fixture department",
    );
    await expect(page.locator(".application-snapshot").first()).toContainText(
      approved.snapshot.department.printed,
    );
    await picture("media-failure-retains-detail");
    await page.unroute(photoPath, failPhoto);
    await page
      .getByRole("button", { name: "重新加载预览", exact: true })
      .click({ timeout: 10_000 });
    await expect(
      page.getByRole("img", { name: "工牌预览", exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: "批准申请", exact: true }),
    ).toBeEnabled();
    const fixed = await readDetail(approved);
    expect(fixed.current.participationActive).toBe(true);
    expect(fixed.current.department.enabled).toBe(false);
    await picture("fixed-preview-and-current-difference");
    await observe("fixed", "after", approved);

    attempt = {
      phase: "approved",
      target: approved,
      lost: true,
      handled: false,
    };
    await page.getByRole("button", { name: "批准申请", exact: true }).click();
    await expect(page.getByRole("dialog")).toBeVisible();
    await picture("approve-confirmation");
    await page.getByRole("button", { name: "确认批准", exact: true }).click();
    await expect(
      page.getByRole("heading", { name: "暂时无法确认审核结果", exact: true }),
    ).toBeVisible();
    expect(attempt.handled).toBe(true);
    attempt = undefined;
    await picture("approval-response-lost");

    const conflict = {
      operation: randomUUID(),
      input: { decision: "approve" } as ReviewInput,
      status: 409,
    };
    await observe("refresh", "before", approved, {
      operation: conflict.operation,
      input: conflict.input,
    });
    const count = writes.length;
    await page.getByRole("button", { name: "刷新详情", exact: true }).click();
    await expect(
      page.getByRole("heading", { name: "审核结果", exact: true }),
    ).toBeVisible();
    await expect(page.locator(".review-decision")).toContainText("审核通过");
    expect(writes).toHaveLength(count);
    const decision = await readDetail(approved);
    expect(decision.application.status).toBe("approved");
    expect((await readPhoto(rejected)).equals(originalPhoto)).toBe(true);
    for (const [status, size] of [
      ["submitted", 2],
      ["approved", 1],
      ["rejected", 1],
    ] as const) {
      const list = await trusted<ManagedApplicationList>(
        await wire(
          await raw(
            reviewer.session.id,
            `/api/v1/workforce/applications?status=${status}&size=1`,
          ),
        ),
        "GET /api/v1/workforce/applications",
        200,
      );
      expect(list.count).toBe(size);
      expect(list.applications).toHaveLength(1);
      expect(list.applications[0].status).toBe(status);
    }
    await selectActor(owner.session);
    await page.goto(`/workforce/applications/${rejected.id}`);
    await expect(page.locator(".review-reason")).toContainText(reason.trim());
    await page.setViewportSize({ width: 320, height: 900 });
    await page.emulateMedia({ colorScheme: "dark" });
    await picture("owner-rejection-320-dark");
    const own = await trusted<ApplicationList>(
      await wire(
        await raw(owner.session.id, "/api/v1/workforce/current/applications"),
      ),
      "GET /api/v1/workforce/current/applications",
      200,
    );
    expect(own.access).toBe("full");
    expect(own.count).toBe(4);
    expect(
      own.applications.filter((item) => item.status === "approved"),
    ).toHaveLength(1);
    expect(
      own.applications.filter((item) => item.status === "rejected"),
    ).toHaveLength(1);
    const duplicate = await trusted<{ code: string }>(
      await wire(
        await raw(
          reviewer.session.id,
          `/api/v1/workforce/applications/${approved.id}/reviews`,
          "POST",
          conflict.input,
          conflict.operation,
        ),
      ),
      "POST /api/v1/workforce/applications/{id}/reviews",
      409,
    );
    expect(duplicate.code).toBe("state_changed");
    await observe("refresh", "after", approved, {
      ...conflict,
      ownRead: {
        caller: {
          accountId: owner.account.id,
          personId: owner.account.personId,
          session: owner.session.id,
        },
        response: own,
      },
    });

    await configureApproval(false);
    const revoked = await rejectedWrite(
      "revoked",
      pending,
      403,
      "permission_denied",
    );
    await selectActor(reviewer.session);
    await page.goto(`/workforce/reviews/${pending.id}`);
    await expect(
      page.getByText("目前无法查看工牌审核。", { exact: true }),
    ).toBeVisible();
    await resourceDenied(reviewer.session.id, pending);
    await picture("revoked-management");
    await observe("revoked", "after", pending, revoked);

    await raw(
      owner.session.id,
      `/api/v1/workforce/grants/${ownerGrantId}/terminations`,
      "POST",
    ).then(async (response) =>
      trusted(
        await wire(response),
        "POST /api/v1/workforce/grants/{id}/terminations",
        201,
      ),
    );
    await selectActor(owner.session);
    await observe("restricted", "before", approved);
    const restricted = await trusted<ApplicationList>(
      await wire(
        await raw(owner.session.id, "/api/v1/workforce/current/applications"),
      ),
      "GET /api/v1/workforce/current/applications",
      200,
    );
    expect(restricted.access).toBe("restricted");
    expect(restricted.count).toBe(3);
    expect(restricted.applications).toHaveLength(3);
    for (const item of restricted.applications)
      expect(Object.keys(item).sort()).toEqual([
        "id",
        "kind",
        "status",
        "submittedAt",
      ]);
    expect(
      restricted.applications.some((item) => item.id === rejected.id),
    ).toBe(false);
    await trusted(
      await wire(
        await raw(
          owner.session.id,
          `/api/v1/workforce/current/applications/${rejected.id}`,
        ),
      ),
      "GET /api/v1/workforce/current/applications/{id}",
      404,
    );
    const detail = await trusted<ApplicationDetail>(
      await wire(
        await raw(
          owner.session.id,
          `/api/v1/workforce/current/applications/${approved.id}`,
        ),
      ),
      "GET /api/v1/workforce/current/applications/{id}",
      200,
    );
    expect(detail.access).toBe("restricted");
    expect(Object.keys(detail.application).sort()).toEqual([
      "id",
      "kind",
      "status",
      "submittedAt",
    ]);
    await resourceDenied(owner.session.id, approved);
    await page.goto("/workforce/applications");
    await expect(
      page.getByRole("link", { name: "查看详情", exact: true }),
    ).toHaveCount(3);
    await expect(page.locator(".review-facts, .review-reason")).toHaveCount(0);
    await picture("restricted-three-summaries");
    await observe("restricted", "after", approved, {
      ownRead: {
        caller: {
          accountId: owner.account.id,
          personId: owner.account.personId,
          session: owner.session.id,
        },
        response: restricted,
      },
    });
    expect(writes).toHaveLength(5);
    expect(writes.filter((value) => value.status === 200)).toHaveLength(2);
    expect(await cookieDigests(page)).toEqual(cookies);
    await writeFile(
      join(material, "badge-reviews.json"),
      JSON.stringify({
        writes,
        ownerGrantId,
        originalGrantId: submitted.grantId,
      }),
    );
    return role;
  } finally {
    // Playwright closes timed-out pages; retain the original request failure.
    if (!page.isClosed()) {
      await page.unroute(routePattern, reviewRoute);
      await page.emulateMedia({ colorScheme: theme });
      await selectActor(manager);
      if (viewport) await page.setViewportSize(viewport);
    }
  }
}
