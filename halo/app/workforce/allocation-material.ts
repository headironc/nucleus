import { UUID } from "../codes/material";
import { registry } from "../protocol/response";
import type { AllocationInput, FixedNumber } from "../protocol/number";
import { summarize } from "./number";

export const KEY = "halo.number-allocation";
export type Store = Pick<Storage, "getItem" | "setItem" | "removeItem">;
export type Allocation = Readonly<{
  version: 1;
  operation: string;
  account: string;
  person: string;
  input: AllocationInput;
  retryAt: number;
  outcome: "pending" | "complete" | "refused" | "mismatch";
  code: string;
  number: FixedNumber | null;
}>;

export function read(storage: Store): Allocation | "missing" | null {
  const bytes = storage.getItem(KEY);
  if (bytes === null) return null;
  try {
    const value = JSON.parse(bytes);
    if (
      !value ||
      typeof value !== "object" ||
      Array.isArray(value) ||
      Object.keys(value).length !== 9 ||
      value.version !== 1 ||
      typeof value.operation !== "string" ||
      !UUID.test(value.operation) ||
      !registry.accountId(value.account) ||
      !registry.personId(value.person) ||
      !registry.allocateWorkforceNumber_input(value.input) ||
      !Number.isSafeInteger(value.retryAt) ||
      value.retryAt < 0 ||
      !["pending", "complete", "refused", "mismatch"].includes(value.outcome) ||
      typeof value.code !== "string" ||
      (value.outcome === "complete"
        ? !registry.allocateWorkforceNumber_201({
            code: "ok",
            number: value.number,
          }) ||
          value.number.subtype !== value.input.subtype ||
          !summarize(value.number) ||
          value.code !== "ok"
        : value.number !== null)
    )
      return "missing";
    if (value.outcome === "mismatch" && value.code !== "request_mismatch")
      return "missing";
    if (
      value.outcome === "refused" &&
      !["number_present", "number_pool_exhausted", "invalid_input"].includes(
        value.code,
      )
    )
      return "missing";
    return value as Allocation;
  } catch {
    return "missing";
  }
}

export function save(storage: Store, value: Allocation) {
  const bytes = JSON.stringify(value);
  storage.setItem(KEY, bytes);
  if (storage.getItem(KEY) !== bytes)
    throw Error("allocation_storage_unavailable");
}

export function remove(storage: Store) {
  storage.removeItem(KEY);
  if (storage.getItem(KEY) !== null)
    throw Error("allocation_storage_unavailable");
}
