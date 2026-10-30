import { decode, registry, retryAfter } from "./response";
import type { paths } from "./generated/types";
import type { Failure } from "./session";

type RoleReceipt =
  paths["/api/v1/accounts/current/role"]["get"]["responses"][200]["content"]["application/json"];
export type Role = RoleReceipt["role"];
export type Candidate =
  paths["/api/v1/workforce/grants/candidates"]["post"]["responses"][200]["content"]["application/json"];
export type AdmissionInput =
  paths["/api/v1/workforce/grants"]["post"]["requestBody"]["content"]["application/json"];
export type Receipt =
  paths["/api/v1/workforce/grants"]["post"]["responses"][201]["content"]["application/json"];
export type RoleResult = { kind: "role"; role: Role } | Failure;
export type CandidateResult =
  { kind: "candidate"; candidate: Candidate } | Failure;
export type AdmissionResult = { kind: "complete"; receipt: Receipt } | Failure;

function options(
  session: string,
  signal: AbortSignal,
  operation: string = crypto.randomUUID(),
): RequestInit {
  return {
    headers: {
      "Operation-Id": operation,
      "Session-Id": session,
      Accept: "application/json",
    },
    credentials: "same-origin",
    cache: "no-store",
    redirect: "error",
    signal,
  };
}
function failure(response: Response, value: { code: unknown }): Failure {
  return {
    kind: "failure",
    status: response.status,
    code: String(value.code),
    retryAfter: retryAfter(response),
  };
}
export function validAdmissionInput(input: unknown): input is AdmissionInput {
  return registry.createWorkforceGrant_input(input);
}
export async function institutionRole(
  session: string,
  signal: AbortSignal,
): Promise<RoleResult> {
  try {
    const response = await fetch(
      "/api/v1/accounts/current/role",
      options(session, signal),
    );
    const value = await decode(response, "GET /api/v1/accounts/current/role");
    if (!value) return { kind: "untrusted" };
    return response.status === 200
      ? { kind: "role", role: (value as RoleReceipt).role }
      : failure(response, value);
  } catch {
    return { kind: "disconnected" };
  }
}
export async function admissionCandidate(
  session: string,
  mobile: string,
  page: number,
  signal: AbortSignal,
): Promise<CandidateResult> {
  try {
    const init = options(session, signal);
    const response = await fetch(
      `/api/v1/workforce/grants/candidates?page=${page}&size=100`,
      {
        ...init,
        method: "POST",
        headers: { ...init.headers, "Content-Type": "application/json" },
        body: JSON.stringify({ entry: "institution", mobile }),
      },
    );
    const value = await decode(
      response,
      "POST /api/v1/workforce/grants/candidates",
    );
    if (!value) return { kind: "untrusted" };
    if (response.status !== 200) return failure(response, value);
    const candidate = value as Candidate;
    if (
      !Number.isSafeInteger(candidate.count) ||
      candidate.count < 0 ||
      candidate.roles.length > 100
    )
      return { kind: "untrusted" };
    return { kind: "candidate", candidate };
  } catch {
    return { kind: "disconnected" };
  }
}
export async function admit(
  operation: string,
  input: AdmissionInput,
  operator: string,
  session: string,
  signal: AbortSignal,
): Promise<AdmissionResult> {
  try {
    const init = options(session, signal, operation);
    const response = await fetch("/api/v1/workforce/grants", {
      ...init,
      method: "POST",
      headers: { ...init.headers, "Content-Type": "application/json" },
      body: JSON.stringify(input),
    });
    const value = await decode(response, "POST /api/v1/workforce/grants");
    if (!value) return { kind: "untrusted" };
    if (response.status !== 201) return failure(response, value);
    const receipt = value as Receipt;
    const { grant, assignment } = receipt;
    if (
      grant.operationId !== operation ||
      assignment.operationId !== operation ||
      grant.subject.personId !== input.target.personId ||
      grant.subject.revisionId !== input.target.revisionId ||
      grant.operator.personId !== operator ||
      assignment.operator.personId !== operator ||
      assignment.operator.revisionId !== grant.operator.revisionId ||
      assignment.subject.personId !== grant.subject.personId ||
      assignment.subject.revisionId !== grant.subject.revisionId ||
      assignment.grantId !== grant.id ||
      assignment.role.id !== input.role.id ||
      assignment.role.changeId !== input.role.changeId ||
      assignment.sequence !== 1 ||
      assignment.assignedAt !== grant.grantedAt
    )
      return { kind: "untrusted" };
    return { kind: "complete", receipt };
  } catch {
    return { kind: "disconnected" };
  }
}
