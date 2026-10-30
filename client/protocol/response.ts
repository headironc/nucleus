export type Contract = { validator: string; codes: string[] };
export type Registry = Record<string, (value: unknown) => boolean>;

/** The same status, media type, schema and application-code check on both clients. */
export function acceptedJson(
  status: number,
  mediaType: string | undefined | null,
  value: unknown,
  contracts: Record<string, Contract>,
  registry: Registry,
): value is { code: string } {
  const contract = contracts[status];
  return !!contract &&
    mediaType?.split(";")[0].trim().toLowerCase() === "application/json" &&
    !!registry[contract.validator]?.(value) &&
    !!value && typeof value === "object" && "code" in value &&
    contract.codes.includes(String(value.code));
}
