import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "../../halo/node_modules/@playwright/test";
import { account, controllers, ids, session, setup } from "../tests/fixture";
import type { View } from "../src/controller";

// Explicit static excerpts of this page, not a WXML interpreter or native platform proof.
const root = fileURLToPath(new URL("../", import.meta.url));
const output = process.env.ORBIT_VISUAL_OUTPUT;
if (!output) throw new Error("An explicit persistent visual evidence directory is required");
const source = await readFile(join(root, "src/pages/sessions/index.wxml"), "utf8");
const accountSource = await readFile(join(root, "src/pages/account/index.wxml"), "utf8");
const css = (await readFile(join(root, "src/app.wxss"), "utf8"))
  .replace(/^page\s*\{/gm, "body {")
  .replace(/(-?[\d.]+)rpx/g, (_, value) => `calc(${Number(value)} * 100vw / 750)`);
const variants: { name: string; view: View; modal?: { title: string; content: string; confirm: string } }[] = [];
const h = setup({ version: 1, session: { ...session(), accountId: ids.account } });
const rows = [
  { id: ids.session, source: "mini", createdAt: "2026-10-30T01:41:00Z", current: true },
  { id: ids.later, source: "website", createdAt: "2026-10-29T08:20:00Z", current: false },
  { id: ids.otherSession, source: "website", createdAt: "2026-10-28T00:56:00Z", current: false },
];
async function show(values = rows) {
  const index = h.platform.requests.length;
  const work = h.controller.show("sessions"); await h.platform.next(index);
  h.platform.reply(index, { code: "ok", account: account() }); await h.platform.next(index + 1);
  h.platform.reply(index + 1, { code: "ok", sessions: values }); await work;
}
function date(value: string) {
  const d = new Date(value), two = (value: number) => String(value).padStart(2, "0");
  return `${d.getFullYear()}年${d.getMonth() + 1}月${d.getDate()}日 ${two(d.getHours())}:${two(d.getMinutes())}`;
}
const escape = (value: string) => value.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll('"', "&quot;");
const clean = (value: string) => value.replace(/\s(?:wx:[a-z-]+|bind[a-z]+|disabled|aria-label)="[^"]*"/g, "").replace(/\s+wx:else\b/g, "");
function list(view: View) {
  if (!view.sessionsReady) return "";
  const row = source.match(/    <view class="session-row"[\s\S]+?    <\/view>/)![0];
  const list = view.sessions.map(item => {
    let value = row.replace(item.current ? /      <button[\s\S]+?<\/button>/ : /      <text[\s\S]+?<\/text>/, "");
    return clean(value).replaceAll("{{item.sourceLabel === '网站' ? 'website-icon' : 'mini-icon'}}", item.source === "website" ? "website-icon" : "mini-icon")
      .replaceAll("{{item.sourceLabel}}", item.source === "website" ? "网站" : "小程序")
      .replaceAll("{{item.date}}", date(item.createdAt)).replaceAll("{{item.id}}", "");
  }).join("");
  const all = source.match(/  <view class="session-all"[\s\S]+?  <\/view>/)![0];
  return `<view class="session-list">${list}</view>${clean(all)}`;
}
await mkdir(output, { recursive: true });
let browser: Awaited<ReturnType<typeof chromium.launch>> | undefined;
try {
  await show();
  variants.push({ name: "list", view: h.controller.snapshot() });
  variants.push({ name: "selected-confirm", view: h.controller.snapshot(), modal: { title: "退出所选登录？",
    content: `网站，登录于 ${date(rows[1].createdAt)}。退出后，该处需重新登录。`, confirm: "退出登录" } });
  variants.push({ name: "all-confirm", view: h.controller.snapshot(), modal: { title: "退出全部登录？",
    content: "将退出网站和小程序的全部登录，包括本次登录。再次进入个人账户需要重新登录。", confirm: "全部退出" } });
  const unknown = h.controller.logoutOther(h.controller.targetSelection(ids.later)!);
  let index = h.platform.requests.length; await h.platform.next(index);
  h.platform.disconnect(index); await unknown; variants.push({ name: "unknown", view: h.controller.snapshot() });
  await show(); variants.push({ name: "refreshed-present", view: h.controller.snapshot() });
  await show(rows.filter(item => item.id !== ids.later)); variants.push({ name: "refreshed-absent", view: h.controller.snapshot() });
  await show(rows.slice(0, 1)); variants.push({ name: "only-current", view: h.controller.snapshot() });
  const all = h.controller.logout("all"); index = h.platform.requests.length; await h.platform.next(index);
  h.platform.reply(index, { code: "ok", completion: { id: h.platform.requests[index].header["Operation-Id"], count: 1, revokedAt: "2026-10-30T08:00:00Z" } });
  await all; variants.push({ name: "all-complete", view: h.controller.snapshot() });
  browser = await chromium.launch();
  for (const width of [320, 390]) {
    const page = await browser.newPage({ viewport: { width, height: 844 }, deviceScaleFactor: 2 });
    await page.route("**/*", route => route.abort());
    for (const variant of variants) {
      const view = variant.view;
      const modal = variant.modal ? `<section class="modal-backdrop"><div class="native-modal"><strong>${variant.modal.title}</strong><p>${escape(variant.modal.content)}</p><div><button>取消</button><button>${variant.modal.confirm}</button></div></div></section>` : "";
      const html = `<!doctype html><html lang="zh-CN"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Static source: ${variant.name}</title><style>${css}\nbody{margin:0}view,navigator{display:block}button{font-family:inherit;width:100%;border:0}button::after{border:0}.session-exit{width:auto}main{display:block}.modal-backdrop{position:fixed;inset:0;background:#0006;display:flex;align-items:center;justify-content:center;padding:30px}.native-modal{background:#fffefa;padding:24px;border-radius:14px;max-width:280px;font-size:16px;line-height:1.7}.native-modal>div{display:flex;gap:12px}.native-modal button{font-size:14px;padding:12px;color:#9b2a4a}</style><body><main class="page sessions-page"><view class="page-title">登录管理</view><view class="body-copy">查看网站和小程序的登录情况，按需退出。</view>${view.message ? `<view class="notice">${escape(view.message)}</view>` : ""}${list(view)}${view.canCheck ? '<button class="secondary">刷新列表</button>' : ""}<navigator class="home-link ${view.homeAction ? "primary" : ""}">${view.homeAction ? "返回公开首页" : "返回个人账户"}</navigator></main>${modal}</body></html>`;
      if (html.includes("{{")) throw new Error("Unresolved static source display");
      await writeFile(join(output, `${variant.name}-${width}.html`), html); await page.setContent(html);
      if (await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)) throw new Error("Static horizontal overflow");
      await page.screenshot({ path: join(output, `${variant.name}-${width}.png`), fullPage: true });
    }
    const entry = clean(accountSource.match(/    <view class="section-label login-security">[\s\S]*?<\/button>/)![0]);
    const html = `<html lang="zh-CN"><meta charset="utf-8"><style>${css}view{display:block}body{margin:0}</style><body><main class="page"><view class="page-title">个人账户</view>${entry}</main></body></html>`;
    await writeFile(join(output, `account-entry-${width}.html`), html); await page.setContent(html);
    await page.screenshot({ path: join(output, `account-entry-${width}.png`), fullPage: true });
    await page.close();
  }
} finally {
  for (const controller of controllers.splice(0)) controller.suspend();
  await browser?.close();
}
console.log(`Static native-source pixels: ${output}; browser closed. No DevTools or native-platform acceptance.`);
