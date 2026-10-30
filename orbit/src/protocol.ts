import contracts from "../.generated/contracts.js";
import * as validators from "../.generated/validators.cjs";
import type { paths } from "../.generated/types";
import { acceptedJson, type Contract, type Registry } from "../../client/protocol/response";
import type { Reply } from "./platform";

type Login = paths["/api/v1/sessions"]["post"];
type Receipt = Login["responses"][200]["content"]["application/json"];
type SharedLoginInput = Login["requestBody"]["content"]["application/json"];
export type LoginInput = Omit<SharedLoginInput, "credential"> & {
  credential: Extract<SharedLoginInput["credential"], { type: "mobile" }>;
};
export type Session = Extract<Extract<Receipt, { session: unknown }>["session"], { token: unknown }>;
export type Registration = Extract<Receipt, { registration: unknown }>["registration"];
export type RegistrationInput = paths["/api/v1/registrations"]["post"]["requestBody"]["content"]["application/json"];
export type Account = paths["/api/v1/accounts/current"]["get"]["responses"][200]["content"]["application/json"]["account"];
export type Collection = paths["/api/v1/revocations"]["get"]["responses"][200]["content"]["application/json"];
export type OwnSession = paths["/api/v1/sessions"]["get"]["responses"][200]["content"]["application/json"]["sessions"][number];
type Endpoint = keyof typeof contracts;
export type Result = { status: number; value: Record<string, any>; retryAfter: number };

const registry = validators as unknown as Registry;
const canonicalSecret = /^[A-Za-z0-9_-]{42}[AEIMQUYcgkosw048]$/;
export class Protocol {
  validLogin(value: unknown): value is LoginInput {
    return registry.createSession_input(value) && (value as LoginInput).credential.type === "mobile";
  }
  validRegistrationInput(value: unknown): value is RegistrationInput { return registry.register_input(value); }
  validSession(value: unknown): value is Session {
    return registry.createSession_200({ code: "ok", session: value }) &&
      !!value && typeof value === "object" && "token" in value;
  }
  validRegistration(value: unknown): value is Registration {
    return registry.createSession_200({ code: "ok", registration: value });
  }
  decode(reply: Reply | undefined, endpoint: Endpoint): Result | undefined {
    if (!reply) return undefined;
    const headers = Object.fromEntries(Object.entries(reply.header).map(([key, value]) => [key.toLowerCase(), value]));
    try {
      // Request uses responseType text. Unexpected binary/object delivery is untrusted.
      if (typeof reply.data !== "string") return undefined;
      const value: unknown = JSON.parse(reply.data);
      if (!acceptedJson(reply.statusCode, headers["content-type"], value,
        contracts[endpoint] as Record<string, Contract>, registry)) return undefined;
      if ((endpoint === "POST /api/v1/sessions" || endpoint === "POST /api/v1/registrations") &&
          (reply.statusCode === 200 || reply.statusCode === 201) && "session" in value && !this.validSession(value.session)) return undefined;
      const seconds = headers["retry-after"];
      const retryAfter = seconds && /^\d+$/.test(seconds) ? Number(seconds) * 1000 : 0;
      return { status: reply.statusCode, value: value as Record<string, any>,
        retryAfter: Number.isSafeInteger(retryAfter) ? retryAfter : Number.MAX_SAFE_INTEGER };
    } catch { return undefined; }
  }
}

export function validAccount(account: Account): boolean {
  if (account.name.normalize("NFC") !== account.name || account.avatar.width !== account.avatar.height) return false;
  const match = /^https:\/\/[^/?#\s@]+\/api\/v1\/avatars\/[0-9a-f]{64}\.webp\?(.+)$/.exec(account.avatar.href);
  if (!match) return false;
  const values: Record<string, string> = {};
  for (const pair of match[1].split("&")) {
    const item = /^(key_id|expires_at|signature)=([A-Za-z0-9_-]+)$/.exec(pair);
    if (!item || values[item[1]] !== undefined) return false;
    values[item[1]] = item[2];
  }
  if (Object.keys(values).length !== 3 || !/^(0|[1-9][0-9]{0,18})$/.test(values.key_id) ||
      (values.key_id.length === 19 && values.key_id > "9223372036854775807") ||
      !/^(0|[1-9][0-9]{0,19})$/.test(values.expires_at) ||
      !canonicalSecret.test(values.signature)) return false;
  const milliseconds = Date.parse(account.avatar.expiresAt);
  const fraction = account.avatar.expiresAt.match(/\.(\d+)/)?.[1];
  return !(fraction && /[1-9]/.test(fraction)) && Number.isSafeInteger(milliseconds) &&
    Number.isSafeInteger(Number(values.expires_at) * 1000) && milliseconds === Number(values.expires_at) * 1000;
}

export function matchesLogout(collection: Collection, scope: "current" | "all", initiator: string): boolean {
  if (!Number.isSafeInteger(collection.count) || collection.count <= 0 ||
      collection.count !== collection.revocations.length || !collection.original ||
      collection.original.scope !== scope || collection.original.initiatorId !== initiator ||
      (scope === "current" && collection.count !== 1)) return false;
  const ids = collection.revocations.map(item => item.id);
  return ids.includes(initiator) && ids.every((id, index) => index === 0 || id > ids[index - 1]) &&
    collection.revocations.every(item => item.revokedAt === collection.revocations[0].revokedAt);
}

export function validSessions(sessions: OwnSession[], selected: string): boolean {
  return sessions.filter(item => item.current).length === 1 &&
    sessions.every((item, index) => item.current === (item.id === selected) &&
      (index === 0 || item.id > sessions[index - 1].id));
}
