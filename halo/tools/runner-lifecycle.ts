// Cancellation and owned-process cleanup shared by the browser runner. The
// production runner passes the full-suite execution budget; tests pass a short
// budget through this same constructor so they exercise the real ordering.

import type { ChildProcess } from "node:child_process";
import { rm } from "node:fs/promises";
import { bounded, signalTree, terminateTree, waitForChildExit, type Termination } from "./process.ts";

export type Managed = {
  label: string;
  child: ChildProcess;
  group: boolean;
  spawnFailed: boolean;
};

export type Cleanup = {
  ok: boolean;
  results: Array<Record<string, unknown>>;
};

export type LifecycleOptions = {
  deadlineMs: number;
  termMs: number;
  killMs: number;
  serviceMs: number;
  drainMs: number;
  removeMs: number;
  record: (event: string, data?: Record<string, unknown>) => void;
  onChildTerminated?: (item: Managed, termination: Termination) => void;
};

export type ServiceTask = {
  label: string;
  run: () => Promise<Record<string, unknown>>;
};

export type DrainTask = { label: string; run: () => Promise<unknown> };

const SUCCESSFUL_OUTCOMES = new Set([
  "terminated",
  "killed",
  "already_exited",
  "not_started",
  "closed",
  "drained",
  "removed",
  "not_created",
]);

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export class RunnerLifecycle {
  readonly cancellation = new AbortController();
  private readonly options: LifecycleOptions;
  private readonly managed: Managed[] = [];
  private phase: "running" | "cleanup" = "running";
  private reason: string | null = null;
  private deadline: NodeJS.Timeout | undefined;
  private recordError: string | null = null;
  private readonly onSigterm = () => this.requestCancellation("sigterm");
  private readonly onSigint = () => this.requestCancellation("sigint");

  constructor(options: LifecycleOptions) {
    this.options = options;
  }

  get cancellationReason(): string | null {
    return this.reason;
  }

  get inCleanup(): boolean {
    return this.phase === "cleanup";
  }

  get children(): readonly Managed[] {
    return this.managed;
  }

  start(): void {
    process.on("SIGTERM", this.onSigterm);
    process.on("SIGINT", this.onSigint);
    this.deadline = setTimeout(
      () => this.requestCancellation(`execution_deadline_${this.options.deadlineMs}ms`),
      this.options.deadlineMs,
    );
  }

  stop(): void {
    if (this.deadline) clearTimeout(this.deadline);
    this.deadline = undefined;
    process.off("SIGTERM", this.onSigterm);
    process.off("SIGINT", this.onSigint);
  }

  requestCancellation(reason: string): void {
    if (this.phase === "cleanup") {
      this.reason ??= reason;
      this.emit("cancellation_during_cleanup", { reason });
      return;
    }
    if (this.cancellation.signal.aborted) {
      this.emit("cancellation_repeat", { reason, first: this.reason });
      return;
    }
    this.reason = reason;
    this.emit("cancellation_requested", { reason });
    this.cancellation.abort(new Error(`Acceptance cancelled: ${reason}`));
  }

  add(item: Managed): void {
    this.managed.push(item);
  }

  async cleanupAll(
    temporary: string | null,
    tasks: { services?: ServiceTask[]; drains?: DrainTask[] } = {},
  ): Promise<Cleanup> {
    this.phase = "cleanup";
    if (this.deadline) clearTimeout(this.deadline);
    this.deadline = undefined;
    this.emit("cleanup_begin", {
      children: this.managed.map((item) => ({
        label: item.label,
        pid: item.child.pid ?? null,
        group: item.group,
        exited: item.child.exitCode !== null || item.child.signalCode !== null,
      })),
    });
    const results: Array<Record<string, unknown>> = [];
    for (const item of [...this.managed].reverse()) {
      if (item.spawnFailed) {
        const result = { label: item.label, outcome: "not_started" };
        results.push(result);
        this.emit("cleanup_child", result);
        continue;
      }
      try {
        // Playwright drains worker events on SIGINT. SIGTERM exits its CLI
        // before pending IPC can establish which tests actually started.
        const interruption = item.label === "playwright"
          ? signalTree(item.child, "SIGINT", false)
          : null;
        if (interruption) {
          this.emit("child_interrupt", { label: item.label, ...interruption });
          await waitForChildExit(item.child, this.options.termMs);
        }
        const termination = await terminateTree(item.child, {
          termMs: interruption ? 0 : this.options.termMs,
          killMs: this.options.killMs,
          group: item.group,
        });
        if (interruption) termination.errors.unshift(...interruption.errors);
        const result = { label: item.label, ...termination };
        results.push(result);
        this.emit("cleanup_child", result);
        this.options.onChildTerminated?.(item, termination);
      } catch (error) {
        const result = {
          label: item.label,
          outcome: "unknown",
          error: errorMessage(error),
        };
        results.push(result);
        this.emit("cleanup_child", result);
      }
    }
    for (const task of tasks.services ?? []) {
      try {
        const outcome = await bounded(
          task.run(),
          this.options.serviceMs,
          `${task.label} cleanup`,
        );
        const result = outcome.ok
          ? outcome.value
          : { label: task.label, outcome: "unknown", error: outcome.error };
        results.push(result);
        this.emit("cleanup_service", result);
      } catch (error) {
        const result = {
          label: task.label,
          outcome: "unknown",
          error: errorMessage(error),
        };
        results.push(result);
        this.emit("cleanup_service", result);
      }
    }
    for (const drain of tasks.drains ?? []) {
      const outcome = await bounded(
        Promise.resolve().then(drain.run),
        this.options.drainMs,
        `${drain.label} drain`,
      );
      const result = outcome.ok
        ? { label: drain.label, outcome: "drained" }
        : { label: drain.label, outcome: "unknown", error: outcome.error };
      results.push(result);
      this.emit("cleanup_service", result);
    }
    const removed = temporary
      ? await bounded(
          rm(temporary, { recursive: true, force: true }),
          this.options.removeMs,
          "temporary directory removal",
        )
      : null;
    const temporaryResult = removed
      ? removed.ok
        ? { label: "temporary_directory", outcome: "removed" }
        : {
            label: "temporary_directory",
            outcome: "unknown",
            error: removed.error,
          }
      : { label: "temporary_directory", outcome: "not_created" };
    results.push(temporaryResult);
    this.emit("cleanup_service", temporaryResult);
    if (this.recordError !== null) {
      results.push({
        label: "evidence_write",
        outcome: "unknown",
        error: this.recordError,
      });
    }
    const ok = results.every((result) =>
      SUCCESSFUL_OUTCOMES.has(String(result.outcome)),
    );
    this.emit("cleanup_end", { ok, results });
    return { ok, results };
  }

  private emit(event: string, data?: Record<string, unknown>): void {
    try {
      this.options.record(event, data);
    } catch (error) {
      this.recordError = errorMessage(error);
    }
  }
}
