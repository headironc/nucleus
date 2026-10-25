import { decode, registry, retryAfter } from "./response";
import type { paths } from "./generated/types";

type Login = paths["/api/v1/sessions"]["post"];
type Receipt = Login["responses"][200]["content"]["application/json"];
export type Input = Login["requestBody"]["content"]["application/json"];
export type Session = Extract<Receipt, { session: unknown }>["session"];
export type Registration = Extract<
  Receipt,
  { registration: unknown }
>["registration"];
type Current =
  paths["/api/v1/accounts/current"]["get"]["responses"][200]["content"]["application/json"];
export type Account = Current["account"];

export type Failure =
  | { kind: "failure"; code: string; status: number; retryAfter: number }
  | { kind: "untrusted" }
  | { kind: "disconnected" };
export type LoginResult =
  | { kind: "session"; session: Session }
  | { kind: "registration"; registration: Registration }
  | Failure;
export type CurrentResult = { kind: "account"; account: Account } | Failure;

export function validInput(input: unknown): input is Input {
  return registry.createSession_input(input);
}

export function validSession(value: unknown): value is Session {
  return registry.createSession_200({ code: "ok", session: value });
}

export function validRegistration(value: unknown): value is Registration {
  return registry.createSession_200({ code: "ok", registration: value });
}

function failure(response: Response, value: { code: unknown }): Failure {
  return {
    kind: "failure",
    status: response.status,
    code: String(value.code),
    retryAfter: retryAfter(response),
  };
}

export async function login(
  operation: string,
  input: Input,
  signal: AbortSignal,
): Promise<LoginResult> {
  try {
    const response = await fetch("/api/v1/sessions", {
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
    const value = await decode(response, "POST /api/v1/sessions");
    if (!value) return { kind: "untrusted" };
    if (response.status !== 200) return failure(response, value);
    const result = value as Receipt;
    return "session" in result
      ? { kind: "session", session: result.session }
      : { kind: "registration", registration: result.registration };
  } catch {
    return { kind: "disconnected" };
  }
}

function validAvatarLink(avatar: Account["avatar"]): boolean {
  try {
    const url = new URL(avatar.href);
    const query = url.searchParams;
    // Rebuilding with the standard serializer rejects repairs, escaping,
    // credentials and fragments, while preserving the supplied query order.
    if (
      url.protocol !== "https:" ||
      avatar.href !== `${url.origin}${url.pathname}?${query}` ||
      query.size !== 3 ||
      ["key_id", "expires_at", "signature"].some(
        (field) => query.getAll(field).length !== 1,
      )
    )
      return false;
    const keyText = query.get("key_id")!;
    const secondsText = query.get("expires_at")!;
    if (keyText.length > 19 || secondsText.length > 20) return false;
    const key = BigInt(keyText);
    const seconds = BigInt(secondsText);
    if (
      key < 0n ||
      key > 9223372036854775807n ||
      key.toString() !== keyText ||
      seconds < 0n ||
      seconds > 18446744073709551615n ||
      seconds.toString() !== secondsText
    )
      return false;
    const signature = query.get("signature")!;
    if (
      signature.length !== 43 ||
      !/^[A-Za-z0-9_-]{42}[AEIMQUYcgkosw048]$/.test(signature)
    )
      return false;
    // Signed links carry whole seconds. Date.parse alone would truncate a
    // smaller fractional value and could conceal a different expiry fact.
    const fraction = avatar.expiresAt.match(/\.(\d+)/)?.[1];
    const milliseconds = Date.parse(avatar.expiresAt);
    return (
      !(fraction && /[1-9]/.test(fraction)) &&
      Number.isSafeInteger(milliseconds) &&
      BigInt(milliseconds) === seconds * 1000n
    );
  } catch {
    return false;
  }
}

export async function current(
  id: string,
  signal: AbortSignal,
): Promise<CurrentResult> {
  try {
    const response = await fetch("/api/v1/accounts/current", {
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
    const value = await decode(response, "GET /api/v1/accounts/current");
    if (!value) return { kind: "untrusted" };
    if (response.status !== 200) return failure(response, value);
    const account = (value as Current).account;
    if (
      account.name.normalize("NFC") !== account.name ||
      account.avatar.width !== account.avatar.height ||
      !validAvatarLink(account.avatar)
    )
      return { kind: "untrusted" };
    return { kind: "account", account };
  } catch {
    return { kind: "disconnected" };
  }
}
