import { expect, test } from "bun:test";
import { generateKeyPairSync, verify } from "node:crypto";
import {
  appPermissions,
  mint,
  mintReview,
  revoke,
  scopes,
  verifyInstallation,
  verifyRepository,
  type Api,
} from "./merge-credentials";
import { appId, ownerId, repository, repositoryId } from "./merge-deployment";
import { field, object, parse, type Json, type Record } from "./review-json";

const { privateKey, publicKey } = generateKeyPairSync("rsa", {
  modulusLength: 2048,
});
const key = privateKey.export({ type: "pkcs8", format: "pem" }).toString();
const owner = { id: ownerId, login: "headironc", type: "User" };
const installation: Record = {
  id: 17n,
  app_id: appId,
  app_slug: "cladiron",
  target_type: "User",
  account: owner,
  suspended_at: null,
  repository_selection: "selected",
  permissions: appPermissions,
};

function service(
  options: {
    installation?: (count: number) => Json;
    response?: (row: Record) => void;
    cleanupFails?: boolean;
  } = {},
) {
  const issued: string[] = [],
    deleted: string[] = [],
    masks: string[] = [];
  let reads = 0;
  const transport =
    (token: string): Api =>
    async (path, method, body): Promise<Json> => {
      if (path === "installation/token") {
        expect(method).toBe("DELETE");
        deleted.push(token);
        if (options.cleanupFails) throw new Error("cleanup refused");
        return null;
      }
      const [header, payload, signature] = token.split(".");
      expect(
        verify(
          "RSA-SHA256",
          Buffer.from(`${header}.${payload}`),
          publicKey,
          Buffer.from(signature, "base64url"),
        ),
      ).toBe(true);
      const claims = parse(Buffer.from(payload, "base64url").toString());
      expect(field(claims, "iss")).toBe(appId);
      if (path === "app")
        return {
          id: appId,
          slug: "cladiron",
          owner,
          permissions: appPermissions,
        };
      if (path.endsWith("/installation"))
        return options.installation?.(++reads) ?? installation;
      expect(path).toBe("app/installations/17/access_tokens");
      expect(method).toBe("POST");
      expect(field(body!, "repository_ids")).toEqual([repositoryId]);
      const name = Object.keys(scopes)[issued.length] as keyof typeof scopes;
      expect(field(body!, "permissions")).toEqual(scopes[name]);
      const returned = `test-${name}`;
      issued.push(returned);
      const row: Record = {
        token: returned,
        permissions: scopes[name],
        repositories: [{ id: repositoryId, full_name: repository }],
        expires_at: new Date(Date.now() + 3_600_000).toISOString(),
      };
      options.response?.(row);
      return row;
    };
  return {
    transport,
    issued,
    deleted,
    masks,
    mask: (token: string) => {
      masks.push(token);
    },
  };
}

test("automatic admission issues only a review token and revokes that token", async () => {
  const state = service();
  const credentials = await mintReview(key, state.transport, state.mask);
  expect(credentials.tokens).toEqual({ review: "test-review" });
  expect(state.issued).toEqual(["test-review"]);
  await revoke(Object.values(credentials.tokens), state.transport);
  expect(state.deleted).toEqual(["test-review"]);
});

test("credentials bind exact account, App, installation, repository and purpose before returning masked tokens", async () => {
  const state = service();
  const result = await mint(key, state.transport, state.mask);
  expect(result).toEqual({
    installation: 17n,
    tokens: {
      review: "test-review",
      record: "test-record",
      merge: "test-merge",
    },
  });
  expect(state.masks).toEqual(state.issued);
  await revoke(Object.values(result.tokens), state.transport);
  expect(state.deleted).toEqual(state.issued);
  expect(() =>
    verifyRepository({
      id: repositoryId,
      full_name: repository,
      private: true,
      default_branch: "dev",
      owner,
    }),
  ).not.toThrow();
});

test("identity, selection and permission drift fail closed", () => {
  for (const changes of [
    { app_id: 1n },
    { id: 0n },
    { suspended_at: "2026-10-26T00:00:00Z" },
    { target_type: "Organization" },
    { repository_selection: "all" },
    { account: { ...owner, id: 1n } },
    { permissions: { ...appPermissions, administration: "write" } },
    { permissions: { metadata: "read" } },
  ] as Record[])
    expect(() => verifyInstallation({ ...installation, ...changes })).toThrow();
  expect(() =>
    verifyRepository({
      id: repositoryId,
      full_name: repository,
      private: false,
      default_branch: "dev",
      owner,
    }),
  ).toThrow();
});

test("malformed token receipts revoke every known token without exporting partially accepted credentials", async () => {
  for (const response of [
    (row: Record) => {
      row.permissions = appPermissions;
    },
    (row: Record) => {
      row.repositories = [{ id: 1n, full_name: repository }];
    },
    (row: Record) => {
      row.expires_at = "invalid";
    },
    (row: Record) => {
      row.expires_at = new Date(Date.now() - 1000).toISOString();
    },
  ]) {
    const state = service({ response });
    await expect(mint(key, state.transport, state.mask)).rejects.toThrow(
      "scope or expiry",
    );
    expect(state.deleted).toEqual(state.issued);
    expect(state.issued).toHaveLength(1);
  }
});

test("installation changes during or after issuance revoke all tokens already issued", async () => {
  for (const changeAt of [3, 5]) {
    const state = service({
      installation: (count) =>
        count >= changeAt ? { ...installation, id: 18n } : installation,
    });
    await expect(mint(key, state.transport, state.mask)).rejects.toThrow(
      "Installation changed",
    );
    expect(state.deleted).toEqual(state.issued);
    expect(state.issued.length).toBe(changeAt === 3 ? 1 : 3);
  }
});

test("failed cleanup attempts every known token and reports unconfirmed revocation", async () => {
  const state = service({ cleanupFails: true });
  await expect(
    revoke(["one", "two", "three"], state.transport),
  ).rejects.toThrow("unconfirmed");
  expect(state.deleted).toEqual(["one", "two", "three"]);
});
