// GitHub evidence stays intact. These adapters preserve the existing v3 bytes
// and comparisons; they do not decide whether a review is acceptable.
import { isDeepStrictEqual } from "node:util";

export type Json = null | boolean | string | bigint | number | Json[] | Record;
export type Record = { [key: string]: Json };
// Python's whitespace contract includes these four information separators,
// while ECMAScript's \s includes the non-whitespace BOM instead.
export const WHITESPACE = /[\p{White_Space}\u001c-\u001f]/u;

export function object(value: unknown): Record {
  if (typeof value !== "object" || value === null || Array.isArray(value))
    throw new Error("review evidence is not an object");
  return value as Record;
}

export function array(value: unknown): Json[] {
  if (!Array.isArray(value)) throw new Error("review evidence is not an array");
  return value;
}

export function string(value: unknown): string {
  if (typeof value !== "string")
    throw new Error("review evidence is not a string");
  return value;
}

export function field(value: unknown, key: string): Json {
  const record = object(value);
  if (!Object.hasOwn(record, key))
    throw new Error(`review evidence is missing ${key}`);
  return record[key];
}

export function get(value: unknown, key: string, fallback: Json = null): Json {
  const record = object(value);
  return Object.hasOwn(record, key) ? record[key] : fallback;
}

export function truth(value: Json): boolean {
  if (Array.isArray(value)) return value.length !== 0;
  if (value !== null && typeof value === "object")
    return Object.keys(value).length !== 0;
  // NaN is not produced by native JSON parsing, but Infinity can be.
  return typeof value === "number" ? value !== 0 : Boolean(value);
}

export function parse(text: string): Json {
  try {
    return JSON.parse(
      text,
      (_key: string, value: unknown, context?: { source?: string }) =>
        typeof value === "number" &&
        context?.source &&
        /^-?(?:0|[1-9][0-9]*)$/.test(context.source)
          ? BigInt(context.source)
          : value,
    ) as Json;
  } catch {
    // Native syntax errors can echo fragments of private response bodies.
    throw new Error("review evidence JSON is invalid");
  }
}

const rawJSON = (JSON as JSON & { rawJSON(text: string): unknown }).rawJSON;

export function wire(value: Json): string {
  return JSON.stringify(value, (_key, item: unknown) =>
    typeof item === "bigint" ? rawJSON(item.toString()) : item,
  );
}

// Only the comparison view normalizes numeric types. The original tree still
// distinguishes 1, 1.0 and true in the published evidence digest.
function comparable(value: Json): Json {
  if (typeof value === "boolean") return value ? 1n : 0n;
  if (
    typeof value === "number" &&
    Number.isFinite(value) &&
    Number.isInteger(value)
  )
    return BigInt(value);
  if (Array.isArray(value)) return value.map(comparable);
  if (value !== null && typeof value === "object")
    return Object.fromEntries(
      Object.entries(value).map(([key, item]) => [key, comparable(item)]),
    );
  return value;
}

export function equal(left: Json, right: Json): boolean {
  return isDeepStrictEqual(comparable(left), comparable(right));
}

export function compare(left: bigint | number, right: bigint | number): number {
  return left < right ? -1 : left > right ? 1 : 0;
}

export function numeric(value: Json): bigint | number {
  if (typeof value === "boolean") return value ? 1n : 0n;
  if (typeof value !== "number" && typeof value !== "bigint")
    throw new Error("review evidence is not numeric");
  return value;
}

export function integer(value: Json): value is bigint {
  return typeof value === "bigint";
}

function keyOrder(left: string, right: string): number {
  const a = Array.from(left, (character) => character.codePointAt(0)!);
  const b = Array.from(right, (character) => character.codePointAt(0)!);
  for (let index = 0; index < Math.min(a.length, b.length); index++)
    if (a[index] !== b[index]) return a[index] - b[index];
  return a.length - b.length;
}

function quoted(value: string): string {
  return JSON.stringify(value).replace(
    /[\u007f-\uffff]/g,
    (character) =>
      `\\u${character.charCodeAt(0).toString(16).padStart(4, "0")}`,
  );
}

// Native conversion supplies the shortest round-tripping digits. Only Python's
// fixed/scientific presentation and float markers differ from ECMAScript.
function float(value: number): string {
  if (!Number.isFinite(value)) return String(value);
  if (Object.is(value, -0)) return "-0.0";
  const [mantissa, exponentText] = value.toExponential().split("e");
  const exponent = Number(exponentText);
  if (exponent < -4 || exponent >= 16)
    return `${mantissa}e${exponent < 0 ? "-" : "+"}${Math.abs(exponent).toString().padStart(2, "0")}`;
  return Number.isInteger(value) ? `${String(value)}.0` : String(value);
}

export function encode(value: Json, compact = false): string {
  if (value === null) return "null";
  if (typeof value === "string") return quoted(value);
  if (typeof value === "bigint" || typeof value === "boolean")
    return String(value);
  if (typeof value === "number") return float(value);
  const comma = compact ? "," : ", ";
  if (Array.isArray(value))
    return `[${value.map((item) => encode(item, compact)).join(comma)}]`;
  const colon = compact ? ":" : ": ";
  return `{${Object.keys(value)
    .sort(keyOrder)
    .map((key) => `${quoted(key)}${colon}${encode(value[key], compact)}`)
    .join(comma)}}`;
}

export function timestamp(value: Json): bigint {
  if (typeof value !== "string")
    throw new Error("review evidence is missing a timestamp");
  // GitHub timestamps and public summary rows use calendar dates and explicit
  // minute offsets. This is intentionally not a general ISO-8601 parser.
  const match =
    /^([0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-5][0-9])(?:\.([0-9]+))?(Z|[+-][0-9]{2}:[0-9]{2})$/.exec(
      value,
    );
  if (!match || match[0] !== value || value.startsWith("0000"))
    throw new Error(
      "review evidence timestamp has an invalid date, time or timezone",
    );
  const fraction = match[2] ? `.${match[2].slice(0, 6)}` : "";
  return Temporal.Instant.from(`${match[1]}${fraction}${match[3]}`)
    .epochNanoseconds;
}

export function second(value: bigint): bigint {
  return Temporal.Instant.fromEpochNanoseconds(value).round({
    smallestUnit: "second",
    roundingMode: "floor",
  }).epochNanoseconds;
}

export function parseNumbers(text: string): bigint[] {
  const values = parse(text);
  if (
    !Array.isArray(values) ||
    values.some((value) => !integer(value) || value <= 0n)
  )
    throw new Error("NUMBERS must contain positive PR numbers");
  return values as bigint[];
}
