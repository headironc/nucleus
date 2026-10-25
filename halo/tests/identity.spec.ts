import { test, expect, type Page, type Browser } from "@playwright/test";
import { build } from "vite";
import tailwindcss from "@tailwindcss/vite";
import { resolve } from "node:path";
import { randomUUID } from "node:crypto";
import { readFile, writeFile } from "node:fs/promises";
import type { Account } from "../app/protocol/session";
import { createServer, type ServerResponse } from "node:http";
import { once } from "node:events";

// This fixture exercises the actual account page, Current controller, protocol
// decoder and IndexedDB/BroadcastChannel selection. Controlled HTTP responses
// do not establish the backend's cookie, transaction or database effects.
const origin = "http://account.localhost";
const first = "019354fd-933c-7e6a-8912-345678901234";
const second = "019354fd-933c-7e6a-8912-345678901235";
const accountId = "00000000-0000-4000-8000-000000000001";
const expiresAt = "2035-01-01T00:00:00Z";
const resources = new Map<
  string,
  { body: string | Buffer; contentType: string }
>();
let html: string;
let avatarBytes: Buffer;

type AccountPayload = Account;

function account(
  name = "示例用户",
  suffix = "012X",
  revisionId = randomUUID(),
  binding: AccountPayload["binding"] = null,
): AccountPayload {
  return {
    id: accountId,
    personId: accountId,
    revisionId,
    name,
    idNumber: { suffix },
    avatar: {
      revision: accountId,
      href: `https://fixture.invalid/api/v1/avatars/${"a".repeat(64)}.webp?key_id=7&expires_at=2051222400&signature=${Buffer.alloc(32, 255).toString("base64url")}`,
      expiresAt,
      mediaType: "image/webp",
      width: 512,
      height: 512,
    },
    binding,
  };
}

function gate() {
  let release!: () => void;
  let mark!: () => void;
  const opened = new Promise<void>((done) => {
    release = done;
  });
  const entered = new Promise<void>((done) => {
    mark = done;
  });
  return {
    opened,
    entered,
    release: () => release(),
    mark: () => mark(),
  };
}

test.beforeAll(async ({ browser }) => {
  console.info(`Account browser: ${browser.version()}`);
  avatarBytes = await readFile(resolve("../assets/avatars/0-cat.webp"));
  const result = await build({
    configFile: false,
    publicDir: false,
    plugins: [tailwindcss()],
    logLevel: "error",
    define: { __BUILD_ID__: JSON.stringify("account-fixture") },
    build: {
      write: false,
      emptyOutDir: false,
      cssCodeSplit: false,
      assetsInlineLimit: 0,
      rolldownOptions: {
        input: resolve("tests/fixtures/identity.tsx"),
        output: { entryFileNames: "fixture.js" },
      },
    },
  });
  if (Array.isArray(result) || !("output" in result))
    throw new Error("Expected one fixture build");
  const styles: string[] = [];
  for (const item of result.output) {
    const script = item.type === "chunk";
    const contentType =
      script || item.fileName.endsWith(".js")
        ? "text/javascript"
        : item.fileName.endsWith(".css")
          ? "text/css"
          : item.fileName.endsWith(".svg")
            ? "image/svg+xml"
            : "image/png";
    resources.set("/" + item.fileName, {
      body: script ? item.code : Buffer.from(item.source),
      contentType,
    });
    if (contentType === "text/css")
      styles.push(`<link rel="stylesheet" href="/${item.fileName}">`);
  }
  html = `<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><link rel="icon" href="data:,">${styles.join("")}</head><body><div id="fixture"></div><script type="module" src="/fixture.js"></script></body></html>`;
});

async function choose(page: Page, id: string) {
  await page.evaluate(
    async ({ id, expiresAt }) => {
      const attempt = await window.accountSessions.begin(crypto.randomUUID());
      const accepted = await window.accountSessions.accept(attempt, {
        id,
        expiresAt,
      });
      if (!accepted) throw new Error("Fixture selection was superseded");
    },
    { id, expiresAt },
  );
}

/** Store a selection without a notification, as another source could. */
async function store(page: Page, id: string, revision: string) {
  await page.evaluate(
    async ({ id, revision, expiresAt }) => {
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
              session: { id, expiresAt },
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
    },
    { id, revision, expiresAt },
  );
}

async function expectSummary(
  page: Page,
  name: string,
  suffix: string,
  binding = "未绑定",
) {
  const facts = page.locator(".current-summary");
  await expect(facts.locator("dt")).toHaveText(["姓名", "身份证号", "微信"]);
  await expect(facts.locator("dd")).toHaveText([
    name,
    `尾号 ${suffix}`,
    binding,
  ]);
}

async function expectAvatarReady(page: Page, href?: string) {
  const image = page.locator(".avatar-image");
  await expect(page.locator('.avatar-slot[data-state="ready"]')).toBeVisible();
  await expect(image).toBeVisible();
  await expect
    .poll(() => image.evaluate((node: HTMLImageElement) => node.naturalWidth))
    .toBe(512);
  await expect
    .poll(() => image.evaluate((node: HTMLImageElement) => node.naturalHeight))
    .toBe(512);
  if (href) await expect(image).toHaveAttribute("src", href);
}

async function expectNoSummary(page: Page, ...values: string[]) {
  await expect(page.locator(".current-summary")).toHaveCount(0);
  const text = await page.evaluate(() => document.body.textContent ?? "");
  for (const value of [...values, "已绑定", "未绑定"])
    expect(text).not.toContain(value);
}

async function selection(page: Page) {
  return page.evaluate(() => window.accountSessions.read());
}

test.beforeEach(async ({ context, page }) => {
  await context.route("**/*", async (route) => {
    const url = new URL(route.request().url());
    if (
      url.origin === "https://fixture.invalid" &&
      url.pathname.startsWith("/api/v1/avatars/")
    ) {
      await route.fulfill({
        contentType: "image/webp",
        headers: { "Cache-Control": "private, max-age=60, immutable" },
        body: avatarBytes,
      });
      return;
    }
    if (url.origin !== origin) {
      await route.abort();
      return;
    }
    if (url.pathname === "/build.json") {
      await route.fulfill({ json: { buildId: "account-fixture" } });
      return;
    }
    if (url.pathname === "/api/v1/accounts/current/corrections") {
      await route.fulfill({ json: { code: "ok", count: 0, corrections: [] } });
      return;
    }
    if (url.pathname.startsWith("/api/")) {
      await route.abort();
      return;
    }
    const resource = resources.get(url.pathname);
    if (resource) await route.fulfill(resource);
    else if (["/", "/account"].includes(url.pathname))
      await route.fulfill({ contentType: "text/html", body: html });
    else await route.abort();
  });
  await page.goto(origin);
  await page.waitForFunction(() => !!window.accountSessions);
  await choose(page, first);
});

test("an initially hidden account waits for visibility before reading private data", async ({
  page,
}) => {
  await page.addInitScript(() => {
    Object.defineProperty(document, "hidden", {
      configurable: true,
      get: () => true,
    });
  });
  const reads: string[] = [];
  await page.route("**/api/v1/accounts/current", async (route) => {
    reads.push(route.request().url());
    await route.fulfill({ json: { code: "ok", account: account() } });
  });
  await page.goto(`${origin}/account`);
  // Allow the real effect, build check and two paint opportunities to run.
  await page.waitForFunction(() => !!window.accountSessions);
  await page.evaluate(
    () =>
      new Promise<void>((resolve) =>
        requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
      ),
  );
  await expectNoSummary(page, "示例用户", "012X");
  expect(reads).toEqual([]);
  await page.evaluate(() => {
    Reflect.deleteProperty(document, "hidden");
    document.dispatchEvent(new Event("visibilitychange"));
  });
  await expectSummary(page, "示例用户", "012X");
  expect(reads).toHaveLength(1);
});

test("renders the trusted 微信 binding fact for explicit unbound and bound accounts", async ({
  page,
}) => {
  const revision = randomUUID();
  const states = [
    { key: "unbound", label: "未绑定", binding: null },
    { key: "bound", label: "已绑定", binding: { revision } },
  ] as const;
  let binding: AccountPayload["binding"] = null;
  let reads = 0;
  await page.route("**/api/v1/accounts/current", (route) => {
    reads++;
    return route.fulfill({
      json: {
        code: "ok",
        account: account("绑定姓名", "007X", randomUUID(), binding),
      },
    });
  });
  await page.goto(`${origin}/account`);
  await expectSummary(page, "绑定姓名", "007X");
  for (const state of states) {
    binding = state.binding;
    const before = reads;
    await page.evaluate(() => window.dispatchEvent(new Event("focus")));
    await expect.poll(() => reads).toBe(before + 1);
    await expectSummary(page, "绑定姓名", "007X", state.label);
    const summary = page.locator(".current-summary");
    const term = summary.locator("dt").filter({ hasText: "微信" });
    await expect(term).toHaveCount(1);
    await expect(term).toHaveRole("term");
    const definition = summary.locator("dd").filter({ hasText: state.label });
    await expect(definition).toHaveCount(1);
    await expect(definition).toHaveRole("definition");
    const snapshot = await summary.ariaSnapshot();
    expect(snapshot).toContain("微信");
    expect(snapshot).toContain(state.label);
    await writeFile(
      `test-results/account-binding-${state.key}.aria.txt`,
      snapshot,
    );
    const text = await page.evaluate(() => document.body.textContent ?? "");
    expect(text).not.toContain(revision);
    expect(text).not.toMatch(/union|openid|open_id/i);
    await expect(page.getByRole("button", { name: /绑定/ })).toHaveCount(0);
    await expect(page.getByRole("link", { name: /绑定/ })).toHaveCount(0);
  }
});

test("renders the complete name, suffix and binding together, hides them while reading, and stores none", async ({
  page,
}) => {
  const longName = "阿依古丽·阿不都热依木·买买提明";
  const boundRevision = randomUUID();
  const firstRead = gate();
  const secondRead = gate();
  let reads = 0;
  await page.route("**/api/v1/accounts/current", async (route) => {
    reads++;
    if (reads === 1) {
      firstRead.mark();
      await firstRead.opened;
      await route.fulfill({
        json: { code: "ok", account: account(longName, "012X") },
      });
      return;
    }
    secondRead.mark();
    await secondRead.opened;
    await route.fulfill({
      json: {
        code: "ok",
        account: account("王小明", "0000", randomUUID(), {
          revision: boundRevision,
        }),
      },
    });
  });
  await page.goto(`${origin}/account`);
  await firstRead.entered;
  await expectNoSummary(page, longName, "012X", boundRevision);
  firstRead.release();
  await expectSummary(page, longName, "012X", "未绑定");
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await secondRead.entered;
  await expectNoSummary(page, longName, "012X", "已绑定", boundRevision);
  secondRead.release();
  await expectSummary(page, "王小明", "0000", "已绑定");
  const text = await page.evaluate(() => document.body.textContent ?? "");
  expect(text).not.toContain(longName);
  expect(text).not.toContain("012X");
  expect(text).not.toContain(boundRevision);
  const persisted = await page.evaluate(async () => ({
    selection: await window.accountSessions.read(),
    session: Object.fromEntries(Object.entries(sessionStorage)),
    local: Object.fromEntries(Object.entries(localStorage)),
  }));
  expect(JSON.stringify(persisted)).not.toContain(longName);
  expect(JSON.stringify(persisted)).not.toContain("012X");
  expect(JSON.stringify(persisted)).not.toContain(boundRevision);
  expect(persisted.selection?.session?.id).toBe(first);
  expect(reads).toBe(2);
});

type Malformed = {
  label: string;
  body: (base: AccountPayload) => unknown;
  contentType?: string;
};
const malformed: Malformed[] = [
  ...["00aX", "012x", "１２３X", "01234"].map((suffix) => ({
    label: `the invalid suffix ${suffix}`,
    body: (base: AccountPayload) => ({
      code: "ok",
      account: { ...base, idNumber: { suffix } },
    }),
  })),
  {
    label: "a suffix shorter than four characters",
    body: (base) => ({
      code: "ok",
      account: { ...base, idNumber: { suffix: "002" } },
    }),
  },
  {
    label: "a full identity number",
    body: (base) => ({
      code: "ok",
      account: { ...base, idNumber: { suffix: "11010519491231002X" } },
    }),
  },
  {
    label: "a missing identity projection",
    body: (base) => {
      const { idNumber: omitted, ...rest } = base;
      void omitted;
      return { code: "ok", account: rest };
    },
  },
  {
    label: "a missing suffix",
    body: (base) => ({ code: "ok", account: { ...base, idNumber: {} } }),
  },
  {
    label: "a numeric suffix",
    body: (base) => ({
      code: "ok",
      account: { ...base, idNumber: { suffix: 1234 } },
    }),
  },
  {
    label: "a missing binding projection",
    body: (base) => {
      const { binding: omitted, ...rest } = base;
      void omitted;
      return { code: "ok", account: rest };
    },
  },
  {
    label: "an empty binding projection",
    body: (base) => ({ code: "ok", account: { ...base, binding: {} } }),
  },
  {
    label: "a binding with an illegal revision",
    body: (base) => ({
      code: "ok",
      account: { ...base, binding: { revision: "invalid" } },
    }),
  },
  {
    label: "a binding with a null revision",
    body: (base) => ({
      code: "ok",
      account: { ...base, binding: { revision: null } },
    }),
  },
  {
    label: "a bare binding value",
    body: (base) => ({ code: "ok", account: { ...base, binding: "bound" } }),
  },
  {
    label: "a null avatar projection",
    body: (base) => ({ code: "ok", account: { ...base, avatar: null } }),
  },
  {
    label: "a missing avatar projection",
    body: (base) => {
      const { avatar: omitted, ...rest } = base;
      void omitted;
      return { code: "ok", account: rest };
    },
  },
  {
    label: "an insecure avatar link",
    body: (base) => ({
      code: "ok",
      account: {
        ...base,
        avatar: {
          ...base.avatar,
          href: base.avatar.href.replace("https:", "http:"),
        },
      },
    }),
  },
  {
    label: "an avatar link with an extra query field",
    body: (base) => ({
      code: "ok",
      account: {
        ...base,
        avatar: { ...base.avatar, href: `${base.avatar.href}&extra=1` },
      },
    }),
  },
  {
    label: "a wrong media type",
    body: (base) => ({ code: "ok", account: base }),
    contentType: "text/plain",
  },
  {
    label: "a contradictory success code",
    body: (base) => ({ code: "rate_limited", account: base }),
  },
  {
    label: "a decomposed name",
    body: (base) => ({
      code: "ok",
      account: { ...base, name: "Á名" },
    }),
  },
];

for (const value of malformed) {
  test(`rejects ${value.label} and conceals the whole summary`, async ({
    page,
  }) => {
    const base = account("可信姓名", "002X");
    let untrusted = false;
    let reads = 0;
    await page.route("**/api/v1/accounts/current", async (route) => {
      reads++;
      if (!untrusted) {
        await route.fulfill({ json: { code: "ok", account: base } });
        return;
      }
      const body = value.body(base);
      if (value.contentType) {
        await route.fulfill({
          status: 200,
          contentType: value.contentType,
          body: JSON.stringify(body),
        });
        return;
      }
      await route.fulfill({ json: body });
    });
    await page.goto(`${origin}/account`);
    await expectSummary(page, "可信姓名", "002X");
    untrusted = true;
    await page.evaluate(() => window.dispatchEvent(new Event("focus")));
    await expect(
      page.getByRole("heading", { name: "请重新加载页面", exact: true }),
    ).toBeVisible();
    await expectNoSummary(page, "可信姓名", "002X");
    expect((await selection(page))?.session?.id).toBe(first);
    expect(reads).toBe(2);
  });
}

test("loads the current avatar only from the trusted projection", async ({
  page,
}) => {
  const trusted = account("图片来源姓名", "0031");
  await page.route("**/api/v1/accounts/current", (route) =>
    route.fulfill({ json: { code: "ok", account: trusted } }),
  );
  await page.goto(`${origin}/account`);
  await expectSummary(page, "图片来源姓名", "0031");
  await expectAvatarReady(page, trusted.avatar.href);
  const image = await page.locator(".avatar-image").evaluate((node) => {
    const picture = node as HTMLImageElement;
    return {
      alt: picture.alt,
      complete: picture.complete,
      referrerPolicy: picture.referrerPolicy,
      width: picture.naturalWidth,
      height: picture.naturalHeight,
    };
  });
  expect(image).toEqual({
    alt: "图片来源姓名的头像",
    complete: true,
    referrerPolicy: "no-referrer",
    width: 512,
    height: 512,
  });
  await expect(page.locator(".avatar-slot")).not.toHaveAttribute("role");
});

test("a verified unchanged avatar reuses its valid link after concealment", async ({
  page,
}) => {
  const original = account();
  const refreshed = {
    ...original,
    avatar: {
      ...original.avatar,
      href: original.avatar.href.replace("key_id=7", "key_id=8"),
    },
  };
  let reads = 0;
  await page.route("**/api/v1/accounts/current", (route) =>
    route.fulfill({
      json: { code: "ok", account: ++reads === 1 ? original : refreshed },
    }),
  );
  await page.goto(`${origin}/account`);
  await expectAvatarReady(page, original.avatar.href);
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect.poll(() => reads).toBe(2);
  await expectAvatarReady(page, original.avatar.href);
});

test("an image failure preserves trusted facts and stops after one explicit recovery", async ({
  page,
}) => {
  await page.setViewportSize({ width: 320, height: 1000 });
  const captureFailure = async (state: "failed" | "terminal") => {
    for (const colorScheme of ["light", "dark"] as const) {
      await page.emulateMedia({ colorScheme });
      await page.screenshot({
        path: `test-results/avatar-${state}-${colorScheme}-320.png`,
        fullPage: true,
      });
    }
  };
  const trusted = account("图片姓名", "0042");
  let reads = 0;
  let images = 0;
  await page.route("**/api/v1/accounts/current", (route) => {
    reads++;
    return route.fulfill({ json: { code: "ok", account: trusted } });
  });
  await page.route("https://fixture.invalid/**", (route) => {
    images++;
    return route.fulfill({ status: 503, body: "" });
  });
  await page.goto(`${origin}/account`);
  await expectSummary(page, "图片姓名", "0042");
  await expect.poll(() => images, { message: "initial image attempt" }).toBe(1);
  await expect(
    page.getByRole("img", { name: "头像暂时无法加载" }),
  ).toBeVisible();
  await expect(page.locator(".avatar-image")).toHaveCount(0);
  await expect(page.locator(".avatar-message")).toContainText(
    "图片暂时无法加载",
  );
  await expectSummary(page, "图片姓名", "0042");

  await captureFailure("failed");
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect.poll(() => reads).toBe(2);
  await expectSummary(page, "图片姓名", "0042");
  await expect(
    page.getByRole("button", { name: "重新加载头像" }),
  ).toBeVisible();
  expect(images).toBe(1);
  await page.getByRole("button", { name: "重新加载头像" }).click();
  await expect.poll(() => reads).toBe(3);
  await expect.poll(() => images).toBe(2);
  await expect(page.locator(".avatar-message")).toContainText(
    "头像仍暂时无法加载，其他资料可继续查看。",
  );
  await expect(page.getByRole("button", { name: "重新加载头像" })).toHaveCount(
    0,
  );
  await expectSummary(page, "图片姓名", "0042");
  await captureFailure("terminal");
  for (const event of ["focus", "visibilitychange"]) {
    const before = reads;
    await page.evaluate((event) => {
      const target = event === "focus" ? window : document;
      target.dispatchEvent(new Event(event));
    }, event);
    await expect.poll(() => reads).toBe(before + 1);
    await expectSummary(page, "图片姓名", "0042");
    await expect(page.locator(".avatar-message")).toContainText(
      "头像仍暂时无法加载，其他资料可继续查看。",
    );
    await expect(page.locator(".avatar-image")).toHaveCount(0);
    expect(images).toBe(2);
  }
});

test("malformed image bytes fail locally without concealing the summary", async ({
  page,
}) => {
  const trusted = account("坏图姓名", "0053");
  await page.route("**/api/v1/accounts/current", (route) =>
    route.fulfill({ json: { code: "ok", account: trusted } }),
  );
  await page.route("https://fixture.invalid/**", (route) =>
    route.fulfill({
      contentType: "image/webp",
      body: Buffer.from("not a WebP image"),
    }),
  );
  await page.goto(`${origin}/account`);
  await expectSummary(page, "坏图姓名", "0053");
  await expect(
    page.getByRole("img", { name: "头像暂时无法加载" }),
  ).toBeVisible();
  await expect(page.locator(".avatar-image")).toHaveCount(0);
  await expect(page.locator(".avatar-message")).toContainText(
    "图片暂时无法加载",
  );
});

test("a delayed old image cannot replace a newer selected account", async ({
  page,
}) => {
  const old = account("旧账户", "0001");
  old.avatar = {
    ...old.avatar,
    revision: randomUUID(),
    href: `https://fixture.invalid/api/v1/avatars/${"a".repeat(64)}.webp?key_id=7&expires_at=2051222400&signature=${"_".repeat(42)}8`,
  };
  const next = account("新账户", "0002");
  next.avatar = {
    ...next.avatar,
    revision: randomUUID(),
    href: `https://fixture.invalid/api/v1/avatars/${"b".repeat(64)}.webp?key_id=7&expires_at=2051222400&signature=${"_".repeat(42)}8`,
  };
  const oldImage = gate();
  await page.route("**/api/v1/accounts/current", (route) => {
    const selected = route.request().headers()["session-id"];
    return route.fulfill({
      json: { code: "ok", account: selected === second ? next : old },
    });
  });
  await page.route("https://fixture.invalid/**", async (route) => {
    if (route.request().url() === old.avatar.href) {
      oldImage.mark();
      await oldImage.opened;
    }
    await route
      .fulfill({ contentType: "image/webp", body: avatarBytes })
      .catch(() => {});
  });
  await page.goto(`${origin}/account`);
  await expectSummary(page, "旧账户", "0001");
  await oldImage.entered;
  await choose(page, second);
  await expectSummary(page, "新账户", "0002");
  await expectAvatarReady(page, next.avatar.href);
  oldImage.release();
  await page.waitForTimeout(100);
  await expect(page.locator(".avatar-image")).toHaveAttribute(
    "src",
    next.avatar.href,
  );
  await expectSummary(page, "新账户", "0002");
  expect((await selection(page))?.session?.id).toBe(second);
});

test("a new avatar revision stays hidden until its own bytes are confirmed", async ({
  page,
}) => {
  const revisionId = randomUUID();
  const before = account("版本姓名", "0064", revisionId);
  before.avatar = {
    ...before.avatar,
    revision: randomUUID(),
    href: `https://fixture.invalid/api/v1/avatars/${"a".repeat(64)}.webp?key_id=7&expires_at=2051222400&signature=${"_".repeat(42)}8`,
  };
  const after = account("版本姓名", "0064", revisionId);
  after.avatar = {
    ...after.avatar,
    revision: randomUUID(),
    href: `https://fixture.invalid/api/v1/avatars/${"b".repeat(64)}.webp?key_id=7&expires_at=2051222400&signature=${"_".repeat(42)}8`,
  };
  const revisedImage = gate();
  let revised = false;
  await page.route("**/api/v1/accounts/current", (route) => {
    return route.fulfill({
      json: { code: "ok", account: revised ? after : before },
    });
  });
  await page.route("https://fixture.invalid/**", async (route) => {
    if (route.request().url() === after.avatar.href) {
      revisedImage.mark();
      await revisedImage.opened;
    }
    await route
      .fulfill({ contentType: "image/webp", body: avatarBytes })
      .catch(() => {});
  });
  await page.goto(`${origin}/account`);
  await expectSummary(page, "版本姓名", "0064");
  await expectAvatarReady(page, before.avatar.href);
  revised = true;
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await revisedImage.entered;
  await expectSummary(page, "版本姓名", "0064");
  await expect(page.locator(".avatar-image")).toHaveAttribute(
    "src",
    after.avatar.href,
  );
  await expect(page.locator(".avatar-image")).toBeHidden();
  await expect(
    page.locator('.avatar-slot[data-state="loading"]'),
  ).toBeVisible();
  revisedImage.release();
  await expectAvatarReady(page, after.avatar.href);
});

test("a storage failure after a trusted read conceals the entire summary", async ({
  page,
}) => {
  await page.route("**/api/v1/accounts/current", (route) =>
    route.fulfill({ json: { code: "ok", account: account() } }),
  );
  await page.goto(`${origin}/account`);
  await expectSummary(page, "示例用户", "012X");
  const selected = await selection(page);
  await page.evaluate(() => {
    const original = IDBDatabase.prototype.transaction;
    IDBDatabase.prototype.transaction = function (...args) {
      if (this.name === "halo.session")
        throw new DOMException("Fixture read refusal", "InvalidStateError");
      return original.apply(this, args);
    };
    Reflect.set(window, "restoreAccountTransaction", () => {
      IDBDatabase.prototype.transaction = original;
    });
    window.dispatchEvent(new Event("focus"));
  });
  await expect(
    page.getByText("浏览器无法读取登录信息。", { exact: false }),
  ).toBeVisible();
  await expectNoSummary(page, "示例用户", "012X");
  await page.evaluate(() => Reflect.get(window, "restoreAccountTransaction")());
  expect(await selection(page)).toEqual(selected);
  await page.getByRole("button", { name: "重新检查", exact: true }).click();
  await expectSummary(page, "示例用户", "012X");
});

test("an anonymous selection hides both values and keeps the login action", async ({
  page,
}) => {
  await page.route("**/api/v1/accounts/current", (route) =>
    route.fulfill({ json: { code: "ok", account: account() } }),
  );
  await page.goto(`${origin}/account`);
  await expectSummary(page, "示例用户", "012X");
  await page.evaluate(async () => {
    const selected = await window.accountSessions.read();
    if (!selected?.session) throw new Error("Missing fixture selection");
    await window.accountSessions.clear({
      id: selected.session.id,
      revision: selected.revision,
    });
  });
  await expect(
    page.getByRole("heading", { name: "请使用手机号登录", exact: true }),
  ).toBeVisible();
  await expectNoSummary(page, "示例用户", "012X");
  await expect(
    page.getByRole("link", { name: "手机号登录", exact: true }),
  ).toBeVisible();
});

type Failure = {
  label: string;
  abort?: boolean;
  status?: number;
  code?: string;
  heading: string;
  clears: boolean;
  headers?: Record<string, string>;
};
const failures: Failure[] = [
  {
    label: "401",
    status: 401,
    code: "authentication_invalid",
    heading: "请重新登录",
    clears: true,
  },
  {
    label: "403",
    status: 403,
    code: "permission_denied",
    heading: "暂时无法完成检查",
    clears: false,
  },
  {
    label: "429",
    status: 429,
    code: "rate_limited",
    heading: "请稍后再检查",
    clears: false,
    headers: { "Retry-After": "30" },
  },
  {
    label: "503",
    status: 503,
    code: "dependency_unavailable",
    heading: "暂时无法检查登录状态",
    clears: false,
  },
  {
    label: "disconnect",
    abort: true,
    heading: "暂时无法检查登录状态",
    clears: false,
  },
];

for (const value of failures) {
  test(`${value.label} conceals both values and ${value.clears ? "clears" : "retains"} the original selection`, async ({
    page,
  }) => {
    let failed = false;
    let reads = 0;
    await page.route("**/api/v1/accounts/current", async (route) => {
      reads++;
      if (!failed) {
        await route.fulfill({
          json: { code: "ok", account: account() },
        });
        return;
      }
      if (value.abort) {
        await route.abort();
        return;
      }
      await route.fulfill({
        status: value.status,
        ...(value.headers ? { headers: value.headers } : {}),
        json: { code: value.code },
      });
    });
    await page.goto(`${origin}/account`);
    await expectSummary(page, "示例用户", "012X");
    failed = true;
    await page.evaluate(() => window.dispatchEvent(new Event("focus")));
    await expect(
      page.getByRole("heading", { name: value.heading, exact: true }),
    ).toBeVisible();
    await expectNoSummary(page, "示例用户", "012X");
    const saved = await selection(page);
    if (value.clears) expect(saved?.session).toBeNull();
    else expect(saved?.session?.id).toBe(first);
    expect(reads).toBe(2);
    if (value.status === 429) {
      await expect(
        page.getByRole("button", { name: /秒后可重新检查/ }),
      ).toBeDisabled();
      await page.evaluate(() => window.dispatchEvent(new Event("focus")));
      expect(reads).toBe(2);
    }
  });
}

test("a storage refusal hides both values and retains the original selection", async ({
  page,
}) => {
  await page.addInitScript(() => {
    const factory = indexedDB;
    Object.defineProperty(factory, "open", {
      configurable: true,
      value: () => {
        throw new DOMException("Fixture storage refusal", "InvalidStateError");
      },
    });
    Reflect.set(window, "restoreAccountStorage", () => {
      Reflect.deleteProperty(factory, "open");
    });
  });
  await page.goto(`${origin}/account`);
  await expect(
    page.getByRole("heading", {
      name: "暂时无法检查登录状态",
      exact: true,
    }),
  ).toBeVisible();
  await expect(
    page.getByText("浏览器无法读取登录信息。", { exact: false }),
  ).toBeVisible();
  await expectNoSummary(page, "示例用户", "012X");
  await page.evaluate(() => Reflect.get(window, "restoreAccountStorage")());
  expect((await selection(page))?.session?.id).toBe(first);
});

test("a build mismatch conceals both values and prevents another account read", async ({
  page,
}) => {
  let broken = false;
  let reads = 0;
  await page.route("**/build.json", (route) =>
    route.fulfill({
      json: { buildId: broken ? "other-fixture" : "account-fixture" },
    }),
  );
  await page.route("**/api/v1/accounts/current", async (route) => {
    reads++;
    await route.fulfill({ json: { code: "ok", account: account() } });
  });
  await page.goto(`${origin}/account`);
  await expectSummary(page, "示例用户", "012X");
  broken = true;
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect(
    page.getByRole("heading", { name: "请重新加载页面", exact: true }),
  ).toBeVisible();
  await expectNoSummary(page, "示例用户", "012X");
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  expect(reads).toBe(1);
  expect((await selection(page))?.session?.id).toBe(first);
});

test("a real selection notification replaces a delayed response and a late 401 cannot clear the new session", async ({
  page,
}) => {
  const oldRead = gate();
  const newRead = gate();
  const newRevision = randomUUID();
  let reads = 0;
  await page.route("**/api/v1/accounts/current", async (route) => {
    reads++;
    if (route.request().headers()["session-id"] === first) {
      oldRead.mark();
      await oldRead.opened;
      await route
        .fulfill({
          status: 401,
          json: { code: "authentication_invalid" },
        })
        .catch(() => {});
      return;
    }
    newRead.mark();
    await newRead.opened;
    await route
      .fulfill({
        json: {
          code: "ok",
          account: account("账户乙", "0000", newRevision),
        },
      })
      .catch(() => {});
  });
  await page.goto(`${origin}/account`);
  await oldRead.entered;
  await choose(page, second);
  await newRead.entered;
  oldRead.release();
  expect((await selection(page))?.session?.id).toBe(second);
  await expectNoSummary(page, "账户乙", "0000");
  newRead.release();
  await expectSummary(page, "账户乙", "0000");
  expect((await selection(page))?.session?.id).toBe(second);
});

test("a delayed bound response cannot flash or overwrite a later unbound account", async ({
  page,
}) => {
  const oldRead = gate();
  const oldFinished = gate();
  const boundRevision = randomUUID();
  const bound = account("旧绑定账户", "0001", randomUUID(), {
    revision: boundRevision,
  });
  const unbound = {
    ...account("新未绑定账户", "0002"),
    id: "00000000-0000-4000-8000-000000000002",
    personId: "00000000-0000-4000-8000-000000000002",
  };
  let reads = 0;
  await page.route("**/api/v1/accounts/current", async (route) => {
    reads++;
    if (route.request().headers()["session-id"] === first) {
      oldRead.mark();
      await oldRead.opened;
      await route
        .fulfill({ json: { code: "ok", account: bound } })
        .catch(() => {});
      oldFinished.mark();
      return;
    }
    await route
      .fulfill({ json: { code: "ok", account: unbound } })
      .catch(() => {});
  });
  await page.goto(`${origin}/account`);
  await oldRead.entered;
  const observed = await page.evaluateHandle(() => {
    const result = { exposed: false };
    const observer = new MutationObserver((records) => {
      for (const record of records) {
        const values = [
          record.oldValue,
          record.target.textContent,
          ...Array.from(record.addedNodes, (node) => node.textContent),
        ];
        if (values.some((text) => /旧绑定账户|已绑定/.test(text ?? "")))
          result.exposed = true;
      }
    });
    observer.observe(document.querySelector("main")!, {
      childList: true,
      subtree: true,
      characterData: true,
      characterDataOldValue: true,
    });
    return { result, stop: () => observer.disconnect() };
  });
  const aborted = page.waitForEvent("requestfailed", {
    predicate: (request) =>
      new URL(request.url()).pathname === "/api/v1/accounts/current" &&
      request.headers()["session-id"] === first,
  });
  await choose(page, second);
  await aborted;
  await expectSummary(page, "新未绑定账户", "0002", "未绑定");
  const before = await page.evaluate(() => document.body.textContent ?? "");
  expect(before).not.toContain("已绑定");
  expect(before).not.toContain(boundRevision);
  oldRead.release();
  await oldFinished.entered;
  await expectSummary(page, "新未绑定账户", "0002", "未绑定");
  const after = await page.evaluate(() => document.body.textContent ?? "");
  expect(after).not.toContain("已绑定");
  expect(after).not.toContain(boundRevision);
  expect(await observed.evaluate(({ result }) => result.exposed)).toBe(false);
  await observed.evaluate(({ stop }) => stop());
  await observed.dispose();
  expect((await selection(page))?.session?.id).toBe(second);
  expect(reads).toBeGreaterThanOrEqual(2);
});

test("a real revision notification discards the old summary and updates name, suffix and binding together", async ({
  page,
}) => {
  const oldRead = gate();
  const newRead = gate();
  const boundRevision = randomUUID();
  let reads = 0;
  await page.route("**/api/v1/accounts/current", async (route) => {
    reads++;
    if (reads === 1) {
      oldRead.mark();
      await oldRead.opened;
      await route
        .fulfill({
          json: { code: "ok", account: account("旧姓名", "012X") },
        })
        .catch(() => {});
      return;
    }
    newRead.mark();
    await newRead.opened;
    await route
      .fulfill({
        json: {
          code: "ok",
          account: account("新姓名", "0000", randomUUID(), {
            revision: boundRevision,
          }),
        },
      })
      .catch(() => {});
  });
  await page.goto(`${origin}/account`);
  await oldRead.entered;
  await page.evaluate(async () => {
    await window.accountSessions.begin(crypto.randomUUID());
  });
  await newRead.entered;
  oldRead.release();
  await expectNoSummary(page, "旧姓名", "012X", "已绑定", boundRevision);
  newRead.release();
  await expectSummary(page, "新姓名", "0000", "已绑定");
  expect((await selection(page))?.session?.id).toBe(first);
});

test("a late 401 cannot clear a newer unannounced local selection", async ({
  page,
}) => {
  const oldRead = gate();
  const newRead = gate();
  const newRevision = randomUUID();
  let reads = 0;
  await page.route("**/api/v1/accounts/current", async (route) => {
    reads++;
    if (route.request().headers()["session-id"] === first) {
      oldRead.mark();
      await oldRead.opened;
      await route.fulfill({
        status: 401,
        json: { code: "authentication_invalid" },
      });
      return;
    }
    newRead.mark();
    await newRead.opened;
    await route.fulfill({
      json: { code: "ok", account: account("账户乙", "0000") },
    });
  });
  await page.goto(`${origin}/account`);
  await oldRead.entered;
  await store(page, second, newRevision);
  oldRead.release();
  await newRead.entered;
  expect(await selection(page)).toMatchObject({
    revision: newRevision,
    session: { id: second },
  });
  newRead.release();
  await expectSummary(page, "账户乙", "0000");
  expect((await selection(page))?.session?.id).toBe(second);
  expect(reads).toBe(2);
});

test("another tab clearing an invalid selection makes a pending reader anonymous", async ({
  page,
  context,
}) => {
  let invalid = false;
  const pending = gate();
  await context.route("**/api/v1/accounts/current", (route) =>
    route.fulfill(
      invalid
        ? { status: 401, json: { code: "authentication_invalid" } }
        : { json: { code: "ok", account: account() } },
    ),
  );
  await page.goto(`${origin}/account`);
  await expectSummary(page, "示例用户", "012X");
  const other = await context.newPage();
  await other.goto(`${origin}/account`);
  await expectSummary(other, "示例用户", "012X");
  await page.route("**/api/v1/accounts/current", async (route) => {
    pending.mark();
    await pending.opened;
    await route
      .fulfill({ status: 401, json: { code: "authentication_invalid" } })
      .catch(() => {});
  });
  invalid = true;
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await pending.entered;
  await other.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect.poll(async () => (await selection(other))?.session).toBeNull();
  pending.release();
  await expect(
    page.getByRole("heading", { name: "请使用手机号登录", exact: true }),
  ).toBeVisible();
  await expectNoSummary(page, "示例用户", "012X");
  await expect(page.locator(".avatar-image")).toHaveCount(0);
  await other.close();
});

test("lifecycle events conceal both values synchronously before any reread", async ({
  page,
}) => {
  const pending: (() => void)[] = [];
  let reads = 0;
  await page.route("**/api/v1/accounts/current", async (route) => {
    reads++;
    await new Promise<void>((resolve) => pending.push(resolve));
    await route
      .fulfill({
        json: { code: "ok", account: account("生命周期姓名", "012X") },
      })
      .catch(() => {});
  });
  const release = async () => {
    await expect.poll(() => pending.length).toBeGreaterThan(0);
    pending.shift()!();
    await expectSummary(page, "生命周期姓名", "012X");
    await expectAvatarReady(page);
  };
  await page.goto(`${origin}/account`);
  await release();
  for (const event of ["focus", "pageshow"] as const) {
    const before = reads;
    const state = await page.evaluate((event) => {
      window.dispatchEvent(new Event(event));
      const text = document.body.textContent ?? "";
      return {
        summary: document.querySelector(".current-summary") !== null,
        avatar: document.querySelector(".avatar-image") !== null,
        name: text.includes("生命周期姓名"),
        suffix: text.includes("012X"),
      };
    }, event);
    expect(state).toEqual({
      summary: false,
      avatar: false,
      name: false,
      suffix: false,
    });
    await expect.poll(() => reads).toBe(before + 1);
    await release();
  }
  {
    const before = reads;
    const state = await page.evaluate(() => {
      document.dispatchEvent(new Event("visibilitychange"));
      const text = document.body.textContent ?? "";
      return {
        summary: document.querySelector(".current-summary") !== null,
        avatar: document.querySelector(".avatar-image") !== null,
        name: text.includes("生命周期姓名"),
        suffix: text.includes("012X"),
      };
    });
    expect(state).toEqual({
      summary: false,
      avatar: false,
      name: false,
      suffix: false,
    });
    await expect.poll(() => reads).toBe(before + 1);
    await release();
  }
  {
    const before = reads;
    const state = await page.evaluate(() => {
      Object.defineProperty(document, "hidden", {
        configurable: true,
        get: () => true,
      });
      document.dispatchEvent(new Event("visibilitychange"));
      const text = document.body.textContent ?? "";
      return {
        summary: document.querySelector(".current-summary") !== null,
        avatar: document.querySelector(".avatar-image") !== null,
        name: text.includes("生命周期姓名"),
        suffix: text.includes("012X"),
      };
    });
    expect(state).toEqual({
      summary: false,
      avatar: false,
      name: false,
      suffix: false,
    });
    await page.evaluate(
      () =>
        new Promise<void>((resolve) =>
          requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
        ),
    );
    expect(reads).toBe(before);
    await page.evaluate(() => {
      Reflect.deleteProperty(document, "hidden");
      document.dispatchEvent(new Event("visibilitychange"));
    });
    await expect.poll(() => reads).toBe(before + 1);
    await release();
  }
  {
    const before = reads;
    const state = await page.evaluate(() => {
      window.dispatchEvent(new Event("pagehide"));
      const text = document.body.textContent ?? "";
      return {
        summary: document.querySelector(".current-summary") !== null,
        avatar: document.querySelector(".avatar-image") !== null,
        name: text.includes("生命周期姓名"),
        suffix: text.includes("012X"),
      };
    });
    expect(state).toEqual({
      summary: false,
      avatar: false,
      name: false,
      suffix: false,
    });
    await page.evaluate(
      () =>
        new Promise<void>((resolve) =>
          requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
        ),
    );
    expect(reads).toBe(before);
    await page.evaluate(() => window.dispatchEvent(new Event("focus")));
    await expect.poll(() => reads).toBe(before + 1);
    await release();
  }
  {
    const before = reads;
    await page.evaluate(() =>
      window.dispatchEvent(new Event("halo:reload-required")),
    );
    await expect(
      page.getByRole("heading", { name: "请重新加载页面", exact: true }),
    ).toBeVisible();
    await expectNoSummary(page, "生命周期姓名", "012X");
    await page.evaluate(() => {
      window.dispatchEvent(new Event("focus"));
      window.dispatchEvent(new Event("pageshow"));
    });
    await page.evaluate(
      () =>
        new Promise<void>((resolve) =>
          requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
        ),
    );
    expect(reads).toBe(before);
  }
});

test("a detached image drops its source and late events cannot bind a newer attempt", async ({
  page,
}) => {
  const pending: (() => void)[] = [];
  let reads = 0;
  await page.route("**/api/v1/accounts/current", async (route) => {
    reads++;
    await new Promise<void>((resolve) => pending.push(resolve));
    await route
      .fulfill({
        json: { code: "ok", account: account("断开姓名", "012X") },
      })
      .catch(() => {});
  });
  const release = async () => {
    await expect.poll(() => pending.length).toBeGreaterThan(0);
    pending.shift()!();
  };
  const paint = () =>
    page.evaluate(
      () =>
        new Promise<void>((resolve) =>
          requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
        ),
    );
  const save = () =>
    page.evaluate(() => {
      window.savedAvatar =
        document.querySelector<HTMLImageElement>(".avatar-image");
      return window.savedAvatar?.getAttribute("src") ?? null;
    });
  const detached = () =>
    page.evaluate(() => ({
      connected: window.savedAvatar?.isConnected ?? true,
      src: window.savedAvatar
        ? window.savedAvatar.getAttribute("src")
        : "missing",
    }));
  const late = () =>
    page.evaluate(() => {
      window.savedAvatar?.dispatchEvent(new Event("load"));
      window.savedAvatar?.dispatchEvent(new Event("error"));
    });

  await page.goto(`${origin}/account`);
  await release();
  await expectSummary(page, "断开姓名", "012X");
  await expectAvatarReady(page);
  expect(await save()).toContain("/api/v1/avatars/");

  // pagehide hides the view and detaches the retained node.
  await page.evaluate(() => window.dispatchEvent(new Event("pagehide")));
  await paint();
  expect(await detached()).toEqual({ connected: false, src: null });
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect.poll(() => reads).toBe(2);
  await expect(page.locator(".avatar-image")).toHaveCount(0);
  await late();
  await expect(page.getByText("图片暂时无法加载")).toHaveCount(0);
  await expect(page.locator(".avatar-image")).toHaveCount(0);
  await release();
  await expectAvatarReady(page);

  // A selection notification builds a newer binding; the saved node stays dead.
  expect(await save()).toContain("/api/v1/avatars/");
  const attempt = await page.evaluate(() =>
    window.accountSessions.begin(crypto.randomUUID()),
  );
  await expect.poll(() => reads).toBe(3);
  const accepted = await page.evaluate(
    ({ attempt, id, expiresAt }) =>
      window.accountSessions.accept(attempt, { id, expiresAt }),
    { attempt, id: second, expiresAt },
  );
  expect(accepted).not.toBeNull();
  await expect.poll(() => reads).toBe(4);
  expect(pending).toHaveLength(2);
  await paint();
  expect(await detached()).toEqual({ connected: false, src: null });
  await late();
  // begin() and accept() each notify. The first response belongs to the
  // superseded selection, so it cannot reveal the newer attempt's image.
  await release();
  await expect(page.locator(".avatar-image")).toHaveCount(0);
  await release();
  await expectAvatarReady(page);

  // SPA navigation unmounts the page and the image with it.
  expect(await save()).toContain("/api/v1/avatars/");
  await page.evaluate(() => {
    history.pushState({}, "", "/");
    window.dispatchEvent(new PopStateEvent("popstate"));
  });
  await expect(page.getByText("Public fixture")).toBeVisible();
  await paint();
  expect(await detached()).toEqual({ connected: false, src: null });
  await late();
  await expect(page.locator(".avatar-image")).toHaveCount(0);

  // Return to the account and prove a later concealment behaves the same.
  await page.evaluate(() => {
    history.pushState({}, "", "/account");
    window.dispatchEvent(new PopStateEvent("popstate"));
  });
  await expect.poll(() => reads).toBe(5);
  await release();
  await expectAvatarReady(page);
  expect(await save()).toContain("/api/v1/avatars/");
  await page.evaluate(() =>
    window.dispatchEvent(new Event("halo:reload-required")),
  );
  await paint();
  expect(await detached()).toEqual({ connected: false, src: null });
  await late();
  await expect(
    page.getByRole("heading", { name: "请重新加载页面", exact: true }),
  ).toBeVisible();
  await expect(page.locator(".avatar-image")).toHaveCount(0);
});

test("long names and both binding statuses fit every width and keep keyboard access", async ({
  page,
}) => {
  const longName = "阿依古丽·阿不都热依木·买买提明";
  const revisionId = randomUUID();
  const bindingRevision = randomUUID();
  const states = [
    { key: "unbound", label: "未绑定", binding: null },
    { key: "bound", label: "已绑定", binding: { revision: bindingRevision } },
  ] as const;
  let binding: AccountPayload["binding"] = null;
  let reads = 0;
  await page.route("**/api/v1/accounts/current", (route) => {
    reads++;
    return route.fulfill({
      json: {
        code: "ok",
        account: account(longName, "012X", revisionId, binding),
      },
    });
  });
  await page.goto(`${origin}/account`);
  await expectSummary(page, longName, "012X");
  await expectAvatarReady(page);
  for (const state of states) {
    binding = state.binding;
    const before = reads;
    await page.evaluate(() => window.dispatchEvent(new Event("focus")));
    await expect.poll(() => reads).toBe(before + 1);
    await expectSummary(page, longName, "012X", state.label);
    await expectAvatarReady(page);
    await expect(page.locator(".correction-panel")).toBeVisible();
    await expect(
      page.getByRole("link", { name: "退出全部设备", exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("link", { name: "退出本次登录", exact: true }),
    ).toBeVisible();
    const summary = page.locator(".current-summary");
    const snapshot = await summary.ariaSnapshot();
    expect(snapshot).toContain("微信");
    expect(snapshot).toContain(state.label);
    await writeFile(
      `test-results/account-binding-layout-${state.key}.aria.txt`,
      snapshot,
    );
    if (state.binding) {
      const text = await page.evaluate(() => document.body.textContent ?? "");
      expect(text).not.toContain(state.binding.revision);
    }
    for (const colorScheme of ["light", "dark"] as const) {
      await page.emulateMedia({ colorScheme });
      for (const width of [1440, 768, 390, 320]) {
        await page.setViewportSize({ width, height: 1000 });
        const layout = await page.evaluate((longName) => {
          const avatar = document.querySelector<HTMLElement>(".avatar-slot");
          const picture =
            document.querySelector<HTMLImageElement>(".avatar-image");
          const avatarBox = avatar?.getBoundingClientRect();
          const rows = Array.from(
            document.querySelectorAll<HTMLElement>(".current-summary > div"),
          ).map((row) => {
            const dt = row.querySelector("dt")!;
            const dd = row.querySelector("dd")!;
            return {
              dt: dt.textContent,
              dd: dd.textContent,
              dtBox: dt.getBoundingClientRect().toJSON(),
              ddBox: dd.getBoundingClientRect().toJSON(),
            };
          });
          return {
            overflow: document.documentElement.scrollWidth > innerWidth,
            longName,
            avatar: {
              width: avatarBox?.width,
              height: avatarBox?.height,
              state: avatar?.dataset.state,
              src: picture?.getAttribute("src"),
              alt: picture?.getAttribute("alt"),
              naturalWidth: picture?.naturalWidth,
              naturalHeight: picture?.naturalHeight,
            },
            rows,
          };
        }, longName);
        expect(layout.overflow).toBe(false);
        expect(layout.rows.map((row) => row.dt)).toEqual([
          "姓名",
          "身份证号",
          "微信",
        ]);
        expect(layout.rows.map((row) => row.dd)).toEqual([
          longName,
          "尾号 012X",
          state.label,
        ]);
        expect(layout.avatar.width).toBe(width <= 600 ? 56 : 72);
        expect(layout.avatar.height).toBe(width <= 600 ? 56 : 72);
        expect(layout.avatar.state).toBe("ready");
        expect(layout.avatar.alt).toBe(`${longName}的头像`);
        expect(layout.avatar.naturalWidth).toBe(512);
        expect(layout.avatar.naturalHeight).toBe(512);
        for (const row of layout.rows) {
          expect(row.ddBox.left).toBeGreaterThan(row.dtBox.left);
          expect(row.dtBox.top).toBeLessThan(row.ddBox.bottom);
          expect(row.ddBox.top).toBeLessThan(row.dtBox.bottom);
        }
        for (let index = 1; index < layout.rows.length; index++)
          expect(layout.rows[index].dtBox.top).toBeGreaterThan(
            layout.rows[index - 1].dtBox.top,
          );
        await writeFile(
          `test-results/account-layout-${state.key}-${colorScheme}-${width}.json`,
          JSON.stringify(layout, null, 2),
        );
        // Paint offscreen content before fullPage capture, matching the other fixtures.
        await page.evaluate(() =>
          scrollTo(0, document.documentElement.scrollHeight),
        );
        await expect(page.locator(".site-footer")).toBeInViewport();
        await page.screenshot({
          path: `test-results/account-bottom-${state.key}-${colorScheme}-${width}.png`,
        });
        await page.evaluate(async () => {
          scrollTo(0, 0);
          await document.fonts.ready;
          await new Promise<void>((resolve) =>
            requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
          );
        });
        await page.screenshot({
          path: `test-results/account-${state.key}-${colorScheme}-${width}.png`,
          fullPage: true,
        });
      }
    }
  }
  await page.setViewportSize({ width: 1440, height: 1000 });
  const heading = page.getByRole("heading", {
    name: "当前已登录",
    exact: true,
  });
  await heading.focus();
  await page.keyboard.press("Tab");
  await expect(
    page.getByRole("link", { name: "政治面貌", exact: true }),
  ).toBeFocused();
  await page.keyboard.press("Tab");
  await expect(
    page.getByRole("link", { name: "查看员工服务", exact: true }),
  ).toBeFocused();
  await page.keyboard.press("Tab");
  await expect(
    page.getByRole("link", { name: "更换手机号", exact: true }),
  ).toBeFocused();
  await page.keyboard.press("Tab");
  await expect(
    page.getByRole("link", { name: "退出全部设备", exact: true }),
  ).toBeFocused();
  await page.keyboard.press("Tab");
  await expect(
    page.getByRole("link", { name: "退出本次登录", exact: true }),
  ).toBeFocused();
  await page.keyboard.press("Tab");
  await expect(
    page.getByRole("link", { name: "注销账户", exact: true }),
  ).toBeFocused();
  await page.keyboard.press("Tab");
  const home = page.getByRole("link", { name: "返回首页", exact: true });
  await expect(home).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(page.getByText("Public fixture", { exact: true })).toBeVisible();
});

for (const cacheControl of ["no-store", "max-age=0"]) {
  test(`native history and reload conceal identity with shell ${cacheControl}`, async ({
    playwright,
  }) => {
    let projection = account("往返前姓名", "012X");
    let hold = false;
    let entered = gate();
    const pending = new Set<ServerResponse>();
    const deliver = (response: ServerResponse) => {
      response.writeHead(200, {
        "Content-Type": "application/json",
        "Cache-Control": "no-store",
      });
      response.end(JSON.stringify({ code: "ok", account: projection }));
    };
    const server = createServer((request, response) => {
      const path = new URL(request.url!, "http://127.0.0.1").pathname;
      if (path === "/api/v1/accounts/current") {
        if (hold) {
          pending.add(response);
          entered.mark();
        } else deliver(response);
        return;
      }
      if (path === "/build.json") {
        response.writeHead(200, {
          "Content-Type": "application/json",
          "Cache-Control": "no-store",
        });
        response.end(JSON.stringify({ buildId: "account-fixture" }));
        return;
      }
      if (path === "/api/v1/accounts/current/corrections") {
        response.writeHead(200, {
          "Content-Type": "application/json",
          "Cache-Control": "no-store",
        });
        response.end(JSON.stringify({ code: "ok", count: 0, corrections: [] }));
        return;
      }
      const resource = resources.get(path);
      if (resource) {
        response.writeHead(200, { "Content-Type": resource.contentType });
        response.end(resource.body);
      } else if (["/", "/account"].includes(path)) {
        // The cacheable public shell deliberately exercises bfcache even when
        // this Chromium version excludes production's no-store application shell.
        response.writeHead(200, {
          "Content-Type": "text/html",
          "Cache-Control": cacheControl,
        });
        response.end(html);
      } else {
        response.writeHead(404);
        response.end();
      }
    });
    server.listen(0, "127.0.0.1");
    await once(server, "listening");
    const address = server.address();
    if (!address || typeof address === "string")
      throw new Error("Missing identity fixture address");
    const base = `http://127.0.0.1:${address.port}`;
    // Playwright disables this browser feature by default. Keep it enabled for
    // native traversal; no request interception may disable the cache here.
    let browser: Browser | undefined;
    try {
      browser = await playwright.chromium.launch({
        channel: "chromium",
        ignoreDefaultArgs: ["--disable-back-forward-cache"],
      });
      const context = await browser.newContext();
      await context.addInitScript(() => {
        window.addEventListener("pageshow", (event) =>
          Reflect.set(window, "identityRestored", event.persisted),
        );
      });
      const page = await context.newPage();
      const cdp = await context.newCDPSession(page);
      const cacheMisses: unknown[] = [];
      cdp.on("Page.backForwardCacheNotUsed", (event) =>
        cacheMisses.push(event),
      );
      await cdp.send("Page.enable");
      await page.goto(base);
      await page.waitForFunction(() => !!window.accountSessions);
      await choose(page, first);
      await page.goto(`${base}/account`);
      await expectSummary(page, "往返前姓名", "012X");
      await page.getByRole("link", { name: "返回首页", exact: true }).click();
      await expect(page).toHaveURL(`${base}/`);
      hold = true;
      projection = account("往返后姓名", "0000");
      // A cache restoration does not emit DOMContentLoaded. Observe the native
      // history traversal directly instead of waiting for a new document event.
      await page.evaluate(() => history.back());
      await expect(page).toHaveURL(`${base}/account`);
      const restoreState = await page.evaluate(() => ({
        restored: Reflect.get(window, "identityRestored"),
        hidden: document.hidden,
        summary: !!document.querySelector(".current-summary"),
        heading: document.querySelector("h1")?.textContent,
      }));
      await writeFile(
        "test-results/identity-restore-state.json",
        JSON.stringify({ restoreState, cacheMisses }),
      );
      await expect
        .poll(() => pending.size, {
          message: "restored account starts a fresh current read",
          timeout: 5_000,
        })
        .toBeGreaterThan(0);
      await expectNoSummary(page, "往返前姓名", "012X");
      const restored = await page.evaluate(() =>
        Reflect.get(window, "identityRestored"),
      );
      const navigation = await page.evaluate(() =>
        performance.getEntriesByType("navigation").map((entry) => ({
          type: Reflect.get(entry, "type"),
          reasons: Reflect.get(entry, "notRestoredReasons")?.toJSON(),
        })),
      );
      for (const response of pending) deliver(response);
      pending.clear();
      hold = false;
      await expectSummary(page, "往返后姓名", "0000");
      entered = gate();
      hold = true;
      projection = account("刷新后姓名", "002X");
      await page.reload({ waitUntil: "domcontentloaded" });
      await entered.entered;
      await expectNoSummary(page, "往返后姓名", "0000");
      for (const response of pending) deliver(response);
      pending.clear();
      hold = false;
      await expectSummary(page, "刷新后姓名", "002X");
      await writeFile(
        `test-results/identity-navigation-${cacheControl === "no-store" ? "no-store" : "cacheable"}.json`,
        JSON.stringify({
          historyRestoredFromCache: restored,
          reloadConcealed: true,
          navigation,
          cacheMisses,
        }),
      );
      if (cacheControl !== "no-store") expect(restored).toBe(true);
    } finally {
      for (const response of pending) response.destroy();
      await browser?.close();
      server.closeAllConnections();
      await new Promise<void>((resolve, reject) =>
        server.close((error) => (error ? reject(error) : resolve())),
      );
    }
  });
}
