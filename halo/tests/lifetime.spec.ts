import { test, expect } from "@playwright/test";
import { widget } from "./widget";

for (const sent of [false, true]) {
  test(`login rate-limit context survives refresh with prior uncertainty: ${sent}`, async ({
    page,
  }) => {
    await page.clock.install();
    await page.goto("/");
    await page.clock.pauseAt(new Date(Date.now() + 1000));
    const original = await page.evaluate(async (sent) => {
      const now = Date.now();
      const operation = crypto.randomUUID();
      const revision = crypto.randomUUID();
      const proof = {
        version: 1,
        kind: "login",
        attempt: { operation, revision },
        send: crypto.randomUUID(),
        input: {
          secret: "A".repeat(43),
          credential: {
            type: "message",
            id: crypto.randomUUID(),
            mobile: "13900000000",
            code: "000001",
          },
        },
        createdAt: now,
        expiresAt: now + (sent ? 30 : 5) * 60_000,
        retryAt: 0,
        sent,
      };
      sessionStorage.setItem("halo.r1", JSON.stringify(proof));
      await new Promise<void>((resolve, reject) => {
        const request = indexedDB.open("halo.session", 1);
        request.onupgradeneeded = () =>
          request.result.createObjectStore("selection");
        request.onerror = () => reject(request.error);
        request.onsuccess = () => {
          const database = request.result;
          const transaction = database.transaction("selection", "readwrite");
          transaction
            .objectStore("selection")
            .put(
              { version: 1, revision, session: null, attempt: operation },
              "current",
            );
          transaction.oncomplete = () => {
            database.close();
            resolve();
          };
          transaction.onabort = () => reject(transaction.error);
        };
      });
      return proof;
    }, sent);
    const writes: { id: string | undefined; input: unknown }[] = [];
    await page.route("**/api/v1/sessions", async (route) => {
      writes.push({
        id: route.request().headers()["operation-id"],
        input: route.request().postDataJSON(),
      });
      await route.fulfill({
        status: 429,
        contentType: "application/json",
        headers: { "Retry-After": "60" },
        body: JSON.stringify({ code: "rate_limited" }),
      });
    });
    await page.goto("/authentication/login");
    await page
      .getByRole("button", {
        name: sent ? "查看登录结果" : "继续登录",
        exact: true,
      })
      .click();
    const title = sent ? "请稍后查看登录结果" : "请稍后继续验证";
    await expect(
      page.getByRole("heading", { name: title, exact: true }),
    ).toBeVisible();
    await page.reload();
    await expect(
      page.getByRole("heading", { name: title, exact: true }),
    ).toBeVisible();
    expect(writes).toHaveLength(1);
    await expect(
      page.getByRole("button", { name: /秒后可继续/ }),
    ).toBeDisabled();
    await page.clock.fastForward(60_000);
    await page
      .getByRole("button", {
        name: sent ? "查看登录结果" : "继续验证",
        exact: true,
      })
      .click();
    await expect.poll(() => writes.length).toBe(2);
    expect(writes).toEqual(
      Array(2).fill({ id: original.attempt.operation, input: original.input }),
    );
    await page.clock.fastForward((sent ? 30 : 5) * 60_000);
    await expect(
      page.getByRole("heading", { name: "登录状态待确认", exact: true }),
    ).toBeVisible();
    expect(writes).toHaveLength(2);
    expect(
      await page.evaluate(() => sessionStorage.getItem("halo.r1")),
    ).not.toContain("000001");
  });
}

test("a damaged receipt retries refused cleanup before opening the saved new code without POST", async ({
  page,
}) => {
  let writes = 0;
  page.on("request", (request) => {
    if (request.method() === "POST") writes++;
  });
  await page.addInitScript(() => {
    const remove = Storage.prototype.removeItem;
    Storage.prototype.removeItem = function (key) {
      if (
        this === sessionStorage &&
        key.startsWith("halo.r1.session.") &&
        sessionStorage.getItem("fixture.refuseReceiptCleanup")
      )
        throw new Error("fixture temporary removal refusal");
      remove.call(this, key);
    };
  });
  await page.goto("/");
  const key = await page.evaluate(() => {
    const now = Date.now();
    const key = "halo.r1.session." + crypto.randomUUID();
    sessionStorage.setItem(key, '{"version":1');
    sessionStorage.setItem("fixture.refuseReceiptCleanup", "true");
    sessionStorage.setItem("unrelated", "preserve");
    sessionStorage.setItem(
      "halo.login.message",
      JSON.stringify({
        version: 1,
        operation: crypto.randomUUID(),
        mobile: "13900000000",
        message: {
          id: crypto.randomUUID(),
          delivery: "sent",
          expiresAt: new Date(now + 60_000).toISOString(),
        },
        createdAt: now,
        expiresAt: now + 60_000,
      }),
    );
    return key;
  });
  await page.goto("/authentication/login");
  await expect(
    page.getByRole("heading", { name: "暂时无法继续登录", exact: true }),
  ).toBeVisible();
  expect(await page.evaluate((key) => sessionStorage.getItem(key), key)).toBe(
    '{"version":1',
  );
  await page.evaluate(() => {
    sessionStorage.removeItem("fixture.refuseReceiptCleanup");
    window.dispatchEvent(new Event("pageshow"));
  });
  await expect
    .poll(() => page.evaluate((key) => sessionStorage.getItem(key), key))
    .toBeNull();
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "请输入验证码", exact: true }),
  ).toBeVisible();
  expect(await page.evaluate(() => sessionStorage.getItem("unrelated"))).toBe(
    "preserve",
  );
  expect(writes).toBe(0);
});

for (const kind of ["login", "registration", "editing"] as const) {
  test(`${kind} material expires after full-document navigation home without business requests`, async ({
    page,
  }) => {
    let writes = 0;
    page.on("request", (request) => {
      if (new URL(request.url()).pathname.startsWith("/api/")) writes++;
    });
    const now = Date.now();
    await page.clock.install({ time: new Date(now) });
    await page.clock.pauseAt(new Date(now + 1000));
    await page.goto("/");
    await page.evaluate(async (kind) => {
      const now = Date.now();
      const operation = crypto.randomUUID();
      const revision = crypto.randomUUID();
      const send = crypto.randomUUID();
      const id = crypto.randomUUID();
      const expiresAt = now + 30_000;
      sessionStorage.setItem("unrelated", "preserve");
      sessionStorage.setItem(
        "halo.login.message",
        JSON.stringify({
          version: 1,
          operation: send,
          mobile: "13900000000",
          message: {
            id,
            delivery: "sent",
            expiresAt: new Date(expiresAt).toISOString(),
          },
          createdAt: now,
          expiresAt,
        }),
      );
      if (kind !== "editing") {
        sessionStorage.setItem(
          "halo.r1",
          JSON.stringify(
            kind === "login"
              ? {
                  version: 1,
                  kind,
                  attempt: { operation, revision },
                  send,
                  input: {
                    secret: "A".repeat(43),
                    credential: {
                      type: "message",
                      id,
                      mobile: "13900000000",
                      code: "000001",
                    },
                  },
                  createdAt: now,
                  expiresAt,
                  retryAt: 0,
                  sent: true,
                }
              : {
                  version: 1,
                  kind,
                  attempt: { operation, revision },
                  registration: {
                    id,
                    token: btoa(
                      String.fromCharCode(...new Uint8Array(32).fill(1)),
                    ).replace(/=+$/, ""),
                    expiresAt: new Date(expiresAt).toISOString(),
                  },
                  createdAt: now,
                  expiresAt,
                },
          ),
        );
      }
      await new Promise<void>((resolve, reject) => {
        const request = indexedDB.open("halo.session", 1);
        request.onupgradeneeded = () =>
          request.result.createObjectStore("selection");
        request.onerror = () => reject(request.error);
        request.onsuccess = () => {
          const database = request.result;
          const tx = database.transaction("selection", "readwrite");
          tx.objectStore("selection").put(
            { version: 1, session: null, attempt: operation, revision },
            "current",
          );
          tx.oncomplete = () => {
            database.close();
            resolve();
          };
        };
      });
    }, kind);
    await page.goto("/authentication/login");
    const heading =
      kind === "login"
        ? "登录结果待确认"
        : kind === "registration"
          ? "填写注册资料"
          : "请输入验证码";
    await expect(
      page.getByRole("heading", { name: heading, exact: true }),
    ).toBeVisible();
    await page.evaluate(() =>
      Reflect.set(window, "previousLoginDocument", true),
    );
    await page.getByRole("link", { name: "司籍处首页" }).click();
    expect(
      await page.evaluate(() => Reflect.has(window, "previousLoginDocument")),
    ).toBe(false);
    await page.clock.fastForward(30_000);
    await expect
      .poll(() =>
        page.evaluate(() => sessionStorage.getItem("halo.login.message")),
      )
      .toBeNull();
    if (kind !== "editing")
      expect(await page.evaluate(() => sessionStorage.getItem("halo.r1"))).toBe(
        '{"version":1,"kind":"closed","reason":"expired"}',
      );
    expect(await page.evaluate(() => sessionStorage.getItem("unrelated"))).toBe(
      "preserve",
    );
    await page.reload();
    expect(writes).toBe(0);
  });
}

for (const boundary of ["expiry", "clock rollback", "damage"] as const) {
  for (const failure of ["throw", "noop", "readback"] as const) {
    test(`an orphaned SMS handoff survives placeholder ${failure} at ${boundary} without reopening C1`, async ({
      page,
    }) => {
      let writes = 0;
      page.on("request", (request) => {
        if (request.method() === "POST") writes++;
      });
      await page.addInitScript(() => {
        const set = Storage.prototype.setItem;
        const get = Storage.prototype.getItem;
        Storage.prototype.getItem = function (key) {
          if (this === sessionStorage && key === "halo.login.message")
            Reflect.set(window, "fixtureMaterialRead", true);
          if (
            this === sessionStorage &&
            key === "halo.c1" &&
            get.call(this, "fixture.refusePlaceholder") === "readback" &&
            get.call(this, key) !== null
          ) {
            throw new Error("fixture placeholder readback refusal");
          }
          return get.call(this, key);
        };
        Storage.prototype.setItem = function (key, bytes) {
          if (
            this === sessionStorage &&
            key === "halo.c1" &&
            sessionStorage.getItem("fixture.refusePlaceholder") &&
            JSON.parse(bytes).closed === true
          ) {
            const failure = get.call(
              sessionStorage,
              "fixture.refusePlaceholder",
            );
            set.call(
              sessionStorage,
              "fixture.placeholderAttempts",
              String(
                Number(
                  get.call(sessionStorage, "fixture.placeholderAttempts"),
                ) + 1,
              ),
            );
            if (failure === "throw")
              throw new Error("fixture placeholder refusal");
            if (failure === "noop") return;
          }
          set.call(this, key, bytes);
        };
      });
      const now = Date.now();
      await page.clock.install({ time: new Date(now) });
      await page.clock.pauseAt(new Date(now + 1000));
      await page.goto("/");
      await page.evaluate(() => {
        const now = Date.now();
        sessionStorage.setItem("unrelated", "preserve");
        sessionStorage.setItem(
          "halo.login.message",
          JSON.stringify({
            version: 1,
            operation: crypto.randomUUID(),
            mobile: "13900000000",
            message: {
              id: crypto.randomUUID(),
              delivery: "sent",
              expiresAt: new Date(now + 30_000).toISOString(),
            },
            createdAt: now,
            expiresAt: now + 30_000,
          }),
        );
      });
      await page.goto("/authentication/login");
      await expect(
        page.getByRole("heading", { name: "请输入验证码" }),
      ).toBeVisible();
      await page.getByRole("link", { name: "司籍处首页" }).click();
      await expect
        .poll(() =>
          page.evaluate(() => Reflect.get(window, "fixtureMaterialRead")),
        )
        .toBe(true);
      await page.evaluate(
        (failure) =>
          sessionStorage.setItem("fixture.refusePlaceholder", failure),
        failure,
      );
      if (boundary === "expiry") await page.clock.fastForward(31_000);
      else {
        if (boundary === "clock rollback")
          await page.clock.setSystemTime(new Date(now - 60_000));
        else
          await page.evaluate(() =>
            sessionStorage.setItem("halo.login.message", "truncated"),
          );
        await page.evaluate(() => window.dispatchEvent(new Event("focus")));
      }
      expect(
        await page.evaluate(() =>
          Number(sessionStorage.getItem("fixture.placeholderAttempts")),
        ),
      ).toBeGreaterThan(0);
      expect(
        await page.evaluate(() => sessionStorage.getItem("halo.login.message")),
      ).not.toBeNull();
      if (failure !== "readback")
        expect(
          await page.evaluate(() => sessionStorage.getItem("halo.c1")),
        ).toBeNull();
      await page.evaluate(() => {
        sessionStorage.removeItem("fixture.refusePlaceholder");
        window.dispatchEvent(new Event("pageshow"));
      });
      await expect
        .poll(() =>
          page.evaluate(() => sessionStorage.getItem("halo.login.message")),
        )
        .toBeNull();
      expect(
        await page.evaluate(() =>
          JSON.parse(sessionStorage.getItem("halo.c1")!),
        ),
      ).toEqual({
        version: 1,
        closed: true,
      });
      await page.addInitScript(() => {
        const random = crypto.randomUUID.bind(crypto);
        Reflect.set(window, "fixtureNewOperations", 0);
        crypto.randomUUID = () => {
          Reflect.set(
            window,
            "fixtureNewOperations",
            Reflect.get(window, "fixtureNewOperations") + 1,
          );
          return random();
        };
      });
      await page.goto("/authentication/codes");
      await expect(
        page.getByRole("heading", {
          name: "暂时无法查看本次发送结果",
          exact: true,
        }),
      ).toBeVisible();
      await expect(
        page.getByLabel("手机号", { exact: true }),
      ).not.toBeVisible();
      await expect(
        page.getByRole("link", { name: "输入短信验证码" }),
      ).toHaveCount(0);
      expect(
        await page.evaluate(() => Reflect.get(window, "fixtureNewOperations")),
      ).toBe(0);
      await page.reload();
      await expect(
        page.getByRole("heading", {
          name: "暂时无法查看本次发送结果",
          exact: true,
        }),
      ).toBeVisible();
      expect(
        await page.evaluate(() => Reflect.get(window, "fixtureNewOperations")),
      ).toBe(0);
      expect(
        await page.evaluate(() => sessionStorage.getItem("unrelated")),
      ).toBe("preserve");
      expect(writes).toBe(0);
    });
  }
}

test("registration cleanup survives Code expiry and temporary C1 removal refusal", async ({
  page,
}) => {
  let writes = 0;
  page.on("request", (request) => {
    if (new URL(request.url()).pathname.startsWith("/api/")) writes++;
  });
  await page.clock.install();
  await page.goto("/");
  await page.clock.pauseAt(new Date(Date.now() + 1000));
  await page.evaluate(() => {
    const now = Date.now(),
      send = crypto.randomUUID(),
      id = crypto.randomUUID();
    sessionStorage.setItem(
      "halo.c1",
      JSON.stringify({
        version: 1,
        operation: send,
        input: { mobile: "13900000000", token: "original-c1-proof" },
        createdAt: now,
        expiresAt: now + 30 * 60_000,
        retryAt: 0,
        attempted: true,
      }),
    );
    sessionStorage.setItem(
      "halo.login.message",
      JSON.stringify({
        version: 1,
        operation: send,
        mobile: "13900000000",
        message: {
          id,
          delivery: "sent",
          expiresAt: new Date(now + 5000).toISOString(),
        },
        createdAt: now,
        expiresAt: now + 5000,
      }),
    );
    sessionStorage.setItem(
      "halo.r1",
      JSON.stringify({
        version: 1,
        kind: "registration",
        attempt: {
          operation: crypto.randomUUID(),
          revision: crypto.randomUUID(),
        },
        registration: {
          id,
          token: btoa(
            String.fromCharCode(...new Uint8Array(32).fill(1)),
          ).replace(/=+$/, ""),
          expiresAt: new Date(now + 30_000).toISOString(),
        },
        createdAt: now,
        expiresAt: now + 30_000,
        cleanup: { message: id, send },
      }),
    );
    const remove = Storage.prototype.removeItem;
    Storage.prototype.removeItem = function (key) {
      if (this === sessionStorage && key === "halo.c1")
        throw new Error("temporarily denied");
      remove.call(this, key);
    };
    Reflect.set(window, "restoreCleanup", () => {
      Storage.prototype.removeItem = remove;
    });
    window.dispatchEvent(new Event("focus"));
  });
  await page.clock.fastForward(5000);
  expect(
    await page.evaluate(() => sessionStorage.getItem("halo.login.message")),
  ).toBeNull();
  await page.clock.fastForward(25_000);
  const pending = await page.evaluate(() =>
    JSON.parse(sessionStorage.getItem("halo.r1")!),
  );
  expect(pending).toMatchObject({
    kind: "closed",
    reason: "expired",
    cleanup: { send: expect.any(String), message: expect.any(String) },
  });
  expect(
    await page.evaluate(() => sessionStorage.getItem("halo.c1")),
  ).toContain("original-c1-proof");
  await page.evaluate(() => {
    Reflect.get(window, "restoreCleanup")();
    window.dispatchEvent(new Event("pageshow"));
  });
  expect(
    await page.evaluate(() => sessionStorage.getItem("halo.c1")),
  ).toBeNull();
  expect(await page.evaluate(() => sessionStorage.getItem("halo.r1"))).toBe(
    '{"version":1,"kind":"closed","reason":"expired"}',
  );
  expect(writes).toBe(0);
});

test("C1 redirects an unresolved Session receipt to local login recovery", async ({
  page,
}) => {
  let writes = 0;
  page.on("request", (request) => {
    if (new URL(request.url()).pathname.startsWith("/api/")) writes++;
  });
  await page.goto("/");
  const completed = await page.evaluate(() => {
    const value = {
      version: 1,
      kind: "session",
      attempt: {
        operation: crypto.randomUUID(),
        revision: crypto.randomUUID(),
      },
      session: {
        id: crypto.randomUUID(),
        expiresAt: new Date(Date.now() + 86_400_000).toISOString(),
      },
    };
    sessionStorage.setItem("halo.r1", JSON.stringify(value));
    return value;
  });
  await page.goto("/authentication/codes");
  await expect(page).toHaveURL(/\/authentication\/login$/);
  await expect(
    page.getByRole("heading", { name: "登录状态有变化", exact: true }),
  ).toBeVisible();
  await page.reload();
  expect(
    await page.evaluate(() => JSON.parse(sessionStorage.getItem("halo.r1")!)),
  ).toEqual(completed);
  expect(writes).toBe(0);
});

test("a determined R1 verification refusal permits explicit new C1 instead of an old-code loop", async ({
  page,
}) => {
  let writes = 0;
  page.on("request", (request) => {
    if (request.method() === "POST") writes++;
  });
  await page.route(
    "https://challenges.cloudflare.com/turnstile/v0/api.js?render=explicit",
    (route) => route.fulfill({ contentType: "text/javascript", body: widget }),
  );
  await page.goto("/");
  await page.evaluate(() => {
    const now = Date.now(),
      id = crypto.randomUUID();
    sessionStorage.setItem(
      "halo.r1",
      JSON.stringify({
        version: 1,
        kind: "closed",
        reason: "verification_required",
      }),
    );
    sessionStorage.setItem(
      "halo.login.message",
      JSON.stringify({
        version: 1,
        operation: crypto.randomUUID(),
        mobile: "13900000000",
        message: {
          id,
          delivery: "sent",
          expiresAt: new Date(now + 30_000).toISOString(),
        },
        createdAt: now,
        expiresAt: now + 30_000,
      }),
    );
  });
  await page.goto("/authentication/login");
  await page.getByRole("link", { name: "重新获取验证码", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "申请短信验证码", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "完成安全验证（测试替身）" }),
  ).toBeVisible();
  await expect(page.getByRole("link", { name: "输入短信验证码" })).toHaveCount(
    0,
  );
  expect(writes).toBe(0);
});
