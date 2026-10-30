import { afterEach, expect, test } from "bun:test";
import { Protocol } from "../src/protocol";
import { account, controllers, epoch, ids, session, setup, token, login } from "./fixture";

afterEach(() => { for (const controller of controllers.splice(0)) controller.suspend(); });

const rows = () => [
  { id: ids.session, source: "mini", createdAt: "2026-10-30T08:00:00Z", current: true },
  { id: ids.later, source: "website", createdAt: "2026-10-29T07:20:00Z", current: false },
  { id: ids.otherSession, source: "mini", createdAt: "2026-10-28T07:20:00Z", current: false },
];
async function opened() {
  const h = setup({ version: 1, session: { ...session(), accountId: ids.account } });
  const showing = h.controller.show("sessions");
  await h.platform.next(0);
  expect(h.controller.snapshot().sessions).toEqual([]);
  h.platform.reply(0, { code: "ok", account: account() });
  await h.platform.next(1); h.platform.reply(1, { code: "ok", sessions: rows() }); await showing;
  return h;
}
async function refreshed(h: Awaited<ReturnType<typeof opened>>, sessions = rows()) {
  const index = h.platform.requests.length;
  const work = h.controller.check(); await h.platform.next(index);
  h.platform.reply(index, { code: "ok", account: account() });
  await h.platform.next(index + 1); h.platform.reply(index + 1, { code: "ok", sessions }); await work;
}

test("real list contract accepts only source facts, and dynamic DELETE uses the generated contract", () => {
  const protocol = new Protocol();
  const reply = (value: unknown, statusCode = 200) => ({ statusCode, header: { "Content-Type": "application/json" }, data: JSON.stringify(value) });
  expect(protocol.decode(reply({ code: "ok", sessions: rows() }), "GET /api/v1/sessions")).toBeDefined();
  expect(protocol.decode(reply({ code: "ok", sessions: [{ ...rows()[0], source: "device" }] }), "GET /api/v1/sessions")).toBeUndefined();
  expect(protocol.decode(reply({ code: "current_session" }, 409), "DELETE /api/v1/sessions/{id}")).toMatchObject({ status: 409 });
  expect(protocol.decode(reply({ code: "resource_not_found" }, 404), "DELETE /api/v1/sessions/{id}")).toMatchObject({ status: 404 });
});

test("list reads Bearer after verified current, has no persistent list, and forbids current selection", async () => {
  const h = await opened();
  expect(h.platform.requests[1].url).toEndWith("/api/v1/sessions");
  expect(h.platform.requests[1].header.Authorization).toBe(`Bearer ${token}`);
  expect(h.controller.snapshot()).toMatchObject({ phase: "sessions", sessionsReady: true, sessions: rows(), name: "" });
  expect(h.controller.targetSelection(ids.session)).toBeUndefined();
  expect(h.controller.targetSelection(ids.later)?.targetId).toBe(ids.later);
  expect(JSON.stringify(h.platform.saved)).not.toContain("createdAt");
});

test("successful other logout removes only the proven target and retains the initiating session", async () => {
  const h = await opened();
  const work = h.controller.logoutOther(h.controller.targetSelection(ids.later)!);
  const request = await h.platform.next(2);
  expect(request.method).toBe("DELETE");
  expect(request).not.toHaveProperty("data");
  expect(request.url).toEndWith(`/sessions/${ids.later}`);
  expect(request.header).not.toHaveProperty("Session-Id");
  expect(request.header).not.toHaveProperty("Origin");
  h.platform.reply(2, { code: "ok", completion: { id: request.header["Operation-Id"], count: 1, revokedAt: "2026-10-30T08:00:00Z" } }); await work;
  expect(h.controller.snapshot().sessions.map(item => item.id)).toEqual([ids.session, ids.otherSession]);
  expect(h.controller.snapshot().message).toBe("已退出所选登录。");
  expect(h.platform.records().session?.id).toBe(ids.session);
  expect(h.platform.records().logouts).toBeUndefined();
});

test.each([true, false])("unknown other logout only refreshes current state, target present=%s", async present => {
  const h = await opened();
  const work = h.controller.logoutOther(h.controller.targetSelection(ids.later)!); await h.platform.next(2);
  h.platform.disconnect(2); await work;
  expect(h.controller.snapshot()).toMatchObject({ canCheck: true, canRecover: false, sessionsReady: false, sessions: [] });
  expect(h.controller.snapshot().message).toContain("结果待确认");
  await refreshed(h, present ? rows() : rows().filter(item => item.id !== ids.later));
  expect(h.controller.snapshot().message).toBe("列表已刷新。");
  expect(h.platform.requests.filter(item => item.method === "DELETE")).toHaveLength(1);
  expect(h.platform.requests.some(item => item.url.includes("revocations"))).toBe(false);
  if (present) {
    const next = h.controller.logoutOther(h.controller.targetSelection(ids.later)!); const request = await h.platform.next(5);
    expect(request.header["Operation-Id"]).not.toBe(h.platform.requests[2].header["Operation-Id"]);
    h.platform.disconnect(5); await next;
  }
});

test("failed refresh after unknown preserves uncertainty without repeating DELETE", async () => {
  const h = await opened();
  const work = h.controller.logoutOther(h.controller.targetSelection(ids.later)!); await h.platform.next(2); h.platform.disconnect(2); await work;
  const refresh = h.controller.check(); await h.platform.next(3); h.platform.reply(3, { code: "ok", account: account() });
  await h.platform.next(4); h.platform.reply(4, { code: "dependency_unavailable" }, 503); await refresh;
  expect(h.controller.snapshot().message).toContain("退出结果仍待确认");
  expect(h.controller.snapshot().sessions).toEqual([]);
  expect(h.platform.requests.filter(item => item.method === "DELETE")).toHaveLength(1);
});

test("failed current check while refreshing also preserves unknown single exit", async () => {
  const h = await opened();
  const work = h.controller.logoutOther(h.controller.targetSelection(ids.later)!); await h.platform.next(2); h.platform.disconnect(2); await work;
  const refresh = h.controller.check(); await h.platform.next(3); h.platform.reply(3, { code: "dependency_unavailable" }, 503); await refresh;
  expect(h.controller.snapshot().message).toContain("退出结果仍待确认");
  expect(h.platform.requests.filter(item => item.method === "DELETE")).toHaveLength(1);
});

test.each([[404, "resource_not_found", "所选登录暂时无法操作"], [409, "current_session", "所选登录是本次登录"]] as const)("%s %s never means successful deletion", async (status, code, message) => {
  const h = await opened();
  const work = h.controller.logoutOther(h.controller.targetSelection(ids.later)!); await h.platform.next(2);
  h.platform.reply(2, { code }, status); await work;
  expect(h.controller.snapshot().message).toContain(message);
  expect(h.controller.snapshot()).toMatchObject({ sessionsReady: false, canCheck: true, canRecover: false });
  expect(h.platform.records().session?.id).toBe(ids.session);
});

test.each(["list", "delete"])("%s 401 clears only selected identity; 403 conceals but retains identity", async endpoint => {
  for (const status of [401, 403]) {
    const h = await opened();
    const work = endpoint === "delete" ? h.controller.logoutOther(h.controller.targetSelection(ids.later)!) : h.controller.check();
    await h.platform.next(2);
    if (endpoint === "list") { h.platform.reply(2, { code: "ok", account: account() }); await h.platform.next(3); }
    h.platform.reply(endpoint === "list" ? 3 : 2, { code: status === 401 ? "authentication_invalid" : "permission_denied" }, status); await work;
    expect(h.controller.snapshot().sessions).toEqual([]);
    expect(h.controller.snapshot().canLogin).toBe(status === 401);
    expect(!!h.platform.records().session).toBe(status === 403);
  }
});

test.each(["unsorted", "wrong-current", "duplicate", "no-current", "untrusted"])("%s list is concealed", async kind => {
  const h = await opened();
  const work = h.controller.check(); await h.platform.next(2); h.platform.reply(2, { code: "ok", account: account() }); await h.platform.next(3);
  const values = kind === "unsorted" ? rows().reverse() : kind === "wrong-current" ? rows().map(item => ({ ...item, current: !item.current })) :
    kind === "duplicate" ? [rows()[0], rows()[0]] : kind === "no-current" ? rows().slice(1) : [{ ...rows()[0], source: "invalid" }];
  h.platform.reply(3, { code: "ok", sessions: values }); await work;
  expect(h.controller.snapshot()).toMatchObject({ sessions: [], sessionsReady: false, canCheck: true, canLogin: false });
});

test("hide rejects late confirmation and late DELETE response; reopening verifies before a new list", async () => {
  const h = await opened();
  const old = h.controller.targetSelection(ids.later)!;
  h.controller.hide(); await h.controller.logoutOther(old); expect(h.platform.requests).toHaveLength(2);
  const show = h.controller.show("sessions"); await h.platform.next(2); h.platform.reply(2, { code: "ok", account: account() });
  await h.platform.next(3); h.platform.reply(3, { code: "ok", sessions: rows() }); await show;
  await h.controller.logoutOther(old); expect(h.platform.requests).toHaveLength(4);
  const work = h.controller.logoutOther(h.controller.targetSelection(ids.later)!); await h.platform.next(4);
  h.controller.hide();
  h.platform.reply(4, { code: "ok", completion: { id: h.platform.requests[4].header["Operation-Id"], count: 1, revokedAt: "2026-10-30T08:00:00Z" } }); await work;
  expect(h.controller.snapshot().sessions).toEqual([]);
  expect(h.platform.records().session?.id).toBe(ids.session);
});

test("out-of-order list cannot replace a later page or later account", async () => {
  const h = await opened();
  const a = h.controller.check(); await h.platform.next(2); h.platform.reply(2, { code: "ok", account: account() }); await h.platform.next(3);
  const b = h.controller.show(); await h.platform.next(4); h.platform.reply(4, { code: "ok", account: account() }); await b;
  h.platform.reply(3, { code: "ok", sessions: rows() }); await a;
  expect(h.controller.snapshot()).toMatchObject({ phase: "account", sessions: [], name: "张宁" });
  await h.controller.switchAccount(); expect(h.controller.snapshot().sessions).toEqual([]);
});

test("a later account ignores the old target confirmation and original DELETE success", async () => {
  const h = await opened();
  const confirmation = h.controller.targetSelection(ids.later)!;
  const work = h.controller.logoutOther(confirmation); await h.platform.next(2);
  const back = h.controller.show(); await h.platform.next(3); h.platform.reply(3, { code: "ok", account: account() }); await back;
  await h.controller.switchAccount(); await login(h, ids.otherSession, ids.otherAccount);
  h.platform.reply(2, { code: "ok", completion: { id: h.platform.requests[2].header["Operation-Id"], count: 1, revokedAt: "2026-10-30T08:00:00Z" } }); await work;
  const before = h.platform.requests.length; await h.controller.logoutOther(confirmation);
  expect(h.platform.requests).toHaveLength(before);
  expect(h.platform.records().session).toMatchObject({ id: ids.otherSession, accountId: ids.otherAccount });
  expect(h.controller.snapshot()).toMatchObject({ phase: "account", name: "李明", sessions: [] });
});

test.each(["wrong-operation", "wrong-count", "untrusted"])("%s completion cannot remove the target", async kind => {
  const h = await opened();
  const work = h.controller.logoutOther(h.controller.targetSelection(ids.later)!); const request = await h.platform.next(2);
  h.platform.reply(2, { code: "ok", completion: { id: kind === "wrong-operation" ? ids.registration : request.header["Operation-Id"],
    count: kind === "wrong-count" ? 2 : 1, revokedAt: "2026-10-30T08:00:00Z" } }, 200,
    kind === "untrusted" ? { "Content-Type": "text/html" } : {}); await work;
  expect(h.controller.snapshot()).toMatchObject({ sessionsReady: false, sessions: [], canCheck: true });
  expect(h.controller.snapshot().message).toContain("结果待确认");
  expect(h.platform.records().session?.id).toBe(ids.session);
});

test("current and all logout retain original recovery; fresh success offers the public home", async () => {
  const h = await opened();
  const work = h.controller.logout("all", h.controller.selection()); const request = await h.platform.next(2);
  expect(request.url).toEndWith("/api/v1/sessions");
  expect(h.platform.records().logouts?.[0]).toMatchObject({ scope: "all", sessionId: ids.session });
  h.platform.reply(2, { code: "ok", completion: { id: request.header["Operation-Id"], count: 3, revokedAt: "2026-10-30T08:00:00Z" } }); await work;
  expect(h.controller.snapshot()).toMatchObject({ phase: "complete", homeAction: true, sessions: [] });
  expect(h.platform.records().session).toBeUndefined();
});

test("foreground expiry and background conceal session lists without extra HTTP", async () => {
  const h = await opened(); const count = h.platform.requests.length;
  h.controller.suspend(); expect(h.controller.snapshot().sessions).toEqual([]);
  h.time(epoch + 31 * 86400000); h.controller.resume();
  expect(h.platform.records().session).toBeUndefined();
  expect(h.platform.requests).toHaveLength(count);
});
