import { validInput, type Input, type Message } from "../protocol/codes";

export const STORAGE_KEY = "halo.c1";
export const LIFETIME = 30 * 60 * 1000;
export const UUID =
  /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

export type Proof = {
  version: 1;
  operation: string;
  input: Input;
  createdAt: number;
  expiresAt: number;
  retryAt: number;
  attempted?: boolean;
  delivery?: "sending";
  reason?:
    | "request_mismatch"
    | "authentication_invalid"
    | "invalid_input"
    | "permission_denied";
};
export type Closed = {
  version: 1;
  closed: true;
  delivery?: Message["delivery"];
  reason?: string;
};
type Material =
  | { kind: "proof"; proof: Proof }
  | { kind: "closed"; value: Closed }
  | { kind: "invalid" };
type Store = Pick<Storage, "getItem" | "setItem">;

export function readMaterial(bytes: string): Material {
  try {
    const value = JSON.parse(bytes);
    if (!value || typeof value !== "object" || Array.isArray(value))
      return { kind: "invalid" };
    if ("closed" in value) {
      if (
        value.version === 1 &&
        value.closed === true &&
        Object.keys(value).every((key) =>
          ["version", "closed", "delivery", "reason"].includes(key),
        ) &&
        (value.delivery === undefined ||
          ["sent", "failed", "unknown"].includes(value.delivery)) &&
        (value.reason === undefined || typeof value.reason === "string")
      )
        return { kind: "closed", value };
      return { kind: "invalid" };
    }
    if (
      value.version !== 1 ||
      !UUID.test(value.operation) ||
      !validInput(value.input) ||
      !Number.isFinite(value.createdAt) ||
      !Number.isFinite(value.expiresAt) ||
      value.expiresAt > value.createdAt + LIFETIME ||
      !Number.isFinite(value.retryAt) ||
      (value.attempted !== undefined && typeof value.attempted !== "boolean") ||
      (value.reason !== undefined &&
        ![
          "request_mismatch",
          "authentication_invalid",
          "invalid_input",
          "permission_denied",
        ].includes(value.reason))
    )
      return { kind: "invalid" };
    return { kind: "proof", proof: value };
  } catch {
    return { kind: "invalid" };
  }
}

export function expired(proof: Proof, now: number) {
  return (
    !Number.isFinite(now) || now < proof.createdAt || now >= proof.expiresAt
  );
}

export function persistMaterial(storage: Store, value: Proof | Closed) {
  const bytes = JSON.stringify(value);
  storage.setItem(STORAGE_KEY, bytes);
  if (storage.getItem(STORAGE_KEY) !== bytes)
    throw new Error("storage_unavailable");
}

// Re-read on every check: an earlier document's deadline cannot close a newer C1.
export function expireMaterial(storage: Store, now: number): boolean {
  try {
    const bytes = storage.getItem(STORAGE_KEY);
    if (bytes === null) return true;
    const material = readMaterial(bytes);
    if (material.kind === "closed") return true;
    if (material.kind === "proof" && !expired(material.proof, now)) return true;
    persistMaterial(storage, { version: 1, closed: true });
    return true;
  } catch {
    // Keep the record on failure; the next executing lifecycle check retries it.
    return false;
  }
}
