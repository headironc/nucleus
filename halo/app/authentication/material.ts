import {
  validInput,
  validRegistration,
  validSession,
  type Input,
  type Registration,
  type Session,
} from "../protocol/session";
import type { Attempt, BrowserSession } from "./session";

import {
  validInput as validCodeInput,
  validMessage as validSmsMessage,
  type Message,
} from "../protocol/codes";
import { STORAGE_KEY as C1_KEY, persistMaterial } from "../codes/material";

export const MESSAGE_KEY = "halo.login.message";
export const LIFETIME = 30 * 60 * 1000;
export type Storage = Pick<
  globalThis.Storage,
  "getItem" | "setItem" | "removeItem" | "key" | "length"
>;
export type Code = {
  version: 1;
  operation: string;
  mobile: string;
  message: Message;
  createdAt: number;
  expiresAt: number;
};

export function save(storage: Storage, key: string, value: unknown) {
  const bytes = JSON.stringify(value);
  storage.setItem(key, bytes);
  if (storage.getItem(key) !== bytes) throw new Error("storage_unavailable");
}

export function remove(storage: Storage, key: string) {
  storage.removeItem(key);
  if (storage.getItem(key) !== null) throw new Error("storage_unavailable");
}

export function bounded(value: { createdAt: number; expiresAt: number }) {
  return (
    Number.isFinite(value.createdAt) &&
    Number.isFinite(value.expiresAt) &&
    value.expiresAt > value.createdAt &&
    value.expiresAt <= value.createdAt + LIFETIME
  );
}

export function validCode(value: unknown): value is Code {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const code = value as Code;
  return (
    code.version === 1 &&
    typeof code.operation === "string" &&
    /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(
      code.operation,
    ) &&
    bounded(code) &&
    validCodeInput({ mobile: code.mobile, token: "shape-only" }) &&
    validSmsMessage(code.message) &&
    code.expiresAt <= Date.parse(code.message.expiresAt)
  );
}

export function readCode(storage: Storage, now: number): Code | undefined {
  const bytes = storage.getItem(MESSAGE_KEY);
  if (bytes === null) return;
  let value;
  try {
    value = JSON.parse(bytes);
  } catch {
    // Damaged and expired handoffs follow the same retirement path.
  }
  if (
    !validCode(value) ||
    !Number.isFinite(now) ||
    now < value.createdAt ||
    now >= value.expiresAt
  ) {
    // Without another stage, C1 must remember this was an existing operation.
    if (storage.getItem(C1_KEY) === null && storage.getItem(LOGIN_KEY) === null)
      persistMaterial(storage, { version: 1, closed: true });
    remove(storage, MESSAGE_KEY);
    return;
  }
  return value;
}

// C1 saves the next stage before discarding its Turnstile proof.
export function receiveCode(
  storage: Storage,
  operation: string,
  mobile: string,
  message: Message,
  now: number,
) {
  const previous = readCode(storage, now);
  const createdAt =
    previous?.message.id === message.id ? previous.createdAt : now;
  const expiresAt = Math.min(
    createdAt + LIFETIME,
    Date.parse(message.expiresAt),
  );
  if (expiresAt <= now) return;
  save(storage, MESSAGE_KEY, {
    version: 1,
    operation,
    mobile,
    message,
    createdAt,
    expiresAt,
  });
  const bytes = storage.getItem(LOGIN_KEY);
  const prior = bytes === null ? undefined : parseMaterial(bytes);
  if (prior?.kind === "closed" && prior.cleanup === undefined)
    remove(storage, LOGIN_KEY);
}

// A saved message retires only the C1 request that produced that handoff.
export function discardSend(storage: Storage, code: Pick<Code, "operation">) {
  const bytes = storage.getItem(C1_KEY);
  if (!bytes) return;
  let value;
  try {
    value = JSON.parse(bytes);
  } catch {
    return;
  }
  if (value?.operation === code.operation) remove(storage, C1_KEY);
}

export function clearCode(storage: Storage, cleanup: Cleanup) {
  discardSend(storage, { operation: cleanup.send });
  const bytes = storage.getItem(MESSAGE_KEY);
  if (bytes === null) return;
  let value;
  try {
    value = JSON.parse(bytes);
  } catch {
    // A malformed handoff contains no usable identity for a newer operation.
  }
  if (validCode(value)) {
    if (value.message.id !== cleanup.message) return;
  }
  remove(storage, MESSAGE_KEY);
}

export const LOGIN_KEY = "halo.r1";
const RECEIPT_PREFIX = "halo.r1.session.";
export const UUID =
  /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
export const REJECTIONS = [
  "verification_invalid",
  "verification_required",
  "authentication_invalid",
  "request_mismatch",
  "permission_denied",
  "invalid_input",
] as const;

export type Proof = {
  version: 1;
  kind: "login";
  attempt: Attempt;
  input: Input;
  send: string;
  createdAt: number;
  expiresAt: number;
  retryAt: number;
  sent: boolean;
  limited?: true;
  reason?: string;
};
export type Next = {
  version: 1;
  kind: "registration";
  attempt: Attempt;
  registration: Registration;
  createdAt: number;
  expiresAt: number;
  cleanup?: Cleanup;
};
export type Closed = {
  version: 1;
  kind: "closed";
  reason: string;
  cleanup?: Cleanup;
};
export type Completed = {
  version: 1;
  kind: "session";
  attempt: Attempt;
  session: Session;
  cleanup?: Cleanup;
};
type Cleanup = { message: string; send: string };
export type Material = Proof | Next | Closed | Completed;

function validAttempt(value: Attempt | undefined): value is Attempt {
  return !!value && UUID.test(value.operation) && UUID.test(value.revision);
}

function validCleanup(value: { cleanup?: Cleanup }) {
  return (
    value.cleanup === undefined ||
    (!!value.cleanup &&
      typeof value.cleanup === "object" &&
      Object.keys(value.cleanup).length === 2 &&
      typeof value.cleanup.send === "string" &&
      UUID.test(value.cleanup.send) &&
      typeof value.cleanup.message === "string" &&
      /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(
        value.cleanup.message,
      ))
  );
}

export function validCompleted(
  value: Completed | undefined,
): value is Completed {
  return (
    !!value &&
    typeof value === "object" &&
    value.version === 1 &&
    value.kind === "session" &&
    Object.keys(value).length === ("cleanup" in value ? 5 : 4) &&
    validAttempt(value.attempt) &&
    Object.keys(value.attempt).length === 2 &&
    validSession(value.session) &&
    Object.keys(value.session).length === 2 &&
    validCleanup(value)
  );
}

export function parseMaterial(bytes: string): Material | undefined {
  try {
    const value = JSON.parse(bytes);
    if (
      !value ||
      typeof value !== "object" ||
      Array.isArray(value) ||
      value.version !== 1
    )
      return;
    if (value.kind === "session")
      return validCompleted(value) ? value : undefined;
    if (value.kind === "closed") {
      if (
        Object.keys(value).length === ("cleanup" in value ? 4 : 3) &&
        [
          "session",
          "registration",
          "expired",
          "missing",
          ...REJECTIONS,
        ].includes(value.reason) &&
        validCleanup(value)
      )
        return value;
      return;
    }
    if (!bounded(value) || !validAttempt(value.attempt)) return;
    if (
      value.kind === "login" &&
      typeof value.send === "string" &&
      UUID.test(value.send) &&
      validInput(value.input) &&
      Number.isFinite(value.retryAt) &&
      typeof value.sent === "boolean" &&
      (value.limited === undefined || value.limited === true) &&
      (value.reason === undefined || REJECTIONS.includes(value.reason))
    )
      return value;
    if (
      value.kind === "registration" &&
      validRegistration(value.registration) &&
      value.expiresAt <= Date.parse(value.registration.expiresAt) &&
      validCleanup(value)
    )
      return value;
  } catch {
    // Malformed records are closed before a caller may use their proof.
  }
}

export function expired(
  value: { createdAt: number; expiresAt: number },
  now: number,
) {
  return (
    !Number.isFinite(now) || now < value.createdAt || now >= value.expiresAt
  );
}

export function receiptKey(operation: string) {
  return RECEIPT_PREFIX + operation;
}

function receiptKeys(storage: Storage) {
  const result: string[] = [];
  for (let index = 0; index < storage.length; index++) {
    const key = storage.key(index);
    if (!key?.startsWith(RECEIPT_PREFIX)) continue;
    result.push(key);
  }
  return result;
}

function readReceipt(storage: Storage, key: string) {
  const bytes = storage.getItem(key);
  if (bytes === null) return;
  const value = parseMaterial(bytes);
  if (
    value?.kind !== "session" ||
    key !== receiptKey(value.attempt.operation)
  ) {
    remove(storage, key);
    return;
  }
  return value;
}

export function receipts(storage: Storage): Completed[] {
  return receiptKeys(storage).flatMap((key) => {
    const value = readReceipt(storage, key);
    return value ? [value] : [];
  });
}

export function matches(completed: Completed, selected: BrowserSession | null) {
  return (
    (selected?.attempt === completed.attempt.operation &&
      selected.revision === completed.attempt.revision) ||
    (selected?.attempt === null &&
      selected.session?.id === completed.session.id)
  );
}

export function blocksSend(storage: Storage) {
  const bytes = storage.getItem(LOGIN_KEY);
  if (bytes === null) return false;
  const value = parseMaterial(bytes);
  return !value || value.kind !== "closed" || value.cleanup !== undefined;
}

export function availableCode(storage: Storage, now: number) {
  if (storage.getItem(LOGIN_KEY) !== null) return;
  const code = readCode(storage, now);
  if (!code || code.message.delivery === "failed") return;
  const bytes = storage.getItem(C1_KEY);
  if (bytes !== null) {
    let value;
    try {
      value = JSON.parse(bytes);
    } catch {
      /* The saved handoff remains usable. */
    }
    if (
      typeof value?.operation === "string" &&
      value.operation !== code.operation
    )
      return;
  }
  return code;
}

export function cleanHandoff<T extends Next | Closed | Completed>(
  storage: Storage,
  key: string,
  material: T,
): T {
  if (material.cleanup === undefined) return material;
  clearCode(storage, material.cleanup);
  const cleaned = { ...material };
  delete cleaned.cleanup;
  save(storage, key, cleaned);
  return cleaned;
}

// This maintenance only touches concrete anonymous material, never session selection or HTTP.
export function maintainMaterial(storage: Storage, now: number): boolean {
  let complete = true;
  try {
    const bytes = storage.getItem(LOGIN_KEY);
    if (bytes !== null) {
      let value = parseMaterial(bytes);
      if (!value) {
        value = { version: 1, kind: "closed", reason: "missing" };
        save(storage, LOGIN_KEY, value);
      } else if (
        (value.kind === "login" || value.kind === "registration") &&
        expired(value, now)
      ) {
        value = {
          version: 1,
          kind: "closed",
          reason: "expired",
          cleanup:
            value.kind === "login"
              ? { message: value.input.credential.id, send: value.send }
              : value.cleanup,
        };
        save(storage, LOGIN_KEY, value);
      }
      if (value.kind !== "login") cleanHandoff(storage, LOGIN_KEY, value);
    }
  } catch {
    complete = false;
  }
  try {
    for (const key of receiptKeys(storage)) {
      try {
        const value = readReceipt(storage, key);
        if (value) cleanHandoff(storage, key, value);
      } catch {
        complete = false;
      }
    }
  } catch {
    complete = false;
  }
  try {
    readCode(storage, now);
  } catch {
    complete = false;
  }
  return complete;
}
