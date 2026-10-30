import { afterEach, expect, spyOn, test } from "bun:test";
import { account, controllers, epoch, ids, login, session, setup } from "./fixture";
let restore: (() => void) | undefined;
afterEach(() => { for (const value of controllers.splice(0)) value.suspend(); restore?.(); restore = undefined; });

function timers() {
  const pending: { callback: () => void; delay: number }[] = [];
  const spy = spyOn(globalThis, "setTimeout").mockImplementation(((callback: () => void, delay: number) => {
    pending.push({ callback, delay }); return 1;
  }) as any);
  restore = () => spy.mockRestore();
  return pending;
}

test("Retry-After activates recovery in place, without automatically repeating the write", async () => {
  const scheduled = timers();
  const h = setup(); await h.controller.show();
  const work = h.controller.authorize("phone-code"); await h.platform.next(0);
  h.platform.reply(0, { code: "rate_limited" }, 429, { "Retry-After": "12" }); await work;
  expect(h.controller.snapshot().canRecover).toBe(false);
  expect(scheduled.at(-1)?.delay).toBe(12000);
  const shown = Promise.withResolvers<void>();
  h.controller.subscribe(view => { if (view.canRecover) shown.resolve(); });
  h.time(epoch + 12000); scheduled.at(-1)!.callback(); await shown.promise;
  expect(h.platform.requests).toHaveLength(1);
  expect(h.controller.snapshot().canRecover).toBe(true);
});

test("a 30-day session schedules a bounded wakeup and never a clamped busy loop", async () => {
  const scheduled = timers();
  const h = setup(); await h.controller.show(); await login(h);
  expect(scheduled.at(-1)?.delay).toBe(2147483647);
  expect(scheduled.every(value => value.delay > 1000 && value.delay <= 2147483647)).toBe(true);
  expect(h.platform.requests).toHaveLength(2);
});

test("public-page foreground expiry removes R2 secrets locally without any HTTP", async () => {
  const scheduled = timers();
  const h = setup(); await h.controller.show();
  const work = h.controller.authorize("phone-code"); await h.platform.next(0);
  h.platform.disconnect(0); await work;
  h.controller.hide();
  h.time(epoch + 600000); scheduled.at(-1)!.callback();
  expect(h.platform.records().authentication).toBeUndefined();
  expect(h.platform.records().unresolved).toBe("authentication");
  expect(h.platform.requests).toHaveLength(1);
});

test("background stops timers and resume clears expired secrets without loading private data", async () => {
  const scheduled = timers();
  const h = setup(); await h.controller.show();
  const work = h.controller.authorize("phone-code"); await h.platform.next(0); h.platform.disconnect(0); await work;
  const count = scheduled.length;
  h.controller.suspend(); expect(scheduled).toHaveLength(count);
  h.time(epoch + 600000); h.controller.resume();
  expect(h.platform.records().authentication).toBeUndefined();
  expect(h.platform.requests).toHaveLength(1);
});

test("expired logout removes the old recovery action while preserving a valid selected account", async () => {
  const scheduled = timers();
  const h = setup({ version: 1, session: { ...session(ids.later), accountId: ids.account },
    logouts: [{ operation: ids.registration, accountId: ids.account, sessionId: ids.session, scope: "all", createdAt: epoch, deadline: epoch + 1000 }] });
  const show = h.controller.show(); await h.platform.next(0); h.platform.reply(0, { code: "ok", account: account() }); await show;
  expect(h.controller.snapshot().logoutPending).toBe(true);
  h.time(epoch + 1000); scheduled.at(-1)!.callback();
  expect(h.controller.snapshot()).toMatchObject({ phase: "account", canRecover: false, logoutPending: false, canLogin: false });
  expect(h.platform.records().expiredLogouts).toEqual([ids.account]);
  const recovery = h.controller.recover(); await h.platform.next(1);
  expect(h.platform.requests[1].url).toEndWith("/accounts/current");
  h.platform.reply(1, { code: "ok", account: account() }); await recovery;
  expect(h.controller.snapshot()).toMatchObject({ phase: "account", canLogin: false });
});
