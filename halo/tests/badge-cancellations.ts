import { expect, test, type Page, type Route } from "@playwright/test";
import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { randomUUID } from "node:crypto";
import { setTimeout as delay } from "node:timers/promises";
import type { Account, Session } from "../app/protocol/session";
import type { components } from "../app/protocol/generated/types";
import type {
  Application,
  ApplicationList,
  ManagedApplicationDetail,
} from "../app/protocol/applications";
import { trusted, wire, type ownBadgeApplications } from "./badge-applications";
import { cookieDigests } from "./identity";
import { assertDocumentPolicy } from "./policy";
import { redactText } from "../tools/redact.ts";

type Actor = { account: Account; session: Session };
type Context = {
  manager: Session;
  owner: Actor;
  reviewer: Actor;
  role: components["schemas"]["role.Role"];
  submitted: Awaited<ReturnType<typeof ownBadgeApplications>>;
  select: (session: Session) => Promise<void>;
};
type Phase = "self" | "manager" | "refresh";
type Attempt = {
  operation: string;
  input: components["schemas"]["application.CancellationInput"];
  response: components["schemas"]["application.Cancellation"] | null;
  status: number | null;
};
type OwnRead = {
  caller: { accountId: string; personId: string; session: string };
  targetId: string;
  resourceId: string;
  response: ApplicationList;
  detailStatus: number;
  resourceStatus: number;
};

/** Continue the accepted four-application fixture without creating another applicant. */
export async function cancelBadgeApplications(page: Page, context: Context) {
  const { manager, owner, reviewer, submitted, select } = context;
  const material = process.env.HALO_TEST_MATERIAL;
  if (!material)
    throw new Error("Badge cancellations require supervised material");
  const records = submitted.applications;
  const pending = records.find((item) => item.kind === "issuance")!;
  const approved = records.find((item) => item.kind === "replacement")!;
  expect(records).toHaveLength(4);
  expect(pending && approved).toBeTruthy();
  const cookies = await cookieDigests(page);
  const viewport = page.viewportSize();
  const theme = await page.evaluate(() =>
    matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light",
  );
  const source = (session: string) =>
    session === manager.id ? "00" : session === owner.session.id ? "17" : "18";
  const actor = (value: Actor) => ({
    accountId: value.account.id,
    personId: value.account.personId,
    session: value.session.id,
  });
  const safeFailure = (error: unknown, stage: string) => {
    let detail = error instanceof Error ? error.message : String(error);
    for (const session of [manager.id, owner.session.id, reviewer.session.id])
      detail = detail.replaceAll(session, "<redacted-session>");
    return new Error(`${stage}: ${redactText(detail).text}`);
  };
  const choose = async (session: Session) => {
    await page.setExtraHTTPHeaders({
      "X-Halo-Fixture-Source": source(session.id),
    });
    await select(session);
  };
  // Setup and independent reads use Playwright's bounded HTTP client. The two
  // cancellation writes themselves must originate in the actual page below.
  const read = async (
    session: string,
    path: string,
    method = "GET",
    data?: unknown,
  ) => {
    try {
      return await page.request.fetch(new URL(path, page.url()).href, {
        method,
        ...(data === undefined ? {} : { data }),
        headers: {
          "Session-Id": session,
          "Operation-Id": randomUUID(),
          Origin: new URL(page.url()).origin,
          "X-Halo-Fixture-Source": source(session),
        },
        timeout: 35_000,
        maxRetries: 0,
        maxRedirects: 0,
      });
    } catch (error) {
      throw safeFailure(error, `${method} ${path} transport outcome unknown`);
    }
  };
  const observe = async (
    phase: Phase,
    position: "before" | "after",
    target: Application,
    attempt: Attempt | null,
    ownRead: OwnRead | null = null,
    managed: ManagedApplicationDetail | null = null,
  ) => {
    const path = join(material, `badge-cancellation-${phase}-${position}`);
    await writeFile(
      `${path}.pending`,
      JSON.stringify({
        caller: actor(phase === "self" ? owner : reviewer),
        targetId: target.id,
        attempt,
        ownRead,
        managed,
      }),
    );
    await rename(`${path}.pending`, `${path}.request`);
    await expect
      .poll(() => readFile(`${path}.ack`, "utf8").catch(() => ""), {
        timeout: 10_000,
        message: `Independent badge cancellation ${phase}/${position}`,
      })
      .toBe("ok");
  };
  const picture = async (name: string) => {
    const path = process.env.HALO_TEST_EVIDENCE_DIR
      ? join(
          process.env.HALO_TEST_EVIDENCE_DIR,
          "badge-cancellations",
          `${name}.png`,
        )
      : test.info().outputPath("badge-cancellations", `${name}.png`);
    await mkdir(dirname(path), { recursive: true });
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    ).toBe(true);
    await page.screenshot({ path, fullPage: true });
    await test.info().attach(name, { path, contentType: "image/png" });
  };
  const own = async (target: Application, count: number): Promise<OwnRead> => {
    const response = await trusted<ApplicationList>(
      await wire(
        await read(owner.session.id, "/api/v1/workforce/current/applications"),
      ),
      "GET /api/v1/workforce/current/applications",
      200,
    );
    expect(response.access).toBe("restricted");
    expect(response.count).toBe(count);
    expect(response.applications).toHaveLength(count);
    expect(response.applications.some((item) => item.id === target.id)).toBe(
      false,
    );
    for (const item of response.applications)
      expect(Object.keys(item).sort()).toEqual([
        "id",
        "kind",
        "status",
        "submittedAt",
      ]);
    const detail = await trusted<{ code: string }>(
      await wire(
        await read(
          owner.session.id,
          `/api/v1/workforce/current/applications/${target.id}`,
        ),
      ),
      "GET /api/v1/workforce/current/applications/{id}",
      404,
    );
    expect(detail.code).toBe("target_unavailable");
    const resource = await read(
      owner.session.id,
      `/api/v1/workforce/application-resources/${target.photo.resourceId}`,
    );
    const unavailable = await trusted<{ code: string }>(
      await wire(resource),
      "GET /api/v1/workforce/application-resources/{id}",
      404,
    );
    expect(unavailable.code).toBe("target_unavailable");
    await resource.dispose();
    return {
      caller: actor(owner),
      targetId: target.id,
      resourceId: target.photo.resourceId,
      response,
      detailStatus: 404,
      resourceStatus: 404,
    };
  };
  const writes: Array<{ targetId: string; attempt: Attempt }> = [];
  let recovering = false;
  const recoveryRequests: Array<{ method: string; path: string }> = [];
  const recoveringRequest = (request: import("@playwright/test").Request) => {
    const path = new URL(request.url()).pathname;
    if (recovering && path.startsWith("/api/"))
      recoveryRequests.push({ method: request.method(), path });
  };
  page.on("request", recoveringRequest);
  let posts = 0;
  const routeErrors: string[] = [];
  let active:
    | {
        phase: "self" | "manager";
        target: Application;
        attempt?: Attempt;
        resolve: () => void;
        reject: (error: unknown) => void;
      }
    | undefined;
  let nextSend = performance.now() + 750;
  const pattern = "**/api/v1/workforce/applications/*/cancellations";
  const cancellation = async (route: Route) => {
    if (route.request().method() !== "POST") return route.continue();
    posts += 1;
    const expected = active;
    try {
      expect(
        expected,
        "only an explicit page confirmation may cancel",
      ).toBeTruthy();
      const target = expected!.target;
      expect(new URL(route.request().url()).pathname).toBe(
        `/api/v1/workforce/applications/${target.id}/cancellations`,
      );
      expect(
        route.request().headers()["session-id"] ===
          (expected!.phase === "self" ? owner.session.id : reviewer.session.id),
      ).toBe(true);
      const input = route.request().postDataJSON();
      expect(input.mode).toBe(expected!.phase);
      expect(input.expectedStatus).toBe(
        expected!.phase === "self" ? "submitted" : "approved",
      );
      const attempt: Attempt = {
        operation: route.request().headers()["operation-id"]!,
        input,
        status: null,
        response: null,
      };
      expect(attempt.operation).toMatch(/^[0-9a-f-]{36}$/);
      expected!.attempt = attempt;
      await delay(Math.max(0, nextSend - performance.now()));
      nextSend = performance.now() + 550;
      await observe(expected!.phase, "before", target, attempt);
      const response = await route.fetch({
        timeout: 35_000,
        maxRetries: 0,
        maxRedirects: 0,
      });
      attempt.status = response.status();
      attempt.response = await trusted(
        await wire(response),
        "POST /api/v1/workforce/applications/{id}/cancellations",
        200,
      );
      expect(attempt.response).toEqual({
        code: "ok",
        application: { id: target.id, status: "cancelled" },
      });
      writes.push({ targetId: target.id, attempt });
      if (expected!.phase === "manager") {
        await observe("manager", "after", target, attempt);
        recovering = true;
        await route.abort("connectionclosed");
      } else await route.fulfill({ response });
      expected!.resolve();
    } catch (error) {
      routeErrors.push("cancellation interception failed");
      expected?.reject(safeFailure(error, "actual page cancellation"));
      await route.abort("failed").catch(() => {});
    }
  };
  const submit = async (
    phase: "self" | "manager",
    target: Application,
    confirm: () => Promise<void>,
  ) => {
    const completed = new Promise<void>((resolve, reject) => {
      active = { phase, target, resolve, reject };
    });
    const timer = setTimeout(
      () =>
        active?.reject(
          new Error(
            `The ${phase} cancellation did not finish within 35 seconds`,
          ),
        ),
      35_000,
    );
    try {
      await Promise.all([confirm(), completed]);
      const attempt = active!.attempt;
      if (!attempt) throw new Error("The page did not send a cancellation");
      return attempt;
    } finally {
      clearTimeout(timer);
      active = undefined;
    }
  };
  await page.route(pattern, cancellation);
  try {
    await page.setViewportSize({ width: 320, height: 900 });
    await page.emulateMedia({ colorScheme: "dark" });
    await choose(owner.session);
    const document = await page.goto(`/workforce/applications/${pending.id}`);
    assertDocumentPolicy(document!.headers(), false);
    await expect(
      page.locator(".application-preview, .review-facts, .review-reason"),
    ).toHaveCount(0);
    await page.getByRole("button", { name: "取消申请", exact: true }).click();
    await picture("self-restricted-confirmation");
    const self = await submit("self", pending, () =>
      page
        .getByRole("dialog")
        .getByRole("button", { name: "取消申请", exact: true })
        .click({ timeout: 10_000 }),
    );
    await expect(
      page.getByRole("heading", { name: "申请已取消", exact: true }),
    ).toBeVisible();
    const afterSelf = await own(pending, 2);
    await observe("self", "after", pending, self, afterSelf);
    const hiddenDetail = page.waitForResponse(
      (response) =>
        response.request().method() === "GET" &&
        new URL(response.url()).pathname ===
          `/api/v1/workforce/current/applications/${pending.id}`,
      { timeout: 35_000 },
    );
    await page.goto(`/workforce/applications/${pending.id}`);
    const hidden = await trusted<{ code: string }>(
      await wire(await hiddenDetail),
      "GET /api/v1/workforce/current/applications/{id}",
      404,
    );
    expect(hidden.code).toBe("target_unavailable");
    await expect(
      page.getByText("无法使用这项内容，请刷新后重新准备。", { exact: true }),
    ).toBeVisible();
    await expect(
      page.locator(
        ".own-application-detail, .application-preview, .application-snapshot, .review-facts, .review-reason",
      ),
    ).toHaveCount(0);
    await expect(
      page.getByRole("button", { name: "取消申请", exact: true }),
    ).toHaveCount(0);
    await picture("self-cancelled-hidden");

    await delay(550);
    const roleResponse = await wire(
      await read(
        manager.id,
        `/api/v1/workforce/roles/${context.role.id}`,
        "PUT",
        {
          entry: "institution",
          changeId: context.role.changeId,
          name: context.role.name,
          level: context.role.level,
          permissions: ["workforce.directory", "workforce.approval"],
        },
      ),
    );
    expect(roleResponse.status).toBe(200);
    expect(roleResponse.cacheControl).toBe("no-store");
    expect(roleResponse.setCookie).toBe(false);
    const updated = JSON.parse(roleResponse.text) as {
      role: components["schemas"]["role.Role"];
    };
    expect(updated.role.id).toBe(context.role.id);
    expect(updated.role.changeId).not.toBe(context.role.changeId);
    nextSend = performance.now() + 550;
    await choose(reviewer.session);
    await page.setViewportSize({ width: 1440, height: 1000 });
    await page.emulateMedia({ colorScheme: "light" });
    await page.goto(`/workforce/reviews/${approved.id}`);
    await expect(page.getByText("审核通过", { exact: true })).toBeVisible();
    await page.getByRole("button", { name: "取消申请", exact: true }).click();
    const reason =
      "  申请人参与资格已结束，停止本次工牌办理。\n保留原审批与提交内容。  ";
    await page.getByRole("textbox", { name: "取消说明" }).fill(reason);
    await picture("manager-ineligible-approved-confirmation");
    await submit("manager", approved, () =>
      page
        .getByRole("dialog")
        .getByRole("button", { name: "取消申请", exact: true })
        .click({ timeout: 10_000 }),
    );
    await expect(
      page.getByRole("heading", { name: "暂时无法确认取消结果", exact: true }),
    ).toBeVisible();
    await expect(
      page.locator(".application-preview, .review-facts, .review-reason"),
    ).toHaveCount(0);
    await picture("manager-unknown-after-real-commit");
    expect(writes).toHaveLength(2);
    await observe("refresh", "before", approved, null);
    const refreshed = page.waitForResponse(
      (response) =>
        response.request().method() === "GET" &&
        new URL(response.url()).pathname ===
          `/api/v1/workforce/applications/${approved.id}`,
      { timeout: 35_000 },
    );
    await page.getByRole("button", { name: "刷新详情", exact: true }).click();
    const managed = await trusted<ManagedApplicationDetail>(
      await wire(await refreshed),
      "GET /api/v1/workforce/applications/{id}",
      200,
    );
    expect(managed.application.status).toBe("cancelled");
    expect(managed.application.snapshot).toEqual(approved.snapshot);
    expect(managed.application.photo).toEqual(approved.photo);
    expect(JSON.stringify(managed)).not.toContain(reason.trim());
    await expect(page.getByText("已取消", { exact: true })).toBeVisible();
    await expect(
      page.getByText("取消前已审核通过。", { exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: "取消申请", exact: true }),
    ).toHaveCount(0);
    const preview = page.getByRole("img", { name: "工牌预览", exact: true });
    await expect(preview).toBeVisible({ timeout: 35_000 });
    await expect
      .poll(() => preview.evaluate((node) => {
        const canvas = node as HTMLCanvasElement;
        return canvas.width > 0 && canvas.height > 0;
      }))
      .toBe(true);
    await picture("manager-refreshed-retained-review");
    const restricted = await own(approved, 1);
    await observe("refresh", "after", approved, null, restricted, managed);
    recovering = false;
    expect(recoveryRequests.length).toBeGreaterThan(0);
    expect(recoveryRequests.every((request) => request.method === "GET")).toBe(
      true,
    );
    expect(writes).toHaveLength(2);
    expect(posts).toBe(2);
    expect(routeErrors).toEqual([]);
    await choose(owner.session);
    await page.goto("/workforce/applications");
    await expect(
      page.getByRole("link", { name: "查看详情", exact: true }),
    ).toHaveCount(1);
    await expect(page.locator(".review-facts, .review-reason")).toHaveCount(0);
    await picture("self-final-single-restricted-summary");
    expect(await cookieDigests(page)).toEqual(cookies);
    expect(posts).toBe(2);
    expect(routeErrors).toEqual([]);
    await writeFile(
      join(material, "badge-cancellations.json"),
      JSON.stringify({
        writes,
        recoveryRequests,
        observations: 6,
        restrictedCount: 1,
      }),
    );
  } finally {
    page.off("request", recoveringRequest);
    if (!page.isClosed()) {
      await page.unroute(pattern, cancellation);
      await page.emulateMedia({ colorScheme: theme });
      await choose(manager);
      if (viewport) await page.setViewportSize(viewport);
    }
  }
}
