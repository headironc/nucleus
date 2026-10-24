import { validInput, type Input } from "../protocol/registration";
import {
  validSession,
  validRegistration,
  type Session,
  type Registration,
} from "../protocol/session";
import {
  bounded,
  cleanHandoff,
  expired,
  LOGIN_KEY,
  parseMaterial,
  save,
  remove,
  UUID,
  type Next,
  type Storage,
} from "../authentication/material";
import type { Attempt } from "../authentication/session";

export const KEY = "halo.r2";
const RECEIPTS = "halo.r2.session.";
export const REJECTIONS = [
  "authentication_invalid",
  "request_mismatch",
  "verification_required",
  "invalid_input",
  "permission_denied",
  "mobile_taken",
  "id_number_taken",
] as const;
export type Rejection = (typeof REJECTIONS)[number];
export type Proof = {
  version: 1;
  kind: "registration";
  operation: string;
  attempt: Attempt;
  input: Input;
  createdAt: number;
  expiresAt: number;
  credentialExpiresAt: number;
  retryAt: number;
  sent: boolean;
  limited?: true;
  reason?: Rejection;
};
export type Completed = {
  version: 1;
  kind: "session";
  operation: string;
  attempt: Attempt;
  session: Session;
  createdAt: number;
  expiresAt: number;
};
export type Refused = {
  version: 1;
  kind: "refused";
  attempt: Attempt;
  registration: Registration;
  createdAt: number;
  expiresAt: number;
  retryAt: number;
  reason:
    | "invalid_input"
    | "permission_denied"
    | "mobile_taken"
    | "id_number_taken"
    | "rate_limited";
};
export type Closed = {
  version: 1;
  kind: "closed";
  reason:
    | "missing"
    | "expired"
    | "verification_required"
    | Exclude<Refused["reason"], "rate_limited">;
  operation?: string;
  attempt?: Attempt;
};
export type Material = Proof | Completed | Refused | Closed;

function record(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === "object" && !Array.isArray(value);
}
function attempt(value: unknown): value is Attempt {
  return (
    record(value) &&
    Object.keys(value).length === 2 &&
    typeof value.operation === "string" &&
    UUID.test(value.operation) &&
    typeof value.revision === "string" &&
    UUID.test(value.revision)
  );
}
export function parse(bytes: string): Material | undefined {
  try {
    const value = JSON.parse(bytes);
    if (!record(value) || value.version !== 1) return;
    if (value.kind === "closed") {
      if (
        (Object.keys(value).length === 3 ||
          (Object.keys(value).length === 5 &&
            typeof value.operation === "string" &&
            UUID.test(value.operation) &&
            attempt(value.attempt))) &&
        [
          "missing",
          "expired",
          "verification_required",
          "invalid_input",
          "permission_denied",
          "mobile_taken",
          "id_number_taken",
        ].includes(String(value.reason))
      )
        return value as Closed;
      return;
    }
    if (!attempt(value.attempt)) return;
    if (value.kind === "refused") {
      const refused = value as Refused;
      if (
        Object.keys(value).length === 8 &&
        bounded(refused) &&
        validRegistration(refused.registration) &&
        Object.keys(refused.registration).length === 3 &&
        refused.expiresAt <= Date.parse(refused.registration.expiresAt) &&
        Number.isFinite(refused.retryAt) &&
        [
          "invalid_input",
          "permission_denied",
          "mobile_taken",
          "id_number_taken",
          "rate_limited",
        ].includes(refused.reason)
      )
        return refused;
      return;
    }
    if (typeof value.operation !== "string" || !UUID.test(value.operation))
      return;
    if (value.kind === "session") {
      if (
        Object.keys(value).length === 7 &&
        bounded(value as Completed) &&
        record(value.session) &&
        Object.keys(value.session).length === 2 &&
        validSession(value.session)
      )
        return value as Completed;
      return;
    }
    if (value.kind !== "registration" || !validInput(value.input)) return;
    const proof = value as Proof;
    if (
      Object.keys(value).length ===
        10 + Number("limited" in value) + Number("reason" in value) &&
      bounded(proof) &&
      Number.isFinite(proof.credentialExpiresAt) &&
      proof.credentialExpiresAt > proof.createdAt &&
      (proof.sent || proof.expiresAt <= proof.credentialExpiresAt) &&
      Number.isFinite(proof.retryAt) &&
      typeof proof.sent === "boolean" &&
      (proof.limited === undefined || proof.limited === true) &&
      (proof.reason === undefined || REJECTIONS.includes(proof.reason))
    )
      return proof;
  } catch {
    // Corrupt material is never used as a registration proof.
  }
}
export function owns(storage: Storage, operation: string) {
  const bytes = storage.getItem(KEY);
  const value = bytes === null ? undefined : parse(bytes);
  return value && "operation" in value && value.operation === operation;
}
export function receiptKey(operation: string) {
  return RECEIPTS + operation;
}
export function blocksSend(storage: Storage) {
  const bytes = storage.getItem(KEY);
  if (bytes === null) return false;
  const value = parse(bytes);
  return !value || value.kind === "registration" || value.kind === "refused";
}

// The saved R2 stage is the consumer of the R1 credential. Close R1 only
// after the caller has durably saved that stage, and never touch a newer R1.
export function retireLogin(storage: Storage, expected: Attempt) {
  const bytes = storage.getItem(LOGIN_KEY);
  const value = bytes === null ? undefined : parseMaterial(bytes);
  if (
    value?.kind !== "registration" ||
    value.attempt.operation !== expected.operation ||
    value.attempt.revision !== expected.revision
  )
    return;
  cleanHandoff(storage, LOGIN_KEY, value);
  save(storage, LOGIN_KEY, {
    version: 1,
    kind: "closed",
    reason: "registration",
  });
}
export function handoff(storage: Storage, now: number): Next | undefined {
  const bytes = storage.getItem(LOGIN_KEY);
  const value = bytes === null ? undefined : parseMaterial(bytes);
  if (value?.kind !== "registration" || expired(value, now)) return;
  return cleanHandoff(storage, LOGIN_KEY, value);
}
export function receipts(storage: Storage): Completed[] {
  const keys: string[] = [];
  for (let index = 0; index < storage.length; index++) {
    const key = storage.key(index);
    if (key?.startsWith(RECEIPTS)) keys.push(key);
  }
  const result: Completed[] = [];
  for (const key of keys) {
    const bytes = storage.getItem(key);
    const value = bytes === null ? undefined : parse(bytes);
    if (value?.kind === "session" && key === receiptKey(value.operation))
      result.push(value);
    else remove(storage, key);
  }
  return result;
}
export function maintain(storage: Storage, now: number): boolean {
  try {
    const bytes = storage.getItem(KEY);
    if (bytes === null) return true;
    const value = parse(bytes);
    if (!value) {
      save(storage, KEY, { version: 1, kind: "closed", reason: "missing" });
    } else if (value.kind === "registration") {
      const receipt = storage.getItem(receiptKey(value.operation));
      const completed = receipt === null ? undefined : parse(receipt);
      if (
        completed?.kind === "session" &&
        completed.operation === value.operation &&
        completed.attempt.operation === value.attempt.operation &&
        completed.attempt.revision === value.attempt.revision
      ) {
        save(storage, KEY, completed);
        retireLogin(storage, value.attempt);
      } else if (expired(value, now)) {
        save(storage, KEY, {
          version: 1,
          kind: "closed",
          reason: "expired",
          operation: value.operation,
          attempt: value.attempt,
        });
        retireLogin(storage, value.attempt);
      } else retireLogin(storage, value.attempt);
    } else if (value.kind === "refused" && expired(value, now)) {
      save(storage, KEY, {
        version: 1,
        kind: "closed",
        reason:
          value.reason === "rate_limited"
            ? "verification_required"
            : value.reason,
      });
    } else if (value.kind === "session") retireLogin(storage, value.attempt);
    return true;
  } catch {
    return false;
  }
}
