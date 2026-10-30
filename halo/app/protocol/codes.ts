import { decode, registry, retryAfter } from "./response";
import type { paths } from "./generated/types";

type Send = paths["/api/v1/codes"]["post"];
export type Input = Send["requestBody"]["content"]["application/json"];
export type Receipt = Send["responses"][200]["content"]["application/json"];
export type Message = Receipt["message"];
type Settings =
  paths["/api/v1/settings"]["get"]["responses"][200]["content"]["application/json"];

export type Result =
  | { kind: "message"; message: Message }
  | { kind: "failure"; code: string; retryAfter: number }
  | { kind: "untrusted" }
  | { kind: "disconnected" };

export function validInput(input: unknown): input is Input {
  return registry.sendCode_input(input);
}

export function validMessage(value: unknown): value is Message {
  return registry.sendCode_200({ code: "ok", message: value });
}

export async function settings(signal: AbortSignal): Promise<string> {
  const response = await fetch("/api/v1/settings", {
    headers: {
      "Operation-Id": crypto.randomUUID(),
      Accept: "application/json",
    },
    credentials: "omit",
    cache: "no-store",
    redirect: "error",
    signal,
  });
  const value = (await decode(response, "GET /api/v1/settings")) as
    Settings | undefined;
  if (!value || response.status !== 200 || value.code !== "ok")
    throw new Error("settings_unavailable");
  return value.settings.challenge.turnstile.siteKey;
}

export async function send(
  operation: string,
  input: Input,
  signal: AbortSignal,
): Promise<Result> {
  try {
    const response = await fetch("/api/v1/codes", {
      method: "POST",
      headers: {
        "Operation-Id": operation,
        "Content-Type": "application/json",
        Accept: "application/json",
      },
      body: JSON.stringify(input),
      credentials: "omit",
      cache: "no-store",
      redirect: "error",
      signal,
    });
    const value = await decode(response, "POST /api/v1/codes");
    if (!value) return { kind: "untrusted" };
    if (response.status === 200)
      return { kind: "message", message: (value as Receipt).message };
    return {
      kind: "failure",
      code: (value as { code: string }).code,
      retryAfter: retryAfter(response),
    };
  } catch {
    return { kind: "disconnected" };
  }
}
