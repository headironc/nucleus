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
    expect(page.data.heading).toBe(flow === "login" ? "暂时无法确认登录结果" : "暂时无法确认注册结果");
    expect(shown("login")).toBe(false); expect(shown("registration")).toBe(false);
    const control = source.match(/<button\b([^>]*bindtap="recover"[^>]*)>([\s\S]*?)<\/button>/)!;
    expect(runInNewContext(control[1].match(/wx:if="\{\{(.*?)\}\}"/)![1], page.data)).toBe(true);
    const label = control[2].replace(/\{\{(.*?)\}\}/g, (_, expression) => String(runInNewContext(expression, page.data)));
    expect(label).toBe(flow === "login" ? "查看登录结果" : "查看注册结果");
    expect(platform.requests).toHaveLength(index + 1);
    expect(JSON.stringify(platform.saved)).not.toContain("recoveryKind");
    page.recover(); await platform.next(index + 1);
    platform.reply(index + 1, { code: "permission_denied" }, 403); await tick();
    expect(page.data.message).toBe(flow === "login" ? "暂时无法确认此前的登录结果。" : "暂时无法确认此前的注册结果。");
    expect(page.data.canRecover).toBe(false); expect(page.data.canCheck).toBe(false); expect(page.data.canLogin).toBe(false);
    expect(source).toContain('url="/pages/home/index" open-type="reLaunch">返回首页</navigator>');
    page.onHide(); page.onShow(); await tick();
    expect(platform.requests).toHaveLength(index + 2);
    expect(page.data.canRecover).toBe(false);
    page.onHide(); expect(page.data.inputName).toBe(""); expect(page.data.inputIdNumber).toBe("");
    runInNewContext(await readFile(packageFile("pages/sessions/index.js"), "utf8"), context);
    const sessionsPage = { ...definition, data: { ...definition.data }, setData(value: unknown) { Object.assign(this.data, value); } };
    sessionsPage.onShow(); await tick();
    const sessionsSource = await readFile(packageFile("pages/sessions/index.wxml"), "utf8");
    const navigation = [...sessionsSource.matchAll(/<navigator\b([^>]*)>([^<]*)<\/navigator>/g)].flatMap(match => {
      const condition = match[1].match(/wx:if="\{\{(.*?)\}\}"/)?.[1];
      return condition && runInNewContext(condition, sessionsPage.data) ? [match[2]] : [];
    });
    expect(navigation).toEqual(["返回首页"]);
    expect(platform.requests).toHaveLength(index + 2);
    sessionsPage.onHide();
  } finally { application.onHide(); }
});

test("the selected native home and account explain available actions without decorative slogans", async () => {
  const home = await readFile(packageFile("pages/home/index.wxml"), "utf8");
  expect(home).toContain("查看个人资料，管理登录。");
  expect(home).toContain("使用手机号登录。首次使用需填写姓名和身份证号，创建个人账户。");
  expect(home).toContain('bindtap="openAccount">打开个人账户</button>');
  const account = await readFile(packageFile("pages/account/index.wxml"), "utf8");
  expect(account).toContain("创建账户后，员工服务需另行加入。");
  for (const text of [home, account]) {
    for (const removed of ["安心办理", "草木相伴", "一页本草", "一段四时", "从本人账户开始", "公开简介无需登录", "返回公开首页"])
      expect(text).not.toContain(removed);
  }
});

test.each(["account", "sessions"] as const)("native %s initialization failure points to an available home action", async name => {
  let definition: any;
  runInNewContext(await readFile(packageFile(`pages/${name}/index.js`), "utf8"), {
    Page(value: unknown) { definition = value; }, getApp() { return {}; },
  });
  const page = { ...definition, data: { ...definition.data }, setData(value: unknown) { Object.assign(this.data, value); } };
  page.onShow();
  expect(page.data.message).toBe("请返回首页后重新打开。");
  const source = await readFile(packageFile(`pages/${name}/index.wxml`), "utf8");
  const home = [...source.matchAll(/<navigator\b([^>]*)>返回首页<\/navigator>/g)]
    .find(match => !match[1].includes("wx:if") || runInNewContext(match[1].match(/wx:if="\{\{(.*?)\}\}"/)![1], page.data));
  expect(home?.[1]).toContain('url="/pages/home/index"');
});
