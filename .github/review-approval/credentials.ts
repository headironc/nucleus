import { createSign } from "node:crypto";
import {
  appId,
  installationId,
  ownerId,
  repositoryId,
  requireThat,
} from "./identity.ts";

export async function installationToken(env = process.env) {
  requireThat(
    env.CLADIRON_CLIENT_ID && env.CLADIRON_PRIVATE_KEY,
    "App credential metadata/value unavailable",
  );
  const encode = (v: unknown) =>
    Buffer.from(JSON.stringify(v)).toString("base64url");
  const now = Math.floor(Date.now() / 1000);
  const message = `${encode({ alg: "RS256", typ: "JWT" })}.${encode({ iat: now - 60, exp: now + 540, iss: env.CLADIRON_CLIENT_ID })}`;
  const signer = createSign("RSA-SHA256");
  signer.update(message);
  const jwt = `${message}.${signer.sign(env.CLADIRON_PRIVATE_KEY, "base64url")}`;
  const request = async (path: string, method = "GET", body?: unknown) => {
    const response = await fetch(`https://api.github.com${path}`, {
      method,
      headers: {
        Authorization: `Bearer ${jwt}`,
        Accept: "application/vnd.github+json",
        "X-GitHub-Api-Version": "2022-11-28",
        "Content-Type": "application/json",
      },
      body: body ? JSON.stringify(body) : undefined,
      redirect: "error",
      signal: AbortSignal.timeout(30000),
    });
    requireThat(
      response.ok,
      `App ${method} ${path} returned ${response.status}`,
    );
    return response.json() as Promise<any>;
  };
  const app = await request("/app");
  requireThat(
    app.id === appId && app.slug === "cladiron" && app.owner?.id === ownerId,
    "Configured credential is not the expected Cladiron App",
  );
  const installation = await request(`/app/installations/${installationId}`);
  requireThat(
    installation.id === installationId &&
      installation.app_id === appId &&
      installation.account?.id === ownerId &&
      installation.repository_selection === "selected" &&
      installation.suspended_at === null,
    "Cladiron installation target changed",
  );
  const result = await request(
    `/app/installations/${installationId}/access_tokens`,
    "POST",
    { repository_ids: [repositoryId], permissions: { pull_requests: "write" } },
  );
  async function revoke() {
    const r = await fetch("https://api.github.com/installation/token", {
      method: "DELETE",
      headers: {
        Authorization: `Bearer ${result.token}`,
        "X-GitHub-Api-Version": "2022-11-28",
      },
      redirect: "error",
      signal: AbortSignal.timeout(30000),
    });
    requireThat(
      r.status === 204,
      "Installation token revocation not confirmed",
    );
  }
  try {
    requireThat(
      typeof result.token === "string" &&
        result.token.length > 0 &&
        result.permissions?.pull_requests === "write" &&
        Object.keys(result.permissions).every((k) =>
          ["metadata", "pull_requests"].includes(k),
        ) &&
        result.repositories?.length === 1 &&
        result.repositories[0].id === repositoryId,
      "Installation token scope mismatch",
    );
  } catch (error) {
    if (typeof result.token === "string" && result.token.length > 0)
      await revoke();
    throw error;
  }
  return { token: result.token as string, revoke };
}
