import { afterEach, beforeEach, expect, spyOn, test } from "bun:test";
import { createVerify, generateKeyPairSync } from "node:crypto";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { appMetadata, metadataContext, metadataError } from "./app-metadata.ts";

// These keys are generated only in memory and are never sent to a real service.
const keys = generateKeyPairSync("rsa", { modulusLength: 2048 });
const privateKey = keys.privateKey
  .export({ type: "pkcs8", format: "pem" })
  .toString();
const now = 1_793_404_800_000;
const revision = "a".repeat(40);
const secret = "synthetic-sensitive-provider-text";
const env = {
  GITHUB_EVENT_NAME: "workflow_dispatch",
  GITHUB_REPOSITORY: "headironc/nucleus",
  GITHUB_REPOSITORY_ID: "1432619931",
  GITHUB_REPOSITORY_OWNER_ID: "19648989",
  GITHUB_REF: "refs/heads/dev",
  GITHUB_ACTOR_ID: "19648989",
  CLADIRON_TRIGGERING_ACTOR_ID: "19648989",
  GITHUB_WORKFLOW_REF:
    "headironc/nucleus/.github/workflows/app-metadata.yml@refs/heads/dev",
  GITHUB_WORKFLOW_SHA: revision,
  GITHUB_SHA: revision,
  GITHUB_RUN_ID: "987654321",
  GITHUB_RUN_ATTEMPT: "2",
  CLADIRON_CLIENT_ID: "synthetic-client",
  CLADIRON_PRIVATE_KEY: privateKey,
};
const paths = [
  "https://api.github.com/app",
  "https://api.github.com/app/installations/175475891",
  "https://api.github.com/repos/headironc/nucleus/installation",
];
const stages = ["app", "installation", "repository"] as const;
const originalFetch = globalThis.fetch;
beforeEach(() => {
  globalThis.fetch = (() => {
    throw new Error("Unmocked network is forbidden");
  }) as unknown as typeof fetch;
});
afterEach(() => {
  globalThis.fetch = originalFetch;
});

function responses(): any[] {
  const installation = {
    id: 175475891,
    app_id: 3104501,
    account: { id: 19648989, type: "User" },
    repository_selection: "selected",
    suspended_at: null,
    suspended_by: null,
    permissions: { metadata: "read", pull_requests: "write", contents: "read" },
    extra: secret,
  };
  return [
    {
      id: 3104501,
      slug: "cladiron",
      client_id: "synthetic-client",
      owner: { id: 19648989, type: "User" },
      extra: secret,
    },
    structuredClone(installation),
    structuredClone(installation),
  ];
}
function fixture(
  values = responses(),
  handle?: (index: number, init: RequestInit) => Response | Promise<Response>,
) {
  const calls: { url: string; init: RequestInit }[] = [];
  const send = (async (input: any, init: RequestInit) => {
    const index = calls.length;
    const url = String(input);
    calls.push({ url, init });
    if (url !== paths[index] || init.method !== "GET")
      throw new Error("Unexpected target or mutation in mocked transport");
    return handle ? handle(index, init) : Response.json(values[index]);
  }) as typeof fetch;
  return { send, calls };
}
async function rejected(operation: Promise<unknown>) {
  try {
    await operation;
  } catch (error) {
    expect(error).toBeInstanceOf(Error);
    const message = String((error as Error).message);
    expect(message).not.toContain(secret);
    expect(message).not.toContain(privateKey);
    expect((error as Error).cause).toBeUndefined();
    expect(JSON.stringify(metadataError(error))).not.toContain(secret);
    return metadataError(error);
  }
  throw new Error("Expected the metadata operation to reject");
}

test("success uses exactly three fixed GETs, one JWT and a narrow safe result", async () => {
  const f = fixture();
  const result = await appMetadata(env, f.send, () => now);
  expect(result).toEqual({
    version: 1,
    source: {
      kind: "cladiron-app-metadata",
      workflow: ".github/workflows/app-metadata.yml",
      run: 987654321,
      attempt: 2,
      revision,
    },
    observedAt: "2026-10-31T00:00:00.000Z",
    app: { id: 3104501, slug: "cladiron", ownerId: 19648989 },
    installation: {
      id: 175475891,
      appId: 3104501,
      accountId: 19648989,
      repositorySelection: "selected",
      suspended: false,
      permissionsSource: "installation-metadata",
      permissions: {
        metadata: "read",
        pull_requests: "write",
        otherPermissionCount: 1,
      },
    },
    repository: {
      name: "headironc/nucleus",
      id: 1432619931,
      idSource: "trusted-actions-context",
      installationId: 175475891,
      associationSource: "GET /repos/headironc/nucleus/installation",
    },
  });
  expect(f.calls.map((c) => c.url)).toEqual(paths);
  const tokens = f.calls.map((c) =>
    new Headers(c.init.headers).get("Authorization")!,
  );
  expect(new Set(tokens).size).toBe(1);
  const jwt = tokens[0].slice("Bearer ".length);
  const [header, payload, signature] = jwt.split(".");
  const verify = createVerify("RSA-SHA256");
  verify.update(`${header}.${payload}`);
  expect(verify.verify(keys.publicKey, signature, "base64url")).toBe(true);
  expect(JSON.parse(Buffer.from(payload, "base64url").toString())).toEqual({
    iat: 1_793_404_740,
    exp: 1_793_405_340,
    iss: "synthetic-client",
  });
  for (const c of f.calls) {
    expect(c.init.method).toBe("GET");
    expect(c.init.redirect).toBe("manual");
    expect(c.init.body).toBeUndefined();
    expect(c.init.signal).toBeInstanceOf(AbortSignal);
    expect(new Headers(c.init.headers).get("X-GitHub-Api-Version")).toBe(
      "2022-11-28",
    );
  }
  const output = JSON.stringify(result);
  for (const hidden of [
    secret,
    privateKey,
    jwt,
    "contents",
    "client_id",
    "Authorization",
  ])
    expect(output).not.toContain(hidden);
});

for (const level of [undefined, "read", "write", "admin"])
  test(`reports the actual PR permission including absence: ${level}`, async () => {
    const values = responses();
    for (const i of [1, 2]) {
      if (level === undefined) delete values[i].permissions.pull_requests;
      else values[i].permissions.pull_requests = level;
    }
    const f = fixture(values);
    expect(
      (await appMetadata(env, f.send, () => now)).installation.permissions
        .pull_requests,
    ).toBe(level ?? null);
  });

test("additional permission names and values never enter output", async () => {
  const values = responses();
  for (const i of [1, 2]) values[i].permissions[secret] = secret;
  const result = await appMetadata(env, fixture(values).send, () => now);
  expect(result.installation.permissions.otherPermissionCount).toBe(2);
  expect(JSON.stringify(result)).not.toContain(secret);
});

for (const [field, value] of Object.entries({
  GITHUB_EVENT_NAME: "pull_request_target",
  GITHUB_REPOSITORY: "other/nucleus",
  GITHUB_REPOSITORY_ID: "1",
  GITHUB_REPOSITORY_OWNER_ID: "1",
  GITHUB_REF: "refs/heads/main",
  GITHUB_ACTOR_ID: "1",
  CLADIRON_TRIGGERING_ACTOR_ID: "1",
  GITHUB_WORKFLOW_REF:
    "headironc/nucleus/.github/workflows/other.yml@refs/heads/dev",
  GITHUB_WORKFLOW_SHA: "bad-revision",
  GITHUB_SHA: "b".repeat(40),
  GITHUB_RUN_ID: "9007199254740992",
  GITHUB_RUN_ATTEMPT: "0",
}))
  test(`rejects execution ${field} before credentials or transport`, async () => {
    const candidate = { ...env, [field]: value };
    let reads = 0;
    Object.defineProperty(candidate, "CLADIRON_PRIVATE_KEY", {
      get() {
        reads++;
        throw new Error(secret);
      },
    });
    const f = fixture();
    expect(
      await rejected(appMetadata(candidate, f.send, () => now)),
    ).toMatchObject({ stage: "context", reason: "untrusted-execution" });
    expect(reads).toBe(0);
    expect(f.calls).toHaveLength(0);
  });

test("missing context, credentials and invalid keys never request metadata", async () => {
  expect(() => metadataContext({})).toThrow();
  const ec = generateKeyPairSync("ec", { namedCurve: "P-256" })
    .privateKey.export({ type: "pkcs8", format: "pem" })
    .toString();
  for (const delta of [
    { CLADIRON_CLIENT_ID: "" },
    { CLADIRON_CLIENT_ID: " padded " },
    { CLADIRON_PRIVATE_KEY: "" },
    { CLADIRON_PRIVATE_KEY: secret },
    { CLADIRON_PRIVATE_KEY: ec },
  ]) {
    const f = fixture();
    expect(
      await rejected(appMetadata({ ...env, ...delta }, f.send, () => now)),
    ).toMatchObject({ stage: "credentials" });
    expect(f.calls).toHaveLength(0);
  }
});

for (const [field, value] of [
  ["id", 1],
  ["id", "3104501"],
  ["slug", secret],
  ["client_id", "other-client"],
  ["owner", { id: 1, type: "User" }],
  ["owner", null],
  ["owner", { id: 19648989, type: "Organization" }],
] as const)
  test(`rejects wrong App identity ${field}/${JSON.stringify(value)}`, async () => {
    const values = responses();
    values[0][field] = value;
    const f = fixture(values);
    expect(await rejected(appMetadata(env, f.send, () => now))).toMatchObject({
      stage: "app",
    });
    expect(f.calls).toHaveLength(1);
  });

for (const index of [1, 2])
  for (const [field, value] of [
    ["id", 1432619931],
    ["app_id", 1],
    ["account", { id: 1, type: "User" }],
    ["account", null],
    ["repository_selection", "all"],
    ["repository_selection", undefined],
    ["suspended_at", "2026-01-01T00:00:00Z"],
    ["suspended_at", undefined],
    ["suspended_by", { id: 1 }],
    ["suspended_by", undefined],
    ["permissions", null],
    ["permissions", []],
    ["permissions", undefined],
    ["permissions", { metadata: "read", pull_requests: secret }],
  ] as const)
    test(`rejects ${stages[index]} bad field ${field}/${JSON.stringify(value)}`, async () => {
      const values = responses();
      values[index][field] = value;
      const f = fixture(values);
      expect(await rejected(appMetadata(env, f.send, () => now))).toMatchObject(
        { stage: stages[index] },
      );
      expect(f.calls).toHaveLength(index + 1);
    });

for (const index of [0, 1, 2]) {
  for (const invalid of [null, [], secret, 1])
    test(`rejects ${stages[index]} non-object response ${JSON.stringify(invalid)}`, async () => {
      const values = responses();
      values[index] = invalid;
      const f = fixture(values);
      expect(await rejected(appMetadata(env, f.send, () => now))).toMatchObject(
        { stage: stages[index], reason: "invalid-response" },
      );
      expect(f.calls).toHaveLength(index + 1);
    });
  for (const status of [201, 301, 401, 403, 404, 500])
    test(`stops once at ${stages[index]} HTTP ${status} without reading error body`, async () => {
      const values = responses();
      let errorBodyReads = 0;
      const f = fixture(values, (i) => {
        if (i !== index) return Response.json(values[i]);
        const response = new Response(secret, {
          status,
          headers: { Location: "https://outside.invalid/secret" },
        });
        Object.defineProperty(response, "json", {
          value: async () => {
            errorBodyReads++;
            throw new Error(secret);
          },
        });
        return response;
      });
      expect(await rejected(appMetadata(env, f.send, () => now))).toEqual({
        source: "cladiron-app-metadata",
        stage: stages[index],
        reason: "http-status",
        status,
      });
      expect(f.calls).toHaveLength(index + 1);
      expect(errorBodyReads).toBe(0);
    });
  test(`unknown transport outcome at ${stages[index]} is safe and never retried`, async () => {
    const values = responses();
    const f = fixture(values, (i, init) => {
      if (i === index)
        throw new Error(
          `${secret} ${privateKey} ${new Headers(init.headers).get("Authorization")}`,
        );
      return Response.json(values[i]);
    });
    expect(await rejected(appMetadata(env, f.send, () => now))).toMatchObject({
      stage: stages[index],
      reason: "transport-unknown",
    });
    expect(f.calls).toHaveLength(index + 1);
  });
  for (const body of [false, true])
    test(`timeout at ${stages[index]} (${body ? "body" : "request"}) retains its stage`, async () => {
      const controllers: AbortController[] = [];
      const timer = spyOn(AbortSignal, "timeout").mockImplementation((ms) => {
        expect(ms).toBe(30_000);
        const c = new AbortController();
        controllers.push(c);
        return c.signal;
      });
      try {
        const values = responses();
        const f = fixture(values, (i) => {
          if (i !== index) return Response.json(values[i]);
          if (!body) {
            controllers[i].abort();
            throw new Error(secret);
          }
          const response = Response.json(values[i]);
          Object.defineProperty(response, "json", {
            value: async () => {
              controllers[i].abort();
              throw new Error(secret);
            },
          });
          return response;
        });
        expect(
          await rejected(appMetadata(env, f.send, () => now)),
        ).toMatchObject({ stage: stages[index], reason: "timeout" });
        expect(f.calls).toHaveLength(index + 1);
      } finally {
        timer.mockRestore();
      }
    });
}

test("permission drift is refused instead of combining two observations", async () => {
  const values = responses();
  values[2].permissions.pull_requests = "read";
  const f = fixture(values);
  expect(await rejected(appMetadata(env, f.send, () => now))).toMatchObject({
    stage: "repository",
    reason: "permissions-changed",
  });
  expect(f.calls).toHaveLength(3);
});

test("unknown exceptions and response bodies do not escape the safe boundary", async () => {
  expect(metadataError(new Error(secret))).toEqual({
    source: "cladiron-app-metadata",
    stage: "internal",
    reason: "unexpected",
  });
  const hostile = {
    toJSON() {
      throw new Error(secret);
    },
    toString() {
      throw new Error(secret);
    },
  };
  expect(metadataError(hostile)).toMatchObject({ stage: "internal" });
  const f = fixture(responses(), () => new Response(secret));
  expect(await rejected(appMetadata(env, f.send, () => now))).toMatchObject({
    stage: "app",
    reason: "invalid-response",
  });
  expect(
    await rejected(
      appMetadata(env, fixture().send, () => {
        throw new Error(secret);
      }),
    ),
  ).toMatchObject({ stage: "credentials" });
});

test("the default network boundary is mocked and cannot reach a real provider", async () => {
  expect(await rejected(appMetadata(env, undefined, () => now))).toMatchObject({
    stage: "app",
    reason: "transport-unknown",
  });
});

test("CLI prints only the safe result or one safe staged error with all network mocked", () => {
  const directory = mkdtempSync(join(tmpdir(), "cladiron-metadata-cli-"));
  try {
    const preload = join(directory, "transport.ts");
    // The fixture file contains responses and code, never a private key or JWT.
    writeFileSync(
      preload,
      `
const paths = ${JSON.stringify(paths)};
const values = ${JSON.stringify(responses())};
let calls = 0;
globalThis.fetch = async (input, init) => {
  const index = calls++;
  if (String(input) !== paths[index] || init.method !== "GET") throw new Error("Unexpected network");
  if (String(index) === process.env.METADATA_TEST_FAIL_AT) throw new Error(${JSON.stringify(secret)} + init.headers.Authorization);
  return Response.json(values[index]);
};
process.on("exit", () => console.error("mock-requests:" + calls));
`,
    );
    for (const failAt of ["none", "1"]) {
      const result = Bun.spawnSync(
        [
          process.execPath,
          "--no-env-file",
          "--preload",
          preload,
          new URL("./app-metadata.ts", import.meta.url).pathname,
        ],
        {
          env: { ...env, METADATA_TEST_FAIL_AT: failAt },
          stdout: "pipe",
          stderr: "pipe",
          timeout: 10_000,
        },
      );
      const stdout = result.stdout.toString();
      const stderr = result.stderr.toString();
      expect(stdout + stderr).not.toContain(privateKey);
      expect(stdout + stderr).not.toContain(secret);
      expect(stdout + stderr).not.toContain("Bearer ");
      expect(stdout + stderr).not.toContain("at appMetadata");
      if (failAt === "none") {
        expect(result.exitCode).toBe(0);
        expect(JSON.parse(stdout).app.id).toBe(3104501);
        expect(stderr.trim()).toBe("mock-requests:3");
      } else {
        expect(result.exitCode).toBe(1);
        expect(stdout).toBe("");
        expect(JSON.parse(stderr.split("\n")[0])).toEqual({
          source: "cladiron-app-metadata",
          stage: "installation",
          reason: "transport-unknown",
        });
        expect(stderr).toContain("mock-requests:2");
      }
    }
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});
