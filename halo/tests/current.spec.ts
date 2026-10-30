import { test, expect, type Page } from "@playwright/test";
import { randomUUID } from "node:crypto";

async function select(
  page: Page,
  id: string,
  revision: string,
  notify = false,
) {
  await page.evaluate(
    async ({ id, revision, notify }) => {
      await new Promise<void>((resolve, reject) => {
        const opening = indexedDB.open("halo.session", 1);
        opening.onupgradeneeded = () =>
          opening.result.createObjectStore("selection");
        opening.onerror = () => reject(opening.error);
        opening.onsuccess = () => {
          const database = opening.result;
          const transaction = database.transaction("selection", "readwrite");
          transaction.objectStore("selection").put(
            {
              version: 1,
              revision,
              attempt: null,
              session: { id, expiresAt: "2035-01-01T00:00:00Z" },
            },
            "current",
          );
          transaction.oncomplete = () => {
            database.close();
            resolve();
          };
          transaction.onabort = () => reject(transaction.error);
        };
      });
      if (notify) {
        const channel = new BroadcastChannel("halo.session");
        channel.postMessage(null);
        channel.close();
      }
    },
    { id, revision, notify },
  );
}

function account(id: string, name: string) {
  return {
    id,
    personId: id,
    revisionId: id,
    name,
    idNumber: { suffix: "002X" },
    avatar: {
      revision: id,
      href: `https://fixture.invalid/api/v1/avatars/${"a".repeat(64)}.webp?key_id=7&expires_at=2051222400&signature=${Buffer.alloc(32, 255).toString("base64url")}`,
      expiresAt: "2035-01-01T00:00:00Z",
      mediaType: "image/webp",
      width: 512,
      height: 512,
    },
    binding: null,
  };
}

for (const change of ["session", "revision", "notification"] as const) {
  test(`current account ${change} change respects an untrusted response generation`, async ({
    page,
  }) => {
    const first = randomUUID();
    const replacement = change === "revision" ? first : randomUUID();
    const revision = randomUUID();
    const reads: string[] = [];
    await page.route("**/api/v1/accounts/current/corrections?*", (route) =>
      route.fulfill({ json: { code: "ok", count: 0, corrections: [] } }),
    );
    await page.route("**/api/v1/accounts/current/role", (route) =>
      route.fulfill({
        json: {
          code: "ok",
          role: {
            id: first,
            changeId: first,
            kind: "default",
            name: "普通成员",
            level: 1,
            permissions: [],
          },
        },
      }),
    );
    // Hold already received response bytes so a real selection notification can
    // supersede the read even after fetch has completed and cannot be aborted.
    await page.addInitScript(() => {
      const json = Response.prototype.json;
      Response.prototype.json = async function () {
        const value = await json.call(this);
        if (
          this.url.endsWith("/api/v1/accounts/current") &&
          value.account === null
        ) {
          await new Promise<void>((resolve) => {
            Reflect.set(window, "releaseUntrusted", resolve);
            Reflect.set(window, "untrustedWaiting", true);
          });
          Reflect.set(window, "untrustedReleased", true);
        }
        return value;
      };
    });
    await page.route("**/api/v1/accounts/current", async (route) => {
      const id = route.request().headers()["session-id"];
      reads.push(id);
      await route.fulfill({
        json: {
          code: "ok",
          account:
            reads.length === 2
              ? null
              : account(
                  id,
                  reads.length === 1
                    ? "Original Fixture"
                    : "Replacement Fixture",
                ),
        },
      });
    });
    await page.goto("/");
    await select(page, first, randomUUID());
    await page.goto("/account");
    await expect(
      page.getByText("Original Fixture", { exact: true }),
    ).toBeVisible();
    await page.evaluate(() => window.dispatchEvent(new Event("focus")));
    await expect
      .poll(() => page.evaluate(() => Reflect.get(window, "untrustedWaiting")))
      .toBe(true);
    await expect(
      page.getByText("Original Fixture", { exact: true }),
    ).toHaveCount(0);
    expect(reads).toEqual([first, first]);
    await select(page, replacement, revision, change === "notification");
    if (change === "notification") {
      await expect(
        page.getByText("Replacement Fixture", { exact: true }),
      ).toBeVisible();
      expect(reads).toEqual([first, first, replacement]);
    }
    await page.evaluate(async () => {
      Reflect.get(window, "releaseUntrusted")();
      await new Promise<void>((resolve) =>
        requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
      );
    });
    expect(
      await page.evaluate(() => Reflect.get(window, "untrustedReleased")),
    ).toBe(true);
    if (change === "notification") {
      await expect(
        page.getByText("Replacement Fixture", { exact: true }),
      ).toBeVisible();
      await expect(
        page.getByRole("heading", { name: "请重新加载页面", exact: true }),
      ).toHaveCount(0);
      await page.evaluate(() => window.dispatchEvent(new Event("focus")));
      await expect.poll(() => reads.length).toBe(4);
      await expect(
        page.getByText("Replacement Fixture", { exact: true }),
      ).toBeVisible();
      expect(reads).toEqual([first, first, replacement, replacement]);
    } else {
      await expect(
        page.getByRole("heading", { name: "请重新加载页面", exact: true }),
      ).toBeVisible();
      await expect(
        page.getByText("Replacement Fixture", { exact: true }),
      ).toHaveCount(0);
      await page.evaluate(async () => {
        window.dispatchEvent(new Event("focus"));
        window.dispatchEvent(new Event("pageshow"));
        await new Promise<void>((resolve) =>
          requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
        );
      });
      expect(reads).toEqual([first, first]);
      await expect(
        page.getByRole("heading", { name: "请重新加载页面", exact: true }),
      ).toBeVisible();
      await expect(
        page.getByRole("button", { name: "重新检查", exact: true }),
      ).toHaveCount(0);
      await page
        .getByRole("button", { name: "重新加载页面", exact: true })
        .click();
      await expect(
        page.getByText("Replacement Fixture", { exact: true }),
      ).toBeVisible();
      expect(reads).toEqual([first, first, replacement]);
    }
    const saved = await page.evaluate(
      () =>
        new Promise<unknown>((resolve, reject) => {
          const opening = indexedDB.open("halo.session", 1);
          opening.onerror = () => reject(opening.error);
          opening.onsuccess = () => {
            const database = opening.result;
            const transaction = database.transaction("selection", "readonly");
            const request = transaction.objectStore("selection").get("current");
            transaction.oncomplete = () => {
              database.close();
              resolve(request.result);
            };
            transaction.onabort = () => reject(transaction.error);
          };
        }),
    );
    expect(saved).toEqual({
      version: 1,
      revision,
      attempt: null,
      session: { id: replacement, expiresAt: "2035-01-01T00:00:00Z" },
    });
  });
}
