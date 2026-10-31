import { afterEach, describe, expect, test } from "bun:test";
import { Protocol, validAccount } from "../src/protocol";
import { Storage, storageKey } from "../src/storage";
import { operation, secret } from "../src/platform";
import { account, controllers, epoch, ids, login, proof, session, setup, token, FakePlatform } from "./fixture";

afterEach(() => { for (const value of controllers.splice(0)) value.suspend(); });

test("canonical secret and UUID use only native secure random; failure issues zero requests", async () => {
  const platform = new FakePlatform();
  expect(await secret(platform)).toBe(Buffer.alloc(32, 1).toString("base64url"));
  expect(await operation(platform)).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
  const h = setup(); await h.controller.show(); h.platform.randomFails = true;
  await h.controller.authorize("phone-code");
  expect(h.platform.requests).toHaveLength(0);
});

test("preflight storage failure sends no login or R2", async () => {
  const h = setup(); await h.controller.show(); h.platform.storageFails = true;
  await h.controller.authorize("phone-code");
  expect(h.platform.requests).toHaveLength(0);
  const r = setup({ version: 1, registration: { proof: proof(), createdAt: epoch, deadline: epoch + 600000 } });
  await r.controller.show(); r.platform.storageFails = true;
  await r.controller.register("张宁", "11010519491231002X");
  expect(r.platform.requests).toHaveLength(0);
});

test("phone authorization consumes code once, persists complete material, uses Bearer only and verifies current", async () => {
  const h = setup(); await h.controller.show();
  const work = h.controller.authorize("phone-event-code");
  const request = await h.platform.next(0);
  expect(request.method).toBe("POST");
  expect(JSON.parse(request.data!)).toEqual(h.platform.records().authentication!.input);
  expect(JSON.parse(request.data!).credential).toEqual({ type: "mobile", code: "phone-event-code" });
  expect(request.header.Authorization).toBeUndefined();
  h.platform.reply(0, { code: "ok", session: session() });
  const current = await h.platform.next(1);
  expect(h.controller.snapshot().name).toBe("");
  expect(current.header).toEqual({ "Operation-Id": expect.any(String), Accept: "application/json", Authorization: `Bearer ${token}` });
  h.platform.reply(1, { code: "ok", account: account() }); await work;
  expect(h.controller.snapshot()).toMatchObject({ phase: "account", name: "张宁", suffix: "002X", bound: false });
  expect(h.platform.records()).toEqual({ version: 1, session: { ...session(), accountId: ids.account } });
  expect(JSON.stringify(h.platform.saved)).not.toContain("张宁");
});

test("no account branches to real R2 fields, independent operation and current verification", async () => {
  const h = setup(); await h.controller.show();
  const authentication = h.controller.authorize("phone-code"); await h.platform.next(0);
  const r1 = h.platform.requests[0].header["Operation-Id"];
  h.platform.reply(0, { code: "ok", registration: proof() }); await authentication;
  expect(h.controller.snapshot().phase).toBe("registration");
  expect(h.platform.records().authentication).toBeUndefined();
  const registration = h.controller.register(" 张宁 ", "11010519491231002x");
  const request = await h.platform.next(1);
  expect(request.header["Operation-Id"]).not.toBe(r1);
  expect(JSON.parse(request.data!)).toEqual({ id: ids.registration, token, name: "张宁", idNumber: "11010519491231002X" });
  h.platform.reply(1, { code: "ok", session: session() }, 201);
  await h.platform.next(2); h.platform.reply(2, { code: "ok", account: account() }); await registration;
  expect(h.controller.snapshot().phase).toBe("account");
  expect(h.platform.records().registration).toBeUndefined();
});

test.each(["authentication", "registration"])("%s rejects a website Cookie handoff without clearing original recovery material", async kind => {
  const h = setup(kind === "registration" ? { version: 1,
    registration: { proof: proof(), createdAt: epoch, deadline: epoch + 600000 } } : undefined);
  await h.controller.show();
  const work = kind === "authentication" ? h.controller.authorize("phone-code") : h.controller.register("张宁", "11010519491231002X");
  await h.platform.next(0);
  const { token: _token, ...cookie } = session();
  h.platform.reply(0, { code: "ok", session: cookie }, kind === "registration" ? 201 : 200);
  await work;
  expect(h.platform.requests).toHaveLength(1);
  const records = h.platform.records();
  expect(records.session).toBeUndefined();
  expect(kind === "authentication" ? records.authentication?.uncertain : records.registration?.pending?.uncertain).toBe(true);
  expect(h.controller.snapshot()).toMatchObject({ phase: "pending", name: "", canLogin: false });
});

test("unknown auth survives reopening, uses original bytes/key, and rejects a replacement code", async () => {
  const h = setup(); await h.controller.show();
  const work = h.controller.authorize("phone-code"); await h.platform.next(0);
  h.platform.disconnect(0); await work;
  const original = h.platform.requests[0];
  await h.controller.authorize("new-code");
  expect(h.platform.requests).toHaveLength(1);
  h.time(epoch + 5000); await h.controller.show();
  const recovery = h.controller.recover(); await h.platform.next(1);
  expect(h.platform.requests[1].data).toBe(original.data);
  expect(h.platform.requests[1].header["Operation-Id"]).toBe(original.header["Operation-Id"]);
  h.platform.reply(1, { code: "authentication_invalid" }, 401); await recovery;
  expect(h.platform.records().authentication).toMatchObject({ stopped: true, uncertain: true });
  expect(h.controller.snapshot()).toMatchObject({ canLogin: false, canRecover: false });
});

test.each(["authentication_invalid", "permission_denied", "invalid_input"])("first trusted %s ends input and offers an explicit new authorization", async code => {
  const h = setup(); await h.controller.show();
  const work = h.controller.authorize("first-code"); await h.platform.next(0);
  h.platform.reply(0, { code }, code === "authentication_invalid" ? 401 : code === "permission_denied" ? 403 : 400); await work;
  expect(h.controller.snapshot()).toMatchObject({ canLogin: true, canRecover: false });
  expect(h.platform.records().authentication).toBeUndefined();
  const next = h.controller.authorize("second-code"); await h.platform.next(1);
  expect(h.platform.requests[1].header["Operation-Id"]).not.toBe(h.platform.requests[0].header["Operation-Id"]);
  h.platform.disconnect(1); await next;
});

test("R2 unknown recovery outlives fill-proof expiry and never exchanges another phone code", async () => {
  const h = setup({ version: 1, registration: { proof: proof(), createdAt: epoch, deadline: epoch + 600000 } });
  await h.controller.show(); h.time(epoch + 599000);
  const work = h.controller.register("张宁", "11010519491231002X"); await h.platform.next(0);
  h.platform.disconnect(0); await work;
  h.time(epoch + 605000); await h.controller.show();
  expect(h.platform.records().registration?.pending).toBeDefined();
  const recovery = h.controller.recover(); await h.platform.next(1);
  expect(h.platform.requests[1].data).toBe(h.platform.requests[0].data);
  h.platform.reply(1, { code: "ok", session: session() });
  await h.platform.next(2); h.platform.reply(2, { code: "ok", account: account() }); await recovery;
  expect(h.platform.requests.filter(value => value.url.endsWith("/sessions"))).toHaveLength(0);
});

test.each(["login", "registration"] as const)("original R2 mobile_taken offers fresh verification and the current result selects %s", async next => {
  const h = setup({ version: 1, registration: { proof: proof(), createdAt: epoch, deadline: epoch + 600000 } });
  await h.controller.show(); const work = h.controller.register("张宁", "11010519491231002X");
  await h.platform.next(0); h.platform.disconnect(0); await work;
  h.time(epoch + 5000);
  const recovery = h.controller.recover(); await h.platform.next(1);
  expect(h.platform.requests[1].header["Operation-Id"]).toBe(h.platform.requests[0].header["Operation-Id"]);
  h.platform.reply(1, { code: "mobile_taken" }, 409); await recovery;
  expect(h.controller.snapshot().message).toBe("本次注册未完成。请重新验证手机号，再按页面提示继续。");
  expect(h.controller.snapshot().canLogin).toBe(true);
  expect(h.platform.records().registration).toBeUndefined();
  const verification = h.controller.authorize("fresh-phone-event"); await h.platform.next(2);
  expect(JSON.parse(h.platform.requests[2].data!).credential).toEqual({ type: "mobile", code: "fresh-phone-event" });
  expect(h.platform.requests[2].header["Operation-Id"]).not.toBe(h.platform.requests[0].header["Operation-Id"]);
  h.platform.reply(2, next === "login" ? { code: "ok", session: session() } : { code: "ok", registration: proof() });
  if (next === "login") { await h.platform.next(3); h.platform.reply(3, { code: "ok", account: account() }); }
  await verification;
  expect(h.controller.snapshot().phase).toBe(next === "login" ? "account" : "registration");
});

test("original R2 id_number_taken describes the original registration without asserting current ownership", async () => {
  const h = setup({ version: 1, registration: { proof: proof(), createdAt: epoch, deadline: epoch + 600000 } });
  await h.controller.show(); const work = h.controller.register("张宁", "11010519491231002X");
  await h.platform.next(0); h.platform.disconnect(0); await work; h.time(epoch + 5000);
  const recovery = h.controller.recover(); await h.platform.next(1);
  h.platform.reply(1, { code: "id_number_taken" }, 409); await recovery;
  expect(h.controller.snapshot()).toMatchObject({ phase: "registration", canLogin: false,
    message: "本次注册时，该身份证号已被使用。请核对填写的号码。" });
  expect(h.platform.records().registration?.pending).toBeUndefined();
  expect(h.platform.records().registration?.proof).toEqual(proof());
});

test("onHide and switching reject ignored-abort late responses; onShow verifies before displaying", async () => {
  const h = setup({ version: 1, session: { ...session(), accountId: ids.account } });
  const first = h.controller.show(); await h.platform.next(0); h.controller.hide();
  h.platform.reply(0, { code: "ok", account: account() }); await first;
  expect(h.controller.snapshot().name).toBe("");
  const second = h.controller.show(); await h.platform.next(1);
  expect(h.controller.snapshot().name).toBe("");
  h.controller.hide();
  h.platform.reply(1, { code: "ok", account: account() }); await second;
  expect(h.controller.snapshot().name).toBe("");
  expect(h.platform.aborted).toEqual([0, 1]);
});

test.each([401, 403, 503])("current %i hides data and distinguishes session clearing", async status => {
  const h = setup({ version: 1, session: { ...session(), accountId: ids.account } });
  const work = h.controller.show(); await h.platform.next(0);
  h.platform.reply(0, { code: status === 401 ? "authentication_invalid" : status === 403 ? "permission_denied" : "dependency_unavailable" }, status);
  await work;
  expect(h.controller.snapshot().name).toBe("");
  expect(!!h.platform.records().session).toBe(status !== 401);
  expect(h.controller.snapshot().message).not.toContain("账户暂停");
});

test("wrong account and malformed current projection are concealed", async () => {
  const h = setup({ version: 1, session: { ...session(), accountId: ids.account } });
  const work = h.controller.show(); await h.platform.next(0); h.platform.reply(0, { code: "ok", account: account(ids.otherAccount) }); await work;
  expect(h.controller.snapshot().name).toBe("");
  expect(validAccount({ ...account(), name: "e\u0301" })).toBe(false);
  expect(validAccount({ ...account(), avatar: { ...account().avatar, href: account().avatar.href + "&signature=bad" } })).toBe(false);
});

test("unknown logout survives A to B to A without blocking B and reads proof with later token", async () => {
  const h = setup(); await h.controller.show(); await login(h);
  const first = h.controller.logout("all"); await h.platform.next(2);
  const original = h.platform.records().logouts![0];
  expect(h.platform.requests[2].data).toBeUndefined();
  h.platform.reply(2, { code: "authentication_invalid" }, 401); await first;
  expect(h.controller.snapshot().canLogin).toBe(true);
  await login(h, ids.otherSession, ids.otherAccount);
  expect(h.controller.snapshot().logoutPending).toBe(false);
  const second = h.controller.logout("current"); await h.platform.next(5);
  h.platform.reply(5, { code: "ok", completion: { id: h.platform.requests[5].header["Operation-Id"], count: 1, revokedAt: "2026-10-30T08:01:00Z" } });
  await second;
  expect(h.platform.records().logouts).toEqual([original]);
  await login(h, ids.later);
  expect(h.controller.snapshot().logoutPending).toBe(true);
  const recovery = h.controller.recover(); const request = await h.platform.next(8);
  expect(request.method).toBe("GET");
  expect(request.url).toEndWith(`/revocations?operation_id=${original.operation}`);
  h.platform.reply(8, { code: "ok", original: { scope: "all", initiatorId: ids.session }, count: 1,
    revocations: [{ id: ids.session, revokedAt: "2026-10-30T08:01:00Z" }] }); await recovery;
  expect(h.platform.records().session?.id).toBe(ids.later);
  expect(h.platform.records().logouts).toEqual([]);
  expect(h.platform.requests.filter(value => value.method === "DELETE")).toHaveLength(2);
});

test("logout proof scope and original initiator must both match; empty proof is unknown", async () => {
  for (const original of [null, { scope: "current", initiatorId: ids.session }, { scope: "all", initiatorId: ids.otherSession }]) {
    const h = setup({ version: 1, session: { ...session(ids.later), accountId: ids.account }, logouts: [{ operation: ids.registration,
      accountId: ids.account, sessionId: ids.session, scope: "all", createdAt: epoch, deadline: epoch + 600000 }] });
    const show = h.controller.show(); await h.platform.next(0); h.platform.reply(0, { code: "ok", account: account() }); await show;
    const work = h.controller.recover(); await h.platform.next(1);
    h.platform.reply(1, { code: "ok", original, count: original ? 1 : 0,
      revocations: original ? [{ id: ids.session, revokedAt: "2026-10-30T08:01:00Z" }] : [] }); await work;
    expect(h.platform.records().logouts).toHaveLength(1);
    expect(h.controller.snapshot().phase).toBe("pending");
  }
});

test("expiry and backwards time close recovery without new writes", async () => {
  const h = setup(); await h.controller.show();
  const work = h.controller.authorize("phone-code"); await h.platform.next(0); h.platform.disconnect(0); await work;
  h.time(epoch - 1); await h.controller.show();
  expect(h.platform.records().authentication).toBeUndefined();
  expect(h.platform.records().unresolved).toBe("authentication");
  expect(h.platform.requests).toHaveLength(1);
  const broken = new FakePlatform(); broken.saved[storageKey] = { version: 1, session: { token: "invalid" } };
  expect(() => new Storage(broken, new Protocol()).load(epoch)).toThrow();
});

test("late logout and switch confirmations cannot mutate a hidden or later account selection", async () => {
  const h = setup(); await h.controller.show(); await login(h);
  const original = h.controller.selection()!;
  await h.controller.switchAccount(original);
  await login(h, ids.otherSession, ids.otherAccount);
  const count = h.platform.requests.length;
  await h.controller.logout("all", original);
  await h.controller.switchAccount(original);
  expect(h.platform.requests).toHaveLength(count);
  expect(h.platform.records().session?.accountId).toBe(ids.otherAccount);
  const later = h.controller.selection()!;
  h.controller.hide();
  await h.controller.logout("current", later);
  expect(h.platform.requests).toHaveLength(count);
  expect(h.platform.records().session?.accountId).toBe(ids.otherAccount);
});
