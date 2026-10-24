import {
  expect,
  type Page,
  type Request,
  type Response,
} from "@playwright/test";
import { createHash } from "node:crypto";
import { readFile, rename, writeFile } from "node:fs/promises";
import { join } from "node:path";

export type IdentityPhase = "original" | "corrected" | "ordinary";

/** Signal one independent before/after business-fact snapshot for a phase. */
export async function identityObservation(
  personId: string,
  phase: IdentityPhase,
  position: "before" | "after",
) {
  const path = join(
    process.env.HALO_TEST_MATERIAL!,
    `identity-${phase}-${position}`,
  );
  await writeFile(`${path}.pending`, personId);
  await rename(`${path}.pending`, `${path}.request`);
  await expect
    .poll(() => readFile(`${path}.ack`, "utf8").catch(() => ""), {
      timeout: 10_000,
    })
    .toBe("ok");
}

/** Non-secret cookie evidence; values are retained only as digests. */
export async function cookieDigests(page: Page) {
  return (await page.context().cookies())
    .map(({ value, ...cookie }) => ({
      ...cookie,
      digest: createHash("sha256").update(value).digest("hex"),
    }))
    .sort((a, b) => a.name.localeCompare(b.name));
}

/** Compare independent database facts around real, cookie-backed Current reads. */
export async function identityReads(
  page: Page,
  personId: string,
  phase: IdentityPhase,
  name: string,
  suffix: string,
) {
  const cookies = () => cookieDigests(page);
  const requests: Request[] = [];
  const capture = (request: Request) => {
    if (new URL(request.url()).pathname.startsWith("/api/"))
      requests.push(request);
  };
  const responses: { status: number; headers: Record<string, string> }[] = [];
  const captureResponse = (response: Response) => {
    if (new URL(response.url()).pathname.startsWith("/api/v1/avatars/"))
      responses.push({
        status: response.status(),
        headers: response.headers(),
      });
  };
  const beforeCookies = await cookies();
  const mediaLog = process.env.HALO_TEST_MEDIA_LOG!;
  const mediaBefore = (await readFile(mediaLog, "utf8").catch(() => ""))
    .split("\n")
    .filter(Boolean).length;
  await identityObservation(personId, phase, "before");
  page.on("request", capture);
  page.on("response", captureResponse);
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("Network.clearBrowserCache");
  await cdp.detach();
  const read = page.waitForResponse(
    (response) =>
      new URL(response.url()).pathname === "/api/v1/accounts/current" &&
      response.status() === 200,
  );
  await page.reload();
  const response = await read;
  const body = await response.json();
  expect(body.account.name).toBe(name);
  expect(body.account.idNumber).toEqual({ suffix });
  // The delivered backend projects an explicit unbound account only.
  expect(body.account.binding).toBeNull();
  expect(response.headers()["cache-control"]).toBe("no-store");
  expect(response.headers()["set-cookie"]).toBeUndefined();
  const summary = page.locator(".current-summary");
  await expect(summary.locator("dt")).toHaveText(["姓名", "身份证号", "微信"]);
  await expect(summary.locator("dd")).toHaveText([
    name,
    `尾号 ${suffix}`,
    "未绑定",
  ]);
  const avatar = page.locator(".avatar-image");
  await expect(page.locator('.avatar-slot[data-state="ready"]')).toBeVisible();
  await expect(avatar).toHaveAttribute("src", body.account.avatar.href);
  await expect(page.locator(".avatar-slot svg")).toHaveCount(0);
  const picture = await avatar.evaluate((node) => {
    const image = node as HTMLImageElement;
    return {
      alt: image.alt,
      width: image.naturalWidth,
      height: image.naturalHeight,
      referrerPolicy: image.referrerPolicy,
    };
  });
  expect(picture).toEqual({
    alt: `${name}的头像`,
    width: body.account.avatar.width,
    height: body.account.avatar.height,
    referrerPolicy: "no-referrer",
  });
  // A later signing second produces a different valid URL for the same
  // revision. Reverification must still reuse the first unexpired image URL.
  await page.waitForTimeout(1_100);
  const refreshed = page.waitForResponse(
    (response) =>
      new URL(response.url()).pathname === "/api/v1/accounts/current" &&
      response.status() === 200,
  );
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  const newer = (await (await refreshed).json()).account;
  expect(newer.avatar.revision).toBe(body.account.avatar.revision);
  expect(newer.avatar.href !== body.account.avatar.href).toBe(true);
  await expect(summary.locator("dd")).toHaveText([
    name,
    `尾号 ${suffix}`,
    "未绑定",
  ]);
  await expect(page.locator('.avatar-slot[data-state="ready"]')).toBeVisible();
  expect((await avatar.getAttribute("src")) === body.account.avatar.href).toBe(
    true,
  );
  const shell = await page.context().request.get("/account");
  expect(shell.status()).toBe(200);
  expect(await shell.text()).not.toContain(name);
  expect(await shell.text()).not.toContain(`尾号 ${suffix}`);
  expect(await shell.text()).not.toContain("/api/v1/avatars/");
  expect(await shell.text()).not.toContain("signature=");
  const storage = await page.evaluate(() =>
    JSON.stringify([localStorage, sessionStorage]),
  );
  expect(storage).not.toContain(name);
  expect(storage).not.toContain('"suffix"');
  expect(storage).not.toContain("/api/v1/avatars/");
  expect(storage).not.toContain("signature=");
  expect(await page.evaluate(() => document.cookie)).not.toContain(
    "__Host-nucleus-session-",
  );
  await page.screenshot({
    path: `test-results/identity-${phase}-real-https.png`,
    fullPage: true,
  });
  page.off("request", capture);
  expect(requests.length).toBeGreaterThanOrEqual(2);
  expect(requests.every((request) => request.method() === "GET")).toBe(true);
  const images = requests.filter((request) =>
    new URL(request.url()).pathname.startsWith("/api/v1/avatars/"),
  );
  expect(images.length).toBeGreaterThanOrEqual(1);
  for (const request of images) {
    const url = new URL(request.url());
    expect(url.protocol).toBe("https:");
    expect(url.pathname).toMatch(/^\/api\/v1\/avatars\/[a-f0-9]{64}\.webp$/);
    expect([...url.searchParams.keys()].sort()).toEqual([
      "expires_at",
      "key_id",
      "signature",
    ]);
    expect(request.headers().authorization).toBeUndefined();
    expect(request.headers()["session-id"]).toBeUndefined();
    expect(request.headers()["operation-id"]).toBeUndefined();
  }
  const json = requests.filter(
    (request) =>
      !new URL(request.url()).pathname.startsWith("/api/v1/avatars/"),
  );
  expect(
    json.every((request) =>
      [
        "/api/v1/accounts/current",
        "/api/v1/accounts/current/corrections",
      ].includes(new URL(request.url()).pathname),
    ),
  ).toBe(true);
  const operations = json.map((request) => request.headers()["operation-id"]);
  expect(operations.every(Boolean)).toBe(true);
  expect(new Set(operations).size).toBe(operations.length);
  const digest = new URL(body.account.avatar.href).pathname
    .split("/")
    .at(-1)!
    .replace(/\.webp$/, "");
  const media = (await readFile(mediaLog, "utf8").catch(() => ""))
    .split("\n")
    .filter(Boolean)
    .slice(mediaBefore)
    .map((line) => JSON.parse(line) as Record<string, unknown>);
  const image = media.filter(
    (entry) =>
      entry.path === new URL(body.account.avatar.href).pathname &&
      entry.sha256 === digest,
  );
  expect(image.length).toBeGreaterThanOrEqual(1);
  for (const entry of image) {
    expect(entry).toMatchObject({
      method: "GET",
      status: 200,
      contentType: "image/webp",
      setCookie: false,
      authorization: false,
      sessionId: false,
      operationId: false,
    });
    expect(entry.cacheControl).toMatch(/^private, max-age=\d+, immutable$/);
    expect(
      Number(String(entry.cacheControl).match(/max-age=(\d+)/)?.[1]),
    ).toBeLessThanOrEqual(60);
  }
  expect(await cookies()).toEqual(beforeCookies);
  page.off("response", captureResponse);
  // The browser independently observes the media entry's actual response.
  expect(responses.length).toBeGreaterThanOrEqual(1);
  for (const response of responses) {
    expect(response.status).toBe(200);
    expect(response.headers["content-type"]).toBe("image/webp");
    expect(response.headers["x-content-type-options"]).toBe("nosniff");
    expect(response.headers["set-cookie"]).toBeUndefined();
    const match = /^private, max-age=(\d+), immutable$/.exec(
      response.headers["cache-control"] ?? "",
    );
    expect(match).not.toBeNull();
    expect(Number(match?.[1])).toBeLessThanOrEqual(60);
  }
  await identityObservation(personId, phase, "after");
}
