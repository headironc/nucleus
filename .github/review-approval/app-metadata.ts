import { createPrivateKey } from "node:crypto";
import { appJwt } from "./app-jwt.ts";
import {
  appId,
  installationId,
  ownerId,
  repository,
  repositoryId,
  sha,
} from "./identity.ts";

export const metadataWorkflow = ".github/workflows/app-metadata.yml";
const source = "cladiron-app-metadata";
type Stage = "context" | "credentials" | "app" | "installation" | "repository";
type Reason =
  | "untrusted-execution"
  | "missing-credential"
  | "invalid-rsa-credential"
  | "invalid-clock"
  | "http-status"
  | "timeout"
  | "transport-unknown"
  | "invalid-response"
  | "identity-mismatch"
  | "installation-unavailable"
  | "invalid-permissions"
  | "permissions-changed";

class MetadataFailure extends Error {
  constructor(
    readonly stage: Stage,
    readonly reason: Reason,
    readonly status?: number,
  ) {
    super(`${source}: ${stage}: ${reason}`);
    Object.freeze(this);
  }
}

export function metadataError(error: unknown): {
  source: string;
  stage: Stage | "internal";
  reason: Reason | "unexpected";
  status?: number;
} {
  return error instanceof MetadataFailure
    ? {
        source,
        stage: error.stage,
        reason: error.reason,
        ...(error.status === undefined ? {} : { status: error.status }),
      }
    : { source, stage: "internal", reason: "unexpected" };
}

function requireMetadata(
  condition: unknown,
  stage: Stage,
  reason: Reason,
): asserts condition {
  if (!condition) throw new MetadataFailure(stage, reason);
}

function positive(value: string | undefined) {
  return (
    typeof value === "string" &&
    /^[1-9][0-9]*$/.test(value) &&
    Number.isSafeInteger(Number(value))
  );
}

export function metadataContext(env: NodeJS.ProcessEnv) {
  const revision = env.GITHUB_WORKFLOW_SHA;
  const run = env.GITHUB_RUN_ID;
  const attempt = env.GITHUB_RUN_ATTEMPT;
  requireMetadata(
    env.GITHUB_EVENT_NAME === "workflow_dispatch" &&
      env.GITHUB_REPOSITORY === repository &&
      env.GITHUB_REPOSITORY_ID === String(repositoryId) &&
      env.GITHUB_REPOSITORY_OWNER_ID === String(ownerId) &&
      env.GITHUB_REF === "refs/heads/dev" &&
      env.GITHUB_ACTOR_ID === String(ownerId) &&
      env.CLADIRON_TRIGGERING_ACTOR_ID === String(ownerId) &&
      env.GITHUB_WORKFLOW_REF ===
        `${repository}/${metadataWorkflow}@refs/heads/dev` &&
      sha(revision) &&
      env.GITHUB_SHA === revision &&
      positive(run) &&
      positive(attempt),
    "context",
    "untrusted-execution",
  );
  return {
    workflow: metadataWorkflow,
    run: Number(run),
    attempt: Number(attempt),
    revision: revision!,
  };
}

function record(value: unknown, stage: Stage) {
  requireMetadata(
    value !== null && typeof value === "object" && !Array.isArray(value),
    stage,
    "invalid-response",
  );
  return value as Record<string, unknown>;
}

function permissions(value: unknown, stage: Stage) {
  const input = record(value, stage);
  const level = (name: "metadata" | "pull_requests") => {
    if (!Object.hasOwn(input, name)) return null;
    const value = input[name];
    requireMetadata(
      value === "read" || value === "write" || value === "admin",
      stage,
      "invalid-permissions",
    );
    return value;
  };
  return {
    metadata: level("metadata"),
    pull_requests: level("pull_requests"),
    otherPermissionCount: Object.keys(input).filter(
      (key) => key !== "metadata" && key !== "pull_requests",
    ).length,
  };
}

function installation(value: unknown, stage: "installation" | "repository") {
  const input = record(value, stage);
  const account = record(input.account, stage);
  requireMetadata(
    input.id === installationId &&
      input.app_id === appId &&
      account.id === ownerId &&
      account.type === "User",
    stage,
    "identity-mismatch",
  );
  requireMetadata(
    input.repository_selection === "selected" &&
      input.suspended_at === null &&
      input.suspended_by === null,
    stage,
    "installation-unavailable",
  );
  return permissions(input.permissions, stage);
}

/** This entry never creates an installation token or enters approval recovery. */
export async function appMetadata(
  env: NodeJS.ProcessEnv,
  transport: typeof fetch = fetch,
  clock: () => number = Date.now,
) {
  let stage: Stage = "context";
  try {
    const execution = metadataContext(env);
    stage = "credentials";
    const clientId = env.CLADIRON_CLIENT_ID;
    const pem = env.CLADIRON_PRIVATE_KEY;
    requireMetadata(
      typeof clientId === "string" &&
        clientId.length > 0 &&
        clientId.trim() === clientId &&
        typeof pem === "string" &&
        pem.length > 0,
      "credentials",
      "missing-credential",
    );
    const started = clock();
    requireMetadata(
      Number.isSafeInteger(started) && started > 0,
      "credentials",
      "invalid-clock",
    );
    let jwt: string;
    try {
      const key = createPrivateKey(pem);
      requireMetadata(
        key.asymmetricKeyType === "rsa",
        "credentials",
        "invalid-rsa-credential",
      );
      jwt = appJwt(clientId, key, started);
    } catch {
      throw new MetadataFailure("credentials", "invalid-rsa-credential");
    }

    const read = async (
      stage: "app" | "installation" | "repository",
      path: string,
    ): Promise<unknown> => {
      const signal = AbortSignal.timeout(30_000);
      try {
        const response = await transport(`https://api.github.com${path}`, {
          method: "GET",
          headers: {
            Authorization: `Bearer ${jwt}`,
            Accept: "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28",
          },
          // Do not follow redirects; retain an observed 301 without its Location.
          redirect: "manual",
          signal,
        });
        const status = response.status;
        requireMetadata(
          Number.isInteger(status) &&
            status >= 100 &&
            status <= 599 &&
            response.redirected !== true,
          stage,
          "invalid-response",
        );
        if (status !== 200)
          throw new MetadataFailure(stage, "http-status", status);
        try {
          return await response.json();
        } catch {
          throw new MetadataFailure(
            stage,
            signal.aborted ? "timeout" : "invalid-response",
          );
        }
      } catch (error) {
        if (error instanceof MetadataFailure) throw error;
        throw new MetadataFailure(
          stage,
          signal.aborted ? "timeout" : "transport-unknown",
        );
      }
    };

    stage = "app";
    const app = record(await read("app", "/app"), "app");
    const owner = record(app.owner, "app");
    requireMetadata(
      app.id === appId &&
        app.slug === "cladiron" &&
        app.client_id === clientId &&
        owner.id === ownerId &&
        owner.type === "User",
      "app",
      "identity-mismatch",
    );
    stage = "installation";
    const installed = installation(
      await read("installation", `/app/installations/${installationId}`),
      "installation",
    );
    stage = "repository";
    const associated = installation(
      await read("repository", `/repos/${repository}/installation`),
      "repository",
    );
    requireMetadata(
      JSON.stringify(installed) === JSON.stringify(associated),
      "repository",
      "permissions-changed",
    );
    const observed = clock();
    requireMetadata(
      Number.isSafeInteger(observed) && observed >= started,
      "repository",
      "invalid-clock",
    );
    return {
      version: 1,
      source: { kind: source, ...execution },
      observedAt: new Date(observed).toISOString(),
      app: { id: appId, slug: "cladiron", ownerId },
      installation: {
        id: installationId,
        appId,
        accountId: ownerId,
        repositorySelection: "selected",
        suspended: false,
        permissionsSource: "installation-metadata",
        permissions: associated,
      },
      repository: {
        name: repository,
        id: repositoryId,
        idSource: "trusted-actions-context",
        installationId,
        associationSource: `GET /repos/${repository}/installation`,
      },
    };
  } catch (error) {
    if (error instanceof MetadataFailure) throw error;
    throw new MetadataFailure(stage, "invalid-response");
  }
}

if (import.meta.main) {
  try {
    console.log(JSON.stringify(await appMetadata(process.env), null, 2));
  } catch (error) {
    console.error(JSON.stringify(metadataError(error)));
    process.exitCode = 1;
  }
}
