import { expect, test } from "vitest";
import { redactText } from "./redact";

test("keeps the assertion operator, values and callsite while removing credentials", () => {
  const original =
    "expect(received).toBe(expected) // Object.is equality\n\nExpected: 2\nReceived: 1\n    at /repo/halo/tests/identity.spec.ts:1390:5";
  const result = redactText(original);
  expect(result.text).toBe(original);
  expect(result.redactions).toBe(0);
  expect(result.truncated).toBe(false);
});

test("redacts credential fields in a nested escaped JSON diagnostic", () => {
  const raw = JSON.stringify({
    request: {
      headers: {
        authorization: "Bearer abcdefghijklmnop",
        cookie: "session=top-secret; other=1",
        "x-api-key": "key-material",
      },
    },
    body: '{"password":"hunter2","token":"tok_123456","sessionId":"s-1"}',
  });
  const result = redactText(raw);
  expect(result.text).not.toContain("abcdefghijklmnop");
  expect(result.text).not.toContain("top-secret");
  expect(result.text).not.toContain("key-material");
  expect(result.text).not.toContain("hunter2");
  expect(result.text).not.toContain("tok_123456");
  expect(result.text).not.toContain("s-1");
  expect(result.redactions).toBeGreaterThanOrEqual(6);
});

test("redacts query secrets and full signed avatar links but keeps ordinary parameters", () => {
  const raw =
    "callback https://example.test/return?token=abc123&state=ok and " +
    "image https://127.0.0.1:4443/api/v1/avatars/0123456789abcdef.webp?expires=1&sig=secret";
  const result = redactText(raw);
  expect(result.text).not.toContain("abc123");
  expect(result.text).not.toContain("expires=1");
  expect(result.text).not.toContain("sig=secret");
  expect(result.text).toContain("state=ok");
  expect(result.text).toContain("/api/v1/avatars/");
  expect(result.text).toContain("<redacted-query>");
});

test("redacts an escaped URL and private key material", () => {
  const raw = JSON.stringify({
    message: "redirect https://example.test/cb?access_token=leaked",
    key: "-----BEGIN PRIVATE KEY-----\nZmFrZQ==\n-----END PRIVATE KEY-----",
  });
  const result = redactText(raw);
  expect(result.text).not.toContain("leaked");
  expect(result.text).not.toContain("ZmFrZQ");
  expect(result.text).toContain("<redacted-private-key>");
});

test("redacts the fictional fixture mobiles", () => {
  const result = redactText("login 13900000000 and 13900000014");
  expect(result.text).not.toContain("13900000000");
  expect(result.text).not.toContain("13900000014");
  expect(result.text).toContain("<redacted-phone>");
});

test("truncation is explicit and bounded", () => {
  const result = redactText("x".repeat(100_000), 128);
  expect(result.truncated).toBe(true);
  expect(result.text.endsWith("…[truncated]")).toBe(true);
  expect(result.text.length).toBeLessThanOrEqual(128 + "…[truncated]".length);
});

test.each([
  '-   "value": "opaque-cookie-before",\n+   "value": "opaque-cookie-after",',
  String.raw`{"name":"__Host-nucleus-session-example","value":"opaque-cookie-before"}`,
  String.raw`{\"name\":\"__Host-nucleus-session-example\",\"value\":\"opaque-cookie-after\"}`,
])("redacts opaque cookie-object values in assertion diffs: %s", (diff) => {
  const result = redactText(`expect(received).toEqual(expected)\n${diff}\n at login.spec.ts:2060:7`);
  expect(result.text).not.toContain("opaque-cookie-");
  expect(result.text).toContain("<redacted>");
  expect(result.text).toContain("expect(received).toEqual(expected)");
  expect(result.text).toContain("login.spec.ts:2060:7");
});

test("values longer than the former 4096 limit leave no suffix", () => {
  const secret = `${"a".repeat(5_000)}END`;
  const result = redactText(`authorization: ${secret}\ncookie=${secret}`);
  expect(result.text).not.toContain("aaaaa");
  expect(result.text).not.toContain("END");
  expect(result.text).toContain("<redacted>");
});

test.each([
  "__Host-nucleus-session-00000000-0000-4000-8000-000000000001=opaque-cookie-scalar; Path=/; Secure; HttpOnly",
  '"__Host-nucleus-session-00000000-0000-4000-8000-000000000001=opaque-cookie-scalar"',
  JSON.stringify({ received: "__Host-nucleus-session-example=opaque-cookie-scalar; Path=/" }),
  JSON.stringify(JSON.stringify("__Host-nucleus-session-example=opaque-cookie-scalar; Path=/")),
  "__Host-nucleus-session-first=opaque-cookie-scalar; __Host-nucleus-session-second=opaque-cookie-scalar",
])("redacts scalar session cookie assignments without a header label: %s", (value) => {
  const result = redactText(`expect(received).toBeUndefined()\nReceived: ${value}\n at login.spec.ts:2116:7`);
  expect(result.text).not.toContain("opaque-cookie-scalar");
  expect(result.text).toContain("<redacted>");
  expect(result.text).toContain("expect(received).toBeUndefined()");
  expect(result.text).toContain("login.spec.ts:2116:7");
});

test("percent-encoded query names are still removed", () => {
  const result = redactText(
    "https://example.test/cb?access%5Ftoken=leaked&state=ok&%73ig=also-secret",
  );
  expect(result.text).not.toContain("leaked");
  expect(result.text).not.toContain("also-secret");
  expect(result.text).toContain("state=ok");
});

test("escaped URLs, fragments and signed avatar links are removed", () => {
  const raw = String.raw`{"redirect":"https:\/\/example.test\/cb?access_token=leaked#fragment-token"}`;
  const result = redactText(raw);
  expect(result.text).not.toContain("leaked");
  expect(result.text).not.toContain("fragment-token");
  const avatar = redactText(
    "https://media.test/api/v1/avatars/abc.webp?key_id=7&expires_at=1&signature=secret#x",
  );
  expect(avatar.text).not.toContain("key_id");
  expect(avatar.text).not.toContain("secret");
  expect(avatar.text).toContain("<redacted-query>");
});

test("sanitization happens before truncation", () => {
  const prefix = "x".repeat(6_000);
  const secret = `token=${"s".repeat(4_000)}`;
  const result = redactText(`${prefix}\n${secret}\n${"y".repeat(5_000)}`, 7_000);
  expect(result.truncated).toBe(true);
  expect(result.text).not.toContain("sssss");
});
