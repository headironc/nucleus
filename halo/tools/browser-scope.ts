import { basename } from "node:path";
import type { ReporterSummary } from "./evidence.ts";

export type BrowserScope = "full" | "badge-cancellation";

const cancellationTests = [
  "real self mobile change revokes both original sessions and reads its result after login",
  "real SMS login, original recovery, cookie-backed identity and cross-tab concealment",
] as const;

export function browserScope(value: string | undefined): BrowserScope {
  if (value === undefined || value === "full") return "full";
  if (value === "badge-cancellation") return value;
  throw new Error("HALO_TEST_SCOPE must be full or badge-cancellation");
}

export function playwrightArguments(scope: BrowserScope): string[] {
  const command = ["node_modules/@playwright/test/cli.js", "test"];
  if (scope === "full") return command;
  // Playwright includes the file and project before the actual test title.
  return command.concat(
    "tests/login.spec.ts",
    "--grep",
    `(?:${cancellationTests.join("|")})$`,
    "--retries=0",
    "--workers=1",
  );
}

export function verifyBrowserScope(
  scope: BrowserScope,
  report: ReporterSummary,
): string[] {
  if (scope === "full") return [];
  const reasons: string[] = [];
  if (report.inventoryCount !== 2 || report.tests.length !== 2)
    reasons.push("badge cancellation requires exactly two real browser tests");
  const actual = new Set<string>();
  for (const test of report.tests) {
    if (
      basename(test.file) !== "login.spec.ts" ||
      !cancellationTests.some((title) => title === test.title) ||
      actual.has(test.title)
    )
      reasons.push(
        "badge cancellation selected an unexpected or duplicate test",
      );
    actual.add(test.title);
    if (
      test.state !== "completed" ||
      test.lastStatus !== "passed" ||
      test.attempts.length !== 1 ||
      test.attempts[0]?.retry !== 0
    )
      reasons.push(
        "badge cancellation requires both tests to pass without retries",
      );
  }
  for (const title of cancellationTests)
    if (!actual.has(title)) reasons.push(`missing real browser test: ${title}`);
  return reasons;
}
