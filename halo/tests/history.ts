import { expect, type Page, type Request } from "@playwright/test";
import { readFile, rename, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { identityReads } from "./identity";

const correctedName = "阿依古丽·阿不都热依木·买买提明";

/** Prepare fictional corrections through the existing management API, then read as their owner. */
export async function ownHistory(
  page: Page,
  manager: string,
  personId: string,
) {
  await expect(
    page.getByText("暂无实名更正记录", { exact: true }),
  ).toBeVisible();
  for (const [index, [name, idNumber]] of [
    [correctedName, "000000200002090000"],
    ["Beta Fixture", "00000020000229007X"],
    ["Second Fixture", "00000020000229007X"],
  ].entries()) {
    // Setup calls represent the manager's separate fictional requests, not the reader's quota.
    await page.setExtraHTTPHeaders({
      "X-Halo-Fixture-Source": String(14 + index),
    });
    const status = await page.evaluate(
      async ({ manager, name, idNumber }) => {
        const post = (path: string, body: unknown) =>
          fetch(path, {
            method: "POST",
            credentials: "same-origin",
            headers: {
              "Operation-Id": crypto.randomUUID(),
              "Session-Id": manager,
              "Content-Type": "application/json",
              Accept: "application/json",
            },
            body: JSON.stringify(body),
          });
        const response = await post("/api/v1/corrections/candidates", {
          mobile: "13900000013",
        });
        if (response.status !== 200) return response.status;
        const { candidate } = await response.json();
        return (
          await post("/api/v1/corrections", {
            target: candidate.target,
            name,
            idNumber,
          })
        ).status;
      },
      { manager, name, idNumber },
    );
    expect(status).toBe(201);
    if (index === 0) {
      await page.setExtraHTTPHeaders({ "X-Halo-Fixture-Source": "13" });
      await identityReads(page, personId, "corrected", name, "0000");
    }
  }
  await page.setExtraHTTPHeaders({ "X-Halo-Fixture-Source": "13" });
  const material = process.env.HALO_TEST_MATERIAL!;
  const observe = async (phase: "before" | "after") => {
    const pending = join(material, `history-${phase}.pending`);
    await writeFile(pending, personId);
    await rename(pending, join(material, `history-${phase}.request`));
    await expect
      .poll(
        async () =>
          readFile(join(material, `history-${phase}.ack`), "utf8").catch(
            () => "",
          ),
        { timeout: 10_000 },
      )
      .toBe("ok");
  };
  await observe("before");
  const cookies = await page.context().cookies();
  const requests: Request[] = [];
  const capture = (request: Request) => {
    const { pathname } = new URL(request.url());
    if (
      pathname.startsWith("/api/") &&
      !pathname.startsWith("/api/v1/avatars/")
    )
      requests.push(request);
  };
  page.on("request", capture);
  const response = page.waitForResponse(
    (r) =>
      new URL(r.url()).pathname === "/api/v1/accounts/current/corrections" &&
      r.status() === 200,
  );
  await page.reload();
  const wire = await response;
  const history = await wire.json();
  expect(history.count).toBe(3);
  expect(
    history.corrections.map((record: { before: unknown; after: unknown }) => [
      record.before,
      record.after,
    ]),
  ).toEqual([
    [
      { name: "Beta Fixture", idNumber: { suffix: "007X" } },
      { name: "Second Fixture", idNumber: { suffix: "007X" } },
    ],
    [
      { name: correctedName, idNumber: { suffix: "0000" } },
      { name: "Beta Fixture", idNumber: { suffix: "007X" } },
    ],
    [
      { name: "Second Fixture", idNumber: { suffix: "007X" } },
      { name: correctedName, idNumber: { suffix: "0000" } },
    ],
  ]);
  expect(wire.headers()["cache-control"]).toBe("no-store");
  expect(wire.headers()["set-cookie"]).toBeUndefined();
  const panel = page.getByRole("region", { name: "实名更正记录" });
  await expect(panel.getByRole("article")).toHaveCount(3);
  for (const [index, record] of history.corrections.entries()) {
    const beijing = new Date(
      Date.parse(record.correctedAt) + 8 * 60 * 60 * 1000,
    );
    const time = `${beijing.getUTCFullYear()}年${beijing.getUTCMonth() + 1}月${beijing.getUTCDate()}日 ${String(beijing.getUTCHours()).padStart(2, "0")}:${String(beijing.getUTCMinutes()).padStart(2, "0")}`;
    await expect(panel.locator("time").nth(index)).toHaveText(time);
  }
  await expect(panel.getByText("共 3 条记录", { exact: true })).toBeVisible();
  expect(await panel.innerText()).not.toMatch(
    /00000020000229|Fixture Operator/,
  );
  await page.getByLabel("每页").selectOption("2");
  await expect(panel.getByRole("article")).toHaveCount(2);
  await panel.getByRole("button", { name: "下一页" }).click();
  await expect(panel.getByText("第 2 页", { exact: true })).toBeVisible();
  await expect(panel.getByRole("article")).toHaveCount(1);
  await expect(panel.getByText(correctedName, { exact: true })).toBeVisible();
  await panel.getByRole("button", { name: "刷新记录" }).click();
  await expect(panel.getByText("第 1 页", { exact: true })).toBeVisible();
  await expect(panel.getByRole("article")).toHaveCount(2);
  await expect(panel.getByRole("button", { name: "上一页" })).toBeDisabled();
  await page.screenshot({
    path: "test-results/history-real-https.png",
    fullPage: true,
  });
  page.off("request", capture);
  expect(requests.length).toBeGreaterThanOrEqual(5);
  expect(requests.every((request) => request.method() === "GET")).toBe(true);
  const operations = requests.map(
    (request) => request.headers()["operation-id"],
  );
  expect(operations.every(Boolean)).toBe(true);
  expect(new Set(operations).size).toBe(operations.length);
  expect(await page.context().cookies()).toEqual(cookies);
  const storage = await page.evaluate(() =>
    JSON.stringify([localStorage, sessionStorage]),
  );
  expect(storage).not.toMatch(
    /阿依古丽|Beta Fixture|Second Fixture|correctedAt|corrections/,
  );
  await observe("after");
  await writeFile(
    join(material, "history.json"),
    JSON.stringify({ personId, operations, records: history.corrections }),
  );
  // Restore the default page size before the existing logout acceptance continues.
  await page.getByLabel("每页").selectOption("20");
  await expect(panel.getByRole("article")).toHaveCount(3);
}
