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

function report(titles = [mobile, identity]) {
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
    titlePath: ["login.spec.ts", title],
    file: "/repo/halo/tests/login.spec.ts",
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

test("the cancellation consumer selects both real cases from Playwright's full title", () => {
  const args = playwrightArguments(browserScope("badge-cancellation"));
  const expression = new RegExp(args[args.indexOf("--grep") + 1]!);
  expect(expression.test(`chromium login.spec.ts ${mobile}`)).toBe(true);
  expect(expression.test(`chromium login.spec.ts ${identity}`)).toBe(true);
  expect(
    expression.test("chromium login.spec.ts damaged closed login records"),
  ).toBe(false);
  expect(args).toContain("tests/login.spec.ts");
  expect(args).toContain("--retries=0");
  expect(args).toContain("--workers=1");
});

test("complete successful real-case evidence satisfies both report and scope checks", () => {
  const actual = report();
  expect(verifyReporter(actual, { childExitCleanly: true }).ok).toBe(true);
  expect(verifyBrowserScope("badge-cancellation", actual)).toEqual([]);
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
