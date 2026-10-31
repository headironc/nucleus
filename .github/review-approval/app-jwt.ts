import { createSign, type KeyLike } from "node:crypto";

// Callers own credential sources and error handling; the JWT stays in memory.
export function appJwt(clientId: string, key: KeyLike, now: number) {
  const encode = (value: unknown) =>
    Buffer.from(JSON.stringify(value)).toString("base64url");
  const seconds = Math.floor(now / 1000);
  const message = `${encode({ alg: "RS256", typ: "JWT" })}.${encode({ iat: seconds - 60, exp: seconds + 540, iss: clientId })}`;
  const signer = createSign("RSA-SHA256");
  signer.update(message);
  return `${message}.${signer.sign(key, "base64url")}`;
}
