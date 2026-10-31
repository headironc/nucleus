import { afterEach, expect, test } from "bun:test";
import { readFile } from "node:fs/promises";
import { runInNewContext } from "node:vm";
import { account, epoch, FakePlatform, ids, session } from "./fixture";
import { storageKey } from "../src/storage";
import { packageFile } from "./package-files";

const applications: { onHide(): void }[] = [];
afterEach(() => { for (const app of applications.splice(0)) app.onHide(); });
const tick = () => new Promise(resolve => setTimeout(resolve, 0));
type NativePage = { data: Record<string, any>; onShow(): void; onHide(): void; recover(): void;
  authorize(event: { detail: { code: string } }): void; logoutCurrent(): void; logoutAll(): void; check(): void };

async function native(scope: "current" | "all", later = false, readFailure?: { path: "current" | "list"; status: 401 | 403 | 503 }, freshSuccess = false) {
  const platform = new FakePlatform();
  platform.setStorageSync(storageKey, { version: 1, session: { ...session(later ? ids.later : ids.session), accountId: ids.account },
    ...(later ? { logouts: [{ operation: ids.registration, accountId: ids.account, sessionId: ids.session, scope, createdAt: epoch, deadline: epoch + 600000 }] } : {}) });
  let application: any, definition: any, modal: any, now = epoch;
  const context = { wx: Object.assign(platform, { showModal(value: unknown) { modal = value; } }),
    App(value: unknown) { application = value; }, getApp() { return application; }, Page(value: unknown) { definition = value; },
    setTimeout() { return 1; }, clearTimeout() {}, Date: class extends Date { static now() { return now; } }, ArrayBuffer, Uint8Array };
  runInNewContext(await readFile(packageFile("app.js"), "utf8"), context);
  application.onLaunch(); applications.push(application);
  async function load(name: "account" | "sessions") {
    runInNewContext(await readFile(packageFile(`pages/${name}/index.js`), "utf8"), context);
    const page = { ...definition, data: { ...definition.data }, setData(value: unknown) { Object.assign(this.data, value); } } as NativePage;
    const source = await readFile(packageFile(`pages/${name}/index.wxml`), "utf8");
    return { page, source };
  }
  const showing = await load(scope === "current" ? "account" : "sessions");
  const failure = { code: readFailure?.status === 401 ? "authentication_invalid" : readFailure?.status === 403 ? "permission_denied" : "dependency_unavailable" };
  showing.page.onShow(); await platform.next(0);
  platform.reply(0, readFailure?.path === "current" ? failure : { code: "ok", account: account() }, readFailure?.path === "current" ? readFailure.status : 200);
  if (scope === "all" && readFailure?.path !== "current") {
    await platform.next(1); platform.reply(1, readFailure?.path === "list" ? failure : { code: "ok", sessions: [
      { id: later ? ids.later : ids.session, source: "mini", createdAt: "2026-10-30T08:00:00Z", current: true },
    ] }, readFailure?.path === "list" ? readFailure.status : 200);
  }
  await tick();
  const index = platform.requests.length;
  if (!later) {
    if (scope === "current") showing.page.logoutCurrent(); else showing.page.logoutAll();
    modal.success({ confirm: true }); await platform.next(index);
  }
  const original = platform.records().logouts![0];
  if (!later) {
    if (freshSuccess) platform.reply(index, { code: "ok", completion: { id: platform.requests[index].header["Operation-Id"], count: 1, revokedAt: "2026-10-30T08:01:00Z" } });
    else platform.disconnect(index);
    await tick();
  }
  return { ...showing, platform, application, original, load, setNow(value: number) { now = value; } };
}

test.each(["current", "all"] as const)("native fresh %s completion states its actual scope and returns home", async scope => {
  const h = await native(scope, false, undefined, true);
  expect(h.page.data.heading).toBe(scope === "current" ? "已退出登录" : "已退出全部登录");
  expect(h.page.data.message).toBe("");
  expect(h.page.data.homeAction).toBe(true);
  const home = [...h.source.matchAll(/<navigator\b([^>]*)>返回首页<\/navigator>/g)]
    .find(match => match[1].includes('wx:if="{{homeAction}}"'))!;
  expect(runInNewContext(home[1].match(/wx:if="\{\{(.*?)\}\}"/)![1], h.page.data)).toBe(true);
  expect(home[1]).toContain('url="/pages/home/index"');
  expect(home[1]).toContain('open-type="reLaunch"');
  expect(h.platform.records().session).toBeUndefined();
});

// These two actual WXML controls own the recovery choice; inspect their real bindings and conditions.
function actions(source: string, page: NativePage) {
  const buttons = [...source.matchAll(/<button\b([^>]*)>([\s\S]*?)<\/button>/g)].flatMap(match => {
    const binding = match[1].match(/bindtap="([^"]+)"/)?.[1];
    if (binding !== "recover" && binding !== "refresh" && binding !== "check") return [];
    if (binding === "check" && match[2] === "刷新个人资料") {
      const panel = source.split("<!-- snapshot:account:start -->")[1].split("<!-- snapshot:account:end -->")[0];
      const parent = panel.match(/wx:if="\{\{(.*?)\}\}"/)![1];
      if (!runInNewContext(parent, page.data)) return [];
    }
    const condition = match[1].match(/wx:if="\{\{(.*?)\}\}"/)?.[1];
    if (condition && !runInNewContext(condition, page.data)) return [];
    const text = match[2].replace(/\{\{(.*?)\}\}/g, (_, expression) => String(runInNewContext(expression, page.data)));
    return [{ binding, text }];
  });
  return buttons.concat([...source.matchAll(/<navigator\b([^>]*)>返回个人账户<\/navigator>/g)].flatMap(match => {
    if (!match[1].includes("primary")) return [];
    const condition = match[1].match(/wx:if="\{\{(.*?)\}\}"/)?.[1];
    if (!condition || !runInNewContext(condition, page.data)) return [];
    expect(match[1]).toContain('url="/pages/account/index"');
    expect(match[1]).toContain('open-type="redirect"');
    return [{ binding: "account", text: "返回个人账户" }];
  }));
}

test.each(["current", "all"] as const)("native %s unknown exposes only the original-result action across page reopening", async scope => {
  const h = await native(scope);
  expect(actions(h.source, h.page)).toEqual([{ binding: "recover", text: "查看退出结果" }]);
  const count = h.platform.requests.length;
  h.page.onHide(); h.page.onShow(); await tick();
  expect(h.platform.requests).toHaveLength(count);
  expect(actions(h.source, h.page)).toEqual([{ binding: "recover", text: "查看退出结果" }]);
  h.page.recover(); const request = await h.platform.next(count);
  expect(request.method).toBe("GET"); expect(request.url).toEndWith(`/revocations?operation_id=${h.original.operation}`);
  h.platform.reply(count, { code: "ok", original: null, count: 0, revocations: [] }); await tick();
  expect(actions(h.source, h.page)).toEqual([{ binding: "recover", text: "查看退出结果" }]);
});

test.each(["current", "all"] as const)("native %s offers current-login checking only when original recovery expires or cannot be read", async scope => {
  for (const reason of ["expired", "storage-unavailable"]) {
    const h = await native(scope); const count = h.platform.requests.length;
    if (reason === "expired") { h.setNow(epoch + 600001); h.application.onShow(); }
    else { h.platform.storageReadFails = true; h.page.recover(); await tick(); }
    expect(actions(h.source, h.page)).toEqual([{ binding: scope === "current" ? "check" : "refresh", text: "重新检查" }]);
    expect(h.platform.requests).toHaveLength(count);
    expect(h.page.data.canLogin).toBe(false);
    h.platform.storageReadFails = false;
  }
});

test.each(["current", "all"] as const)("native %s restores the current-state action when a later-session displayed page outlives original materials", async scope => {
  const h = await native(scope, true);
  expect(h.page.data.phase).toBe(scope === "current" ? "account" : "sessions");
  expect(actions(h.source, h.page)).toEqual([{ binding: "recover", text: "查看退出结果" }]);
  const count = h.platform.requests.length;
  h.setNow(epoch + 600001); h.application.onShow();
  expect(actions(h.source, h.page)).toEqual([{ binding: scope === "current" ? "check" : "refresh", text: scope === "current" ? "刷新个人资料" : "刷新列表" }]);
  expect(h.platform.records().session?.id).toBe(ids.later);
  expect(h.platform.requests).toHaveLength(count);
});

test.each([
  ["current", "current", 403], ["current", "current", 503],
  ["all", "current", 403], ["all", "current", 503],
  ["all", "list", 403], ["all", "list", 503],
] as const)("native %s retains original recovery after later-session %s read fails with %s", async (scope, path, status) => {
  const h = await native(scope, true, { path, status });
  expect(actions(h.source, h.page)).toEqual([{ binding: "recover", text: "查看退出结果" }]);
  expect(h.page.data.sessionsReady ?? false).toBe(false);
  const index = h.platform.requests.length;
  h.page.recover(); const request = await h.platform.next(index);
  expect(request.url).toEndWith(`/revocations?operation_id=${h.original.operation}`);
  h.platform.reply(index, { code: "ok", original: { scope, initiatorId: ids.session }, count: 1,
    revocations: [{ id: ids.session, revokedAt: "2026-10-30T08:01:00Z" }] }); await tick();
  expect(h.platform.records().session?.id).toBe(ids.later);
  expect(h.page.data.canRecover).toBe(false);
  expect(h.platform.requests.every(item => item.method === "GET")).toBe(true);
});

test.each([["current", "current"], ["all", "current"], ["all", "list"]] as const)("native %s still requires login after a later-session %s read returns 401", async (scope, path) => {
  const h = await native(scope, true, { path, status: 401 });
  expect(actions(h.source, h.page)).toEqual([]);
  expect(h.page.data.canLogin).toBe(true);
  expect(h.platform.records().session).toBeUndefined();
  expect(h.platform.records().logouts?.[0].operation).toBe(h.original.operation);
});

test.each(["current", "all"] as const)("native %s requires login only after 401 and preserves the later login after previous success", async scope => {
  const h = await native(scope);
  let index = h.platform.requests.length;
  h.page.recover(); await h.platform.next(index); h.platform.reply(index, { code: "authentication_invalid" }, 401); await tick();
  expect(actions(h.source, h.page)).toEqual([]); expect(h.page.data.canLogin).toBe(true);
  h.page.onHide();
  const entry = await h.load("account"); entry.page.onShow(); await tick();
  index = h.platform.requests.length;
  entry.page.authorize({ detail: { code: "new-explicit-phone-code" } }); await h.platform.next(index);
  h.platform.reply(index, { code: "ok", session: session(ids.later) }); await h.platform.next(index + 1);
  h.platform.reply(index + 1, { code: "ok", account: account() }); await tick();
  let originalPage = entry;
  if (scope === "all") {
    entry.page.onHide(); originalPage = await h.load("sessions"); index = h.platform.requests.length;
    originalPage.page.onShow(); await h.platform.next(index); h.platform.reply(index, { code: "ok", account: account() });
    await h.platform.next(index + 1); h.platform.reply(index + 1, { code: "ok", sessions: [
      { id: ids.later, source: "mini", createdAt: "2026-10-30T08:02:00Z", current: true },
    ] }); await tick();
  }
  expect(actions(originalPage.source, originalPage.page)).toEqual([{ binding: "recover", text: "查看退出结果" }]);
  index = h.platform.requests.length; originalPage.page.recover(); await h.platform.next(index);
  h.platform.reply(index, { code: "ok", original: { scope, initiatorId: ids.session }, count: 1,
    revocations: [{ id: ids.session, revokedAt: "2026-10-30T08:01:00Z" }] }); await tick();
  expect(originalPage.page.data.heading).toBe(scope === "current" ? "此前的登录已退出" : "此前的登录已全部退出");
  expect(actions(originalPage.source, originalPage.page)).toEqual([{ binding: scope === "current" ? "check" : "account", text: "返回个人账户" }]);
  expect(h.platform.records().session?.id).toBe(ids.later); expect(originalPage.page.data.canLogin).toBe(false);
  index = h.platform.requests.length;
  if (scope === "current") originalPage.page.check();
  else { originalPage.page.onHide(); originalPage = await h.load("account"); originalPage.page.onShow(); }
  const returning = await h.platform.next(index);
  expect(returning.method).toBe("GET"); expect(returning.url).toEndWith("/accounts/current");
  h.platform.reply(index, { code: "ok", account: account() }); await tick();
  expect(originalPage.page.data.phase).toBe("account");
  expect(h.platform.records().session?.id).toBe(ids.later);
  expect(h.platform.requests.filter(item => item.method === "DELETE")).toHaveLength(1);
});

test.each(["current", "all"] as const)("native previous %s completion without a later session offers phone login, not current-account access", async scope => {
  const h = await native(scope);
  const index = h.platform.requests.length;
  h.page.recover(); await h.platform.next(index);
  h.platform.reply(index, { code: "ok", original: { scope, initiatorId: ids.session }, count: 1,
    revocations: [{ id: ids.session, revokedAt: "2026-10-30T08:01:00Z" }] }); await tick();
  expect(h.page.data.heading).toBe(scope === "current" ? "此前的登录已退出" : "此前的登录已全部退出");
  expect(h.platform.records().session).toBeUndefined();
  expect(h.page.data.canLogin).toBe(true); expect(h.page.data.canCheck).toBe(false);
  expect(h.page.data.message).not.toContain("当前登录可继续");
  const phone = [...h.source.matchAll(/<(?:button|navigator)\b([^>]*)>使用手机号登录<\/(?:button|navigator)>/g)]
    .find(match => match[1].includes("primary"))!;
  expect(runInNewContext(phone[1].match(/wx:if="\{\{(.*?)\}\}"/)![1], h.page.data)).toBe(true);
  if (scope === "current") h.page.check();
  else { h.page.onHide(); const entry = await h.load("account"); entry.page.onShow(); }
  await tick(); expect(h.platform.requests).toHaveLength(index + 1);
});
