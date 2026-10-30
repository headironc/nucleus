import contracts from "./generated/contracts.js";
import * as validators from "./generated/validators.cjs";
import { acceptedJson, type Contract } from "../../../client/protocol/response";

export const registry = validators as unknown as Record<
  string,
  (value: unknown) => boolean
>;

export async function decode(
  response: Response,
  endpoint: keyof typeof contracts,
) {
  const contract = (contracts[endpoint] as Record<string, Contract>)[
    response.status
  ];
  if (!contract || response.headers.get("content-type")?.split(";")[0].trim().toLowerCase() !== "application/json") {
    return undefined;
  }
  try {
    const value: unknown = await response.json();
    if (
      !acceptedJson(response.status, response.headers.get("content-type"), value,
        contracts[endpoint] as Record<string, Contract>, registry)
    )
      return undefined;
    return value;
  } catch {
    return undefined;
  }
}

export function retryAfter(response: Response): number {
  const seconds = response.headers.get("retry-after");
  if (!seconds || !/^\d+$/.test(seconds)) return 0;
  const value = Number(seconds) * 1_000;
  return Number.isSafeInteger(value) ? value : Number.MAX_SAFE_INTEGER;
}
