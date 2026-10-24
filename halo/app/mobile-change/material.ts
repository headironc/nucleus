import { UUID } from "../codes/material";
import { registry } from "../protocol/response";
import type { Selection } from "../authentication/session";

export const KEY = "halo.mobile-change";

/** The local secret lasts at most ten minutes from this proof's creation. */
export const LIFETIME = 10 * 60 * 1000;

export type Store = Pick<Storage, "getItem" | "setItem" | "removeItem">;

export type Step =
  | "current-send"
  | "current-code"
  | "establish"
  | "transfer"
  | "new-send"
  | "new-code"
  | "complete";

export type Reason =
  | "mismatch"
  | "wrong_code"
  | "verification_required"
  | "mobile_taken"
  | "state_conflict"
  | "denied"
  | "expired"
  | "limited"
  | "refused"
  | "unavailable"
  | "invalid"
  | "challenge";

export type Known = Readonly<{
  account: string;
  person: string;
  transfer: string;
  completedAt: string;
}>;

export type Active = Readonly<{
  step: Step;
  account: string;
  person: string;
  selection: Selection;
  createdAt: number;
  expiresAt: number;
  retryAt: number;
  attempted: boolean;
  operation?: string;
  currentMobile?: string;
  newMobile?: string;
  token?: string;
  code?: string;
  messageId?: string;
  messageExpiresAt?: number;
  delivery?: "sending" | "sent" | "failed" | "unknown";
  transfer?: string;
  transferExpiresAt?: number;
  reason?: Reason;
}>;

export type Material = Readonly<{
  version: 1;
  known: readonly Known[];
  active: Active | null;
  closed: "unknown" | "expired" | null;
}>;

export type Saved = Material | "missing" | null;

const STEPS: readonly Step[] = [
  "current-send",
  "current-code",
  "establish",
  "transfer",
  "new-send",
  "new-code",
  "complete",
];
const REASONS: readonly Reason[] = [
  "mismatch",
  "wrong_code",
  "verification_required",
  "mobile_taken",
  "state_conflict",
  "denied",
  "expired",
  "limited",
  "refused",
  "unavailable",
  "invalid",
  "challenge",
];
const DELIVERIES = ["sending", "sent", "failed", "unknown"] as const;
const BASE = [
  "step",
  "account",
  "person",
  "selection",
  "createdAt",
  "expiresAt",
  "retryAt",
  "attempted",
] as const;

function object(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === "object" && !Array.isArray(value);
}

function selection(value: unknown): value is Selection {
  return (
    object(value) &&
    Object.keys(value).length === 2 &&
    registry.sessionId(value.id) &&
    typeof value.revision === "string" &&
    UUID.test(value.revision)
  );
}

function timestamp(value: unknown): value is string {
  return typeof value === "string" && Number.isFinite(Date.parse(value));
}

function mobile(value: unknown): value is string {
  return (
    typeof value === "string" &&
    registry.sendCurrentMobileCode_input({ mobile: value, token: "x" })
  );
}

function known(value: unknown): value is Known {
  return (
    object(value) &&
    Object.keys(value).length === 4 &&
    registry.accountId(value.account) &&
    registry.personId(value.person) &&
    registry.transferId(value.transfer) &&
    timestamp(value.completedAt)
  );
}

function active(value: unknown): value is Active {
  if (!object(value)) return false;
  if (!STEPS.some((step) => step === value.step)) return false;
  const keys = Object.keys(value);
  const required = new Set<string>(BASE);
  const allowed = new Set<string>(BASE);
  const optional = (fields: readonly string[]) =>
    fields.forEach((field) => allowed.add(field));
  if (value.attempted && value.step !== "transfer") required.add("operation");
  switch (value.step) {
    case "current-send":
      optional(["operation", "currentMobile", "token", "reason"]);
      break;
    case "current-code":
      optional([
        "operation",
        "currentMobile",
        "messageId",
        "messageExpiresAt",
        "delivery",
      ]);
      break;
    case "establish":
      optional([
        "operation",
        "currentMobile",
        "messageId",
        "messageExpiresAt",
        "delivery",
        "code",
        "reason",
      ]);
      break;
    case "transfer":
      required.add("transfer");
      optional(["transfer", "transferExpiresAt", "currentMobile", "reason"]);
      break;
    case "new-send":
      required.add("newMobile");
      optional([
        "operation",
        "transfer",
        "transferExpiresAt",
        "currentMobile",
        "newMobile",
        "token",
        "reason",
      ]);
      break;
    case "new-code":
      required.add("newMobile");
      required.add("messageId");
      optional([
        "operation",
        "transfer",
        "transferExpiresAt",
        "currentMobile",
        "newMobile",
        "messageId",
        "messageExpiresAt",
        "delivery",
        "reason",
      ]);
      break;
    case "complete":
      required.add("operation");
      required.add("newMobile");
      required.add("messageId");
      required.add("code");
      optional([
        "operation",
        "transfer",
        "transferExpiresAt",
        "currentMobile",
        "newMobile",
        "messageId",
        "messageExpiresAt",
        "code",
        "reason",
      ]);
      break;
  }
  if (
    value.reason === "limited" &&
    (value.step === "current-send" || value.step === "new-send")
  ) {
    for (const key of [
      "operation",
      "token",
      value.step === "current-send" ? "currentMobile" : "newMobile",
    ])
      required.add(key);
  }
  if (
    ![...required].every((key) => keys.includes(key)) ||
    !keys.every((key) => allowed.has(key)) ||
    !registry.accountId(value.account) ||
    !registry.personId(value.person) ||
    !selection(value.selection) ||
    !Number.isSafeInteger(value.createdAt) ||
    Number(value.createdAt) < 0 ||
    !Number.isSafeInteger(value.expiresAt) ||
    Number(value.expiresAt) <= Number(value.createdAt) ||
    Number(value.expiresAt) > Number(value.createdAt) + LIFETIME ||
    !Number.isSafeInteger(value.retryAt) ||
    Number(value.retryAt) < 0 ||
    typeof value.attempted !== "boolean"
  )
    return false;
  if (value.operation !== undefined && !UUID.test(String(value.operation)))
    return false;
  if (value.currentMobile !== undefined && !mobile(value.currentMobile))
    return false;
  if (value.newMobile !== undefined && !mobile(value.newMobile)) return false;
  if (
    value.token !== undefined &&
    !(
      typeof value.token === "string" &&
      value.token.length >= 1 &&
      value.token.length <= 2048
    )
  )
    return false;
  if (value.code !== undefined && !/^[0-9]{6}$/.test(String(value.code)))
    return false;
  if (value.messageId !== undefined && !registry.messageId(value.messageId))
    return false;
  if (
    value.messageExpiresAt !== undefined &&
    (!Number.isSafeInteger(value.messageExpiresAt) ||
      Number(value.messageExpiresAt) < 0)
  )
    return false;
  if (value.transfer !== undefined && !registry.transferId(value.transfer))
    return false;
  if (
    value.transferExpiresAt !== undefined &&
    (!Number.isSafeInteger(value.transferExpiresAt) ||
      Number(value.transferExpiresAt) < 0)
  )
    return false;
  if (
    value.delivery !== undefined &&
    !DELIVERIES.some((delivery) => delivery === value.delivery)
  )
    return false;
  if (
    value.reason !== undefined &&
    !REASONS.some((reason) => reason === value.reason)
  )
    return false;
  if (
    (value.step === "current-code" || value.step === "establish") &&
    !value.messageId
  )
    return false;
  if (
    (value.step === "new-send" ||
      value.step === "new-code" ||
      value.step === "complete") &&
    !value.transfer
  )
    return false;
  return true;
}

export function read(storage: Store): Saved {
  let bytes: string | null;
  try {
    bytes = storage.getItem(KEY);
  } catch {
    return "missing";
  }
  if (bytes === null) return null;
  let value: unknown;
  try {
    value = JSON.parse(bytes);
  } catch {
    return "missing";
  }
  if (
    !object(value) ||
    Object.keys(value).length !== 4 ||
    value.version !== 1 ||
    !Array.isArray(value.known) ||
    !value.known.every(known) ||
    !(value.active === null || active(value.active)) ||
    !(
      value.closed === null ||
      value.closed === "unknown" ||
      value.closed === "expired"
    )
  )
    return "missing";
  if (
    new Set(value.known.map((result) => result.transfer)).size !==
    value.known.length
  )
    return "missing";
  if (value.closed !== null && value.active !== null) return "missing";
  if (
    value.known.length === 0 &&
    value.active === null &&
    value.closed === null
  )
    return null;
  return value as Material;
}

export function save(storage: Store, value: Material | "missing") {
  const bytes = JSON.stringify(value);
  storage.setItem(KEY, bytes);
  if (storage.getItem(KEY) !== bytes)
    throw new Error("mobile_change_storage_unavailable");
}

export function remove(storage: Store) {
  storage.removeItem(KEY);
  if (storage.getItem(KEY) !== null)
    throw new Error("mobile_change_storage_unavailable");
}

export function expired(material: Active, now: number) {
  return (
    !Number.isFinite(now) ||
    now < material.createdAt ||
    now >= material.expiresAt
  );
}

/** The fixed proof creation also bounds a message deadline the server reports. */
export function messageDeadline(
  createdAt: number,
  expiresAt: string | undefined,
) {
  const parsed = expiresAt ? Date.parse(expiresAt) : Number.NaN;
  if (!Number.isFinite(parsed)) return createdAt + LIFETIME;
  return Math.min(createdAt + LIFETIME, parsed);
}

export function transferDeadline(
  createdAt: number,
  expiresAt: string | undefined,
) {
  return messageDeadline(createdAt, expiresAt);
}

/** Discard expired proof without ending a separately valid transfer. */
export function expireProof(active: Active, now: number): Active {
  const deadline = Math.min(
    active.createdAt + LIFETIME,
    active.transferExpiresAt ?? active.expiresAt,
  );
  const canContinue =
    !active.attempted &&
    (active.step === "new-code" || active.step === "complete") &&
    now >= active.createdAt &&
    now < deadline;
  return {
    step: "transfer",
    account: active.account,
    person: active.person,
    selection: active.selection,
    createdAt: active.createdAt,
    expiresAt: canContinue ? deadline : active.expiresAt,
    retryAt: active.retryAt,
    attempted: active.attempted,
    transfer: active.transfer,
    ...(active.transferExpiresAt !== undefined
      ? { transferExpiresAt: active.transferExpiresAt }
      : {}),
    ...(canContinue ? {} : { reason: active.reason ?? "expired" }),
  };
}

/**
 * Lifecycle maintenance runs on every website page. It only closes expired
 * secret use or records a damaged record as unknown; it never sends a request
 * or starts another application.
 */
export function maintain(storage: Store, now: number): boolean {
  try {
    const saved = read(storage);
    if (saved === "missing") {
      save(storage, {
        version: 1,
        known: [],
        active: null,
        closed: "unknown",
      });
      return true;
    }
    if (saved === null) return true;
    const active = saved.active;
    if (!active || !expired(active, now)) return true;
    let next: Material | null;
    if (active.transfer) {
      next = {
        version: 1,
        known: saved.known,
        active: expireProof(active, now),
        closed: null,
      };
    } else if (active.attempted) {
      next = {
        version: 1,
        known: saved.known,
        active: null,
        closed: "unknown",
      };
    } else {
      next = saved.known.length
        ? { version: 1, known: saved.known, active: null, closed: null }
        : null;
    }
    if (next) save(storage, next);
    else {
      storage.removeItem(KEY);
      if (storage.getItem(KEY) !== null)
        throw new Error("mobile_change_storage_unavailable");
    }
    return true;
  } catch {
    // Keep the record; the next executing lifecycle check retries it.
    return false;
  }
}
