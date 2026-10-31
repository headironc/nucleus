import { expect, test } from "bun:test";
import { runInNewContext } from "node:vm";
import { operation, random, secret } from "../src/platform";
import { FakePlatform } from "./fixture";

function returning(value: unknown, asynchronous = false) {
  const platform = new FakePlatform();
  platform.getRandomValues = ({ success }) => {
    // Deliberately exercise values that violate the native callback's declared ABI.
    const reply = () => success({ randomValues: value as ArrayBuffer });
    if (asynchronous) queueMicrotask(reply); else reply();
  };
  return platform;
}

test("secure random accepts genuine cross-realm ArrayBuffer bytes and returns an owned local copy", async () => {
  const buffer = runInNewContext("Uint8Array.from({length: 32}, (_, index) => index).buffer");
  expect(buffer instanceof ArrayBuffer).toBe(false);
  const bytes = await random(returning(buffer), 32);
  expect(bytes instanceof Uint8Array).toBe(true);
  expect([...bytes]).toEqual(Array.from({ length: 32 }, (_, index) => index));
  new Uint8Array(buffer).fill(255);
  expect([...bytes]).toEqual(Array.from({ length: 32 }, (_, index) => index));
});

test("cross-realm random bytes preserve the canonical secret and UUID bit layout", async () => {
  const secretBytes = runInNewContext("Uint8Array.from({length: 32}, (_, index) => index).buffer");
  const operationBytes = runInNewContext("Uint8Array.from({length: 16}, (_, index) => index).buffer");
  expect(secretBytes instanceof ArrayBuffer).toBe(false);
  expect(operationBytes instanceof ArrayBuffer).toBe(false);
  expect(await secret(returning(secretBytes))).toBe(Buffer.from(Array.from({ length: 32 }, (_, index) => index)).toString("base64url"));
  expect(await operation(returning(operationBytes))).toBe("00010203-0405-4607-8809-0a0b0c0d0e0f");
  expect([...new Uint8Array(operationBytes)]).toEqual(Array.from({ length: 16 }, (_, index) => index));
});

const invalid: [string, (length: number) => unknown][] = [
  ["undefined", () => undefined],
  ["null", () => null],
  ["plain array", length => Array(length).fill(0)],
  ["matching byteLength object", length => ({ byteLength: length })],
  ["spoofed ArrayBuffer tag", length => ({ [Symbol.toStringTag]: "ArrayBuffer", byteLength: length, length })],
  ["inherited ArrayBuffer prototype", () => Object.create(ArrayBuffer.prototype)],
  ["Uint8Array view", length => new Uint8Array(length)],
  ["DataView", length => new DataView(new ArrayBuffer(length))],
  ["cross-realm view", length => runInNewContext(`new Uint8Array(${length})`)],
  ["SharedArrayBuffer", length => new SharedArrayBuffer(length)],
  ["ArrayBuffer proxy", length => new Proxy(new ArrayBuffer(length), {})],
  ["short buffer", length => new ArrayBuffer(length - 1)],
  ["long buffer", length => new ArrayBuffer(length + 1)],
  ["detached buffer", length => {
    const buffer = new ArrayBuffer(length);
    structuredClone(buffer, { transfer: [buffer] });
    expect(buffer.byteLength).toBe(0);
    return buffer;
  }],
];
test.each(invalid)("secure random rejects %s for both native lengths", async (_name, value) => {
  for (const length of [16, 32]) await expect(random(returning(value(length)), length)).rejects.toThrow("Invalid random bytes");
});

test("byte length uses the intrinsic brand rather than an own property or string tag", async () => {
  const buffer = runInNewContext("new ArrayBuffer(32)");
  let reads = 0;
  Object.defineProperty(buffer, "byteLength", { get() { reads++; throw new Error("Untrusted length getter"); } });
  Object.defineProperty(buffer, Symbol.toStringTag, { value: "Object" });
  expect((await random(returning(buffer), 32)).byteLength).toBe(32);
  expect(reads).toBe(0);
  const short = new ArrayBuffer(31);
  Object.defineProperty(short, "byteLength", { value: 32 });
  await expect(random(returning(short), 32)).rejects.toThrow("Invalid random bytes");
});

test("an invalid asynchronous native callback rejects instead of escaping the callback", async () => {
  const platform = new FakePlatform();
  platform.getRandomValues = ({ success }) => { queueMicrotask(() => success(null as never)); };
  await expect(random(platform, 32)).rejects.toThrow("Invalid random bytes");
});
