import { decode, retryAfter } from "./response";
import type { paths } from "./generated/types";
import type { Failure } from "./session";

type Current =
  paths["/api/v1/accounts/current/affiliation"]["get"]["responses"][200]["content"]["application/json"];
type Values =
  paths["/api/v1/affiliations"]["get"]["responses"][200]["content"]["application/json"];
export type AffiliationCode = Current["affiliation"]["value"];
export type AffiliationValue = Readonly<{
  value: AffiliationCode;
  createdAt: string;
}>;
export type NamedAffiliation = Readonly<{
  value: AffiliationCode;
  name: string;
}>;
export type CurrentAffiliationResult =
  | { kind: "affiliation"; affiliation: AffiliationValue }
  /** Only a trusted 404 affiliation_unset may reach this state. */
  | { kind: "unset" }
  | Failure;
export type AffiliationValuesResult =
  | { kind: "values"; affiliations: readonly NamedAffiliation[] }
  | Failure;
export type AffiliationWriteResult = { kind: "ok" } | Failure;

/** Fixed GB/T 4762 codes in their only display order. Names come from the API. */
export const AFFILIATION_CODES: readonly AffiliationCode[] = Array.from(
  { length: 13 },
  (_, index) => String(index + 1).padStart(2, "0") as AffiliationCode,
);

function isCode(value: unknown): value is AffiliationCode {
  return (
    typeof value === "string" &&
    (AFFILIATION_CODES as readonly string[]).includes(value)
  );
}

function exactKeys(value: object, expected: readonly string[]): boolean {
  const keys = Object.keys(value).sort();
  return (
    keys.length === expected.length &&
    [...expected].sort().every((key, index) => keys[index] === key)
  );
}

function validName(name: unknown): name is string {
  return (
    typeof name === "string" &&
    name.length > 0 &&
    [...name].length <= 64 &&
    name.normalize("NFC") === name &&
    name.trim() === name &&
    !/[\p{Cc}\uD800-\uDFFF]/u.test(name)
  );
}

function validTimestamp(value: unknown): value is string {
  if (typeof value !== "string") return false;
  const match =
    /^(\d{4})-(\d{2})-(\d{2})T([01]\d|2[0-3]):([0-5]\d):([0-5]\d)(?:\.(\d{1,9}))?(Z|([+-])([01]\d|2[0-3]):([0-5]\d))$/.exec(
      value,
    );
  if (!match) return false;
  const [, year, month, day, hour, minute, second, fraction] = match;
  const milliseconds = Number((fraction ?? "").padEnd(3, "0").slice(0, 3) || 0);
  const check = new Date(
    Date.UTC(
      Number(year),
      Number(month) - 1,
      Number(day),
      Number(hour),
      Number(minute),
      Number(second),
      milliseconds,
    ),
  );
  return (
    check.getUTCFullYear() === Number(year) &&
    check.getUTCMonth() === Number(month) - 1 &&
    check.getUTCDate() === Number(day) &&
    check.getUTCHours() === Number(hour) &&
    check.getUTCMinutes() === Number(minute) &&
    check.getUTCSeconds() === Number(second) &&
    check.getUTCMilliseconds() === milliseconds
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

export async function currentAffiliation(
  id: string,
  signal: AbortSignal,
): Promise<CurrentAffiliationResult> {
  try {
    const response = await fetch("/api/v1/accounts/current/affiliation", {
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
      "GET /api/v1/accounts/current/affiliation",
    );
    if (!value) return { kind: "untrusted" };
    if (response.status === 200) {
      const result = value as Current;
      const affiliation = result.affiliation;
      if (
        !exactKeys(result, ["code", "affiliation"]) ||
        !exactKeys(affiliation, ["value", "createdAt"]) ||
        !isCode(affiliation.value) ||
        !validTimestamp(affiliation.createdAt)
      )
        return { kind: "untrusted" };
      return {
        kind: "affiliation",
        affiliation: {
          value: affiliation.value,
          createdAt: affiliation.createdAt,
        },
      };
    }
    // A 404 with any other reachable code is not the unset fact.
    if (response.status === 404 && value.code === "affiliation_unset")
      return { kind: "unset" };
    return failure(response, value);
  } catch {
    return { kind: "disconnected" };
  }
}

function validNamed(value: unknown, index: number): value is NamedAffiliation {
  return (
    !!value &&
    typeof value === "object" &&
    exactKeys(value, ["name", "value"]) &&
    "value" in value &&
    isCode(value.value) &&
    value.value === AFFILIATION_CODES[index] &&
    "name" in value &&
    validName(value.name)
  );
}

export async function affiliationValues(
  id: string,
  signal: AbortSignal,
): Promise<AffiliationValuesResult> {
  try {
    const response = await fetch("/api/v1/affiliations", {
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
    const value = await decode(response, "GET /api/v1/affiliations");
    if (!value) return { kind: "untrusted" };
    if (response.status !== 200) return failure(response, value);
    const result = value as Values;
    const affiliations = result.affiliations;
    if (
      !exactKeys(result, ["code", "affiliations"]) ||
      !Array.isArray(affiliations) ||
      affiliations.length !== AFFILIATION_CODES.length ||
      !affiliations.every(validNamed) ||
      new Set(affiliations.map((entry) => entry.name)).size !==
        affiliations.length
    )
      return { kind: "untrusted" };
    return { kind: "values", affiliations };
  } catch {
    return { kind: "disconnected" };
  }
}

export async function createAffiliation(
  operation: string,
  id: string,
  code: AffiliationCode,
  signal: AbortSignal,
): Promise<AffiliationWriteResult> {
  try {
    const response = await fetch("/api/v1/accounts/current/affiliation", {
      method: "PUT",
      headers: {
        "Operation-Id": operation,
        "Session-Id": id,
        "Content-Type": "application/json",
        Accept: "application/json",
      },
      body: JSON.stringify({ affiliation: code }),
      credentials: "same-origin",
      cache: "no-store",
      redirect: "error",
      signal,
    });
    const value = await decode(
      response,
      "PUT /api/v1/accounts/current/affiliation",
    );
    if (!value) return { kind: "untrusted" };
    if (response.status !== 200) return failure(response, value);
    // A receipt confirms only the original operation. It never carries the
    // current value, effective time or a result identity.
    if (value.code !== "ok" || !exactKeys(value, ["code"]))
      return { kind: "untrusted" };
    return { kind: "ok" };
  } catch {
    return { kind: "disconnected" };
  }
}
