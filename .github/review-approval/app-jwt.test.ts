import { expect, test } from "bun:test";
import { createVerify, generateKeyPairSync } from "node:crypto";
import { appJwt } from "./app-jwt.ts";

const keys = generateKeyPairSync("rsa", { modulusLength: 2048 });
test("App JWT uses the documented RS256 bytes and one bounded lifetime", () => {
  const now = 1_793_404_800_987;
  const token = appJwt("synthetic-client", keys.privateKey, now);
  const [header, payload, signature] = token.split(".");
  expect(JSON.parse(Buffer.from(header, "base64url").toString())).toEqual({
    alg: "RS256",
    typ: "JWT",
  });
  expect(JSON.parse(Buffer.from(payload, "base64url").toString())).toEqual({
    iat: 1_793_404_740,
    exp: 1_793_405_340,
    iss: "synthetic-client",
  });
  const verify = createVerify("RSA-SHA256");
  verify.update(`${header}.${payload}`);
  expect(verify.verify(keys.publicKey, signature, "base64url")).toBe(true);
});

test("PEM and parsed keys preserve the existing credential signature", () => {
  const pem = keys.privateKey.export({ type: "pkcs8", format: "pem" });
  expect(appJwt("synthetic-client", pem, 1_000_000)).toBe(
    appJwt("synthetic-client", keys.privateKey, 1_000_000),
  );
  expect(() => appJwt("synthetic-client", "not-a-key", 1_000_000)).toThrow();
});
