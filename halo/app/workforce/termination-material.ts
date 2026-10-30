import { UUID } from "../codes/material";
import type { Selection } from "../authentication/session";
import { registry } from "../protocol/response";

export const KEY = "halo.workforce-termination";
export type Store = Pick<Storage, "getItem" | "setItem" | "removeItem">;

/** A completed self termination is a permanent business fact. */
export type Known = Readonly<{
  operation: string;
  account: string;
  person: string;
  grant: string;
  terminatedAt: string;
}>;

export type Notice =
  | "pending"
  | "limited"
  | "unavailable"
  | "denied"
  | "authentication"
  | "refused"
  | "mismatch"
  | "conflict"
  | "invalid";

/**
 * One original termination that is not known to be complete. `sent` is only
 * false before any request was attempted; a false value never means unknown.
 */
export type Active = Readonly<{
  operation: string;
  account: string;
  person: string;
  grant: string;
  selection: Selection;
  outcome: "pending" | "rejected";
  notice: Notice;
  sent: boolean;
  retryAt: number;
}>;

export type Material = Readonly<{
  version: 1;
  known: readonly Known[];
  active: Active | null;
}>;
export type Saved = Material | "missing" | null;

const NOTICES = [
  "pending",
  "limited",
  "unavailable",
  "denied",
  "authentication",
  "refused",
  "mismatch",
  "conflict",
  "invalid",
] as const;

function object(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === "object" && !Array.isArray(value);
}

function timestamp(value: unknown): value is string {
  return typeof value === "string" && Number.isFinite(Date.parse(value));
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

function identity(value: Record<string, unknown>): boolean {
  return (
    typeof value.operation === "string" &&
    UUID.test(value.operation) &&
    registry.accountId(value.account) &&
    registry.personId(value.person) &&
    registry.grantId(value.grant)
  );
}

function known(value: unknown): value is Known {
  return (
    object(value) &&
    Object.keys(value).length === 5 &&
    identity(value) &&
    timestamp(value.terminatedAt)
  );
}

function active(value: unknown): value is Active {
  if (
    !object(value) ||
    Object.keys(value).length !== 9 ||
    !identity(value) ||
    !selection(value.selection) ||
    (value.outcome !== "pending" && value.outcome !== "rejected") ||
    typeof value.notice !== "string" ||
    !NOTICES.some((notice) => notice === value.notice) ||
    typeof value.sent !== "boolean" ||
    !Number.isSafeInteger(value.retryAt) ||
    Number(value.retryAt) < 0
  )
    return false;
  if (value.outcome === "rejected")
    return (
      ["limited", "conflict", "invalid"].includes(value.notice) &&
      value.sent === true
    );
  if (value.notice === "mismatch") return value.sent === true;
  if (!value.sent) return value.notice === "pending";
  return true;
}

export function read(storage: Store): Saved {
  const bytes = storage.getItem(KEY);
  if (bytes === null) return null;
  let value: unknown;
  try {
    value = JSON.parse(bytes);
  } catch {
    return "missing";
  }
  if (
    !object(value) ||
    Object.keys(value).length !== 3 ||
    value.version !== 1 ||
    !Array.isArray(value.known) ||
    !value.known.every(known) ||
    !(value.active === null || active(value.active))
  )
    return "missing";
  if (value.known.length === 0 && value.active === null) return null;
  if (
    new Set(value.known.map((result) => result.account)).size !== value.known.length
  )
    return "missing";
  const pending = value.active;
  if (
    pending !== null &&
    value.known.some(
      (result) =>
        result.account === pending.account && result.person !== pending.person,
    )
  )
    return "missing";
  return value as Material;
}

export function save(storage: Store, value: Material | "missing") {
  const bytes = JSON.stringify(value);
  storage.setItem(KEY, bytes);
  if (storage.getItem(KEY) !== bytes)
    throw new Error("termination_storage_unavailable");
}

export function remove(storage: Store) {
  storage.removeItem(KEY);
  if (storage.getItem(KEY) !== null)
    throw new Error("termination_storage_unavailable");
}
