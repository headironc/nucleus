import {
  expect,
  test,
  type APIResponse,
  type Page,
  type Response as PageResponse,
  type Route,
} from "@playwright/test";
import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import type contracts from "../app/protocol/generated/contracts.js";
import type { components, paths } from "../app/protocol/generated/types";
import { decode, registry } from "../app/protocol/response";
import type { Account, Session } from "../app/protocol/session";
import type {
  Application,
  ApplicationDetail,
  ApplicationInput,
  ApplicationList,
  ApplicationPhoto,
  ApplicationPreview,
  PreviewInput,
} from "../app/protocol/applications";
import { cookieDigests } from "./identity";
import { assertDocumentPolicy } from "./policy";

type Phase =
  | PreviewInput["kind"]
  | "lost"
  | "refresh"
  | "foreign"
  | "capacity"
  | "restricted";
type Position = "before" | "after";
type Actor = { account: Account; session: Session };
type Wire = {
  status: number;
  media: string | null;
  text: string;
  cacheControl: string | null;
  setCookie: boolean;
  retryAfter: string | null;
};
type Created = components["schemas"]["application.Created"];
type CurrentGrant =
  paths["/api/v1/workforce/current/grant"]["get"]["responses"][200]["content"]["application/json"];

export type BadgeObservation = {
  // These identify the caller; applicationIds retain the target owner's records.
  accountId: string;
  personId: string;
  session: string;
  operation: string | null;
  input: ApplicationInput | null;
  application: Application | null;
  applicationIds: string[];
  resourceId: string | null;
};

export async function wire(response: APIResponse | PageResponse): Promise<Wire> {
  const headers = response.headers();
  return {
    status: response.status(),
    media: headers["content-type"] ?? null,
    text: await response.text(),
    cacheControl: headers["cache-control"] ?? null,
    setCookie: Object.hasOwn(headers, "set-cookie"),
    retryAfter: headers["retry-after"] ?? null,
  };
}

/** Observer facts come from the same generated contract used by the real page. */
export async function trusted<T>(
  value: Wire,
  endpoint: keyof typeof contracts,
  status: number,
): Promise<T> {
  const body = await decode(
    new Response(value.text, {
      status: value.status,
      headers: { "Content-Type": value.media ?? "" },
    }),
    endpoint,
  );
  const code =
    body && typeof body === "object" && "code" in body
      ? String(body.code)
      : "untrusted";
  const retryAfter =
    value.retryAfter === null
      ? "absent"
      : /^\d{1,20}$/.test(value.retryAfter)
        ? value.retryAfter
        : "present_non_numeric";
  const diagnostic = `${endpoint}: HTTP ${value.status}, code=${code}, Retry-After=${retryAfter}`;
  expect(value.status, diagnostic).toBe(status);
  expect(body !== undefined, diagnostic).toBe(true);
  expect(value.cacheControl, diagnostic).toBe("no-store");
  expect(value.setCookie, diagnostic).toBe(false);
  return body as T;
}

async function picture(page: Page, name: string, dialog = false) {
  const path = process.env.HALO_TEST_EVIDENCE_DIR
    ? join(
        process.env.HALO_TEST_EVIDENCE_DIR,
        "badge-applications",
        `${name}.png`,
      )
    : test.info().outputPath("badge-applications", `${name}.png`);
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

/** Requires the real onboarding result and a separate active directory-only member. */
export async function ownBadgeApplications(
  page: Page,
  manager: Session,
  owner: Actor,
  other: Actor,
  select: (session: Session) => Promise<void>,
) {
  const material = process.env.HALO_TEST_MATERIAL;
  if (!material)
    throw new Error(
      "Real badge checks require the supervised material directory",
    );
  const viewport = page.viewportSize();
  // The shared source bucket refills at 2/s with capacity 4: fill for 2s first.
  let nextSend = performance.now() + 2_000;
  let previousSend = Promise.resolve();
  const forward = async (route: Route) => {
    const ready = previousSend.then(async () => {
      while (performance.now() < nextSend)
        await delay(Math.ceil(nextSend - performance.now()));
      nextSend = performance.now() + 500;
    });
    previousSend = ready;
    await ready;
    return route.fetch({
      maxRetries: 0,
      maxRedirects: 0,
      timeout: 35_000,
    });
  };
  const records: Application[] = [];
  let preparedPhoto: ApplicationPhoto | undefined;
  const calls: { operation: string; input: ApplicationInput }[] = [];
  let photos = 0,
    previews = 0,
    resources = 0,
    capacity = false,
    capacityHandled = false;
  let attempt:
    | {
        phase: Phase;
        preview: ApplicationPreview;
        input: PreviewInput;
        lost: boolean;
        handled: boolean;
      }
    | undefined;

  const observe = async (
    phase: Phase,
    position: Position,
    value: Partial<
      Pick<
        BadgeObservation,
        | "accountId"
        | "personId"
        | "session"
        | "operation"
        | "input"
        | "application"
        | "resourceId"
      >
    > = {},
  ) => {
    const observation: BadgeObservation = {
      accountId: owner.account.id,
      personId: owner.account.personId,
      session: owner.session.id,
      operation: null,
      input: null,
      application: null,
      applicationIds: records.map((record) => record.id),
      resourceId: null,
      ...value,
    };
    const path = join(material, `badge-${phase}-${position}`);
    await writeFile(`${path}.pending`, JSON.stringify(observation));
    await rename(`${path}.pending`, `${path}.request`);
    await expect
      .poll(() => readFile(`${path}.ack`, "utf8").catch(() => ""), {
        timeout: 10_000,
        message: `Independent badge ${phase} ${position} observation`,
      })
      .toBe("ok");
  };

  const raw = (session: string, path: string, method: "GET" | "POST" = "GET") =>
    page.evaluate(
      async ({ session, path, method }) => {
        const headers: Record<string, string> = { "Session-Id": session };
        if (!path.startsWith("/api/v1/workforce/application-resources/"))
          headers["Operation-Id"] = crypto.randomUUID();
        const response = await fetch(path, {
          method,
          headers,
          credentials: "same-origin",
          redirect: "error",
          cache: "no-store",
        });
        return {
          status: response.status,
          media: response.headers.get("content-type"),
          text: await response.text(),
          cacheControl: response.headers.get("cache-control"),
          setCookie: response.headers.has("set-cookie"),
          retryAfter: response.headers.get("retry-after"),
        };
      },
      { session, path, method },
    );

  const submit = async (route: Route) => {
    if (route.request().method() !== "POST") return route.continue();
    const current = attempt;
    if (!current || current.handled) {
      await route.abort();
      throw new Error("Unexpected application submission or retry");
    }
    current.handled = true;
    const request = route.request();
    expect(request.headers()["session-id"]).toBe(owner.session.id);
    const input = request.postDataJSON();
    expect(registry.workforceApplicationSubmit_input(input)).toBe(true);
    expect(input.kind).toBe(current.input.kind);
    expect(input.selections).toEqual(current.input.selections);
    expect(input.expected).toEqual(current.preview.snapshot);
    const operation = request.headers()["operation-id"];
    expect(typeof operation).toBe("string");
    calls.push({ operation, input });
    await observe(current.phase, "before", { operation, input });
    const response = await forward(route);
    const receipt = await trusted<Created>(
      await wire(response),
      "POST /api/v1/workforce/applications",
      201,
    );
    expect(receipt.application.kind).toBe(input.kind);
    expect(receipt.application.snapshot).toEqual(current.preview.snapshot);
    expect(receipt.application.photo).toEqual(current.preview.photo);
    records.push(receipt.application);
    await observe(current.phase, "after", {
      operation,
      input,
      application: receipt.application,
    });
    attempt = undefined;
    if (current.lost) await route.abort();
    else await route.fulfill({ response });
  };

  const photo = async (route: Route) => {
    photos++;
    if (!capacity) {
      const response = await forward(route);
      preparedPhoto = await trusted<ApplicationPhoto>(
        await wire(response),
        "POST /api/v1/workforce/application-photos",
        201,
      );
      await route.fulfill({ response });
      return;
    }
    if (capacityHandled) {
      await route.abort();
      throw new Error("Unexpected automatic photograph retry");
    }
    capacityHandled = true;
    const operation = route.request().headers()["operation-id"];
    await observe("capacity", "before", { operation });
    const response = await forward(route);
    const failure = await trusted<{ code: string }>(
      await wire(response),
      "POST /api/v1/workforce/application-photos",
      503,
    );
    expect(failure.code).toBe("dependency_unavailable");
    await observe("capacity", "after", { operation });
    await route.fulfill({ response });
  };

  const previewRequest = async (route: Route) => {
    expect(route.request().method()).toBe("POST");
    await route.fulfill({ response: await forward(route) });
  };

  const count = (request: import("@playwright/test").Request) => {
    const path = new URL(request.url()).pathname;
    if (path === "/api/v1/workforce/applications/preview") previews++;
    if (path.startsWith("/api/v1/workforce/application-resources/")) {
      resources++;
      expect(request.headers()["operation-id"]).toBeUndefined();
    }
  };

  const prepare = async (kind: PreviewInput["kind"]) => {
    const html = await page.goto("/workforce/applications/new");
    assertDocumentPolicy(html!.headers(), false);
    await expect(page.getByLabel("实际科室", { exact: true })).toBeVisible();
    const department = page.getByLabel("实际科室", { exact: true });
    const departmentId = await department
      .locator("option")
      .evaluateAll(
        (options) =>
          (options as HTMLOptionElement[]).find(
            (option) => option.value && !option.disabled,
          )?.value,
      );
    expect(departmentId).toBeTruthy();
    await department.selectOption(departmentId!);
    await page
      .getByRole("checkbox", { name: "我确认本次工牌使用上述科室" })
      .check();
    for (const label of ["职务（选填）", "职称（选填）"]) {
      const field = page.getByLabel(label, { exact: true });
      if (await field.count()) await field.selectOption("");
    }
    await page
      .getByLabel("领取地点", { exact: true })
      .selectOption({ index: 1 });
    const label = {
      issuance: "新办",
      duplication: "增办",
      replacement: "换证",
    }[kind];
    await page.getByRole("radio", { name: label, exact: true }).check();
    const png = await page.evaluate((index) => {
      const canvas = document.createElement("canvas");
      canvas.width = 640;
      canvas.height = 960;
      const context = canvas.getContext("2d")!;
      context.fillStyle = ["#c8d2c6", "#d3c9b9", "#c4cedd", "#d0c5d9"][
        index % 4
      ];
      context.fillRect(0, 0, 640, 960);
      context.clearRect(0, 0, 80, 960);
      context.fillStyle = "#ddbc9a";
      context.beginPath();
      context.arc(340, 280, 140, 0, Math.PI * 2);
      context.fill();
      context.fillStyle = ["#365c4e", "#3d5a78", "#76575d", "#6c6c41"][
        index % 4
      ];
      context.beginPath();
      context.ellipse(340, 800, 230, 330, 0, 0, Math.PI * 2);
      context.fill();
      return canvas.toDataURL("image/png").split(",")[1];
    }, photos);
    await page.locator('input[type="file"]').setInputFiles({
      name: `fictional-${kind}.png`,
      mimeType: "image/png",
      buffer: Buffer.from(png, "base64"),
    });
    await page
      .getByRole("button", { name: "确认裁剪效果", exact: true })
      .click();
    await expect(
      page.getByRole("button", { name: "已确认裁剪效果" }),
    ).toBeDisabled();
    return {
      departmentId,
      pickup: await page.getByLabel("领取地点", { exact: true }).inputValue(),
    };
  };

  await select(owner.session);
  await page.setExtraHTTPHeaders({ "X-Halo-Fixture-Source": "17" });
  await page.setViewportSize({ width: 390, height: 844 });
  const cookies = await cookieDigests(page);
  await page.route("**/api/v1/workforce/applications", submit);
  await page.route("**/api/v1/workforce/application-photos", photo);
  await page.route("**/api/v1/workforce/applications/preview", previewRequest);
  page.on("request", count);
  try {
    const current = await trusted<{ account: Account }>(
      await raw(owner.session.id, "/api/v1/accounts/current"),
      "GET /api/v1/accounts/current",
      200,
    );
    expect(current.account.id).toBe(owner.account.id);
    expect(current.account.personId).toBe(owner.account.personId);

    for (const phase of [
      "issuance",
      "duplication",
      "replacement",
      "lost",
    ] as const) {
      const kind = phase === "lost" ? "issuance" : phase;
      preparedPhoto = undefined;
      const selected = await prepare(kind);
      const received = responseFor(
        page,
        "/api/v1/workforce/applications/preview",
        owner.session.id,
      );
      await page.getByRole("button", { name: "预览工牌", exact: true }).click();
      const response = await received;
      const preview = await trusted<ApplicationPreview>(
        await wire(response),
        "POST /api/v1/workforce/applications/preview",
        200,
      );
      const input = response.request().postDataJSON();
      expect(registry.workforceApplicationPreview_input(input)).toBe(true);
      expect(input.kind).toBe(kind);
      expect(input.selections.departmentId).toBe(selected.departmentId);
      expect(input.selections.pickup).toBe(selected.pickup);
      expect(input.selections.rosterId).toBeNull();
      expect(input.selections.titleId).toBeNull();
      expect(preview.kind).toBe(kind);
      expect(preview.snapshot.name).toBe(current.account.name);
      expect(preview.snapshot.photoId).toBe(input.selections.photoId);
      expect(preview.photo.photoId).toBe(input.selections.photoId);
      expect(preparedPhoto).toBeDefined();
      expect({ code: "ok", ...preview.photo }).toEqual(preparedPhoto);
      const dialog = page.getByRole("dialog");
      await expect(dialog).toBeVisible();
      const details = dialog.locator(".application-snapshot");
      for (const text of [
        preview.snapshot.name,
        preview.snapshot.printedNumber,
        preview.snapshot.department.printed,
        preview.snapshot.printedTitle,
        preview.snapshot.pickup,
      ])
        await expect(details.getByText(text, { exact: true })).toBeVisible();
      await expect(
        dialog.getByRole("img", { name: "工牌预览", exact: true }),
      ).toBeVisible();
      await expect
        .poll(() =>
          dialog
            .getByRole("img", { name: "工牌预览", exact: true })
            .evaluate((node, layout) => {
              const canvas = node as HTMLCanvasElement;
              const x = Math.floor(
                ((layout.photo.xUm + layout.photo.widthUm / 2) * canvas.width) /
                  layout.widthUm,
              );
              const y = Math.floor(
                ((layout.photo.yUm + layout.photo.heightUm / 2) *
                  canvas.height) /
                  layout.heightUm,
              );
              return canvas.getContext("2d")!.getImageData(x, y, 1, 1).data[3];
            }, preview.snapshot.template.layout),
        )
        .toBe(255);
      await expect(
        dialog.getByRole("button", { name: "确认并提交申请", exact: true }),
      ).toBeEnabled();
      await picture(page, `${phase}-confirm`, true);
      attempt = {
        phase,
        preview,
        input,
        lost: phase === "lost",
        handled: false,
      };
      await dialog
        .getByRole("button", { name: "确认并提交申请", exact: true })
        .click();
      await expect(
        page.getByRole("heading", {
          name: phase === "lost" ? "未能确认，请刷新查看" : "工牌申请已提交",
          exact: true,
        }),
      ).toBeVisible();
      await picture(page, `${phase}-result`);
    }
    expect(calls).toHaveLength(4);
    expect(records.map((record) => record.kind)).toEqual([
      "issuance",
      "duplication",
      "replacement",
      "issuance",
    ]);
    await expect(page.getByRole("button", { name: /提交/ })).toHaveCount(0);

    await observe("refresh", "before");
    const listResponse = responseFor(
      page,
      "/api/v1/workforce/current/applications",
      owner.session.id,
    );
    await page.getByRole("link", { name: "刷新申请列表", exact: true }).click();
    const list = await trusted<ApplicationList>(
      await wire(await listResponse),
      "GET /api/v1/workforce/current/applications",
      200,
    );
    expect(list.access).toBe("full");
    expect(list.count).toBe(4);
    expect(new Set(list.applications.map((record) => record.id))).toEqual(
      new Set(records.map((record) => record.id)),
    );
    await expect(
      page.getByRole("link", { name: "查看详情", exact: true }),
    ).toHaveCount(4);
    await picture(page, "refreshed-history");
    for (const [index, expected] of records.entries()) {
      const response = responseFor(
        page,
        `/api/v1/workforce/current/applications/${expected.id}`,
        owner.session.id,
      );
      await page.goto(`/workforce/applications/${expected.id}`);
      const detail = await trusted<ApplicationDetail>(
        await wire(await response),
        "GET /api/v1/workforce/current/applications/{id}",
        200,
      );
      expect(detail.access).toBe("full");
      expect(detail.application).toEqual(expected);
      await expect(
        page.getByRole("img", { name: "工牌预览", exact: true }),
      ).toBeVisible();
      await picture(page, `history-${index + 1}`);
    }
    expect(calls).toHaveLength(4);
    expect(photos).toBe(4);
    expect(previews).toBe(4);
    await observe("refresh", "after");

    await select(other.session);
    await page.setExtraHTTPHeaders({ "X-Halo-Fixture-Source": "18" });
    const otherCurrent = await trusted<{ account: Account }>(
      await raw(other.session.id, "/api/v1/accounts/current"),
      "GET /api/v1/accounts/current",
      200,
    );
    expect(otherCurrent.account.id).toBe(other.account.id);
    expect(otherCurrent.account.personId).toBe(other.account.personId);
    expect(other.account.id).not.toBe(owner.account.id);
    expect(other.account.personId).not.toBe(owner.account.personId);
    const otherGrant = await trusted<CurrentGrant>(
      await raw(other.session.id, "/api/v1/workforce/current/grant"),
      "GET /api/v1/workforce/current/grant",
      200,
    );
    expect(otherGrant.grant.status).toBe("active");
    const resourceId = records[0].photo.resourceId;
    await observe("foreign", "before", {
      accountId: other.account.id,
      personId: other.account.personId,
      session: other.session.id,
      resourceId,
    });
    for (const [path, endpoint] of [
      [
        `/api/v1/workforce/application-resources/${resourceId}`,
        "GET /api/v1/workforce/application-resources/{id}",
      ],
      [
        `/api/v1/workforce/current/applications/${records[0].id}`,
        "GET /api/v1/workforce/current/applications/{id}",
      ],
    ] as const) {
      const failure = await trusted<{ code: string }>(
        await raw(other.session.id, path),
        endpoint,
        404,
      );
      expect(failure.code).toBe("target_unavailable");
    }
    const otherList = responseFor(
      page,
      "/api/v1/workforce/current/applications",
      other.session.id,
    );
    await page.goto("/workforce/applications");
    const empty = await trusted<ApplicationList>(
      await wire(await otherList),
      "GET /api/v1/workforce/current/applications",
      200,
    );
    expect(empty.access).toBe("full");
    expect(empty.count).toBe(0);
    await expect(page.getByText("暂无申请", { exact: true })).toBeVisible();
    await picture(page, "foreign-own-history");
    await observe("foreign", "after", {
      accountId: other.account.id,
      personId: other.account.personId,
      session: other.session.id,
      resourceId,
    });

    await select(owner.session);
    await page.setExtraHTTPHeaders({ "X-Halo-Fixture-Source": "17" });
    capacity = true;
    await prepare("issuance");
    await page.getByRole("button", { name: "预览工牌", exact: true }).click();
    await expect(page.getByRole("alert")).toContainText(
      "暂时无法加载，请刷新后重试。",
    );
    await expect(page.getByRole("dialog")).toHaveCount(0);
    await expect(
      page.getByRole("heading", { name: "未能确认，请刷新查看", exact: true }),
    ).toHaveCount(0);
    await expect(
      page.getByRole("button", { name: "预览工牌", exact: true }),
    ).toBeEnabled({ timeout: 10_000 });
    expect(photos).toBe(5);
    expect(previews).toBe(4);
    expect(calls).toHaveLength(4);
    await picture(page, "capacity-refusal");

    // Complete the lawful eligibility change before the independent read window.
    const grant = await trusted<CurrentGrant>(
      await raw(owner.session.id, "/api/v1/workforce/current/grant"),
      "GET /api/v1/workforce/current/grant",
      200,
    );
    await trusted(
      await raw(
        owner.session.id,
        `/api/v1/workforce/grants/${grant.grant.id}/terminations`,
        "POST",
      ),
      "POST /api/v1/workforce/grants/{id}/terminations",
      201,
    );
    await observe("restricted", "before", { resourceId });
    const resourceCount = resources;
    const restrictedList = responseFor(
      page,
      "/api/v1/workforce/current/applications",
      owner.session.id,
    );
    await page.goto("/workforce/applications");
    const restricted = await trusted<ApplicationList>(
      await wire(await restrictedList),
      "GET /api/v1/workforce/current/applications",
      200,
    );
    expect(restricted.access).toBe("restricted");
    expect(restricted.count).toBe(4);
    for (const item of restricted.applications)
      expect(Object.keys(item).sort()).toEqual([
        "id",
        "kind",
        "status",
        "submittedAt",
      ]);
    await expect(
      page.getByRole("link", { name: "查看详情", exact: true }),
    ).toHaveCount(4);
    await expect(
      page.getByRole("img", { name: "工牌预览", exact: true }),
    ).toHaveCount(0);
    await picture(page, "restricted-history");
    const restrictedDetail = responseFor(
      page,
      `/api/v1/workforce/current/applications/${records[0].id}`,
      owner.session.id,
    );
    await page.goto(`/workforce/applications/${records[0].id}`);
    const summary = await trusted<ApplicationDetail>(
      await wire(await restrictedDetail),
      "GET /api/v1/workforce/current/applications/{id}",
      200,
    );
    expect(summary.access).toBe("restricted");
    expect(Object.keys(summary.application).sort()).toEqual([
      "id",
      "kind",
      "status",
      "submittedAt",
    ]);
    await expect(page.locator(".application-snapshot")).toHaveCount(0);
    await expect(
      page.getByRole("img", { name: "工牌预览", exact: true }),
    ).toHaveCount(0);
    expect(resources).toBe(resourceCount);
    const denied = await trusted<{ code: string }>(
      await raw(
        owner.session.id,
        `/api/v1/workforce/application-resources/${resourceId}`,
      ),
      "GET /api/v1/workforce/application-resources/{id}",
      404,
    );
    expect(denied.code).toBe("target_unavailable");
    await picture(page, "restricted-detail");
    await observe("restricted", "after", { resourceId });
    expect(calls).toHaveLength(4);
    expect(photos).toBe(5);
    expect(previews).toBe(4);
    expect(await cookieDigests(page)).toEqual(cookies);
    await writeFile(
      join(material, "badge-applications.json"),
      JSON.stringify({
        calls,
        applications: records,
        photoRequests: photos,
        previewRequests: previews,
      }),
    );
    return { applications: records, grantId: grant.grant.id };
  } finally {
    await page.unroute("**/api/v1/workforce/applications", submit);
    await page.unroute("**/api/v1/workforce/application-photos", photo);
    await page.unroute(
      "**/api/v1/workforce/applications/preview",
      previewRequest,
    );
    page.off("request", count);
    await select(manager);
    await page.setExtraHTTPHeaders({ "X-Halo-Fixture-Source": "00" });
    if (viewport) await page.setViewportSize(viewport);
  }
}
