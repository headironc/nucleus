import { decode, retryAfter } from "./response";
import type { paths } from "./generated/types";
import type { Failure } from "./session";
import { summarize, type NumberSummary } from "../workforce/number";

type Receipt =
  paths["/api/v1/workforce/current/numbers"]["get"]["responses"][200]["content"]["application/json"];
export type NumberResult =
  { kind: "number"; number: NumberSummary | null } | Failure;

export async function currentNumber(
  id: string,
  signal: AbortSignal,
): Promise<NumberResult> {
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
      if (fixed === null) return { kind: "number", number: null };
      const number = summarize(fixed);
      return number ? { kind: "number", number } : { kind: "untrusted" };
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
