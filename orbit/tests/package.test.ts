import { expect, test } from "bun:test";
import { readFile } from "node:fs/promises";
import { runInNewContext } from "node:vm";
import { createHash } from "node:crypto";
import { account, epoch, FakePlatform, proof, session } from "./fixture";

test.each(["login", "registration"])("the exported native package handles %s without Node, DOM, fetch, URL or structuredClone globals", async flow => {
  const platform = new FakePlatform();
  let application: any;
  let definition: any;
  const context = {
    wx: platform, App(value: unknown) { application = value; }, getApp() { return application; },
    Page(value: unknown) { definition = value; },
    setTimeout() { return 1; }, clearTimeout() {},
    Date: class extends Date { static now() { return epoch; } },
    ArrayBuffer, Uint8Array,
  };
  runInNewContext(await readFile(new URL("../build/app.js", import.meta.url), "utf8"), context);
  application.onLaunch();
  expect(application.identity).toBeDefined();
  runInNewContext(await readFile(new URL("../build/pages/account/index.js", import.meta.url), "utf8"), context);
  const page = { ...definition, data: { ...definition.data }, setData(value: unknown) { Object.assign(this.data, value); } };
  page.onShow();
  let work = application.identity.authorize("phone-event-code"); await platform.next(0);
  let index = 0;
  if (flow === "registration") {
    platform.reply(0, { code: "ok", registration: proof() }); await work;
    expect(page.data.phase).toBe("registration");
    work = application.identity.register("张宁", "11010519491231002X");
    await platform.next(1); index = 1;
  }
  platform.reply(index, { code: "ok", session: session() }, flow === "registration" ? 201 : 200);
  await platform.next(index + 1);
  platform.reply(index + 1, { code: "ok", account: account() }); await work;
  expect(page.data.name).toBe("张宁");
  expect(page.data).not.toHaveProperty("token");
  expect(page.data).not.toHaveProperty("secret");
  page.onHide();
  expect(page.data.name).toBe("");
  expect(page.data.inputIdNumber).toBe("");
  application.onHide();
});

test("the native build records the one actual shared OpenAPI snapshot", async () => {
  const snapshot = await readFile(new URL("../../halo/protocol/openapi.json", import.meta.url));
  const source = JSON.parse(await readFile(new URL("../build/build-source.json", import.meta.url), "utf8"));
  expect(source).toMatchObject({ source: "export", snapshot: "halo/protocol/openapi.json",
    sha256: createHash("sha256").update(snapshot).digest("hex"), platformAcceptance: false });
});
