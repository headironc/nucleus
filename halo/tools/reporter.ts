import type {
  FullConfig,
  FullResult,
  Reporter,
  Suite,
  TestCase,
  TestError,
  TestResult,
  WorkerInfo,
} from "@playwright/test/reporter";
import { EvidenceLog, ensureRun, type SanitizedError } from "./evidence.ts";
import { redactText } from "./redact.ts";

function sanitize(error: TestError | undefined): SanitizedError {
  const message = redactText(error?.message ?? "");
  const stack = redactText(error?.stack ?? "");
  return {
    message: error?.message === undefined ? null : message.text,
    stack: error?.stack === undefined ? null : stack.text,
    value:
      error?.value === undefined ? null : redactText(error.value).text,
    location: error?.location ?? null,
    truncated: message.truncated || stack.truncated,
  };
}

function inventoryTest(test: TestCase) {
  return {
    id: test.id,
    title: test.title,
    titlePath: test.titlePath(),
    file: test.location.file,
    line: test.location.line,
    expectedStatus: test.expectedStatus,
    project: test.parent?.project()?.name ?? null,
  };
}

export default class HaloEvidenceReporter implements Reporter {
  private readonly run = ensureRun();
  private readonly log = new EvidenceLog(this.run, "reporter.jsonl");
  private internalError: string | null = null;

  printsToStdio(): boolean {
    return false;
  }

  onBegin(config: FullConfig, suite: Suite): void {
    this.write("inventory", {
      config: {
        workers: config.workers,
        failOnFlakyTests: config.failOnFlakyTests,
        configFile: config.configFile ?? null,
        projects: config.projects.map((project) => ({
          name: project.name,
          retries: project.retries,
          timeout: project.timeout,
        })),
      },
      tests: suite.allTests().map(inventoryTest),
    });
  }

  onTestBegin(test: TestCase, result: TestResult): void {
    this.write("test_begin", {
      id: test.id,
      title: test.title,
      titlePath: test.titlePath(),
      retry: result.retry,
      workerIndex: result.workerIndex,
      parallelIndex: result.parallelIndex,
    });
  }

  onTestEnd(test: TestCase, result: TestResult): void {
    const errors =
      result.errors.length > 0
        ? result.errors
        : result.error
          ? [result.error]
          : [];
    this.write("test_end", {
      id: test.id,
      title: test.title,
      titlePath: test.titlePath(),
      retry: result.retry,
      status: result.status,
      duration: result.duration,
      expectedStatus: test.expectedStatus,
      outcome: test.outcome(),
      ok: test.ok(),
      workerIndex: result.workerIndex,
      parallelIndex: result.parallelIndex,
      startTime: result.startTime.toISOString(),
      errors: errors.map(sanitize),
      // Attachment bodies, traces and screenshots stay in Playwright's own
      // result directory; evidence records only metadata and the path.
      attachments: result.attachments.map((attachment) => ({
        name: attachment.name,
        contentType: attachment.contentType,
        path:
          attachment.path === undefined
            ? null
            : redactText(attachment.path, 1_000).text,
        hasBody: attachment.body !== undefined,
        bytes: attachment.body?.length ?? null,
      })),
    });
  }

  onError(error: TestError, workerInfo?: WorkerInfo): void {
    this.write("error", {
      ...sanitize(error),
      workerIndex: workerInfo?.workerIndex ?? null,
      parallelIndex: workerInfo?.parallelIndex ?? null,
    });
  }

  async onEnd(result: FullResult): Promise<{ status?: FullResult["status"] } | void> {
    this.write("end", {
      status: result.status,
      startTime: result.startTime.toISOString(),
      duration: result.duration,
    });
    // A reporter write failure is swallowed by Playwright's multiplexer except
    // for its reporter-error flag. Return an explicit failure as a second
    // guard, and let the parent verify the final record independently.
    if (this.internalError) return { status: "failed" };
  }

  async onExit(): Promise<void> {
    try {
      this.write("exit", {});
    } catch {
      // The run already failed through the reporter-error path; the parent
      // treats the missing final record as unknown.
    } finally {
      this.log.close();
    }
  }

  private write(event: string, data: Record<string, unknown>): void {
    try {
      this.log.append(event, data);
    } catch (error) {
      this.internalError =
        error instanceof Error ? error.message : String(error);
      try {
        this.log.append("write_error", { message: this.internalError });
      } catch {
        // The original write failure already makes the report incomplete.
      }
      throw error;
    }
  }
}
