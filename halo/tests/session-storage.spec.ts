import { test, expect, type Page } from "@playwright/test";
import { build } from "vite";
import { fileURLToPath } from "node:url";
import type { Sessions } from "../app/authentication/session";

declare global {
  interface Window {
    sessionFixture: Sessions;
    notifications: number;
  }
}

const origin = "http://session.localhost";
const first = "56ad73b5-d5b9-4cb7-aa22-7c446b97247e";
const second = "0d3d3fb2-e285-4123-b2be-bee3fe77cd02";
const expiresAt = "2035-01-01T00:00:00Z";
let source: string;

test.beforeAll(async () => {
  const result = await build({
    configFile: false,
    logLevel: "silent",
    build: {
      write: false,
      minify: false,
      lib: {
        entry: fileURLToPath(
          new URL("../app/authentication/session.ts", import.meta.url),
        ),
        formats: ["es"],
        fileName: "session",
      },
    },
  });
  const output = Array.isArray(result) ? result[0] : result;
  if (!("output" in output))
    throw new Error("Expected an in-memory browser bundle");
  const chunk = output.output.find((item) => item.type === "chunk");
  if (!chunk || chunk.type !== "chunk")
    throw new Error("Missing session module");
  source = chunk.code;
});

async function open(page: Page) {
  await page.goto(`${origin}/fixture`);
  await page.waitForFunction(() => Boolean(window.sessionFixture));
}

test.beforeEach(async ({ context, page }) => {
  // This isolated browser fixture tests the real storage module, not business
  // responses or production CSP. The website suite covers the real HTTP flow.
  await context.route(`${origin}/**`, async (route) => {
    if (new URL(route.request().url()).pathname === "/session.js") {
      await route.fulfill({ contentType: "text/javascript", body: source });
    } else {
      await route.fulfill({
        contentType: "text/html",
        body: `<!doctype html><title>Browser session fixture</title>
          <script type="module">
            import { Sessions } from "/session.js";
            window.sessionFixture = new Sessions();
          </script>`,
      });
    }
  });
  await open(page);
});

test("concurrent tabs accept only the final registered login attempt", async ({
  page,
  context,
}) => {
  const other = await context.newPage();
  await open(other);
  const attempts = await Promise.all([
    page.evaluate((id) => window.sessionFixture.begin(id), first),
    other.evaluate((id) => window.sessionFixture.begin(id), second),
  ]);
  const selected = await page.evaluate(() => window.sessionFixture.read());
  const winner = attempts.find(
    (attempt) => attempt.operation === selected?.attempt,
  )!;
  const loser = attempts.find((attempt) => attempt !== winner)!;
  const rejected = await page.evaluate(
    ({ attempt, expiresAt }) =>
      window.sessionFixture.accept(attempt, {
        id: attempt.operation,
        expiresAt,
      }),
    { attempt: loser, expiresAt },
  );
  expect(rejected).toBeNull();
  const accepted = await other.evaluate(
    ({ attempt, expiresAt }) =>
      window.sessionFixture.accept(attempt, {
        id: attempt.operation,
        expiresAt,
      }),
    { attempt: winner, expiresAt },
  );
  expect(accepted?.id).toBe(winner.operation);
  expect(await page.evaluate(() => window.sessionFixture.read())).toEqual({
    version: 1,
    revision: accepted!.revision,
    session: { id: winner.operation, expiresAt },
    attempt: null,
  });
});

test("a new login attempt protects even the same session from a late 401 clear", async ({
  page,
  context,
}) => {
  const attempt = await page.evaluate(
    (id) => window.sessionFixture.begin(id),
    first,
  );
  const selected = await page.evaluate(
    ({ attempt, id, expiresAt }) =>
      window.sessionFixture.accept(attempt, { id, expiresAt }),
    { attempt, id: first, expiresAt },
  );
  const other = await context.newPage();
  await open(other);
  const later = await other.evaluate(
    (id) => window.sessionFixture.begin(id),
    second,
  );
  const cleared = await page.evaluate(
    (selected) => window.sessionFixture.clear(selected!),
    selected,
  );
  expect(cleared).toBe(false);
  const current = await other.evaluate(() => window.sessionFixture.read());
  expect(current?.session?.id).toBe(first);
  expect(current?.attempt).toBe(second);
  expect(current?.revision).toBe(later.revision);
});

test("reopening keeps the original expiry and stores only the non-secret projection", async ({
  page,
  context,
}) => {
  const attempt = await page.evaluate(
    (id) => window.sessionFixture.begin(id),
    first,
  );
  const rejected = await page.evaluate(
    async ({ attempt, id, expiresAt }) => {
      try {
        await window.sessionFixture.accept(attempt, {
          id,
          expiresAt,
          token: "fixture-must-not-be-stored",
          name: "测试资料",
        } as { id: string; expiresAt: string });
        return false;
      } catch {
        return true;
      }
    },
    { attempt, id: first, expiresAt },
  );
  expect(rejected).toBe(true);
  expect(await page.evaluate(() => window.sessionFixture.read())).toEqual({
    version: 1,
    revision: attempt.revision,
    session: null,
    attempt: first,
  });
  await page.evaluate(
    ({ attempt, id, expiresAt }) =>
      window.sessionFixture.accept(attempt, { id, expiresAt }),
    { attempt, id: first, expiresAt },
  );
  await page.close();
  const reopened = await context.newPage();
  await open(reopened);
  const saved = await reopened.evaluate(() => window.sessionFixture.read());
  expect(Object.keys(saved!).sort()).toEqual([
    "attempt",
    "revision",
    "session",
    "version",
  ]);
  expect(saved?.session).toEqual({ id: first, expiresAt });
  expect(JSON.stringify(saved)).not.toContain("fixture-must-not-be-stored");
  expect(JSON.stringify(saved)).not.toContain("测试资料");
});

test("clearing the old selected session preserves a concurrent login attempt", async ({
  page,
  context,
}) => {
  await page.evaluate(
    async ({ id, expiresAt }) => {
      const attempt = await window.sessionFixture.begin(id);
      await window.sessionFixture.accept(attempt, { id, expiresAt });
    },
    { id: first, expiresAt },
  );
  const other = await context.newPage();
  await open(other);
  const attempt = await other.evaluate(
    (id) => window.sessionFixture.begin(id),
    second,
  );
  const cleared = await page.evaluate(async () => {
    const observed = await window.sessionFixture.read();
    return window.sessionFixture.clear({
      id: observed!.session!.id,
      revision: observed!.revision,
    });
  });
  expect(cleared).toBe(true);
  const accepted = await other.evaluate(
    ({ attempt, id, expiresAt }) =>
      window.sessionFixture.accept(attempt, { id, expiresAt }),
    { attempt, id: second, expiresAt },
  );
  expect(accepted?.id).toBe(second);
  expect(await page.evaluate(() => window.sessionFixture.read())).toMatchObject(
    {
      session: { id: second, expiresAt },
      attempt: null,
    },
  );
});

test("release returns the exact revision it committed and clear delegates to it", async ({
  page,
}) => {
  await page.evaluate(
    async ({ id, expiresAt }) => {
      const attempt = await window.sessionFixture.begin(id);
      await window.sessionFixture.accept(attempt, { id, expiresAt });
    },
    { id: first, expiresAt },
  );
  const original = await page.evaluate(() => window.sessionFixture.read());
  const released = await page.evaluate(
    (selection) => window.sessionFixture.release(selection),
    { id: first, revision: original!.revision },
  );
  expect(released).not.toBeNull();
  expect(released).not.toBe(original!.revision);
  const cleared = await page.evaluate(() => window.sessionFixture.read());
  expect(cleared).toEqual({
    version: 1,
    revision: released,
    session: null,
    attempt: null,
  });
  expect(
    await page.evaluate(
      (selection) => window.sessionFixture.clear(selection),
      { id: first, revision: released! },
    ),
  ).toBe(false);
});

test("release preserves an active login attempt and its revision", async ({
  page,
}) => {
  await page.evaluate(
    async ({ id, expiresAt }) => {
      const attempt = await window.sessionFixture.begin(id);
      await window.sessionFixture.accept(attempt, { id, expiresAt });
    },
    { id: first, expiresAt },
  );
  const attempt = await page.evaluate(
    (id) => window.sessionFixture.begin(id),
    second,
  );
  const observed = await page.evaluate(() => window.sessionFixture.read());
  expect(observed).toMatchObject({
    session: { id: first, expiresAt },
    attempt: second,
    revision: attempt.revision,
  });
  const released = await page.evaluate(
    (selection) => window.sessionFixture.release(selection),
    { id: first, revision: observed!.revision },
  );
  expect(released).toBe(observed!.revision);
  expect(await page.evaluate(() => window.sessionFixture.read())).toEqual({
    version: 1,
    revision: released,
    session: null,
    attempt: second,
  });
  const accepted = await page.evaluate(
    ({ attempt, id, expiresAt }) =>
      window.sessionFixture.accept(attempt, { id, expiresAt }),
    { attempt, id: second, expiresAt },
  );
  expect(accepted?.id).toBe(second);
});

test("release refuses a stale revision without changing the selection", async ({
  page,
}) => {
  await page.evaluate(
    async ({ id, expiresAt }) => {
      const attempt = await window.sessionFixture.begin(id);
      await window.sessionFixture.accept(attempt, { id, expiresAt });
    },
    { id: first, expiresAt },
  );
  const original = await page.evaluate(() => window.sessionFixture.read());
  const released = await page.evaluate(
    (selection) => window.sessionFixture.release(selection),
    { id: first, revision: second },
  );
  expect(released).toBeNull();
  expect(await page.evaluate(() => window.sessionFixture.read())).toEqual(
    original,
  );
});

test("an IndexedDB open refusal stays unavailable and the same instance recovers", async ({
  page,
}) => {
  const failure = await page.evaluate(async () => {
    const open = IDBFactory.prototype.open;
    try {
      IDBFactory.prototype.open = () => {
        throw new DOMException("Fixture storage refusal", "SecurityError");
      };
      try {
        await window.sessionFixture.read();
        return null;
      } catch (error) {
        if (!(error instanceof Error)) throw error;
        return { name: error.name, kind: Reflect.get(error, "kind") };
      }
    } finally {
      IDBFactory.prototype.open = open;
    }
  });
  expect(failure).toEqual({
    name: "SessionStorageError",
    kind: "unavailable",
  });
  expect(await page.evaluate(() => window.sessionFixture.read())).toBeNull();
  const attempt = await page.evaluate(
    (id) => window.sessionFixture.begin(id),
    first,
  );
  expect(await page.evaluate(() => window.sessionFixture.read())).toEqual({
    version: 1,
    revision: attempt.revision,
    session: null,
    attempt: first,
  });
});

test("a refused IndexedDB put aborts the attempt and preserves the committed selection", async ({
  page,
}) => {
  await page.evaluate(
    async ({ id, expiresAt }) => {
      const attempt = await window.sessionFixture.begin(id);
      await window.sessionFixture.accept(attempt, { id, expiresAt });
    },
    { id: first, expiresAt },
  );
  const original = await page.evaluate(() => window.sessionFixture.read());
  expect(original?.session).toEqual({ id: first, expiresAt });
  const outcome = await page.evaluate(async (id) => {
    const put = IDBObjectStore.prototype.put;
    let writes = 0;
    let aborts = 0;
    let aborted: Promise<void> | undefined;
    try {
      IDBObjectStore.prototype.put = function () {
        writes++;
        aborted = new Promise((resolve) => {
          this.transaction.addEventListener(
            "abort",
            () => {
              aborts++;
              resolve();
            },
            { once: true },
          );
        });
        throw new DOMException("Fixture quota refusal", "QuotaExceededError");
      };
      try {
        await window.sessionFixture.begin(id);
        return { failure: null, writes, aborts };
      } catch (error) {
        if (!(error instanceof Error)) throw error;
        await aborted;
        return {
          failure: { name: error.name, kind: Reflect.get(error, "kind") },
          writes,
          aborts,
        };
      }
    } finally {
      IDBObjectStore.prototype.put = put;
    }
  }, second);
  expect(outcome).toEqual({
    failure: { name: "SessionStorageError", kind: "unavailable" },
    writes: 1,
    aborts: 1,
  });
  expect(await page.evaluate(() => window.sessionFixture.read())).toEqual(
    original,
  );
  const attempt = await page.evaluate(
    (id) => window.sessionFixture.begin(id),
    second,
  );
  expect(attempt.revision).not.toBe(original!.revision);
  expect(await page.evaluate(() => window.sessionFixture.read())).toEqual({
    version: 1,
    revision: attempt.revision,
    session: { id: first, expiresAt },
    attempt: second,
  });
});

test("damaged storage fails closed until the user explicitly starts authentication", async ({
  page,
}) => {
  const outcome = await page.evaluate(async () => {
    await window.sessionFixture.read();
    const database = await new Promise<IDBDatabase>((resolve, reject) => {
      const request = indexedDB.open("halo.session", 1);
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
    await new Promise<void>((resolve, reject) => {
      const transaction = database.transaction("selection", "readwrite");
      transaction
        .objectStore("selection")
        .put({ version: 99, session: "invalid" }, "current");
      transaction.oncomplete = () => resolve();
      transaction.onabort = () => reject(transaction.error);
    });
    database.close();
    try {
      await window.sessionFixture.read();
      return "unexpectedly accepted";
    } catch {
      return "rejected";
    }
  });
  expect(outcome).toBe("rejected");
  await page.evaluate((id) => window.sessionFixture.begin(id), first);
  expect(await page.evaluate(() => window.sessionFixture.read())).toMatchObject(
    {
      version: 1,
      attempt: first,
      session: null,
    },
  );
});

test("cross-tab notifications carry no selection authority", async ({
  page,
  context,
}) => {
  await page.evaluate(() => {
    window.notifications = 0;
    window.sessionFixture.watch(() => window.notifications++);
  });
  const other = await context.newPage();
  await open(other);
  await other.evaluate((id) => {
    const channel = new BroadcastChannel("halo.session");
    channel.postMessage({ session: { id }, revision: "forged" });
    channel.close();
  }, second);
  await expect.poll(() => page.evaluate(() => window.notifications)).toBe(1);
  expect(await page.evaluate(() => window.sessionFixture.read())).toBeNull();
  await other.evaluate((id) => window.sessionFixture.begin(id), first);
  await expect.poll(() => page.evaluate(() => window.notifications)).toBe(2);
  expect(await page.evaluate(() => window.sessionFixture.read())).toMatchObject(
    { attempt: first },
  );
});

test("notification failure preserves the committed selection result", async ({
  page,
  context,
}) => {
  await page.evaluate(() => {
    BroadcastChannel.prototype.postMessage = () => {
      throw new DOMException("Fixture channel failure", "InvalidStateError");
    };
  });
  const attempt = await page.evaluate(
    (id) => window.sessionFixture.begin(id),
    first,
  );
  expect(await page.evaluate(() => window.sessionFixture.read())).toMatchObject(
    {
      attempt: first,
      revision: attempt.revision,
    },
  );
  const selection = await page.evaluate(
    ({ attempt, id, expiresAt }) =>
      window.sessionFixture.accept(attempt, { id, expiresAt }),
    { attempt, id: first, expiresAt },
  );
  expect(selection?.id).toBe(first);
  expect(await page.evaluate(() => window.sessionFixture.read())).toMatchObject(
    {
      session: { id: first, expiresAt },
      revision: selection!.revision,
    },
  );
  const other = await context.newPage();
  await open(other);
  expect(
    await other.evaluate(() => window.sessionFixture.read()),
  ).toMatchObject({
    session: { id: first, expiresAt },
    revision: selection!.revision,
    attempt: null,
  });
});
