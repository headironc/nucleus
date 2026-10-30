import { expect, type Page, type Request } from "@playwright/test";
import { readFile, rename, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { cookieDigests } from "./identity";
import { assertDocumentPolicy } from "./policy";

/** Separate lawful manager setup from each independently observed self-read window. */
export async function ownNumbers(
  page: Page,
  institution: string,
  ownerSession: string,
  selectOwner: () => Promise<void>,
) {
  const material = process.env.HALO_TEST_MATERIAL!;
  let source = 0;
  const post = async (
    actor: string | null,
    path: string,
    body: unknown,
    operation?: string,
  ) => {
    await page.setExtraHTTPHeaders({
      "X-Halo-Fixture-Source": String((++source % 16) + 1).padStart(2, "0"),
    });
    return page.evaluate(
      async ({ actor, path, body, operation }) => {
        const response = await fetch(path, {
          method: "POST",
          credentials: "same-origin",
          headers: {
            "Operation-Id": operation ?? crypto.randomUUID(),
            ...(body === undefined
              ? {}
              : { "Content-Type": "application/json" }),
            ...(actor ? { "Session-Id": actor } : {}),
          },
          ...(body === undefined ? {} : { body: JSON.stringify(body) }),
        });
        const result = await response.json();
        if (!response.ok)
          throw Error(
            `Fixture setup ${path}: ${response.status} ${result.code}`,
          );
        return result;
      },
      { actor, path, body, operation },
    );
  };
  // A third fictional person is necessary for a real two-person recovery/revocation.
  const operation = crypto.randomUUID();
  const sent = await post(
    null,
    "/api/v1/codes",
    { mobile: "13900000014", token: operation },
    operation,
  );
  const code = await readFile(
    join(material, `${sent.message.id}.code`),
    "utf8",
  );
  const login = await post(null, "/api/v1/sessions", {
    secret: "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8",
    credential: {
      type: "message",
      id: sent.message.id,
      mobile: "13900000014",
      code,
    },
  });
  expect(login.registration).toBeDefined();
  await post(null, "/api/v1/registrations", {
    id: login.registration.id,
    token: login.registration.token,
    name: "Number Beneficiary",
    idNumber: "000000200002290088",
  });

  // The institutional owner needs a real Workforce numbers role, assigned by the existing domain manager.
  const { role } = await post(ownerSession, "/api/v1/workforce/roles", {
    entry: "domain",
    name: "Number read fixture management",
    level: 200,
    permissions: ["workforce.numbers"],
  });
  const assignment = await post(
    ownerSession,
    "/api/v1/workforce/assignments/candidates",
    { entry: "domain", mobile: "13900000000" },
  );
  await post(ownerSession, "/api/v1/workforce/assignments", {
    entry: "domain",
    target: {
      accountId: assignment.identity.accountId,
      personId: assignment.identity.personId,
      revisionId: assignment.identity.revisionId,
      mobile: "13900000000",
      grantId: assignment.grantId,
      assignmentId: assignment.assignmentId,
    },
    role: { id: role.id, changeId: role.changeId },
  });
  const admit = async (mobile: string) => {
    const candidate = await post(
      institution,
      "/api/v1/workforce/grants/candidates",
      { entry: "institution", mobile },
    );
    const role = candidate.roles.find(
      (role: { kind: string }) => role.kind === "default",
    );
    expect(role).toBeDefined();
    return post(institution, "/api/v1/workforce/grants", {
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
  };
  await admit("13900000014");
  const beneficiary = await post(
    institution,
    "/api/v1/workforce/numbers/candidates",
    { mobile: "13900000014" },
  );
  const target = (
    identity: { accountId: string; personId: string; revisionId: string },
    mobile: string,
  ) => ({
    accountId: identity.accountId,
    personId: identity.personId,
    revisionId: identity.revisionId,
    mobile,
  });
  const original = (
    await post(
      institution,
      `/api/v1/workforce/persons/${beneficiary.identity.personId}/supplements`,
      {
        target: target(beneficiary.identity, "13900000014"),
        subtype: 902,
        year: 2025,
        serial: 7,
        reason: "Verified fictional original entry",
      },
    )
  ).number;
  const temporary = (
    await post(
      institution,
      `/api/v1/workforce/persons/${beneficiary.identity.personId}/corrections`,
      {
        revisionId: beneficiary.identity.revisionId,
        expected: original.id,
        replacement: { subtype: 1000, year: 2026, serial: 1 },
        reason: "Verified fictional temporary number",
      },
    )
  ).number;

  const observe = async (phase: string, position: "before" | "after") => {
    const path = join(material, `numbers-${phase}-${position}`);
    await writeFile(`${path}.pending`, "read-only");
    await rename(`${path}.pending`, `${path}.request`);
    await expect
      .poll(() => readFile(`${path}.ack`, "utf8").catch(() => ""), {
        timeout: 10_000,
      })
      .toBe("ok");
  };
  const read = async (
    phase: string,
    expected: string | null,
    navigate = false,
  ) => {
    await page.setExtraHTTPHeaders({ "X-Halo-Fixture-Source": "12" });
    const cookies = await cookieDigests(page);
    await observe(phase, "before");
    const requests: Request[] = [];
    const capture = (request: Request) => {
      if (new URL(request.url()).pathname.startsWith("/api/"))
        requests.push(request);
    };
    page.on("request", capture);
    const received = page.waitForResponse(
      (response) =>
        new URL(response.url()).pathname ===
        "/api/v1/workforce/current/numbers",
    );
    if (navigate) {
      const response = await page.goto("/workforce");
      assertDocumentPolicy(response!.headers(), false);
      const shell = await response!.text();
      expect(shell).toContain('content="noindex"');
      expect(shell).not.toContain("Second Fixture");
      if (expected) expect(shell).not.toContain(expected);
    } else
      await page.getByRole("button", { name: "刷新工号", exact: true }).click();
    const response = await received;
    expect(response.status()).toBe(200);
    expect(response.headers()["cache-control"]).toBe("no-store");
    expect(response.headers()["set-cookie"]).toBeUndefined();
    const body = await response.json();
    expect(body.code).toBe("ok");
    if (expected) {
      expect(body.number).not.toBeNull();
      await expect(page.getByText(expected, { exact: true })).toBeVisible();
      await expect(page.getByText("实习", { exact: true })).toBeVisible();
    } else {
      expect(body.number).toBeNull();
      await expect(
        page.getByText("当前没有工号。", { exact: true }),
      ).toBeVisible();
      expect(await page.locator("main").innerText()).not.toMatch(
        /从未|已被撤销/,
      );
    }
    await expect(
      page.getByText("Second Fixture", { exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("heading", { name: "员工服务已开通" }),
    ).toBeVisible();
    page.off("request", capture);
    expect(requests.length).toBeGreaterThan(0);
    expect(requests.every((request) => request.method() === "GET")).toBe(true);
    expect(await cookieDigests(page)).toEqual(cookies);
    await page.screenshot({
      path: `test-results/numbers-real-${phase}.png`,
      fullPage: true,
    });
    await observe(phase, "after");
  };
  await selectOwner();
  await read("empty", null, true);
  const owner = await post(
    institution,
    "/api/v1/workforce/numbers/candidates",
    { mobile: "13900000013" },
  );
  const allocated = (
    await post(
      institution,
      `/api/v1/workforce/persons/${owner.identity.personId}/numbers`,
      {
        target: target(owner.identity, "13900000013"),
        subtype: 902,
        year: 2025,
        reason: "Verified fictional entry",
      },
    )
  ).number;
  expect(allocated.serial).toBe(1);
  await read("allocated", "92501");
  const service = await page.evaluate(
    async (id) =>
      (
        await (
          await fetch("/api/v1/workforce/current/grant", {
            headers: { "Operation-Id": crypto.randomUUID(), "Session-Id": id },
          })
        ).json()
      ).grant,
    ownerSession,
  );
  await post(
    ownerSession,
    `/api/v1/workforce/grants/${service.id}/terminations`,
    undefined,
  );
  await admit("13900000013");
  await read("rejoined", "92501", true);
  const corrected = (
    await post(
      institution,
      `/api/v1/workforce/persons/${owner.identity.personId}/corrections`,
      {
        revisionId: owner.identity.revisionId,
        expected: allocated.id,
        replacement: { subtype: 902, year: 2025, serial: 7 },
        reason: "Verified fictional correction",
      },
    )
  ).number;
  await read("corrected", "92507");
  const recovery = await post(institution, "/api/v1/workforce/recoveries", {
    beneficiary: {
      personId: beneficiary.identity.personId,
      revisionId: beneficiary.identity.revisionId,
      current: temporary.id,
      origin: original.id,
    },
    occupier: {
      personId: owner.identity.personId,
      revisionId: owner.identity.revisionId,
      current: corrected.id,
    },
    reason: "Verified fictional original holder",
  });
  expect(recovery.recovery.revocations).toHaveLength(2);
  await read("revoked", null);
  await page.setExtraHTTPHeaders({ "X-Halo-Fixture-Source": "00" });
}
