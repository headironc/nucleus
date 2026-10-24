import { expect, test } from "bun:test";
import { createHash } from "node:crypto";
import {
  encode,
  equal,
  get,
  integer,
  object,
  parse,
  parseNumbers,
  second,
  timestamp,
  truth,
  wire,
} from "./review-json";
import { binding, entryIdentity } from "./review-queue";

const digest = (text: string) =>
  createHash("sha256").update(text).digest("hex");

test("native JSON retains lexical integers and exact large identities", () => {
  const value = parse(
    '{"int":9007199254740993,"float":1.0,"exponent":1e0,"boolean":true,"negative":-0}',
  );
  expect(get(value, "int")).toBe(9007199254740993n);
  expect(get(value, "float")).toBe(1);
  expect(integer(get(value, "float"))).toBe(false);
  expect(integer(get(value, "exponent"))).toBe(false);
  expect(integer(get(value, "boolean"))).toBe(false);
  expect(get(value, "negative")).toBe(0n);
  expect(wire({ id: 9007199254740993n })).toBe('{"id":9007199254740993}');
  expect(parse('{"duplicate":1,"duplicate":2}')).toEqual({ duplicate: 2n });
  expect(() => parse('{"secret":"DO-NOT-ECHO",}')).toThrow(
    "review evidence JSON is invalid",
  );
  try {
    parse('{"secret":"DO-NOT-ECHO",}');
  } catch (error) {
    expect(String(error)).not.toContain("DO-NOT-ECHO");
  }
});
test("comparison view preserves Python numeric equivalence without changing evidence bytes", () => {
  for (const left of [1n, 1, true])
    for (const right of [1n, 1, true])
      expect(equal({ value: [left] }, { value: [right] })).toBe(true);
  for (const left of [0n, 0, -0, false])
    for (const right of [0n, 0, -0, false])
      expect(equal(left, right)).toBe(true);
  expect(equal(9007199254740993n, 9007199254740992)).toBe(false);
  expect(equal(9007199254740992n, 9007199254740992)).toBe(true);
  expect(equal({ value: null }, {})).toBe(false);
  expect(equal([], {})).toBe(false);
  expect(equal("1", 1n)).toBe(false);
  expect([encode(1n), encode(1), encode(true), encode(-0)]).toEqual([
    "1",
    "1.0",
    "true",
    "-0.0",
  ]);
  expect(truth([])).toBe(false);
  expect(truth({})).toBe(false);
  expect(truth([false])).toBe(true);
  expect(get({}, "field", {})).toEqual({});
  expect(get({ field: null }, "field", {})).toBeNull();
  expect(() => get(null, "field")).toThrow();
});
test("Python v3 ASCII and separator bytes use independently recorded vectors", () => {
  const value = {
    z: "中文😀\u007f\n\u2028",
    a: 9007199254740993n,
    b: true,
    empty: null,
    list: [{}, [], false],
    n: -1n,
  };
  const compact =
    '{"a":9007199254740993,"b":true,"empty":null,"list":[{},[],false],"n":-1,"z":"\\u4e2d\\u6587\\ud83d\\ude00\\u007f\\n\\u2028"}';
  const spaced =
    '{"a": 9007199254740993, "b": true, "empty": null, "list": [{}, [], false], "n": -1, "z": "\\u4e2d\\u6587\\ud83d\\ude00\\u007f\\n\\u2028"}';
  expect(encode(value, true)).toBe(compact);
  expect(encode(value)).toBe(spaced);
  expect(digest(compact)).toBe(
    "b4678043864710781361b82943662f83006762d033d88b3246ebaafcf25216e3",
  );
  expect(digest(spaced)).toBe(
    "37926bb9bed4021f160fad938bbd2f5f312b0285b8cee432990608e10a16ffcb",
  );
  expect(encode({ "2": "two", "10": "ten" }, true)).toBe(
    '{"10":"ten","2":"two"}',
  );
  expect(digest(encode({ "2": "two", "10": "ten" }, true))).toBe(
    "b71e124675fc80e7314688bffdb68e83515851fb51474125db9a0c4c8aca3808",
  );
  expect(encode({ "\u{10000}": 1n, "\ue000": 2n }, true)).toBe(
    '{"\\ue000":2,"\\ud800\\udc00":1}',
  );
  expect(encode({ s: "\ud800" }, true)).toBe('{"s":"\\ud800"}');
});
test("Python float representation retains markers exponents and unknown fields", () => {
  for (const [value, expected] of [
    [0, "0.0"],
    [-0, "-0.0"],
    [1, "1.0"],
    [1e-4, "0.0001"],
    [1e-5, "1e-05"],
    [1e15, "1000000000000000.0"],
    [1e16, "1e+16"],
    [1.234567890123456, "1.234567890123456"],
    [Number.MIN_VALUE, "5e-324"],
    [Number.MAX_VALUE, "1.7976931348623157e+308"],
    [Infinity, "Infinity"],
    [-Infinity, "-Infinity"],
  ] as [number, string][])
    expect(encode(value)).toBe(expected);
  const value = parse('{"i":1,"f":1.0,"e":1e0,"n":-0.0}');
  expect(encode(value, true)).toBe('{"e":1.0,"f":1.0,"i":1,"n":-0.0}');
  expect(digest(encode(value, true))).toBe(
    "5441b375ee242dfeae814883c7ea42a32806f8d96f4955b78b26157097ddd785",
  );
  expect(
    encode(parse('{"unknown":{"float":0.125,"overflow":1e999}}'), true),
  ).toBe('{"unknown":{"float":0.125,"overflow":Infinity}}');
});
test("admission and queue v4 bindings match independent Python goldens", () => {
  // Preserve the original vector's input independently of current branch rules.
  const snapshot = object(
    parse(`{
    "review": {
      "view": {"baseRefName":"dev","baseRefOid":"cccccccccccccccccccccccccccccccccccccccc","headRefOid":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","isDraft":false,"lifecycle":null,"state":"OPEN"},
      "evidence": {
        "rules": [
          {"type":"non_fast_forward"},
          {"type":"deletion"},
          {"type":"pull_request","parameters":{"required_review_thread_resolution":true,"dismiss_stale_reviews_on_push":true,"dismissal_restriction":{"enabled":false,"allowed_actors":[]}}},
          {"type":"merge_queue","parameters":{"grouping_strategy":"ALLGREEN","max_entries_to_build":1,"max_entries_to_merge":1,"min_entries_to_merge":1,"merge_method":"SQUASH"}},
          {"type":"required_status_checks","parameters":{"strict_required_status_checks_policy":true,"required_status_checks":[{"context":"docs","integration_id":15368},{"context":"rust","integration_id":15368},{"context":"Cladiron review gate","integration_id":3104501}]}}
        ],
        "reviewed": {"baseRefName":"dev","baseRefOid":"cccccccccccccccccccccccccccccccccccccccc","headRefOid":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","isDraft":false,"lifecycle":null,"state":"OPEN"},
        "integration": {"base":"cccccccccccccccccccccccccccccccccccccccc","merge_base":"cccccccccccccccccccccccccccccccccccccccc"}
      },
      "approval":7,"reasons":[]
    },
    "entry": {
      "id":"entry-1","enqueuedAt":"2026-10-19T00:00:00Z","position":1,"state":"AWAITING_CHECKS",
      "headCommit":{"oid":"dddddddddddddddddddddddddddddddddddddddd"},
      "baseCommit":{"oid":"cccccccccccccccccccccccccccccccccccccccc"},
      "pullRequest":{"number":42,"headRefOid":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","baseRefOid":"cccccccccccccccccccccccccccccccccccccccc","baseRefName":"dev"},
      "mergeQueue":{"id":"queue-1","repository":{"nameWithOwner":"owner/repository"}}
    },
    "ref":"gh-readonly-queue/dev/pr-42-abcdef",
    "head":"dddddddddddddddddddddddddddddddddddddddd"
  }`),
  );
  snapshot.entry = entryIdentity(snapshot.entry);
  const review = object(snapshot.review),
    repo = "owner/repository";
  expect(`nucleus-review-gate:v4:${digest(binding(repo, 42n, review))}`).toBe(
    "nucleus-review-gate:v4:3b652caf74380df4a0a9f0179bf24ac7a217c586f2b58a75fc48004eafb8ee3d",
  );
  expect(digest(encode(review))).toBe(
    "afb2377ebc15432eafd8e891382cd510b4b080c8ca04c917134e2c3653b707d5",
  );
  expect(
    `nucleus-review-gate:v4:${digest(binding(repo, 42n, snapshot, true))}`,
  ).toBe(
    "nucleus-review-gate:v4:4d719322c915b3c95294a9b299ac9686fb75e2283654261e46ec99ced18727de",
  );
  expect(digest(encode(snapshot))).toBe(
    "1deaed0f6ebf04e8f9592bb3521c9f23e44e577ed48e26df269e10f04f67c01c",
  );
});
test("timestamp uses actual calendar shapes and truncates microseconds before comparison", () => {
  expect(timestamp("2026-10-18T08:06:00+08:00")).toBe(
    timestamp("2026-10-18T00:06:00Z"),
  );
  expect(timestamp("1970-01-01T00:00:00.123456789Z")).toBe(123456000n);
  expect(timestamp("1969-12-31T23:59:59.999999999Z")).toBe(-1000n);
  expect(second(timestamp("1969-12-31T23:59:59.999999Z"))).toBe(-1000000000n);
  expect(second(timestamp("1970-01-01T00:00:00.999999Z"))).toBe(0n);
  for (const value of [
    null,
    "",
    "invalid",
    "2026-10-18T00:05:00",
    "2026-02-30T00:00:00Z",
    "2026-10-18T00:00:60Z",
    "0000-01-01T00:00:00Z",
    "2026-10-18T00:00:00+24:00",
    "2026-10-18T00:00:00Z[UTC]",
    "2026-10-18T00:00:00Z\n",
  ])
    expect(() => timestamp(value)).toThrow();
});
test("number validation shares lexical positive integer boundary", () => {
  expect(parseNumbers("[3,1,9007199254740993,3]")).toEqual([
    3n,
    1n,
    9007199254740993n,
    3n,
  ]);
  expect(parseNumbers("[]")).toEqual([]);
  for (const input of [
    "[1.0]",
    "[1e0]",
    "[true]",
    "[0]",
    "[-1]",
    "[null]",
    '["1"]',
    "{}",
    "null",
    "not JSON",
  ])
    expect(() => parseNumbers(input)).toThrow();
});
