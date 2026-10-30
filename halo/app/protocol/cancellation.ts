import { decode, registry, retryAfter } from "./response";
import type { paths } from "./generated/types";
import type { Failure } from "./session";

type Operation = paths["/api/v1/accounts/current/cancellations"]["post"];
type Receipt = Operation["responses"][200]["content"]["application/json"];
export type Input = Operation["requestBody"]["content"]["application/json"];
export type Result = Receipt["cancellation"];

export type CancelResult = { kind: "cancelled"; result: Result } | Failure;

export function validInput(input: unknown): input is Input {
  return registry.cancelCurrentAccount_input(input);
}

export function validResult(value: unknown): value is Result {
  if (!registry.cancelCurrentAccount_200({ code: "ok", cancellation: value }))
    return false;
  const result = value as Result;
  return Number.isFinite(Date.parse(result.cancelledAt));
}

export async function cancel(
  operation: string,
  session: string,
  input: Input,
  signal: AbortSignal,
): Promise<CancelResult> {
  if (!validInput(input)) return { kind: "untrusted" };
  try {
    const response = await fetch("/api/v1/accounts/current/cancellations", {
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
    const value = await decode(
      response,
      "POST /api/v1/accounts/current/cancellations",
    );
    if (!value) return { kind: "untrusted" };
    if (response.status !== 200)
      return {
        kind: "failure",
        status: response.status,
        code: String(value.code),
        retryAfter: retryAfter(response),
      };
    const receipt = value as Receipt;
    if (!validResult(receipt.cancellation)) return { kind: "untrusted" };
    return { kind: "cancelled", result: receipt.cancellation };
  } catch {
    return { kind: "disconnected" };
  }
}
