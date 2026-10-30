import { expect, test } from "bun:test";
import { readFile } from "node:fs/promises";
import { runInNewContext } from "node:vm";
import { epoch, FakePlatform, proof } from "./fixture";
import { packageFile } from "./package-files";

const tick = () => new Promise(resolve => setTimeout(resolve, 0));

test.each(["login", "registration"] as const)("the actual self page labels unknown %s and keeps the form out of a pending write", async flow => {
  const platform = new FakePlatform();
  let application: any, definition: any, now = epoch;
  const context = { wx: platform, App(value: unknown) { application = value; }, getApp() { return application; },
    Page(value: unknown) { definition = value; }, setTimeout() { return 1; }, clearTimeout() {},
    Date: class extends Date { static now() { return now; } }, ArrayBuffer, Uint8Array };
  runInNewContext(await readFile(packageFile("app.js"), "utf8"), context);
  application.onLaunch();
  runInNewContext(await readFile(packageFile("pages/account/index.js"), "utf8"), context);
  const page = { ...definition, data: { ...definition.data }, setData(value: unknown) { Object.assign(this.data, value); } };
  const source = await readFile(packageFile("pages/account/index.wxml"), "utf8");
  function shown(panel: "login" | "registration") {
    const excerpt = source.split(`<!-- snapshot:${panel}:start -->`)[1].split(`<!-- snapshot:${panel}:end -->`)[0];
    return runInNewContext(excerpt.match(/wx:if="\{\{(.*?)\}\}"/)![1], page.data);
  }
  try {
    page.onShow();
    expect(page.data.heading).toBe("登录个人账户"); expect(shown("login")).toBe(true);
    page.authorize({ detail: { code: "explicit-native-phone-event" } }); await platform.next(0);
    let index = 0;
    if (flow === "registration") {
      platform.reply(0, { code: "ok", registration: proof() }); await tick();
      expect(page.data.heading).toBe("创建个人账户"); expect(shown("registration")).toBe(true);
      page.inputName({ detail: { value: "张宁" } }); page.inputIdNumber({ detail: { value: "11010519491231002X" } });
      page.submit(); await platform.next(1); index = 1;
      expect(shown("registration")).toBe(false);
    }
    platform.disconnect(index); await tick(); now += 5001; application.onShow();
    expect(page.data.heading).toBe(flow === "login" ? "登录结果待确认" : "注册结果待确认");
    expect(shown("login")).toBe(false); expect(shown("registration")).toBe(false);
    const control = source.match(/<button\b([^>]*bindtap="recover"[^>]*)>([\s\S]*?)<\/button>/)!;
    expect(runInNewContext(control[1].match(/wx:if="\{\{(.*?)\}\}"/)![1], page.data)).toBe(true);
    const label = control[2].replace(/\{\{(.*?)\}\}/g, (_, expression) => String(runInNewContext(expression, page.data)));
    expect(label).toBe(flow === "login" ? "查看登录结果" : "查看注册结果");
    expect(platform.requests).toHaveLength(index + 1);
    expect(JSON.stringify(platform.saved)).not.toContain("recoveryKind");
    page.onHide(); expect(page.data.inputName).toBe(""); expect(page.data.inputIdNumber).toBe("");
  } finally { application.onHide(); }
});
