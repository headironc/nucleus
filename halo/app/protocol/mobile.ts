import { decode, registry, retryAfter } from "./response";
import type { paths } from "./generated/types";

type Send = paths["/api/v1/accounts/current/codes"]["post"];
type NewSend = paths["/api/v1/mobile-changes/{id}/codes"]["post"];
type Establish = paths["/api/v1/mobile-changes"]["post"];
type Complete = paths["/api/v1/accounts/current/mobile"]["put"];
type Read = paths["/api/v1/mobile-changes/{id}"]["get"];

export type SendInput = Send["requestBody"]["content"]["application/json"];
export type Message = Extract<
  Send["responses"][200]["content"]["application/json"],
  { message: unknown }
>["message"];
export type EstablishInput =
  Establish["requestBody"]["content"]["application/json"];
export type Transfer = Extract<
  Establish["responses"][201]["content"]["application/json"],
  { transfer: unknown }
>["transfer"];
export type CompleteInput =
  Complete["requestBody"]["content"]["application/json"];
export type Completion = Extract<
  Complete["responses"][200]["content"]["application/json"],
  { completion: unknown }
>["completion"];
export type State = Extract<
  Read["responses"][200]["content"]["application/json"],
  { transfer: unknown }
>["transfer"];

export type Failure = {
  kind: "failure";
  status: number;
  code: string;
  retryAfter: number;
};
export type Result<T> =
  | { kind: "ok"; value: T }
  | Failure
  | { kind: "untrusted" }
  | { kind: "disconnected" };

export function validSendInput(value: unknown): value is SendInput {
  return (
    registry.sendCurrentMobileCode_input(value) &&
    registry.sendNewMobileCode_input(value)
  );
}

export function validEstablishInput(value: unknown): value is EstablishInput {
  return registry.createMobileTransfer_input(value);
}

export function validCompleteInput(value: unknown): value is CompleteInput {
  return registry.completeMobileTransfer_input(value);
}

export function validTransfer(value: unknown): value is Transfer {
  return (
    !!value &&
    typeof value === "object" &&
    !Array.isArray(value) &&
    Object.keys(value).length === 2 &&
    registry.createMobileTransfer_201({ code: "ok", transfer: value })
  );
}

function timestamp(value: unknown): value is string {
  return typeof value === "string" && Number.isFinite(Date.parse(value));
}

/** A state is exactly the branch its status names; extra fields are rejected. */
export function validState(value: unknown): value is State {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const state = value as Record<string, unknown>;
  const keys = Object.keys(value);
  if (state.status === "pending")
    return (
      keys.length === 2 &&
      keys.includes("expiresAt") &&
      timestamp(state.expiresAt)
    );
  if (state.status === "completed")
    return (
      keys.length === 2 &&
      keys.includes("completedAt") &&
      timestamp(state.completedAt)
    );
  return state.status === "expired" && keys.length === 1;
}

export function validCompletion(
  value: unknown,
  transfer: string,
): value is Completion {
  return (
    !!value &&
    typeof value === "object" &&
    !Array.isArray(value) &&
    Object.keys(value).length === 2 &&
    (value as Completion).id === transfer &&
    timestamp((value as Completion).completedAt) &&
    registry.completeMobileTransfer_200({ code: "ok", completion: value })
  );
}

function failure(response: Response, value: { code: unknown }): Failure {
  return {
    kind: "failure",
    status: response.status,
    code: String(value.code),
    retryAfter: retryAfter(response),
  };
}

async function send(
  operation: string,
  session: string,
  path: string,
  input: SendInput,
  signal: AbortSignal,
): Promise<Result<Message>> {
  try {
    const response = await fetch(path, {
      method: "POST",
      headers: {
        "Operation-Id": operation,
        "Session-Id": session,
        "Content-Type": "application/json",
        Accept: "application/json",
      },
      body: JSON.stringify(input),
      credentials: "same-origin",
      cache: "no-store",
      redirect: "error",
      signal,
    });
    const endpoint =
      path === "/api/v1/accounts/current/codes"
        ? "POST /api/v1/accounts/current/codes"
        : "POST /api/v1/mobile-changes/{id}/codes";
    const value = await decode(response, endpoint);
    if (!value) return { kind: "untrusted" };
    if (response.status !== 200) return failure(response, value);
    const message = (value as unknown as { message: unknown }).message;
    const validator =
      path === "/api/v1/accounts/current/codes"
        ? registry.sendCurrentMobileCode_200
        : registry.sendNewMobileCode_200;
    if (!validator({ code: "ok", message })) return { kind: "untrusted" };
    return { kind: "ok", value: message as Message };
  } catch {
    return { kind: "disconnected" };
  }
}

export function sendCurrentCode(
  operation: string,
  session: string,
  input: SendInput,
  signal: AbortSignal,
): Promise<Result<Message>> {
  return send(
    operation,
    session,
    "/api/v1/accounts/current/codes",
    input,
    signal,
  );
}

export function sendNewCode(
  operation: string,
  session: string,
  transfer: string,
  input: SendInput,
  signal: AbortSignal,
): Promise<Result<Message>> {
  return send(
    operation,
    session,
    `/api/v1/mobile-changes/${encodeURIComponent(transfer)}/codes`,
    input,
    signal,
  );
}

export async function establish(
  operation: string,
  session: string,
  input: EstablishInput,
  signal: AbortSignal,
): Promise<Result<Transfer>> {
  try {
    const response = await fetch("/api/v1/mobile-changes", {
      method: "POST",
      headers: {
        "Operation-Id": operation,
        "Session-Id": session,
        "Content-Type": "application/json",
        Accept: "application/json",
      },
      body: JSON.stringify(input),
      credentials: "same-origin",
      cache: "no-store",
      redirect: "error",
      signal,
    });
    const value = await decode(response, "POST /api/v1/mobile-changes");
    if (!value) return { kind: "untrusted" };
    if (response.status !== 201) return failure(response, value);
    const transfer = (value as unknown as { transfer: unknown }).transfer;
    if (!validTransfer(transfer)) return { kind: "untrusted" };
    return { kind: "ok", value: transfer };
  } catch {
    return { kind: "disconnected" };
  }
}

export async function complete(
  operation: string,
  session: string,
  transfer: string,
  input: CompleteInput,
  signal: AbortSignal,
): Promise<Result<Completion>> {
  try {
    const response = await fetch("/api/v1/accounts/current/mobile", {
      method: "PUT",
      headers: {
        "Operation-Id": operation,
        "Session-Id": session,
        "Content-Type": "application/json",
        Accept: "application/json",
      },
      body: JSON.stringify(input),
      credentials: "same-origin",
      cache: "no-store",
      redirect: "error",
      signal,
    });
    const value = await decode(response, "PUT /api/v1/accounts/current/mobile");
    if (!value) return { kind: "untrusted" };
    if (response.status !== 200) return failure(response, value);
    const completion = (value as unknown as { completion: unknown }).completion;
    if (!validCompletion(completion, transfer)) return { kind: "untrusted" };
    return { kind: "ok", value: completion };
  } catch {
    return { kind: "disconnected" };
  }
}

export async function read(
  session: string,
  transfer: string,
  signal: AbortSignal,
): Promise<Result<State>> {
  try {
    const response = await fetch(
      `/api/v1/mobile-changes/${encodeURIComponent(transfer)}`,
      {
        headers: {
          "Operation-Id": crypto.randomUUID(),
          "Session-Id": session,
          Accept: "application/json",
        },
        credentials: "same-origin",
        cache: "no-store",
        redirect: "error",
        signal,
      },
    );
    const value = await decode(response, "GET /api/v1/mobile-changes/{id}");
    if (!value) return { kind: "untrusted" };
    if (response.status !== 200) return failure(response, value);
    const state = (value as unknown as { transfer: unknown }).transfer;
    if (!validState(state)) return { kind: "untrusted" };
    return { kind: "ok", value: state };
  } catch {
    return { kind: "disconnected" };
  }
}
