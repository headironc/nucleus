import { decode, retryAfter } from "./response";
import type { paths } from "./generated/types";
import type { Failure } from "./session";

type Receipt =
  paths["/api/v1/sessions/current"]["delete"]["responses"][200]["content"]["application/json"];
type Collection =
  paths["/api/v1/revocations"]["get"]["responses"][200]["content"]["application/json"];
export type Completion = Receipt["completion"];
export type RevokeResult =
  { kind: "complete"; completion: Completion } | Failure;
export type ReadResult =
  { kind: "collection"; collection: Collection } | Failure;

type AllReceipt =
  paths[`/api/v1/sessions`][`delete`][`responses`][200][`content`][`application/json`];

function validTimestamp(value: unknown): value is string {
  return typeof value === `string` && Number.isFinite(Date.parse(value));
}

function identity(value: string): string {
  return value.replace(/^urn:uuid:/i, ``).toLowerCase();
}

/** The completion must belong to this operation and positively describe its set. */
function validCompletion(
  completion: Completion,
  operation: string,
  current: boolean,
): boolean {
  return (
    completion.id === operation &&
    Number.isSafeInteger(completion.count) &&
    completion.count > 0 &&
    (!current || completion.count === 1) &&
    validTimestamp(completion.revokedAt)
  );
}

/** A nonempty account record must be a complete, ordered, single-time set. */
function validCollection(collection: Collection): boolean {
  if (
    !Number.isSafeInteger(collection.count) ||
    collection.count < 0 ||
    collection.count !== collection.revocations.length
  )
    return false;
  if (collection.count === 0) return collection.original === null;
  if (!collection.original) return false;
  if (collection.original.scope !== "all" && collection.count !== 1)
    return false;
  const seen = new Set<string>();
  let previous: string | undefined;
  let revokedAt: string | undefined;
  for (const item of collection.revocations) {
    if (!validTimestamp(item.revokedAt)) return false;
    if (revokedAt === undefined) revokedAt = item.revokedAt;
    else if (item.revokedAt !== revokedAt) return false;
    const id = identity(item.id);
    if (seen.has(id)) return false;
    if (previous !== undefined && id <= previous) return false;
    seen.add(id);
    previous = id;
  }
  const includesInitiator = seen.has(identity(collection.original.initiatorId));
  return collection.original.scope === "other"
    ? !includesInitiator
    : includesInitiator;
}

export function matchesOriginal(
  collection: Collection,
  scope: "current" | "all",
  session: string,
): boolean {
  return (
    collection.original?.scope === scope &&
    identity(collection.original.initiatorId) === identity(session)
  );
}

export async function revokeAll(
  operation: string,
  session: string,
  signal: AbortSignal,
): Promise<RevokeResult> {
  try {
    const response = await fetch(`/api/v1/sessions`, {
      method: `DELETE`,
      headers: {
        [`Operation-Id`]: operation,
        [`Session-Id`]: session,
        Accept: `application/json`,
      },
      credentials: `same-origin`,
      cache: `no-store`,
      redirect: `error`,
      signal,
    });
    const value = await decode(response, `DELETE /api/v1/sessions`);
    if (!value) return { kind: `untrusted` };
    if (response.status !== 200) return failure(response, String(value.code));
    const { completion } = value as AllReceipt;
    if (!validCompletion(completion, operation, false))
      return { kind: `untrusted` };
    return { kind: `complete`, completion };
  } catch {
    return { kind: `disconnected` };
  }
}

export async function readAllRevocations(
  operation: string,
  _originalSession: string,
  session: string,
  signal: AbortSignal,
): Promise<ReadResult> {
  try {
    const query = new URLSearchParams({ operation_id: operation });
    const response = await fetch(`/api/v1/revocations?${query}`, {
      headers: {
        [`Operation-Id`]: crypto.randomUUID(),
        [`Session-Id`]: session,
        Accept: `application/json`,
      },
      credentials: `same-origin`,
      cache: `no-store`,
      redirect: `error`,
      signal,
    });
    const value = await decode(response, `GET /api/v1/revocations`);
    if (!value) return { kind: `untrusted` };
    if (response.status !== 200) return failure(response, String(value.code));
    const collection = value as Collection;
    if (!validCollection(collection)) return { kind: `untrusted` };
    return { kind: `collection`, collection };
  } catch {
    return { kind: `disconnected` };
  }
}

function failure(response: Response, code: string): Failure {
  return {
    kind: "failure",
    status: response.status,
    code,
    retryAfter: retryAfter(response),
  };
}

export async function revoke(
  operation: string,
  session: string,
  signal: AbortSignal,
): Promise<RevokeResult> {
  try {
    const response = await fetch("/api/v1/sessions/current", {
      method: "DELETE",
      headers: {
        "Operation-Id": operation,
        "Session-Id": session,
        Accept: "application/json",
      },
      credentials: "same-origin",
      cache: "no-store",
      redirect: "error",
      signal,
    });
    const value = await decode(response, "DELETE /api/v1/sessions/current");
    if (!value) return { kind: "untrusted" };
    if (response.status !== 200) return failure(response, String(value.code));
    const { completion } = value as Receipt;
    // This consumer confirms exactly one original session, never all devices.
    if (!validCompletion(completion, operation, true))
      return { kind: "untrusted" };
    return { kind: "complete", completion };
  } catch {
    return { kind: "disconnected" };
  }
}

export async function readRevocations(
  operation: string,
  originalSession: string,
  session: string,
  signal: AbortSignal,
): Promise<ReadResult> {
  try {
    const query = new URLSearchParams({ operation_id: operation });
    const response = await fetch(`/api/v1/revocations?${query}`, {
      headers: {
        "Operation-Id": crypto.randomUUID(),
        "Session-Id": session,
        Accept: "application/json",
      },
      credentials: "same-origin",
      cache: "no-store",
      redirect: "error",
      signal,
    });
    const value = await decode(response, "GET /api/v1/revocations");
    if (!value) return { kind: "untrusted" };
    if (response.status !== 200) return failure(response, String(value.code));
    const collection = value as Collection;
    if (
      !validCollection(collection) ||
      (collection.count > 0 &&
        !matchesOriginal(collection, "current", originalSession))
    )
      return { kind: "untrusted" };
    return { kind: "collection", collection };
  } catch {
    return { kind: "disconnected" };
  }
}
