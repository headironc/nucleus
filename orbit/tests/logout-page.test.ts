import { afterEach, expect, test } from "bun:test";
import { readFile } from "node:fs/promises";
import { runInNewContext } from "node:vm";
import { account, epoch, FakePlatform, ids, session } from "./fixture";
import { storageKey } from "../src/storage";

const applications: { onHide(): void }[] = [];
afterEach(() => { for (const app of applications.splice(0)) app.onHide(); });
const tick = () => new Promise(resolve => setTimeout(resolve, 0));
type NativePage = { data: Record<string, any>; onShow(): void; onHide(): void; recover(): void;
  authorize(event: { detail: { code: string } }): void; logoutCurrent(): void; logoutAll(): void };

async function native(scope: "current" | "all") {
  const platform = new FakePlatform();
  platform.setStorageSync(storageKey, { version: 1, session: { ...session(), accountId: ids.account } });
  let application: any, definition: any, modal: any, now = epoch;
  const context = { wx: Object.assign(platform, { showModal(value: unknown) { modal = value; } }),
    App(value: unknown) { application = value; }, getApp() { return application; }, Page(value: unknown) { definition = value; },
    setTimeout() { return 1; }, clearTimeout() {}, Date: class extends Date { static now() { return now; } }, ArrayBuffer, Uint8Array };
  runInNewContext(await readFile(new URL("../build/app.js", import.meta.url), "utf8"), context);
  application.onLaunch(); applications.push(application);
  async function load(name: "account" | "sessions") {
    runInNewContext(await readFile(new URL(`../build/pages/${name}/index.js`, import.meta.url), "utf8"), context);
    const page = { ...definition, data: { ...definition.data }, setData(value: unknown) { Object.assign(this.data, value); } } as NativePage;
    const source = await readFile(new URL(`../build/pages/${name}/index.wxml`, import.meta.url), "utf8");
    return { page, source };
  }
  const showing = await load(scope === "current" ? "account" : "sessions");
  showing.page.onShow(); await platform.next(0); platform.reply(0, { code: "ok", account: account() });
  if (scope === "all") {
    await platform.next(1); platform.reply(1, { code: "ok", sessions: [
      { id: ids.session, source: "mini", createdAt: "2026-10-30T08:00:00Z", current: true },
    ] });
  }
  await tick();
  const index = platform.requests.length;
  if (scope === "current") showing.page.logoutCurrent(); else showing.page.logoutAll();
  modal.success({ confirm: true }); await platform.next(index);
  const original = platform.records().logouts![0];
  platform.disconnect(index); await tick();
  return { ...showing, platform, application, original, load, setNow(value: number) { now = value; } };
}

// These two actual WXML controls own the recovery choice; inspect their real bindings and conditions.
function actions(source: string, page: NativePage) {
  return [...source.matchAll(/<button\b([^>]*)>([\s\S]*?)<\/button>/g)].flatMap(match => {
    const binding = match[1].match(/bindtap="([^"]+)"/)?.[1];
    if (binding !== "recover" && binding !== "refresh" && binding !== "check") return [];
    if (binding === "check" && match[2] === "刷新个人资料") {
      const panel = source.split("<!-- snapshot:account:start -->")[1].split("<!-- snapshot:account:end -->")[0];
      const parent = panel.match(/wx:if="\{\{(.*?)\}\}"/)![1];
      if (!runInNewContext(parent, page.data)) return [];
    }
    const condition = match[1].match(/wx:if="\{\{(.*?)\}\}"/)?.[1].replaceAll("&amp;", "&");
    if (condition && !runInNewContext(condition, page.data)) return [];
    const text = match[2].replace(/\{\{(.*?)\}\}/g, (_, expression) => String(runInNewContext(expression, page.data)));
    return [{ binding, text }];
  });
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
    expect(actions(h.source, h.page)).toEqual([{ binding: scope === "current" ? "check" : "refresh", text: "检查登录状态" }]);
    expect(h.platform.requests).toHaveLength(count);
    expect(h.page.data.canLogin).toBe(false);
    h.platform.storageReadFails = false;
  }
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
  expect(actions(originalPage.source, originalPage.page)).toEqual([{ binding: scope === "current" ? "check" : "refresh", text: "检查登录状态" }]);
  expect(h.platform.records().session?.id).toBe(ids.later); expect(originalPage.page.data.canLogin).toBe(false);
  expect(h.platform.requests.filter(item => item.method === "DELETE")).toHaveLength(1);
});
