import { UUID } from "../codes/material";
import { registry } from "../protocol/response";
import {
  validAdmissionInput,
  type AdmissionInput,
} from "../protocol/admission";
import type { Store } from "./termination-material";

export const PREFIX = "halo.workforce-admission:";
export type Material = Readonly<{
  version: 1;
  operation: string;
  account: string;
  operator: string;
  input: AdmissionInput;
  retryAt: number;
  blocked: boolean;
}>;
export function readAdmission(
  storage: Store,
  account: string,
): Material | "missing" | null {
  const bytes = storage.getItem(PREFIX + account);
  if (bytes === null) return null;
  try {
    const value = JSON.parse(bytes);
    if (
      value &&
      typeof value === "object" &&
      Object.keys(value).length === 7 &&
      value.version === 1 &&
      value.account === account &&
      registry.accountId(value.account) &&
      registry.personId(value.operator) &&
      typeof value.operation === "string" &&
      UUID.test(value.operation) &&
      validAdmissionInput(value.input) &&
      value.input.entry === "institution" &&
      Number.isSafeInteger(value.retryAt) &&
      value.retryAt >= 0 &&
      typeof value.blocked === "boolean"
    )
      return value;
  } catch {
    /* Damaged material cannot become a new admission. */
  }
  return "missing";
}
export function saveAdmission(storage: Store, material: Material) {
  const bytes = JSON.stringify(material);
  storage.setItem(PREFIX + material.account, bytes);
  if (storage.getItem(PREFIX + material.account) !== bytes)
    throw Error("admission_storage_unavailable");
}
export function removeAdmission(storage: Store, account: string) {
  storage.removeItem(PREFIX + account);
  if (storage.getItem(PREFIX + account) !== null)
    throw Error("admission_storage_unavailable");
}
