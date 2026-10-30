import { afterEach, expect, spyOn, test } from "bun:test";
import { account, controllers, epoch, ids, proof, session, setup } from "./fixture";

type Kind = "authentication" | "registration";
type Harness = ReturnType<typeof setup>;
let restoreTimers: (() => void) | undefined;
afterEach(() => {
  for (const controller of controllers.splice(0)) controller.suspend();
  restoreTimers?.(); restoreTimers = undefined;
});

function pending(harness: Harness, kind: Kind) {
  const records = harness.platform.records();
  return kind === "authentication" ? records.authentication : records.registration?.pending;
}
async function begin(kind: Kind) {
  const h = setup(kind === "registration" ? { version: 1,
    registration: { proof: proof(), createdAt: epoch, deadline: epoch + 600000 } } : undefined);
  await h.controller.show();
  const work = kind === "authentication" ? h.controller.authorize("phone-code") : h.controller.register("张宁", "11010519491231002X");
  await h.platform.next(0);
  return { h, work };
}
function timers() {
  const scheduled: { callback(): void; delay: number; active: boolean }[] = [];
  const set = spyOn(globalThis, "setTimeout").mockImplementation(((callback: () => void, delay: number) => {
    const timer = { callback() { timer.active = false; callback(); }, delay, active: true };
    scheduled.push(timer); return scheduled.length;
  }) as any);
  const clear = spyOn(globalThis, "clearTimeout").mockImplementation(((id: number) => {
    const timer = scheduled[id - 1]; if (timer) timer.active = false;
  }) as any);
  restoreTimers = () => { set.mockRestore(); clear.mockRestore(); };
  return scheduled;
}
function failWrite(h: Harness, ordinal: number) {
  const save = h.platform.setStorageSync.bind(h.platform);
  let attempts = 0;
  const spy = spyOn(h.platform, "setStorageSync").mockImplementation((key, value) => {
    if (++attempts === ordinal) throw new Error("Injected preflight storage failure");
    save(key, value);
  });
  return { spy, attempts: () => attempts };
}

for (const kind of ["authentication", "registration"] as const) {
  test(`${kind}: second preflight write failure reports zero HTTP and not sent`, async () => {
    const h = setup(kind === "registration" ? { version: 1,
      registration: { proof: proof(), createdAt: epoch, deadline: epoch + 600000 } } : undefined);
    await h.controller.show();
    const fault = failWrite(h, 2);
    try {
      await (kind === "authentication" ? h.controller.authorize("phone-code") : h.controller.register("张宁", "11010519491231002X"));
      expect(fault.attempts()).toBe(2);
      expect(h.platform.requests).toHaveLength(0);
      expect(h.controller.snapshot().message).toBe("设备暂时无法保存办理信息，本次操作尚未发送。");
      expect(pending(h, kind)?.uncertain).toBe(false);
    } finally { fault.spy.mockRestore(); }
  });

  for (const unknown of [false, true]) {
    test(`${kind}: recovery preflight failure preserves prior ${unknown ? "unknown" : "known"} outcome`, async () => {
      const { h, work } = await begin(kind);
      if (unknown) h.platform.disconnect(0);
      else h.platform.reply(0, { code: "rate_limited" }, 429, { "Retry-After": "5" });
      await work;
      const before = pending(h, kind)!;
      h.time(epoch + 5000);
      const fault = failWrite(h, 1);
      try {
        await h.controller.recover();
        expect(fault.attempts()).toBe(1);
        expect(h.platform.requests).toHaveLength(1);
        expect(pending(h, kind)).toEqual(before);
        expect(h.controller.snapshot().message).toBe(unknown ?
          "设备暂时无法保存办理结果，请重新检查。此前办理结果仍待确认。" :
          "设备暂时无法保存办理信息，本次操作尚未发送。");
      } finally { fault.spy.mockRestore(); }
    });
  }

  for (const exit of ["hide", "crash"] as const) {
    test(`${kind}: rate limit, recovery, ${exit} and reopening preserve an unknown attempt`, async () => {
      const { h, work } = await begin(kind);
      h.platform.reply(0, { code: "rate_limited" }, 429, { "Retry-After": "5" }); await work;
      const original = pending(h, kind)!;
      expect(original.uncertain).toBe(false);
      h.time(epoch + 5000);
      const recovery = h.controller.recover(); await h.platform.next(1);
      const attempted = pending(h, kind)!;
      expect(attempted.uncertain).toBe(true);
      expect(attempted.retryAt).toBeGreaterThanOrEqual(epoch + 45000);
      if (exit === "hide") h.controller.hide();
      // Capture durable bytes before any lifecycle callback for the crash branch.
      const reboot = setup(h.platform.records());
      h.controller.suspend(); await recovery;
      reboot.time(epoch + 45000); await reboot.controller.show();
      const repeated = reboot.controller.recover(); await reboot.platform.next(0);
      expect(reboot.platform.requests[0].data).toBe(h.platform.requests[0].data);
      expect(reboot.platform.requests[0].header["Operation-Id"]).toBe(original.operation);
      reboot.platform.reply(0, { code: kind === "authentication" ? "permission_denied" : "invalid_input" }, kind === "authentication" ? 403 : 400);
      await repeated;
      expect(pending(reboot, kind)).toMatchObject({ operation: original.operation, uncertain: true, stopped: true, input: original.input });
      expect(reboot.controller.snapshot()).toMatchObject({ canLogin: false, canRecover: false });
    });
  }

  test(`${kind}: cancellation and reopening cannot bypass the durable retry delay`, async () => {
    const { h, work } = await begin(kind);
    h.time(epoch + 1000); h.controller.hide(); await work;
    const interrupted = pending(h, kind)!;
    expect(interrupted.retryAt).toBeGreaterThanOrEqual(epoch + 6000);
    const reboot = setup(h.platform.records());
    reboot.time(epoch + 5999); await reboot.controller.show(); await reboot.controller.recover();
    expect(reboot.platform.requests).toHaveLength(0);
    expect(reboot.controller.snapshot().canRecover).toBe(false);
    reboot.time(interrupted.retryAt);
    await reboot.controller.show();
    const recovery = reboot.controller.recover(); await reboot.platform.next(0);
    expect(reboot.platform.requests[0].header["Operation-Id"]).toBe(interrupted.operation);
    reboot.platform.disconnect(0); await recovery;
  });

  test(`${kind}: failed cancellation storage preserves the preflight guard without throwing`, async () => {
    const { h, work } = await begin(kind);
    h.time(epoch + 34000); h.platform.storageFails = true;
    expect(() => h.controller.hide()).not.toThrow(); await work;
    expect(h.controller.snapshot().name).toBe("");
    h.platform.storageFails = false;
    const durable = pending(h, kind)!;
    expect(durable).toMatchObject({ uncertain: true });
    expect(durable.retryAt).toBeGreaterThanOrEqual(epoch + 39000);
    const reboot = setup(h.platform.records());
    reboot.time(epoch + 38999); await reboot.controller.show(); await reboot.controller.recover();
    expect(reboot.platform.requests).toHaveLength(0);
    await h.controller.show();
    expect(pending(h, kind)!.retryAt).toBeGreaterThanOrEqual(durable.retryAt);
  });

  test(`${kind}: completed requests are not aborted or delayed by unrelated navigation`, async () => {
    const { h, work } = await begin(kind);
    h.platform.reply(0, { code: "rate_limited" }, 429, { "Retry-After": "5" }); await work;
    const before = pending(h, kind)!;
    const writes = h.platform.writes;
    h.time(epoch + 1000); h.controller.hide();
    expect(h.platform.aborted).toEqual([]);
    expect(h.platform.writes).toBe(writes);
    expect(pending(h, kind)).toEqual(before);
  });

  for (const visible of [true, false]) {
    for (const failure of ["read", "write"] as const) {
      test(`${kind}: foreground cleanup retries a transient ${failure} failure with account page ${visible ? "visible" : "hidden"}`, async () => {
        const scheduled = timers();
        const { h, work } = await begin(kind);
        h.platform.disconnect(0); await work;
        if (!visible) h.controller.hide();
        h.time(epoch + 600000);
        if (failure === "read") h.platform.storageReadFails = true;
        else h.platform.storageWriteFails = true;
        scheduled.at(-1)!.callback();
        expect(scheduled.at(-1)).toMatchObject({ delay: 5000, active: true });
        expect(JSON.stringify(h.platform.saved)).toContain(kind === "authentication" ? "phone-code" : "11010519491231002X");
        expect(h.controller.snapshot().name).toBe("");
        h.platform.storageReadFails = false; h.platform.storageWriteFails = false;
        h.time(epoch + 605000);
        const retries = scheduled.length; scheduled.at(-1)!.callback();
        expect(pending(h, kind)).toBeUndefined();
        expect(JSON.stringify(h.platform.saved)).not.toContain(kind === "authentication" ? "phone-code" : "11010519491231002X");
        expect(scheduled).toHaveLength(retries);
        expect(scheduled.filter(value => value.active)).toHaveLength(0);
        expect(h.platform.requests).toHaveLength(1);
      });
    }
  }
}

test("background stops a pending local cleanup retry and resume cleans without HTTP", async () => {
  const scheduled = timers();
  const { h, work } = await begin("registration"); h.platform.disconnect(0); await work;
  h.time(epoch + 600000); h.platform.storageReadFails = true; scheduled.at(-1)!.callback();
  expect(scheduled.filter(value => value.active)).toHaveLength(1);
  h.controller.suspend();
  expect(scheduled.filter(value => value.active)).toHaveLength(0);
  h.platform.storageReadFails = false; h.time(epoch + 610000); h.controller.resume();
  expect(pending(h, "registration")).toBeUndefined();
  expect(h.platform.requests).toHaveLength(1);
});

test("GET cancellation leaves no authentication backoff", async () => {
  const h = setup({ version: 1, session: { ...session(), accountId: ids.account } });
  const show = h.controller.show(); await h.platform.next(0); const writes = h.platform.writes;
  h.controller.hide(); await show;
  expect(h.platform.writes).toBe(writes);
  expect(h.platform.records()).toEqual({ version: 1, session: { ...session(), accountId: ids.account } });
});

test("logout recovery retaining a later session offers only a real current-login check", async () => {
  const h = setup({ version: 1, session: { ...session(ids.later), accountId: ids.account },
    logouts: [{ operation: ids.registration, accountId: ids.account, sessionId: ids.session, scope: "all", createdAt: epoch, deadline: epoch + 600000 }] });
  const show = h.controller.show(); await h.platform.next(0); h.platform.reply(0, { code: "ok", account: account() }); await show;
  const recovery = h.controller.recover(); await h.platform.next(1);
  h.platform.reply(1, { code: "ok", original: { scope: "all", initiatorId: ids.session }, count: 1,
    revocations: [{ id: ids.session, revokedAt: "2026-10-30T08:01:00Z" }] }); await recovery;
  expect(h.controller.snapshot()).toMatchObject({ phase: "complete", canLogin: false, canCheck: true });
  expect(h.platform.records().session?.id).toBe(ids.later);
  const check = h.controller.check(); await h.platform.next(2);
  h.platform.reply(2, { code: "ok", account: account() }); await check;
  expect(h.controller.snapshot()).toMatchObject({ phase: "account", name: "张宁", canLogin: false });
  expect(h.platform.requests.every(value => value.method === "GET")).toBe(true);
});
