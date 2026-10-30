import { expect, test } from "bun:test";
import { Protocol } from "../src/protocol";
import { Transport } from "../src/transport";
import { account, FakePlatform, ids, session } from "./fixture";

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
