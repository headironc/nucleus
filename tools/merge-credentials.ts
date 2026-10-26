import { sign } from "node:crypto";
import { isDeepStrictEqual } from "node:util";
import { array, field, parse, string, wire, type Json } from "./review-json";
import { appId, ownerId, repository, repositoryId } from "./merge-deployment";

export type Api = (path: string, method?: string, body?: Json) => Promise<Json>;
export class ApiFailure extends Error {
  constructor(readonly status: number) {
    super(`GitHub request failed (${status}); no retry`);
  }
}
export function api(token: string): Api {
  return async (path, method = "GET", body) => {
    if (!/^[a-zA-Z0-9_/?=&.%-]+$/.test(path) || path.split("/").includes(".."))
      throw new Error("Invalid GitHub API path");
    const response = await fetch(`https://api.github.com/${path}`, {
      method,
      redirect: "error",
      signal: AbortSignal.timeout(60_000),
      headers: {
        Authorization: `Bearer ${token}`,
        Accept: "application/vnd.github+json",
        "Content-Type": "application/json",
        "X-GitHub-Api-Version": "2026-03-10",
      },
      ...(body === undefined ? {} : { body: wire(body) }),
    });
    if (!response.ok) throw new ApiFailure(response.status);
    return response.status === 204 ? null : parse(await response.text());
  };
}

export const appPermissions = {
  metadata: "read",
  contents: "write",
  pull_requests: "write",
  checks: "write",
  statuses: "read",
  issues: "write",
};
export const scopes = {
  review: {
    metadata: "read",
    contents: "read",
    pull_requests: "write",
    checks: "write",
    statuses: "read",
  },
  record: { metadata: "read", issues: "write" },
  merge: {
    metadata: "read",
    contents: "write",
    pull_requests: "read",
    checks: "read",
    statuses: "read",
  },
} as const;
export type Credentials<K extends keyof typeof scopes = keyof typeof scopes> = {
  installation: bigint;
  tokens: { [P in K]: string };
};

function owner(value: Json): void {
  if (
    field(value, "id") !== ownerId ||
    field(value, "login") !== "headironc" ||
    field(value, "type") !== "User"
  )
    throw new Error("Trusted merge account identity mismatch");
}
export function verifyRepository(value: Json): void {
  if (
    field(value, "id") !== repositoryId ||
    field(value, "full_name") !== repository ||
    field(value, "private") !== true ||
    field(value, "default_branch") !== "dev"
  )
    throw new Error("Trusted merge repository identity mismatch");
  owner(field(value, "owner"));
}
export function verifyInstallation(value: Json): bigint {
  const id = field(value, "id");
  owner(field(value, "account"));
  if (
    typeof id !== "bigint" ||
    id <= 0n ||
    field(value, "app_id") !== appId ||
    field(value, "app_slug") !== "cladiron" ||
    field(value, "target_type") !== "User" ||
    field(value, "suspended_at") !== null ||
    field(value, "repository_selection") !== "selected" ||
    !isDeepStrictEqual(field(value, "permissions"), appPermissions)
  )
    throw new Error("Trusted merge installation or permission mismatch");
  return id;
}

export async function revoke(tokens: string[], transport = api): Promise<void> {
  const results = await Promise.allSettled(
    tokens.map((token) => transport(token)("installation/token", "DELETE")),
  );
  if (results.some((result) => result.status === "rejected"))
    throw new Error(
      "Token cleanup is unconfirmed; validity may continue until expiry",
    );
}

// Called only after source/run/deployment verification. No token is exported to a later job.
export function mint(
  key: string,
  transport = api,
  mask = (token: string) => console.log(`::add-mask::${token}`),
): Promise<Credentials> {
  return issue(key, ["review", "record", "merge"], transport, mask);
}

export function mintReview(
  key: string,
  transport = api,
  mask = (token: string) => console.log(`::add-mask::${token}`),
): Promise<Credentials<"review">> {
  return issue(key, ["review"], transport, mask);
}

async function issue<K extends keyof typeof scopes>(
  key: string,
  purposes: readonly K[],
  transport: typeof api,
  mask: (token: string) => void,
): Promise<Credentials<K>> {
  const at = Math.floor(Date.now() / 1000);
  const encode = (value: Json) =>
    Buffer.from(wire(value)).toString("base64url");
  const unsigned = `${encode({ alg: "RS256", typ: "JWT" })}.${encode({ iat: at - 60, exp: at + 540, iss: appId })}`;
  const jwt = `${unsigned}.${sign("RSA-SHA256", Buffer.from(unsigned), key).toString("base64url")}`;
  const app = transport(jwt),
    actual = await app("app");
  owner(field(actual, "owner"));
  if (
    field(actual, "id") !== appId ||
    field(actual, "slug") !== "cladiron" ||
    !isDeepStrictEqual(field(actual, "permissions"), appPermissions)
  )
    throw new Error("Trusted merge App identity or permission mismatch");
  const installation = async () =>
    verifyInstallation(await app(`repos/${repository}/installation`));
  const id = await installation(),
    known: string[] = [];
  const tokens = {} as Credentials<K>["tokens"];
  try {
    for (const name of purposes) {
      if ((await installation()) !== id)
        throw new Error("Installation changed during issuance");
      const response = await app(
        `app/installations/${id}/access_tokens`,
        "POST",
        {
          repository_ids: [repositoryId],
          permissions: scopes[name],
        },
      );
      const token = string(field(response, "token"));
      if (!token || /[\r\n]/.test(token))
        throw new Error("Invalid credential response");
      known.push(token);
      mask(token);
      const repos = array(field(response, "repositories"));
      const expiry = Date.parse(string(field(response, "expires_at")));
      if (
        repos.length !== 1 ||
        field(repos[0], "id") !== repositoryId ||
        field(repos[0], "full_name") !== repository ||
        !isDeepStrictEqual(field(response, "permissions"), scopes[name]) ||
        !Number.isFinite(expiry) ||
        expiry <= Date.now() ||
        expiry > Date.now() + 3_660_000
      )
        throw new Error("Returned credential scope or expiry mismatch");
      tokens[name] = token;
    }
    if ((await installation()) !== id)
      throw new Error("Installation changed during issuance");
    return { installation: id, tokens };
  } catch (error) {
    try {
      await revoke(known, transport);
    } catch (cleanup) {
      throw new AggregateError([error, cleanup], "Issuance and cleanup failed");
    }
    throw error;
  }
}
