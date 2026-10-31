import { expect, test } from "vitest";
import {
  browserScope,
  playwrightArguments,
  verifyBrowserScope,
} from "./browser-scope.ts";
import {
  summarizeReporter,
  verifyReporter,
  type EvidenceRecord,
} from "./evidence.ts";

const mobile =
  "real self mobile change revokes both original sessions and reads its result after login";
const identity =
  "real SMS login, original recovery, cookie-backed identity and cross-tab concealment";
const supplier = [
  "normal supplier acceptance, terminal reload and real quota refusal",
  "lost response and refresh reuse the exact input and original proof",
  "PNVS refusal and unknown supplier outcome are distinct",
  "untrusted media response never completes the operation",
  "untrusted shape response never completes the operation",
  "untrusted code response never completes the operation",
  "a held send expires after full-document navigation home without another SMS",
  "a replay invalid_input cannot imply that the original SMS was not sent",
  "a replay permission_denied cannot imply that the original SMS was not sent",
  "a replay authentication_invalid cannot imply that the original SMS was not sent",
  "a replay request_mismatch cannot imply that the original SMS was not sent",
];
const required = [...supplier, mobile, identity];

function report(titles = required) {
  const event = (
    event: string,
    facts: Record<string, unknown> = {},
  ): EvidenceRecord => ({
    version: 1,
    runId: "selection",
    at: "2026-10-31T00:00:00Z",
    event,
    ...facts,
  });
  const tests = titles.map((title, index) => ({
    id: String(index),
    title,
    titlePath: [title === mobile || title === identity ? "login.spec.ts" : "codes.spec.ts", title],
    file: `/repo/halo/tests/${title === mobile || title === identity ? "login.spec.ts" : "codes.spec.ts"}`,
    line: index + 1,
    expectedStatus: "passed",
    project: null,
  }));
  return summarizeReporter(
    [
      event("inventory", { tests }),
      ...tests.flatMap(({ id }) => [
        event("test_begin", { id, retry: 0 }),
        event("test_end", {
          id,
          retry: 0,
          status: "passed",
          outcome: "expected",
        }),
      ]),
      event("end", { status: "passed" }),
      event("exit"),
    ],
    "selection",
  );
}

test("full remains the default and arbitrary filtering is rejected", () => {
  expect(browserScope(undefined)).toBe("full");
  expect(browserScope("full")).toBe("full");
  expect(playwrightArguments("full")).toEqual([
    "node_modules/@playwright/test/cli.js",
    "test",
  ]);
  for (const value of ["", "login", "--grep", "cancellation"])
    expect(() => browserScope(value)).toThrow("HALO_TEST_SCOPE");
});

test("the cancellation consumer selects all thirteen real prerequisites from Playwright's full title", () => {
  const args = playwrightArguments(browserScope("badge-cancellation"));
  const expression = new RegExp(args[args.indexOf("--grep") + 1]!);
  expect(expression.test(`chromium login.spec.ts ${mobile}`)).toBe(true);
  expect(expression.test(`chromium login.spec.ts ${identity}`)).toBe(true);
  for (const title of supplier)
    expect(expression.test(`chromium codes.spec.ts ${title}`)).toBe(true);
  expect(
    expression.test("chromium login.spec.ts damaged closed login records"),
  ).toBe(false);
  expect(args).toContain("tests/login.spec.ts");
  expect(args).toContain("tests/codes.spec.ts");
  expect(args).toContain("--retries=0");
  expect(args).toContain("--workers=1");
});

test("complete successful real-case evidence satisfies both report and scope checks", () => {
  const actual = report();
  expect(actual.inventoryCount).toBe(13);
  expect(verifyReporter(actual, { childExitCleanly: true }).ok).toBe(true);
  expect(verifyBrowserScope("badge-cancellation", actual)).toEqual([]);
});

test("the two long scenarios alone omit the actual PNVS prerequisite consumers", () => {
  expect(verifyBrowserScope("badge-cancellation", report([mobile, identity]))).not.toEqual([]);
});

test("a replacement or duplicate cannot hide an omitted supplier consumer behind a correct count", () => {
  for (const replacement of ["unexpected supplier case", supplier[1]!]) {
    const titles = [...required];
    titles[0] = replacement;
    const actual = report(titles);
    expect(actual.inventoryCount).toBe(13);
    expect(verifyBrowserScope("badge-cancellation", actual)).not.toEqual([]);
  }
});

test.each([
  [identity],
  [mobile],
  [mobile, identity, "extra"],
  [mobile, mobile],
])("missing, additional and duplicate real cases fail: %j", (...titles) => {
  expect(verifyBrowserScope("badge-cancellation", report(titles))).not.toEqual(
    [],
  );
});

test("a matching title from another file or a retry cannot satisfy the consumer", () => {
  const wrongFile = report();
  wrongFile.tests[0]!.file = "/repo/halo/tests/controlled.spec.ts";
  expect(verifyBrowserScope("badge-cancellation", wrongFile)).not.toEqual([]);
  const retried = report();
  retried.tests[1]!.attempts[0]!.retry = 1;
  expect(verifyBrowserScope("badge-cancellation", retried)).not.toEqual([]);
});

test("skipped or incomplete evidence cannot satisfy the consumer", () => {
  const skipped = report();
  skipped.tests[0]!.lastStatus = "skipped";
  expect(verifyBrowserScope("badge-cancellation", skipped)).not.toEqual([]);
  const unfinished = report();
  unfinished.tests[1]!.state = "never_started";
  unfinished.tests[1]!.attempts = [];
  expect(verifyBrowserScope("badge-cancellation", unfinished)).not.toEqual([]);
});
