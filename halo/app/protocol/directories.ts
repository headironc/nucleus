import type { paths } from "./generated/types";
import type { Failure } from "./session";
import { decode, retryAfter } from "./response";

export type DirectorySelection = paths["/api/v1/workforce/directories"]["get"]["responses"][200]["content"]["application/json"];
export type DirectoryResult = { kind: "directories"; selection: DirectorySelection } | Failure;

export async function directories(id: string, signal: AbortSignal): Promise<DirectoryResult> {
  try {
    const response = await fetch("/api/v1/workforce/directories", {
      headers: { "Operation-Id": crypto.randomUUID(), "Session-Id": id, Accept: "application/json" },
      credentials: "same-origin", cache: "no-store", redirect: "error", signal,
    });
    const value = await decode(response, "GET /api/v1/workforce/directories");
    if (!value) return { kind: "untrusted" };
    if (response.status === 200) return { kind: "directories", selection: value as DirectorySelection };
    return { kind: "failure", status: response.status, code: String(value.code), retryAfter: retryAfter(response) };
  } catch { return { kind: "disconnected" }; }
}
