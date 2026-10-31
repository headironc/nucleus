import { expect, type Page, type Request } from "@playwright/test";
import { readFile, rename, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { cookieDigests } from "./identity";
import { assertDocumentPolicy } from "./policy";

/** Legal manager setup is separate from every independently observed read window. */
export async function ownWorkforce(
  page: Page,
  institution: string,
  manager: string,
) {
  const material = process.env.HALO_TEST_MATERIAL!;
  const observe = async (phase: string, position: "before" | "after") => {
    const path = join(material, `workforce-${phase}-${position}`);
    await writeFile(`${path}.pending`, "read-only");
    await rename(`${path}.pending`, `${path}.request`);
    await expect
      .poll(() => readFile(`${path}.ack`, "utf8").catch(() => ""), {
        timeout: 10_000,
      })
      .toBe("ok");
  };
  const read = async (phase: string, title: string, status: string) => {
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
        new URL(response.url()).pathname === "/api/v1/workforce/current/grant",
    );
    const html = await page.goto("/workforce");
    const shell = await html!.text();
    assertDocumentPolicy(html!.headers(), false);
    expect(shell).toContain('content="noindex"');
    expect(shell).not.toContain("Fixture Operator");
    const response = await received;
    expect(response.status()).toBe(status === "absent" ? 404 : 200);
    const body = await response.json();
    expect(status === "absent" ? body.code : body.grant.status).toBe(
      status === "absent" ? "grant_absent" : status,
    );
    expect(response.headers()["cache-control"]).toBe("no-store");
    expect(response.headers()["set-cookie"]).toBeUndefined();
    await expect(
      page.getByRole("heading", { name: title, exact: true }),
    ).toBeVisible();
    await expect(
      page.getByText("Fixture Operator", { exact: true }),
    ).toBeVisible();
    await page
      .getByRole("button", { name: "刷新服务状态", exact: true })
      .click();
    await expect(
      page.getByRole("heading", { name: title, exact: true }),
    ).toBeVisible();
    await page.screenshot({
      path: `test-results/workforce-real-${phase}.png`,
      fullPage: true,
    });
    page.off("request", capture);
    expect(requests.every((request) => request.method() === "GET")).toBe(true);
    expect(await cookieDigests(page)).toEqual(cookies);
    await observe(phase, "after");
  };
  let commandSource = 0;
  const command = async (
    action: "manager" | "admit" | "suspend" | "resume" | "exit",
    reference?: string,
  ) => {
    // Separate fictional management requests use the fixture's existing source map.
    await page.setExtraHTTPHeaders({
      "X-Halo-Fixture-Source": String(++commandSource).padStart(2, "0"),
    });
    return page.evaluate(
      async ({ action, institution, manager, reference }) => {
        const post = async (actor: string, path: string, body?: unknown) => {
          const response = await fetch(path, {
            method: "POST",
            credentials: "same-origin",
            headers: {
              "Operation-Id": crypto.randomUUID(),
              "Session-Id": actor,
              ...(body ? { "Content-Type": "application/json" } : {}),
            },
            ...(body ? { body: JSON.stringify(body) } : {}),
          });
          const value = await response.json();
          if (!response.ok)
            throw Error(`${path}: ${response.status} ${value.code}`);
          return value;
        };
        if (action === "manager" || action === "admit") {
          const actor = action === "manager" ? institution : manager;
          const mobile = action === "manager" ? "13900000013" : "13900000000";
          const entry = action === "manager" ? "institution" : "domain";
          const candidate = await post(
            actor,
            "/api/v1/workforce/grants/candidates",
            { entry, mobile },
          );
          const role = candidate.roles.find(
            (role: { kind: string }) =>
              role.kind === (action === "manager" ? "full" : "default"),
          );
          if (!role) throw Error("Missing eligible system role");
          const receipt = await post(actor, "/api/v1/workforce/grants", {
            entry,
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
        }
        if (action === "exit") {
          await post(
            institution,
            `/api/v1/workforce/grants/${reference}/terminations`,
          );
          return reference!;
        }
        const candidate = await post(
          manager,
          `/api/v1/workforce/${action === "suspend" ? "suspensions" : "resumptions"}/candidates`,
          { mobile: "13900000000" },
        );
        const path =
          action === "suspend"
            ? `/api/v1/workforce/grants/${candidate.grantId}/suspensions`
            : `/api/v1/workforce/suspensions/${candidate.suspensionId}/resumptions`;
        await post(manager, path, {
          target: {
            accountId: candidate.identity.accountId,
            personId: candidate.identity.personId,
            revisionId: candidate.identity.revisionId,
            mobile: "13900000000",
          },
        });
        return candidate.grantId as string;
      },
      { action, institution, manager, reference },
    );
  };
  await read("absent", "尚未加入员工服务", "absent");
  await command("manager");
  const first = await command("admit");
  await read("active", "已加入员工服务", "active");
  await command("suspend");
  await read("suspended", "员工服务已暂停", "suspended");
  await command("resume");
  await read("resumed", "已加入员工服务", "active");
  await command("exit", first);
  await read("terminated", "员工服务已结束", "terminated");
  const next = await command("admit");
  expect(next).not.toBe(first);
  await read("rejoined", "已加入员工服务", "active");
  await page.setExtraHTTPHeaders({ "X-Halo-Fixture-Source": "00" });
  await page.goto("/account");
}
