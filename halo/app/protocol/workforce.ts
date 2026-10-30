import { decode, retryAfter } from "./response";
import type { paths } from "./generated/types";
import type { Failure } from "./session";

type Receipt =
  paths["/api/v1/workforce/current/grant"]["get"]["responses"][200]["content"]["application/json"];
export type ServiceStatus = Receipt["grant"]["status"] | "absent";
/** The exact current Grant and person a visible service action binds to. */
export type GrantLocator = Readonly<{
  id: Receipt["grant"]["id"];
  personId: Receipt["grant"]["subject"]["personId"];
}>;
export type WorkforceResult =
  | { kind: "status"; status: ServiceStatus; grant: GrantLocator | null }
  | Failure;

/** Discard fixed admission identities and internal references after decoding. */
export async function workforce(
  id: string,
  signal: AbortSignal,
): Promise<WorkforceResult> {
  try {
    const response = await fetch("/api/v1/workforce/current/grant", {
      headers: {
        "Operation-Id": crypto.randomUUID(),
        "Session-Id": id,
        Accept: "application/json",
      },
      credentials: "same-origin",
      cache: "no-store",
      redirect: "error",
      signal,
    });
    const value = await decode(response, "GET /api/v1/workforce/current/grant");
    if (!value) return { kind: "untrusted" };
    if (response.status === 200) {
      const grant = (value as Receipt).grant;
      return {
        kind: "status",
        status: grant.status,
        grant: { id: grant.id, personId: grant.subject.personId },
      };
    }
    if (response.status === 404 && value.code === "grant_absent")
      return { kind: "status", status: "absent", grant: null };
    return {
      kind: "failure",
      status: response.status,
      code: String(value.code),
      retryAfter: retryAfter(response),
    };
  } catch {
    return { kind: "disconnected" };
  }
}

type Termination =
  paths["/api/v1/workforce/grants/{id}/terminations"]["post"]["responses"][201]["content"]["application/json"];
export type TerminationResult =
  | { kind: "termination"; terminatedAt: string }
  | Failure;

/**
 * Self termination sends no body at all. `{}` and `null` both select a
 * different contract, so the request must not attach a JSON input.
 */
export async function terminate(
  operation: string,
  grant: string,
  person: string,
  session: string,
  signal: AbortSignal,
): Promise<TerminationResult> {
  try {
    const response = await fetch(
      `/api/v1/workforce/grants/${encodeURIComponent(grant)}/terminations`,
      {
        method: "POST",
        headers: {
          "Operation-Id": operation,
          "Session-Id": session,
          Accept: "application/json",
        },
        credentials: "same-origin",
        cache: "no-store",
        redirect: "error",
        signal,
      },
    );
    const value = await decode(
      response,
      "POST /api/v1/workforce/grants/{id}/terminations",
    );
    if (!value) return { kind: "untrusted" };
    if (response.status !== 201)
      return {
        kind: "failure",
        status: response.status,
        code: String(value.code),
        retryAfter: retryAfter(response),
      };
    const termination = (value as Termination).termination;
    if (
      termination.operationId !== operation ||
      termination.id !== grant ||
      termination.mode !== "self" ||
      termination.subject.personId !== person ||
      termination.operator.personId !== person
    )
      return { kind: "untrusted" };
    return { kind: "termination", terminatedAt: termination.terminatedAt };
  } catch {
    return { kind: "disconnected" };
  }
}
