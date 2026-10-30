import { UUID } from "../codes/material";
import { registry } from "../protocol/response";
import type { Selection } from "../authentication/session";

export const KEY = "halo.logout";
export type Store = Pick<Storage, "getItem" | "setItem" | "removeItem">;
export type Material = Readonly<{
  scope?: never;
  version: 1;
  operation: string;
  account: string;
  selection: Selection;
  outcome: "pending" | "complete" | "rejected";
  notice:
    | "pending"
    | "empty"
    | "unavailable"
    | "limited"
    | "denied"
    | "authentication"
    | "invalid"
    | "mismatch";
  retryAt: number;
  revokedAt: string | null;
}>;

export type Saved = Material | "missing" | null;

export const ALL_KEY = `halo.logout-all`;

export type AllRecord = Readonly<{ count: number; revokedAt: string }>;

export type AllMaterial = Readonly<{
  version: 1;
  scope: `all`;
  operation: string;
  account: string;
  selection: Selection;
  outcome: Material[`outcome`];
  notice: Material[`notice`];
  binding: "pending" | "matched" | "mismatch";
  count: number | null;
  record: AllRecord | null;
  retryAt: number;
  revokedAt: string | null;
}>;

export type AllSaved = AllMaterial | `missing` | null;

const NOTICES = [
  `pending`,
  `empty`,
  `unavailable`,
  `limited`,
  `denied`,
  `authentication`,
  `invalid`,
  `mismatch`,
] as const;

const OUTCOMES = [`pending`, `complete`, `rejected`] as const;

function timestamp(value: unknown): value is string {
  return typeof value === `string` && Number.isFinite(Date.parse(value));
}

function positive(value: unknown): value is number {
  return Number.isSafeInteger(value) && Number(value) > 0;
}

export function readAll(storage: Store): AllSaved {
  const bytes = storage.getItem(ALL_KEY);
  if (bytes === null) return null;
  let value: unknown;
  try {
    value = JSON.parse(bytes);
  } catch {
    return `missing`;
  }
  if (
    !object(value) ||
    Object.keys(value).length !== 12 ||
    value.version !== 1 ||
    value.scope !== `all` ||
    typeof value.operation !== `string` ||
    !UUID.test(value.operation) ||
    !registry.accountId(value.account) ||
    !object(value.selection) ||
    Object.keys(value.selection).length !== 2 ||
    !registry.sessionId(value.selection.id) ||
    typeof value.selection.revision !== `string` ||
    !UUID.test(value.selection.revision) ||
    typeof value.outcome !== `string` ||
    !OUTCOMES.some((outcome) => outcome === value.outcome) ||
    typeof value.notice !== `string` ||
    !NOTICES.some((notice) => notice === value.notice) ||
    typeof value.binding !== "string" ||
    !["pending", "matched", "mismatch"].includes(value.binding) ||
    !(value.count === null || positive(value.count)) ||
    !(
      value.record === null ||
      (object(value.record) &&
        Object.keys(value.record).length === 2 &&
        positive(value.record.count) &&
        timestamp(value.record.revokedAt))
    ) ||
    !Number.isSafeInteger(value.retryAt) ||
    Number(value.retryAt) < 0 ||
    !(value.revokedAt === null || timestamp(value.revokedAt))
  )
    return `missing`;
  if (value.outcome === `complete`) {
    if (
      value.binding !== "matched" ||
      !positive(value.count) ||
      !timestamp(value.revokedAt) ||
      value.record !== null
    )
      return `missing`;
  } else if (value.count !== null || value.revokedAt !== null) return `missing`;
  if (
    value.record !== null &&
    (value.outcome !== `pending` || value.binding !== "mismatch")
  )
    return `missing`;
  if (
    (value.binding === "mismatch" && value.outcome !== "pending") ||
    (value.notice === "mismatch" && value.binding !== "mismatch") ||
    (value.binding === "matched" && value.outcome !== "complete")
  )
    return "missing";
  const completion =
    value.outcome === "complete"
      ? { id: value.operation, count: value.count, revokedAt: value.revokedAt }
      : value.record === null
        ? null
        : { id: value.operation, ...(value.record as AllRecord) };
  if (completion && !registry.revokeAllSessions_200({ code: "ok", completion }))
    return "missing";
  return value as AllMaterial;
}

export function saveAll(storage: Store, value: AllMaterial | `missing`) {
  const bytes = JSON.stringify(value);
  storage.setItem(ALL_KEY, bytes);
  if (storage.getItem(ALL_KEY) !== bytes)
    throw new Error(`logout_storage_unavailable`);
}

export function removeAll(storage: Store) {
  storage.removeItem(ALL_KEY);
  if (storage.getItem(ALL_KEY) !== null)
    throw new Error(`logout_storage_unavailable`);
}

function object(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === "object" && !Array.isArray(value);
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
    Object.keys(value).length !== 8 ||
    value.version !== 1 ||
    typeof value.operation !== "string" ||
    !UUID.test(value.operation) ||
    !registry.accountId(value.account) ||
    !object(value.selection) ||
    Object.keys(value.selection).length !== 2 ||
    !registry.sessionId(value.selection.id) ||
    typeof value.selection.revision !== "string" ||
    !UUID.test(value.selection.revision) ||
    typeof value.outcome !== "string" ||
    !["pending", "complete", "rejected"].includes(value.outcome) ||
    typeof value.notice !== "string" ||
    ![
      "pending",
      "empty",
      "unavailable",
      "limited",
      "denied",
      "authentication",
      "invalid",
      "mismatch",
    ].includes(value.notice) ||
    !Number.isSafeInteger(value.retryAt) ||
    Number(value.retryAt) < 0 ||
    !(value.revokedAt === null || typeof value.revokedAt === "string")
  )
    return "missing";
  if (value.outcome === "complete") {
    if (
      !registry.revokeCurrentSession_200({
        code: "ok",
        completion: {
          id: value.operation,
          count: 1,
          revokedAt: value.revokedAt,
        },
      })
    )
      return "missing";
  } else if (value.revokedAt !== null) return "missing";
  return value as Material;
}

export function save(storage: Store, value: Material | "missing") {
  const bytes = JSON.stringify(value);
  storage.setItem(KEY, bytes);
  if (storage.getItem(KEY) !== bytes)
    throw new Error("logout_storage_unavailable");
}

export function remove(storage: Store) {
  storage.removeItem(KEY);
  if (storage.getItem(KEY) !== null)
    throw new Error("logout_storage_unavailable");
}
