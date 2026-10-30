import { expect, test } from "bun:test";
import { readFile } from "node:fs/promises";
import { runInNewContext } from "node:vm";
import { account, FakePlatform, session } from "./fixture";

test("the built native package boots without Node, DOM, fetch, URL or structuredClone globals", async () => {
  const platform = new FakePlatform();
  let application: any;
  let definition: any;
  const context = {
    wx: platform, App(value: unknown) { application = value; }, getApp() { return application; },
    Page(value: unknown) { definition = value; },
    setTimeout() { return 1; }, clearTimeout() {},
    ArrayBuffer, Uint8Array,
  };
  runInNewContext(await readFile(new URL("../test-build/app.js", import.meta.url), "utf8"), context);
  application.onLaunch();
  expect(application.identity).toBeDefined();
  runInNewContext(await readFile(new URL("../test-build/pages/account/index.js", import.meta.url), "utf8"), context);
  const page = { ...definition, data: { ...definition.data }, setData(value: unknown) { Object.assign(this.data, value); } };
  page.onShow();
  const work = application.identity.authorize("phone-event-code"); await platform.next(0);
  platform.reply(0, { code: "ok", session: session() }); await platform.next(1);
  platform.reply(1, { code: "ok", account: account() }); await work;
  expect(page.data.name).toBe("张宁");
  expect(page.data).not.toHaveProperty("token");
  expect(page.data).not.toHaveProperty("secret");
  page.onHide();
  expect(page.data.name).toBe("");
  expect(page.data.inputIdNumber).toBe("");
  application.onHide();
});
