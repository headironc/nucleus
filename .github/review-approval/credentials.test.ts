import { afterEach, expect, test } from "bun:test";
import { generateKeyPairSync } from "node:crypto";
import { installationToken } from "./credentials.ts";

// Generated in memory for a fully mocked transport; never saved or sent to GitHub.
const { privateKey } = generateKeyPairSync("rsa", {
  modulusLength: 2048,
  privateKeyEncoding: { type: "pkcs8", format: "pem" },
  publicKeyEncoding: { type: "spki", format: "pem" },
});
const env = {
  CLADIRON_CLIENT_ID: "synthetic-client",
  CLADIRON_PRIVATE_KEY: privateKey,
};
const originalFetch = globalThis.fetch;
afterEach(() => {
  globalThis.fetch = originalFetch;
});
function transport(
  options: {
    wrongApp?: boolean;
    extraScope?: boolean;
    failMint?: boolean;
  } = {},
) {
  const calls: any[] = [];
  globalThis.fetch = (async (input: any, init: any) => {
    const url = String(input);
    calls.push({ url, ...init });
    if (url.endsWith("/app"))
      return Response.json({
        id: options.wrongApp ? 1 : 3104501,
        slug: "cladiron",
        owner: { id: 19648989 },
      });
    if (url.endsWith("/app/installations/175475891"))
      return Response.json({
        id: 175475891,
        app_id: 3104501,
        account: { id: 19648989 },
        repository_selection: "selected",
        suspended_at: null,
      });
    if (url.endsWith("/access_tokens")) {
      if (options.failMint) throw new Error("Synthetic unknown issuance");
      return Response.json({
        token: "synthetic-not-an-access-token",
        permissions: {
          metadata: "read",
          pull_requests: "write",
          ...(options.extraScope ? { contents: "write" } : {}),
        },
        repositories: [{ id: 1432619931 }],
      });
    }
    if (url.endsWith("/installation/token"))
      return new Response(null, { status: 204 });
    throw new Error("No real network is permitted by this fixture");
  }) as unknown as typeof fetch;
  return calls;
}
test("only selected installation/repository PR-write is requested; credential is revoked", async () => {
  const calls = transport();
  const result = await installationToken(env);
  expect(
    JSON.parse(calls.find((c) => c.url.endsWith("/access_tokens")).body),
  ).toEqual({
    repository_ids: [1432619931],
    permissions: { pull_requests: "write" },
  });
  await result.revoke();
  expect(calls.at(-1).method).toBe("DELETE");
});
test("different App cannot mint and excess scope is revoked before use", async () => {
  const wrong = transport({ wrongApp: true });
  await expect(installationToken(env)).rejects.toThrow("expected Cladiron");
  expect(wrong).toHaveLength(1);
  const excess = transport({ extraScope: true });
  await expect(installationToken(env)).rejects.toThrow("scope mismatch");
  expect(excess.at(-1).method).toBe("DELETE");
});
test("unknown token issuance is not retried", async () => {
  const calls = transport({ failMint: true });
  await expect(installationToken(env)).rejects.toThrow("unknown issuance");
  expect(calls.filter((c) => c.url.endsWith("/access_tokens"))).toHaveLength(1);
});
