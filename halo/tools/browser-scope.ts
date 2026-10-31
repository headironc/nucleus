import { basename } from "node:path";
import type { ReporterSummary } from "./evidence.ts";

export type BrowserScope = "full" | "badge-cancellation";

const cancellationTests = [
  { file: "codes.spec.ts", title: "normal supplier acceptance, terminal reload and real quota refusal" },
  { file: "codes.spec.ts", title: "lost response and refresh reuse the exact input and original proof" },
  { file: "codes.spec.ts", title: "PNVS refusal and unknown supplier outcome are distinct" },
  { file: "codes.spec.ts", title: "untrusted media response never completes the operation" },
  { file: "codes.spec.ts", title: "untrusted shape response never completes the operation" },
  { file: "codes.spec.ts", title: "untrusted code response never completes the operation" },
  { file: "codes.spec.ts", title: "a held send expires after full-document navigation home without another SMS" },
  { file: "codes.spec.ts", title: "a replay invalid_input cannot imply that the original SMS was not sent" },
  { file: "codes.spec.ts", title: "a replay permission_denied cannot imply that the original SMS was not sent" },
  { file: "codes.spec.ts", title: "a replay authentication_invalid cannot imply that the original SMS was not sent" },
  { file: "codes.spec.ts", title: "a replay request_mismatch cannot imply that the original SMS was not sent" },
  { file: "login.spec.ts", title: "real self mobile change revokes both original sessions and reads its result after login" },
  { file: "login.spec.ts", title: "real SMS login, original recovery, cookie-backed identity and cross-tab concealment" },
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
    "tests/codes.spec.ts",
    "--grep",
    `(?:${cancellationTests.map((item) => item.title).join("|")})$`,
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
  if (
    report.inventoryCount !== cancellationTests.length ||
    report.tests.length !== cancellationTests.length
  )
    reasons.push("badge cancellation requires exactly thirteen prerequisite browser tests");
  const actual = new Set<string>();
  for (const test of report.tests) {
    const key = `${basename(test.file)}:${test.title}`;
    if (
      !cancellationTests.some((item) =>
        item.file === basename(test.file) && item.title === test.title
      ) ||
      actual.has(key)
    )
      reasons.push(
        "badge cancellation selected an unexpected or duplicate test",
      );
    actual.add(key);
    if (
      test.state !== "completed" ||
      test.lastStatus !== "passed" ||
      test.attempts.length !== 1 ||
      test.attempts[0]?.retry !== 0
    )
      reasons.push(
        "badge cancellation requires every prerequisite to pass without retries",
      );
  }
  for (const item of cancellationTests)
    if (!actual.has(`${item.file}:${item.title}`))
      reasons.push(`missing real browser test: ${item.file}: ${item.title}`);
  return reasons;
}
