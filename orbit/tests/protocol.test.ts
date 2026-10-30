import { expect, test } from "bun:test";
import { Protocol } from "../src/protocol";
import { Transport } from "../src/transport";
import { account, FakePlatform, ids, session } from "./fixture";
import * as validators from "../.generated/validators.cjs";

test("the real shared schema retains website branches while Orbit accepts only mobile and Bearer", () => {
  const registry = validators as unknown as Record<string, (value: unknown) => boolean>;
  const protocol = new Protocol();
  const mobile = { secret: "A".repeat(43), credential: { type: "mobile", code: "phone-event-code" } };
  const website = { secret: "A".repeat(43), credential: { type: "message", id: ids.registration, mobile: "13800138000", code: "123456" } };
  const { token: _token, ...cookie } = session();
  expect(registry.createSession_input(website)).toBe(true);
  expect(protocol.validLogin(website)).toBe(false);
  expect(protocol.validLogin(mobile)).toBe(true);
  expect(protocol.validLogin({ ...mobile, credential: { type: "mobile", code: "x".repeat(2049) } })).toBe(false);
  expect(registry.createSession_200({ code: "ok", session: cookie })).toBe(true);
  expect(protocol.validSession(cookie)).toBe(false);
  expect(protocol.validSession(session())).toBe(true);
  for (const [endpoint, status] of [["POST /api/v1/sessions", 200], ["POST /api/v1/registrations", 200], ["POST /api/v1/registrations", 201]] as const) {
    expect(protocol.decode({ statusCode: status, header: { "Content-Type": "application/json" }, data: JSON.stringify({ code: "ok", session: cookie }) }, endpoint)).toBeUndefined();
  }
});

test.each([
  { status: 200, media: "text/html", body: { code: "ok", session: session() } },
  { status: 200, media: "application/json", body: { code: "unknown", session: session() } },
  { status: 401, media: "application/json", body: { code: "ok" } },
  { status: 200, media: "application/json", body: { code: "ok", session: { id: ids.session } } },
  { status: 202, media: "application/json", body: { code: "ok", session: session() } },
])("rejects incompatible status, media, app code and incomplete shape: %j", ({ status, media, body }) => {
  expect(new Protocol().decode({ statusCode: status, header: { "content-type": media }, data: JSON.stringify(body) }, "POST /api/v1/sessions")).toBeUndefined();
});

test("binary or eagerly decoded object is never assumed to be trusted JSON text", () => {
  for (const data of [new Uint8Array([123, 125]).buffer, { code: "ok", account: account() }]) {
    expect(new Protocol().decode({ statusCode: 200, header: { "content-type": "application/json" }, data }, "GET /api/v1/accounts/current")).toBeUndefined();
  }
});

test("native cancellation completes local wait and ignores late success without retry", async () => {
  const platform = new FakePlatform();
  const exchange = new Transport(platform, "https://api.example.test").send({ method: "DELETE", path: "/api/v1/sessions/current", operation: ids.registration, token: "secret" });
  exchange.cancel();
  platform.reply(0, { code: "ok" });
  expect(await exchange.promise).toBeUndefined();
  expect(platform.requests).toHaveLength(1);
  expect(platform.aborted).toEqual([0]);
  expect(platform.requests[0].header).not.toHaveProperty("Session-Id");
  expect(platform.requests[0].header).not.toHaveProperty("Origin");
  expect(platform.requests[0].header).not.toHaveProperty("Cookie");
});

test("cancelling a settled native exchange does not abort its completed request", async () => {
  const platform = new FakePlatform();
  const exchange = new Transport(platform, "https://api.example.test").send({ method: "POST", path: "/api/v1/sessions", operation: ids.registration });
  platform.reply(0, { code: "authentication_invalid" }, 401);
  expect(exchange.pending).toBe(false);
  exchange.cancel();
  expect(platform.aborted).toEqual([]);
  expect(await exchange.promise).toMatchObject({ statusCode: 401 });
});
