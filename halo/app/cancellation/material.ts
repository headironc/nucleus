import { UUID } from "../codes/material";
import type { Selection } from "../authentication/session";
import type { Input, Result } from "../protocol/cancellation";
import { validResult } from "../protocol/cancellation";
import { registry } from "../protocol/response";

export const KEY = "halo.account-cancellation";
export type Store = Pick<Storage, "getItem" | "setItem" | "removeItem">;

export type Notice =
  | "pending"
  | "limited"
  | "invalid"
  | "authentication"
  | "denied"
  | "target"
  | "dependency"
  | "mismatch"
  | "unknown";

/**
 * One original self cancellation. The target and selection are copied from a
 * single trusted Current projection. No name, suffix, mobile, avatar URL,
 * cookie or token belongs here.
 */
export type Material = Readonly<{
  version: 1;
  operation: string;
  target: Input;
  selection: Selection;
  phase: "prepared" | "possibly-sent";
  outcome: "pending" | "refused" | "complete";
  notice: Notice;
  retryAt: number;
  result: Result | null;
  hidden: boolean;
  clearedRevision: string | null;
}>;

export type Saved = Material | "missing" | null;

const NOTICES = [
  "pending",
  "limited",
  "invalid",
  "authentication",
  "denied",
  "target",
  "dependency",
  "mismatch",
  "unknown",
] as const;

const REFUSALS: readonly Notice[] = [
  "limited",
  "invalid",
  "authentication",
  "denied",
  "target",
  "dependency",
];

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

function material(value: unknown): value is Material {
  if (
    !object(value) ||
    Object.keys(value).length !== 11 ||
    value.version !== 1 ||
    typeof value.operation !== "string" ||
    !UUID.test(value.operation) ||
    !registry.cancelCurrentAccount_input(value.target) ||
    !selection(value.selection) ||
    (value.phase !== "prepared" && value.phase !== "possibly-sent") ||
    !["pending", "refused", "complete"].includes(String(value.outcome)) ||
    typeof value.notice !== "string" ||
    !NOTICES.some((notice) => notice === value.notice) ||
    !Number.isSafeInteger(value.retryAt) ||
    Number(value.retryAt) < 0 ||
    !(
      value.result === null ||
      (object(value.result) && validResult(value.result))
    ) ||
    typeof value.hidden !== "boolean" ||
    !(
      value.clearedRevision === null ||
      (typeof value.clearedRevision === "string" &&
        UUID.test(value.clearedRevision))
    )
  )
    return false;

  if (value.phase === "prepared")
    return (
      value.outcome === "pending" &&
      value.notice === "pending" &&
      value.retryAt === 0 &&
      value.result === null &&
      value.hidden === false &&
      value.clearedRevision === null
    );
  if (value.outcome === "complete")
    return (
      value.notice === "pending" && value.retryAt === 0 && value.result !== null
    );
  if (value.outcome === "refused")
    return REFUSALS.includes(value.notice as Notice) && value.result === null;
  return (
    value.result === null &&
    ["pending", "mismatch", "unknown"].includes(value.notice)
  );
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
  return material(value) ? value : "missing";
}

export function save(storage: Store, value: Material | "missing") {
  const bytes = JSON.stringify(value);
  storage.setItem(KEY, bytes);
  if (storage.getItem(KEY) !== bytes)
    throw new Error("cancellation_storage_unavailable");
}

export function remove(storage: Store) {
  storage.removeItem(KEY);
  if (storage.getItem(KEY) !== null)
    throw new Error("cancellation_storage_unavailable");
}
