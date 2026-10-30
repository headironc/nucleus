import { decode, retryAfter } from "./response";
import type { paths } from "./generated/types";
import type { Failure } from "./session";
import { summarize, type NumberSummary } from "../workforce/number";

type Receipt =
  paths["/api/v1/workforce/current/numbers"]["get"]["responses"][200]["content"]["application/json"];
export type NumberResult =
  { kind: "number"; number: NumberSummary | null } | Failure;

export type AllocationInput =
  paths["/api/v1/workforce/numbers"]["post"]["requestBody"]["content"]["application/json"];
export type FixedNumber = NonNullable<Receipt["number"]>;
export type AllocationResult =
  { kind: "allocated"; number: FixedNumber } | Failure;

/** Replays preserve both the operation and the complete original subtype input. */
export async function allocateNumber(
  operation: string,
  id: string,
  input: AllocationInput,
  signal: AbortSignal,
): Promise<AllocationResult> {
  try {
    const response = await fetch("/api/v1/workforce/numbers", {
      method: "POST",
      headers: {
        "Operation-Id": operation,
        "Session-Id": id,
        "Content-Type": "application/json",
        Accept: "application/json",
      },
      body: JSON.stringify(input),
      credentials: "same-origin",
      cache: "no-store",
      redirect: "error",
      signal,
    });
    const value = await decode(response, "POST /api/v1/workforce/numbers");
    if (!value) return { kind: "untrusted" };
    if (response.status === 201) {
      const number = (value as { code: string; number: FixedNumber }).number;
      return number.subtype === input.subtype && summarize(number)
        ? { kind: "allocated", number }
        : { kind: "untrusted" };
    }
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

export async function currentNumber(
  id: string,
  signal: AbortSignal,
): Promise<NumberResult> {
  const result = await currentNumberValue(id, signal);
  if (result.kind !== "value") return result;
  return {
    kind: "number",
    number: result.number === null ? null : summarize(result.number)!,
  };
}

/** The application selector needs the actual subtype, rather than its display label. */
export async function currentNumberValue(
  id: string,
  signal: AbortSignal,
): Promise<{ kind: "value"; number: FixedNumber | null } | Failure> {
  try {
    const response = await fetch("/api/v1/workforce/current/numbers", {
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
    const value = await decode(
      response,
      "GET /api/v1/workforce/current/numbers",
    );
    if (!value) return { kind: "untrusted" };
    if (response.status === 200) {
      const fixed = (value as Receipt).number;
      return fixed === null || summarize(fixed)
        ? { kind: "value", number: fixed }
        : { kind: "untrusted" };
    }
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
