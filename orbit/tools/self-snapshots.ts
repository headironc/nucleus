import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "../../halo/node_modules/@playwright/test";
import { account, controllers, epoch, ids, proof, session, setup } from "../tests/fixture";
import type { View } from "../src/controller";
import { selfPresentation } from "../src/presentation";
import { packageOutput } from "./package-output";

// Explicit excerpts of these three pages, not a WXML interpreter or native-platform proof.
const root = fileURLToPath(new URL("../", import.meta.url));
const output = process.env.ORBIT_VISUAL_OUTPUT;
if (!output) throw new Error("An explicit persistent visual evidence directory is required");
const selected = await packageOutput();
for (const file of ["pages/home/index.wxml", "pages/account/index.wxml", "pages/sessions/index.wxml", "app.wxss", "self.wxss", "assets/self-garden.jpg"]) {
  if (!(await readFile(join(root, "src", file))).equals(await readFile(join(selected.directory, file)))) {
    throw new Error(`Selected package does not match current visual source: ${file}`);
  }
}
const source = await readFile(join(selected.directory, "pages/sessions/index.wxml"), "utf8");
const accountSource = await readFile(join(selected.directory, "pages/account/index.wxml"), "utf8");
const homeSource = await readFile(join(selected.directory, "pages/home/index.wxml"), "utf8");
const garden = `data:image/jpeg;base64,${(await readFile(join(selected.directory, "assets/self-garden.jpg"))).toString("base64")}`;
const css = (await readFile(join(selected.directory, "app.wxss"), "utf8"))
  .replace('@import "./self.wxss";', await readFile(join(selected.directory, "self.wxss"), "utf8"))
  .replace(/^page\s*\{/gm, "body {")
  .replace(/(-?[\d.]+)rpx/g, (_, value) => `calc(${Number(value)} * 100vw / 750)`);
type Variant = { name: string; view: View; page?: "account" | "home"; modal?: { title: string; content: string; confirm: string } };
const variants: Variant[] = [];
const h = setup({ version: 1, session: { ...session(), accountId: ids.account } });
const rows = [
  { id: ids.session, source: "mini", createdAt: "2026-10-30T01:41:00Z", current: true },
  { id: ids.later, source: "website", createdAt: "2026-10-29T08:20:00Z", current: false },
  { id: ids.otherSession, source: "website", createdAt: "2026-10-28T00:56:00Z", current: false },
];
const escape = (value: string) => value.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll('"', "&quot;");
const clean = (value: string) => value.replace(/\s(?:wx:[a-z-]+|bind[a-z]+|disabled|aria-label)="[^"]*"/g, "").replace(/\s+wx:else\b/g, "");
const flags = (value: string, view: View) => value.replaceAll('disabled="{{busy || logoutPending}}"', view.busy || view.logoutPending ? "disabled" : "")
  .replaceAll('disabled="{{busy}}"', view.busy ? "disabled" : "");
function panel(kind: string) { return accountSource.split(`<!-- snapshot:${kind}:start -->`)[1].split(`<!-- snapshot:${kind}:end -->`)[0]; }
function date(value: string) {
  const d = new Date(value), two = (value: number) => String(value).padStart(2, "0");
  return `${d.getFullYear()}年${d.getMonth() + 1}月${d.getDate()}日 ${two(d.getHours())}:${two(d.getMinutes())}`;
}
function list(view: View) {
  if (!view.sessionsReady) return "";
  const row = source.match(/    <view class="session-row"[\s\S]+?    <\/view>/)![0];
  const list = view.sessions.map(item => {
    let value = row.replace(item.current ? /      <button[\s\S]+?<\/button>/ : /      <text[\s\S]+?<\/text>/, "");
    return clean(flags(value, view)).replaceAll("{{item.sourceLabel === '网站' ? 'website-icon' : 'mini-icon'}}", item.source === "website" ? "website-icon" : "mini-icon")
      .replaceAll("{{item.sourceLabel}}", item.source === "website" ? "网站" : "小程序")
      .replaceAll("{{item.date}}", date(item.createdAt)).replaceAll("{{item.id}}", "");
  }).join("");
  const all = source.match(/  <view class="session-all"[\s\S]+?  <\/view>/)![0];
  return `<view class="session-list">${list}</view>${clean(flags(all, view))}`;
}
function accountPanel(view: View) {
  const kind = view.phase === "account" ? "account" : view.phase === "registration" && !view.busy ? "registration" :
    view.canLogin && view.phase !== "complete" ? "login" : undefined;
  if (!kind) return "";
  let value = panel(kind);
  if (kind === "account") {
    // This offline account pixel covers the actual avatar-error branch, not a fake avatar.
    value = value.replace(/<image class="avatar"[^>]+\/>/, "");
    if (view.logoutPending) value = value.replace(/<button[^>]+bindtap="check"[^>]*>刷新个人资料<\/button>/, "");
  }
  if (!view.message) value = value.replace(/<view class="notice" wx:if="\{\{message\}\}"[^>]*>\{\{message\}\}<\/view>/, "");
  return clean(flags(value, view)).replaceAll("{{name}}", escape(view.name)).replaceAll("{{suffix}}", escape(view.suffix))
    .replaceAll("{{message}}", escape(view.message)).replaceAll("{{bound ? '已关联' : '未关联'}}", view.bound ? "已关联" : "未关联")
    .replaceAll("{{inputName}}", "").replaceAll("{{inputIdNumber}}", "");
}
function recoveryActions(view: View, accountPage: boolean) {
  const wxml = accountPage ? accountSource : source;
  const controls = [...wxml.matchAll(/<button\b([^>]*)>([\s\S]*?)<\/button>/g)];
  const values: string[] = [];
  if (view.canRecover) values.push(controls.find(value => value[1].includes('bindtap="recover"'))![0]);
  if (view.canCheck && (accountPage ? view.phase !== "account" : !view.sessionsReady && view.phase !== "complete")) values.push(controls.find(value =>
    accountPage ? value[2].includes("检查登录状态") : value[1].includes('bindtap="refresh"'))![0]);
  if (accountPage && view.canReset) values.push(controls.find(value => value[1].includes('bindtap="restartLogin"'))![0]);
  if (accountPage && view.phase === "complete" && view.canLogin && !view.homeAction) values.push(controls.find(value => value[2] === "使用手机号登录")![0]);
  if (!values.length) return "";
  return `<view class="self-actions">${clean(flags(values.join(""), view))
    .replaceAll("{{phase === 'complete' && !homeAction ? 'primary' : 'secondary'}}", view.phase === "complete" && !view.homeAction ? "primary" : "secondary")
    .replaceAll("{{phase === 'complete' && !homeAction ? '返回个人账户' : phase === 'unavailable' ? '重新检查' : '检查登录状态'}}", view.phase === "complete" && !view.homeAction ? "返回个人账户" : view.phase === "unavailable" ? "重新检查" : "检查登录状态")
    .replaceAll("{{recoveryLabel}}", selfPresentation(view, accountPage ? "account" : "sessions").recoveryLabel)
    .replaceAll("{{phase === 'sessions' ? '刷新列表' : phase === 'unavailable' ? '重新检查' : '检查登录状态'}}", view.phase === "sessions" ? "刷新列表" : view.phase === "unavailable" ? "重新检查" : "检查登录状态")}</view>`;
}
function footer(view: View, own: boolean) {
  if (!own && view.sessionsReady) {
    const refresh = source.match(/<button[^>]+bindtap="refresh"[^>]*>刷新列表<\/button>/)![0];
    return `<view class="self-inline-actions">${view.canCheck ? clean(flags(refresh, view)) : ""}<navigator class="home-link">返回个人账户</navigator></view>`;
  }
  let first = "";
  if (!own && !view.homeAction && (view.canLogin || view.phase === "complete" && view.canCheck)) {
    first = `<navigator class="home-link primary self-button">${view.canLogin ? "使用手机号登录" : "返回个人账户"}</navigator>`;
  }
  const home = view.homeAction || own || view.canLogin || view.phase === "complete" ||
    view.phase === "pending" && (view.recoveryKind === "login" || view.recoveryKind === "registration") && !view.canRecover;
  const relogin = own && view.homeAction && view.canLogin ? clean([...accountSource.matchAll(/<button\b([^>]*)>([\s\S]*?)<\/button>/g)].find(value => value[2] === "重新登录")![0]) : "";
  return `${first}<navigator class="home-link ${view.homeAction ? "primary self-button" : ""}">${home ? "返回首页" : "返回个人账户"}</navigator>${relogin}`;
}
async function show(values = rows, capture = false) {
  const index = h.platform.requests.length;
  const work = h.controller.show("sessions"); await h.platform.next(index);
  if (capture) variants.push({ name: "sessions-checking", view: h.controller.snapshot() });
  h.platform.reply(index, { code: "ok", account: account() }); await h.platform.next(index + 1);
  if (capture) variants.push({ name: "sessions-loading", view: h.controller.snapshot() });
  h.platform.reply(index + 1, { code: "ok", sessions: values }); await work;
}
async function entryStates() {
  const entry = setup(); await entry.controller.show();
  variants.push({ name: "home", view: entry.controller.snapshot(), page: "home" });
  variants.push({ name: "login", view: entry.controller.snapshot(), page: "account" });
  await entry.controller.authorize(undefined);
  variants.push({ name: "phone-cancelled", view: entry.controller.snapshot(), page: "account" });
  let work = entry.controller.authorize("explicit-controlled-phone-event"); await entry.platform.next(0);
  variants.push({ name: "phone-processing", view: entry.controller.snapshot(), page: "account" });
  entry.platform.reply(0, { code: "ok", registration: proof() }); await work;
  variants.push({ name: "registration", view: entry.controller.snapshot(), page: "account" });
  await entry.controller.register("", "");
  variants.push({ name: "registration-invalid", view: entry.controller.snapshot(), page: "account" });
  work = entry.controller.register("张宁", "11010519491231002X"); await entry.platform.next(1);
  variants.push({ name: "registration-processing", view: entry.controller.snapshot(), page: "account" });
  entry.platform.disconnect(1); await work; entry.time(epoch + 5001); entry.controller.resume();
  variants.push({ name: "registration-unknown", view: entry.controller.snapshot(), page: "account" });
  work = entry.controller.recover(); await entry.platform.next(2);
  entry.platform.reply(2, { code: "permission_denied" }, 403); await work;
  variants.push({ name: "registration-stopped", view: entry.controller.snapshot(), page: "account" });
  entry.time(epoch + 600001); entry.controller.resume();
  variants.push({ name: "registration-expired", view: entry.controller.snapshot(), page: "account" });
  const login = setup(); await login.controller.show();
  work = login.controller.authorize("another-explicit-phone-event"); await login.platform.next(0); login.platform.disconnect(0); await work;
  login.time(epoch + 5001); login.controller.resume();
  variants.push({ name: "login-unknown", view: login.controller.snapshot(), page: "account" });
  work = login.controller.recover(); await login.platform.next(1);
  login.platform.reply(1, { code: "permission_denied" }, 403); await work;
  variants.push({ name: "login-stopped", view: login.controller.snapshot(), page: "account" });
  login.time(epoch + 600001); login.controller.resume();
  variants.push({ name: "login-expired", view: login.controller.snapshot(), page: "account" });
  for (const code of ["permission_denied", "authentication_invalid", "mobile_unsupported", "verification_required"]) {
    const rejected = setup(); await rejected.controller.show();
    work = rejected.controller.authorize("explicit-rejected-phone-event"); await rejected.platform.next(0);
    rejected.platform.reply(0, { code }, code === "authentication_invalid" ? 401 : code === "verification_required" ? 410 : 403); await work;
    variants.push({ name: `login-${code}`, view: rejected.controller.snapshot(), page: "account" });
  }
  for (const code of ["id_number_taken", "mobile_taken", "permission_denied", "invalid_input"]) {
    const rejected = setup({ version: 1, registration: { proof: proof(), createdAt: epoch, deadline: epoch + 600000 } });
    await rejected.controller.show(); work = rejected.controller.register("张宁", "11010519491231002X"); await rejected.platform.next(0);
    rejected.platform.reply(0, { code }, code === "permission_denied" ? 403 : code === "invalid_input" ? 400 : 409); await work;
    variants.push({ name: `registration-${code}`, view: rejected.controller.snapshot(), page: "account" });
  }
  const storage = setup(); await storage.controller.show(); storage.platform.storageWriteFails = true;
  await storage.controller.authorize("explicit-unsent-phone-event");
  variants.push({ name: "storage-before", view: storage.controller.snapshot(), page: "account" });
  variants.push({ name: "reset-confirm", view: storage.controller.snapshot(), page: "account", modal: { title: "重新验证手机号？", content: "将清除本机无法使用的登录信息。此前办理结果仍无法确认。", confirm: "重新验证" } });
  const sent = setup(); await sent.controller.show(); work = sent.controller.authorize("explicit-sent-phone-event"); await sent.platform.next(0);
  sent.platform.storageWriteFails = true; sent.platform.disconnect(0); await work;
  variants.push({ name: "storage-after", view: sent.controller.snapshot(), page: "account" });
  for (const status of [200, 401, 403, 503]) {
    const own = setup({ version: 1, session: { ...session(), accountId: ids.account } });
    work = own.controller.show(); await own.platform.next(0);
    if (status === 200) variants.push({ name: "account-checking", view: own.controller.snapshot(), page: "account" });
    const person = account(); person.name = "陈欧阳司徒慕容夏侯东方闻人长孙南宫诸葛公孙上官司马清和".repeat(3).slice(0, 64);
    own.platform.reply(0, status === 200 ? { code: "ok", account: person } : { code: status === 401 ? "authentication_invalid" : status === 403 ? "permission_denied" : "dependency_unavailable" }, status);
    await work; variants.push({ name: `account-${status === 200 ? "long-avatar-error" : status}`, view: own.controller.snapshot(), page: "account" });
  }
}
async function exitStates(scope: "current" | "all") {
  const page = scope === "current" ? "account" : "sessions";
  const current = setup({ version: 1, session: { ...session(), accountId: ids.account } });
  const opening = current.controller.show(page); await current.platform.next(0); current.platform.reply(0, { code: "ok", account: account() });
  if (scope === "all") { await current.platform.next(1); current.platform.reply(1, { code: "ok", sessions: rows }); }
  await opening;
  const location = scope === "current" ? { page: "account" as const } : {};
  if (scope === "current") variants.push({ name: "account-avatar-error", view: current.controller.snapshot(), ...location });
  if (scope === "current") {
    variants.push({ name: "current-confirm", view: current.controller.snapshot(), ...location, modal: { title: "退出登录？", content: "退出后，再次进入个人账户需要重新登录。其他登录不受影响。", confirm: "退出登录" } });
    variants.push({ name: "switch-confirm", view: current.controller.snapshot(), ...location, modal: { title: "切换账户？", content: "继续后，请重新选择手机号登录。", confirm: "继续切换" } });
    const fresh = setup(current.platform.records());
    const reading = fresh.controller.show(); await fresh.platform.next(0); fresh.platform.reply(0, { code: "ok", account: account() }); await reading;
    const exiting = fresh.controller.logout("current"); await fresh.platform.next(1);
    fresh.platform.reply(1, { code: "ok", completion: { id: fresh.platform.requests[1].header["Operation-Id"], count: 1, revokedAt: "2026-10-30T08:01:00Z" } }); await exiting;
    variants.push({ name: "current-complete", view: fresh.controller.snapshot(), ...location });
  }
  let index = current.platform.requests.length;
  const pending = current.controller.logout(scope); await current.platform.next(index); current.platform.disconnect(index); await pending;
  variants.push({ name: `${scope}-unknown`, view: current.controller.snapshot(), ...location });
  const original = current.platform.records().logouts![0];
  index = current.platform.requests.length;
  const previousWithoutLater = current.controller.recover(); await current.platform.next(index);
  current.platform.reply(index, { code: "ok", original: { scope, initiatorId: ids.session }, count: 1,
    revocations: [{ id: ids.session, revokedAt: "2026-10-30T08:01:00Z" }] }); await previousWithoutLater;
  variants.push({ name: `previous-${scope}-without-later`, view: current.controller.snapshot(), ...location });
  const later = setup({ version: 1, session: { ...session(ids.later), accountId: ids.account }, logouts: [original] });
  const returning = later.controller.show(page); await later.platform.next(0); later.platform.reply(0, { code: "ok", account: account() });
  if (scope === "all") { await later.platform.next(1); later.platform.reply(1, { code: "ok", sessions: [{ ...rows[0], id: ids.later }] }); }
  await returning; index = later.platform.requests.length;
  variants.push({ name: `later-${scope}-pending`, view: later.controller.snapshot(), ...location });
  const recovery = later.controller.recover(); await later.platform.next(index);
  later.platform.reply(index, { code: "ok", original: { scope, initiatorId: ids.session }, count: 1,
    revocations: [{ id: ids.session, revokedAt: "2026-10-30T08:01:00Z" }] }); await recovery;
  variants.push({ name: `previous-${scope}`, view: later.controller.snapshot(), ...location });
}
await mkdir(output, { recursive: true });
let browser: Awaited<ReturnType<typeof chromium.launch>> | undefined;
try {
  await entryStates(); await show(rows, true); variants.push({ name: "list", view: h.controller.snapshot() });
  variants.push({ name: "selected-confirm", view: h.controller.snapshot(), modal: { title: "退出所选登录？", content: `网站，登录于 ${date(rows[1].createdAt)}。退出后，该处需重新登录。`, confirm: "退出登录" } });
  variants.push({ name: "all-confirm", view: h.controller.snapshot(), modal: { title: "退出全部登录？", content: "将退出网站和小程序的全部登录，包括本次登录。再次进入个人账户需要重新登录。", confirm: "全部退出" } });
  const unknown = h.controller.logoutOther(h.controller.targetSelection(ids.later)!);
  let index = h.platform.requests.length; await h.platform.next(index); h.platform.disconnect(index); await unknown;
  variants.push({ name: "single-unknown", view: h.controller.snapshot() });
  const failedRefresh = h.controller.check(); index = h.platform.requests.length; await h.platform.next(index);
  h.platform.reply(index, { code: "ok", account: account() }); await h.platform.next(index + 1);
  h.platform.reply(index + 1, { code: "dependency_unavailable" }, 503); await failedRefresh;
  variants.push({ name: "single-refresh-failed", view: h.controller.snapshot() });
  await show(); variants.push({ name: "refreshed-present", view: h.controller.snapshot() });
  await show(rows.filter(item => item.id !== ids.later)); variants.push({ name: "refreshed-absent", view: h.controller.snapshot() });
  await show(rows.slice(0, 1)); variants.push({ name: "only-current", view: h.controller.snapshot() });
  const all = h.controller.logout("all"); index = h.platform.requests.length; await h.platform.next(index);
  h.platform.reply(index, { code: "ok", completion: { id: h.platform.requests[index].header["Operation-Id"], count: 1, revokedAt: "2026-10-30T08:00:00Z" } }); await all;
  variants.push({ name: "all-complete", view: h.controller.snapshot() }); await exitStates("current"); await exitStates("all");
  for (const status of [401, 403, 503]) {
    const failing = setup({ version: 1, session: { ...session(), accountId: ids.account } });
    const opening = failing.controller.show("sessions"); await failing.platform.next(0);
    failing.platform.reply(0, { code: "ok", account: account() }); await failing.platform.next(1);
    failing.platform.reply(1, { code: status === 401 ? "authentication_invalid" : status === 403 ? "permission_denied" : "dependency_unavailable" }, status);
    await opening; variants.push({ name: `sessions-${status}`, view: failing.controller.snapshot() });
  }
  const requested = process.env.ORBIT_VISUAL_STATES?.split(",");
  const selectedVariants = requested ? variants.filter(variant => requested.includes(variant.name)) : variants;
  if (requested && (new Set(requested).size !== requested.length || selectedVariants.length !== requested.length)) {
    throw new Error("Expected distinct, known visual state names");
  }
  browser = await chromium.launch();
  for (const width of [320, 390]) {
    const page = await browser.newPage({ viewport: { width, height: 844 }, deviceScaleFactor: 2 });
    await page.route("**/*", route => route.abort());
    for (const variant of selectedVariants) {
      const view = variant.view, own = variant.page === "account", home = variant.page === "home", { heading } = selfPresentation(view, own ? "account" : "sessions");
      const emblem = `<view class="self-status-emblem ${view.busy ? "self-spinner" : ""}">${view.busy ? "" : view.phase === "complete" ? "✓" : "!"}</view>`;
      const notice = view.message && !view.busy ? `<view class="notice">${escape(view.message)}</view>` : "";
      const statusCopy = view.message && !view.busy ? `<view class="body-copy self-status-copy">${escape(view.message)}</view>` : "";
      const status = `<view class="self-status">${emblem}<view class="page-title">${escape(heading)}</view>${statusCopy}</view>`;
      const content = home ? clean(homeSource) : own ? accountPanel(view) || status : `${view.sessionsReady ? "" : emblem}<view class="page-title">${escape(heading)}</view>${view.sessionsReady ? '<view class="body-copy">查看网站和小程序的登录情况，按需退出。</view>' : ""}${view.sessionsReady ? notice : statusCopy}${list(view)}`;
      const modal = variant.modal ? `<section class="modal-backdrop"><div class="native-modal"><strong>${variant.modal.title}</strong><p>${escape(variant.modal.content)}</p><div><button>取消</button><button>${variant.modal.confirm}</button></div></div></section>` : "";
      let html = `<!doctype html><html lang="zh-CN"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Static selected package: ${variant.name}</title><style>${css}\nbody{margin:0}view,navigator{display:block}button,input{font-family:inherit}button{border:0}button::after{border:0}main{display:block}.self-garden{object-fit:cover}.modal-backdrop{position:fixed;inset:0;background:#0006;display:flex;align-items:center;justify-content:center;padding:30px}.native-modal{background:#f7f4ec;padding:24px;border-radius:14px;max-width:280px;font-size:16px;line-height:1.7}.native-modal>div{display:flex;gap:12px}.native-modal button{font-size:14px;padding:12px;color:#b34239}</style><body>${home ? content : `<main class="page self-page ${own ? "account" : `sessions-page ${view.sessionsReady ? "" : "self-state"}`}">${content}${recoveryActions(view, own)}${footer(view, own)}</main>`}${modal}</body></html>`;
      html = html.replaceAll('src="/assets/self-garden.jpg"', `src="${garden}"`).replace(/<image\b([^>]+)\/>/g, "<img$1>");
      if (html.includes("{{")) throw new Error(`Unresolved static source display: ${variant.name}`);
      await writeFile(join(output, `${variant.name}-${width}.html`), html); await page.setContent(html);
      await page.evaluate(async () => { await Promise.all([...document.images].map(image => image.decode())); });
      if (await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)) throw new Error(`Static horizontal overflow: ${variant.name}`);
      await page.screenshot({ path: join(output, `${variant.name}-${width}.png`), fullPage: true });
    }
    await page.close();
  }
} finally {
  for (const controller of controllers.splice(0)) controller.suspend();
  await browser?.close();
}
console.log(`Static selected-package pixels: ${output}; browser closed. No native-platform acceptance.`);
