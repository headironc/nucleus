import type { components } from "./generated/types";
import binary from "./generated/binary.js";
import { decode, retryAfter } from "./response";
import type contracts from "./generated/contracts.js";
import type { Failure } from "./session";

export type ApplicationSnapshot = components["schemas"]["application.Snapshot"];
export type ApplicationTemplate =
  components["schemas"]["application.TemplateResponse"];
export type ApplicationPhoto =
  components["schemas"]["application.PhotoResponse"];
export type ApplicationPreview =
  components["schemas"]["application.PreviewResponse"];
export type PreviewInput = components["schemas"]["application.PreviewInput"];
export type ApplicationInput = components["schemas"]["application.Input"];
export type Application = components["schemas"]["application.Application"];
export type ApplicationList = components["schemas"]["application.ListResponse"];
export type ApplicationDetail =
  components["schemas"]["application.DetailResponse"];
export type ApplicationStatus = components["schemas"]["application.Status"];
export type ManagedApplicationList =
  components["schemas"]["application.ManagedListResponse"];
export type ManagedApplicationDetail =
  components["schemas"]["application.ManagedDetailResponse"];
export type ReviewInput = components["schemas"]["application.ReviewInput"];
export type CancellationInput =
  components["schemas"]["application.CancellationInput"];
export type PhotoMetadata = components["schemas"]["application.Photo"];
export type ApplicationResult<T> = { kind: "ok"; value: T } | Failure;

/** Compares actual JSON content, without introducing a digest or version identity. */
export function sameApplicationContent(left: unknown, right: unknown): boolean {
  if (left === right) return true;
  if (!left || !right || typeof left !== "object" || typeof right !== "object")
    return false;
  if (Array.isArray(left) !== Array.isArray(right)) return false;
  const a = Object.keys(left).sort(),
    b = Object.keys(right).sort();
  return (
    a.length === b.length &&
    a.every(
      (key, index) =>
        key === b[index] &&
        sameApplicationContent(
          (left as Record<string, unknown>)[key],
          (right as Record<string, unknown>)[key],
        ),
    )
  );
}

async function request<T>(
  endpoint: keyof typeof contracts,
  url: string,
  session: string,
  signal: AbortSignal,
  input?: unknown | File,
): Promise<ApplicationResult<T>> {
  const method = endpoint.startsWith("POST ") ? "POST" : "GET";
  try {
    const headers: Record<string, string> = {
      "Operation-Id": crypto.randomUUID(),
      "Session-Id": session,
      Accept: "application/json",
    };
    let body: BodyInit | undefined;
    if (input instanceof File) {
      headers["Content-Type"] = input.type;
      body = input;
    } else if (input !== undefined) {
      headers["Content-Type"] = "application/json";
      body = JSON.stringify(input);
    }
    const response = await fetch(url, {
      method,
      headers,
      body,
      credentials: "same-origin",
      redirect: "error",
      cache: "no-store",
      signal,
    });
    const value = await decode(response, endpoint);
    if (!value) return { kind: "untrusted" };
    const expected =
      endpoint === "POST /api/v1/workforce/applications" ||
      endpoint === "POST /api/v1/workforce/application-photos"
        ? 201
        : 200;
    if (response.status === expected) return { kind: "ok", value: value as T };
    return {
      kind: "failure",
      code: String(value.code),
      status: response.status,
      retryAfter: retryAfter(response),
    };
  } catch {
    return { kind: "disconnected" };
  }
}

export async function managedApplications(
  session: string,
  page: number,
  status: ApplicationStatus,
  signal: AbortSignal,
): Promise<ApplicationResult<ManagedApplicationList>> {
  const result = await request<ManagedApplicationList>(
    "GET /api/v1/workforce/applications",
    `/api/v1/workforce/applications?page=${page}&size=20&status=${encodeURIComponent(status)}`,
    session,
    signal,
  );
  if (
    result.kind === "ok" &&
    (!Number.isSafeInteger(result.value.count) ||
      result.value.count < 0 ||
      result.value.applications.length > 20 ||
      result.value.applications.some((item) => item.status !== status))
  )
    return { kind: "untrusted" };
  return result;
}
export async function managedApplication(
  session: string,
  id: string,
  signal: AbortSignal,
): Promise<ApplicationResult<ManagedApplicationDetail>> {
  const result = await request<ManagedApplicationDetail>(
    "GET /api/v1/workforce/applications/{id}",
    `/api/v1/workforce/applications/${encodeURIComponent(id)}`,
    session,
    signal,
  );
  if (result.kind === "ok" && result.value.application.id !== id)
    return { kind: "untrusted" };
  return result;
}
export async function reviewApplication(
  session: string,
  id: string,
  input: ReviewInput,
  signal: AbortSignal,
): Promise<ApplicationResult<Application>> {
  const result = await request<components["schemas"]["application.Created"]>(
    "POST /api/v1/workforce/applications/{id}/reviews",
    `/api/v1/workforce/applications/${encodeURIComponent(id)}/reviews`,
    session,
    signal,
    input,
  );
  if (result.kind !== "ok") return result;
  const item = result.value.application;
  if (
    item.id !== id ||
    item.status !== (input.decision === "approve" ? "approved" : "rejected")
  )
    return { kind: "untrusted" };
  return { kind: "ok", value: item };
}

export async function cancelApplication(
  session: string,
  id: string,
  input: CancellationInput,
  signal: AbortSignal,
): Promise<ApplicationResult<components["schemas"]["application.Cancelled"]>> {
  const result = await request<
    components["schemas"]["application.Cancellation"]
  >(
    "POST /api/v1/workforce/applications/{id}/cancellations",
    `/api/v1/workforce/applications/${encodeURIComponent(id)}/cancellations`,
    session,
    signal,
    input,
  );
  if (result.kind !== "ok") return result;
  if (result.value.application.id !== id) return { kind: "untrusted" };
  return { kind: "ok", value: result.value.application };
}

export function applicationTemplate(session: string, signal: AbortSignal) {
  return request<ApplicationTemplate>(
    "GET /api/v1/workforce/application-template",
    "/api/v1/workforce/application-template",
    session,
    signal,
  );
}
export function prepareApplicationPhoto(
  session: string,
  file: File,
  signal: AbortSignal,
) {
  if (
    !binary["POST /api/v1/workforce/application-photos"].input.includes(
      file.type,
    ) ||
    file.size > 8 * 1024 * 1024
  )
    return Promise.resolve<ApplicationResult<ApplicationPhoto>>({
      kind: "untrusted",
    });
  return request<ApplicationPhoto>(
    "POST /api/v1/workforce/application-photos",
    "/api/v1/workforce/application-photos",
    session,
    signal,
    file,
  );
}
export async function previewApplication(
  session: string,
  input: PreviewInput,
  signal: AbortSignal,
): Promise<ApplicationResult<ApplicationPreview>> {
  const result = await request<ApplicationPreview>(
    "POST /api/v1/workforce/applications/preview",
    "/api/v1/workforce/applications/preview",
    session,
    signal,
    input,
  );
  if (
    result.kind === "ok" &&
    (result.value.kind !== input.kind ||
      result.value.snapshot.photoId !== input.selections.photoId ||
      result.value.photo.photoId !== input.selections.photoId ||
      result.value.snapshot.department.id !== input.selections.departmentId ||
      (result.value.snapshot.position?.rosterId ?? null) !==
        input.selections.rosterId ||
      (result.value.snapshot.title?.id ?? null) !== input.selections.titleId ||
      result.value.snapshot.pickup !== input.selections.pickup)
  )
    return { kind: "untrusted" };
  return result;
}
export async function submitApplication(
  session: string,
  input: ApplicationInput,
  signal: AbortSignal,
): Promise<ApplicationResult<Application>> {
  const result = await request<components["schemas"]["application.Created"]>(
    "POST /api/v1/workforce/applications",
    "/api/v1/workforce/applications",
    session,
    signal,
    input,
  );
  if (result.kind !== "ok") return result;
  const application = result.value.application;
  if (
    application.status !== "submitted" ||
    application.kind !== input.kind ||
    !sameApplicationContent(application.snapshot, input.expected) ||
    application.photo.photoId !== input.expected.photoId
  )
    return { kind: "untrusted" };
  return { kind: "ok", value: application };
}
export async function applications(
  session: string,
  page: number,
  signal: AbortSignal,
): Promise<ApplicationResult<ApplicationList>> {
  const result = await request<ApplicationList>(
    "GET /api/v1/workforce/current/applications",
    `/api/v1/workforce/current/applications?page=${page}&size=20`,
    session,
    signal,
  );
  if (result.kind !== "ok") return result;
  if (!Number.isSafeInteger(result.value.count) || result.value.count < 0)
    return { kind: "untrusted" };
  if (
    result.value.access === "restricted" &&
    result.value.applications.some(
      (item) => item.status !== "submitted" && item.status !== "approved",
    )
  )
    return { kind: "untrusted" };
  if (result.value.access === "restricted")
    return {
      kind: "ok",
      value: {
        ...result.value,
        applications: result.value.applications.map(
          ({ id, kind, status, submittedAt }) => ({
            id,
            kind,
            status,
            submittedAt,
          }),
        ),
      },
    };
  return result;
}
export async function application(
  session: string,
  id: string,
  signal: AbortSignal,
): Promise<ApplicationResult<ApplicationDetail>> {
  const result = await request<ApplicationDetail>(
    "GET /api/v1/workforce/current/applications/{id}",
    `/api/v1/workforce/current/applications/${encodeURIComponent(id)}`,
    session,
    signal,
  );
  if (result.kind === "ok" && result.value.application.id !== id)
    return { kind: "untrusted" };
  if (result.kind === "ok" && result.value.access === "restricted") {
    const { id, kind, status, submittedAt } = result.value.application;
    if (status !== "submitted" && status !== "approved")
      return { kind: "untrusted" };
    return {
      kind: "ok",
      value: {
        code: "ok",
        access: "restricted",
        application: { id, kind, status, submittedAt },
      },
    };
  }
  return result;
}

export async function applicationResource(
  session: string,
  id: string,
  signal: AbortSignal,
): Promise<ApplicationResult<{ blob: Blob; mediaType: string }>> {
  try {
    const response = await fetch(
      `/api/v1/workforce/application-resources/${encodeURIComponent(id)}`,
      {
        headers: { "Session-Id": session },
        credentials: "same-origin",
        cache: "no-store",
        redirect: "error",
        signal,
      },
    );
    if (response.status !== 200) {
      const value = await decode(
        response,
        "GET /api/v1/workforce/application-resources/{id}",
      );
      return value
        ? {
            kind: "failure",
            status: response.status,
            code: String(value.code),
            retryAfter: retryAfter(response),
          }
        : { kind: "untrusted" };
    }
    const mediaType = response.headers
      .get("content-type")
      ?.split(";")[0]
      .trim()
      .toLowerCase();
    if (
      !mediaType ||
      !binary["GET /api/v1/workforce/application-resources/{id}"].responses[
        "200"
      ].includes(mediaType)
    )
      return { kind: "untrusted" };
    // The resource contract bounds fonts at 32 MiB and image input at 16M pixels.
    const reader = response.body?.getReader();
    if (!reader) return { kind: "untrusted" };
    const chunks: Uint8Array<ArrayBuffer>[] = [];
    let size = 0;
    try {
      for (;;) {
        const item = await reader.read();
        if (item.done) break;
        size += item.value.byteLength;
        if (
          size >
          (mediaType.startsWith("font/") ? 32 * 1024 * 1024 : 65 * 1024 * 1024)
        ) {
          await reader.cancel();
          return { kind: "untrusted" };
        }
        chunks.push(new Uint8Array(item.value));
      }
    } finally {
      reader.releaseLock();
    }
    if (!size) return { kind: "untrusted" };
    return {
      kind: "ok",
      value: { blob: new Blob(chunks, { type: mediaType }), mediaType },
    };
  } catch {
    return { kind: "disconnected" };
  }
}
