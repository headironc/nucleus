import { decode, registry, retryAfter } from "./response";
import type { paths } from "./generated/types";
import type { Failure } from "./session";

type Registration = paths["/api/v1/registrations"]["post"];
type Receipt = Registration["responses"][201]["content"]["application/json"];
export type Input = Registration["requestBody"]["content"]["application/json"];
export type Result = { kind: "session"; session: Receipt["session"] } | Failure;

export function validInput(value: unknown): value is Input {
  return registry.register_input(value);
}

export async function register(
  operation: string,
  input: Input,
  signal: AbortSignal,
): Promise<Result> {
  try {
    const response = await fetch("/api/v1/registrations", {
      method: "POST",
      headers: {
        "Operation-Id": operation,
        "Content-Type": "application/json",
        Accept: "application/json",
      },
      body: JSON.stringify(input),
      credentials: "same-origin",
      cache: "no-store",
      redirect: "error",
      signal,
    });
    const value = await decode(response, "POST /api/v1/registrations");
    if (!value) return { kind: "untrusted" };
    if (response.status === 200 || response.status === 201)
      return { kind: "session", session: (value as Receipt).session };
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
