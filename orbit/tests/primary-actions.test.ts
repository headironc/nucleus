import { expect, test } from "bun:test";
import { readFile } from "node:fs/promises";
import { runInNewContext } from "node:vm";
import { account, epoch, FakePlatform, ids, session } from "./fixture";
import { storageKey } from "../src/storage";
import { packageFile } from "./package-files";

const tick = () => new Promise(resolve => setTimeout(resolve, 0));

async function native(original = false, later = false) {
  const platform = new FakePlatform();
  platform.setStorageSync(storageKey, { version: 1, session: { ...session(later ? ids.later : ids.session), accountId: ids.account },
    ...(original ? { logouts: [{ operation: ids.registration, accountId: ids.account, sessionId: ids.session,
      scope: "current", createdAt: epoch, deadline: epoch + 600000 }] } : {}) });
  let application: any, definition: any, modal: any, now = epoch;
  const context = { wx: Object.assign(platform, { showModal(value: unknown) { modal = value; } }),
    App(value: unknown) { application = value; }, getApp() { return application; }, Page(value: unknown) { definition = value; },
    setTimeout() { return 1; }, clearTimeout() {}, Date: class extends Date { static now() { return now; } } };
  runInNewContext(await readFile(packageFile("app.js"), "utf8"), context); application.onLaunch();
  runInNewContext(await readFile(packageFile("pages/account/index.js"), "utf8"), context);
  const page = { ...definition, data: { ...definition.data }, setData(value: unknown) { Object.assign(this.data, value); } };
  const source = await readFile(packageFile("pages/account/index.wxml"), "utf8");
  const control = [...source.matchAll(/<button\b([^>]*)>([\s\S]*?)<\/button>/g)]
    .find(value => value[1].includes('bindtap="check"') && value[2].includes("检查登录状态"))!;
  function check() {
    const attributes = control[1];
    const evaluate = (expression: string) => runInNewContext(expression, page.data);
    return {
      shown: evaluate(attributes.match(/wx:if="\{\{(.*?)\}\}"/)![1]),
      role: evaluate(attributes.match(/class="\{\{(.*?)\}\}/)![1]),
      disabled: evaluate(attributes.match(/disabled="\{\{(.*?)\}\}"/)![1]),
      text: control[2].replace(/\{\{(.*?)\}\}/g, (_, expression) => String(evaluate(expression))),
    };
  }
  return { platform, application, page, source, check, modal: () => modal, time(value: number) { now = value; } };
}

test.each([403, 503])("the actual account %s read failure makes its only check action primary", async status => {
  const h = await native();
  try {
    h.page.onShow(); await h.platform.next(0);
    h.platform.reply(0, { code: status === 403 ? "permission_denied" : "dependency_unavailable" }, status); await tick();
    expect(h.page.data.phase).toBe("unavailable");
    expect(h.check()).toEqual({ shown: true, role: "primary", disabled: false, text: "重新检查" });
    expect(h.page.data.canRecover).toBe(false);
    expect(h.page.data.name).toBe("");
    h.page.check(); await h.platform.next(1);
    expect(h.platform.requests[1].method).toBe("GET");
    expect(h.platform.requests[1].url).toEndWith("/api/v1/accounts/current");
    h.platform.reply(1, { code: "ok", account: account() }); await tick();
    expect(h.page.data.phase).toBe("account");
    expect(h.check().shown).toBe(false);
  } finally { h.application.onHide(); }
});

test("local storage failure keeps its real check primary and reset secondary text", async () => {
  const h = await native();
  try {
    h.platform.storageReadFails = true; h.page.onShow(); await tick();
    expect(h.check()).toEqual({ shown: true, role: "primary", disabled: false, text: "重新检查" });
    expect(h.page.data.canReset).toBe(true);
    expect(h.source).toContain('class="text-button self-text-action" wx:if="{{canReset}}" bindtap="restartLogin"');
    expect(h.platform.requests).toHaveLength(0);
  } finally { h.application.onHide(); }
});

test("original logout recovery stays primary until expiry exposes the current check", async () => {
  const h = await native(true);
  try {
    h.page.onShow(); await tick();
    expect(h.page.data.canRecover).toBe(true);
    expect(h.check().shown).toBe(false);
    expect(h.source).toContain('class="primary self-button" wx:if="{{canRecover}}" bindtap="recover"');
    h.time(epoch + 600001); h.application.onShow();
    expect(h.check()).toEqual({ shown: true, role: "primary", disabled: false, text: "重新检查" });
    expect(h.page.data.canRecover).toBe(false);
    expect(h.platform.requests).toHaveLength(0);
  } finally { h.application.onHide(); }
});

test("previous logout completion preserves the later login and a primary return through check", async () => {
  const h = await native(true, true);
  try {
    h.page.onShow(); await h.platform.next(0); h.platform.reply(0, { code: "ok", account: account() }); await tick();
    expect(h.check().shown).toBe(false);
    h.page.recover(); await h.platform.next(1);
    h.platform.reply(1, { code: "ok", original: { scope: "current", initiatorId: ids.session }, count: 1,
      revocations: [{ id: ids.session, revokedAt: "2026-10-30T08:01:00Z" }] }); await tick();
    expect(h.check()).toEqual({ shown: true, role: "primary", disabled: false, text: "返回个人账户" });
    expect(h.platform.records().session?.id).toBe(ids.later);
  } finally { h.application.onHide(); }
});

test("fresh logout keeps its disabled danger button while busy and home as its completed primary", async () => {
  const h = await native();
  try {
    h.page.onShow(); await h.platform.next(0); h.platform.reply(0, { code: "ok", account: account() }); await tick();
    h.page.logoutCurrent(); h.modal().success({ confirm: true }); await h.platform.next(1);
    const danger = h.source.match(/<button\b([^>]*bindtap="logoutCurrent"[^>]*)>/)![1];
    expect(danger).toContain('class="secondary self-button self-danger"');
    expect(runInNewContext(danger.match(/disabled="\{\{(.*?)\}\}"/)![1], h.page.data)).toBe(true);
    h.platform.reply(1, { code: "ok", completion: { id: h.platform.requests[1].header["Operation-Id"], count: 1, revokedAt: "2026-10-30T08:01:00Z" } }); await tick();
    expect(h.check().shown).toBe(false);
    expect(h.page.data.homeAction).toBe(true);
    expect(h.source).toContain('class="home-link primary self-button" wx:if="{{homeAction}}"');
  } finally { h.application.onHide(); }
});

test("the template keeps check secondary beside an original-result or home primary", async () => {
  const h = await native();
  try {
    for (const flags of [{ canRecover: true, homeAction: false }, { canRecover: false, homeAction: true }]) {
      Object.assign(h.page.data, flags);
      expect(h.check().role).toBe("secondary");
    }
  } finally { h.application.onHide(); }
});
