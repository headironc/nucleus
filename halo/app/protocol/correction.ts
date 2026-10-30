import { decode, retryAfter } from "./response";
import type { paths } from "./generated/types";
import type { Failure } from "./session";

type History =
  paths["/api/v1/accounts/current/corrections"]["get"]["responses"][200]["content"]["application/json"];
export type Correction = History["corrections"][number];
export type HistoryResult =
  { kind: "history"; count: number; corrections: Correction[] } | Failure;

function validIdentity(value: Correction["before"]): boolean {
  const name = value.name;
  const length = [...name].length;
  return (
    Object.keys(value).length === 2 &&
    length >= 1 &&
    length <= 64 &&
    name.normalize("NFC") === name &&
    !/^\p{White_Space}|\p{White_Space}$/u.test(name) &&
    !/[\p{Cc}\uD800-\uDFFF]/u.test(name)
  );
}

export async function history(
  id: string,
  page: number,
  size: number,
  signal: AbortSignal,
): Promise<HistoryResult> {
  try {
    const query = new URLSearchParams({
      page: String(page),
      size: String(size),
    });
    const response = await fetch(
      `/api/v1/accounts/current/corrections?${query}`,
      {
        headers: {
          "Operation-Id": crypto.randomUUID(),
          "Session-Id": id,
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
      "GET /api/v1/accounts/current/corrections",
    );
    if (!value) return { kind: "untrusted" };
    if (response.status !== 200)
      return {
        kind: "failure",
        status: response.status,
        code: String(value.code),
        retryAfter: retryAfter(response),
      };
    const result = value as History;
    // int64 counts outside JS's exact range cannot safely drive pagination.
    // This is a client representation failure, not a new backend business limit.
    if (
      Object.keys(result).length !== 3 ||
      !Number.isSafeInteger(result.count) ||
      result.count < 0 ||
      result.corrections.length !==
        Math.min(size, Math.max(0, result.count - (page - 1) * size)) ||
      new Set(result.corrections.map((record) => record.id.toLowerCase()))
        .size !== result.corrections.length ||
      result.corrections.some(
        (record) =>
          Object.keys(record).length !== 4 ||
          !validIdentity(record.before) ||
          !validIdentity(record.after) ||
          !Number.isFinite(Date.parse(record.correctedAt)),
      )
    )
      return { kind: "untrusted" };
    return {
      kind: "history",
      count: result.count,
      corrections: result.corrections,
    };
  } catch {
    return { kind: "disconnected" };
  }
}
