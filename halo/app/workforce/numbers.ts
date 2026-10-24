import { Current } from "../authentication/current";
import type { NumberResult } from "../protocol/number";
import type { NumberSummary } from "./number";
import { Workforce } from "./status";

export type NumberState =
  | { kind: "hidden" | "loading" }
  | { kind: "ready"; number: NumberSummary | null }
  | { kind: "unavailable" | "limited"; retryAt: number };

type Dependencies = {
  current: Current;
  workforce: Workforce;
  number(id: string, signal: AbortSignal): Promise<NumberResult>;
  build(): Promise<boolean>;
  now(): number;
};

/** Only one currently verified active-service generation can display a number. */
export class Numbers {
  private state: NumberState = { kind: "hidden" };
  private readonly listeners = new Set<() => void>();
  private generation = 0;
  private request?: AbortController;
  private waiting?: {
    id: string;
    revision: string;
    at: number;
    limited: boolean;
  };
  private readonly unwatch: () => void;

  constructor(private readonly dependencies: Dependencies) {
    this.unwatch = dependencies.workforce.subscribe(this.changed);
    this.changed();
  }
  snapshot = (): NumberState => this.state;
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };
  private show(state: NumberState) {
    this.state = state;
    this.listeners.forEach((listener) => listener());
  }
  private cancel() {
    this.generation++;
    this.request?.abort();
    this.request = undefined;
  }
  private changed = () => {
    this.cancel();
    this.show({ kind: "hidden" });
    const service = this.dependencies.workforce.snapshot();
    if (service.kind === "ready" && service.status === "active")
      void this.refresh();
  };
  close() {
    this.unwatch();
    this.cancel();
    this.waiting = undefined;
    this.show({ kind: "hidden" });
  }
  async refresh(): Promise<void> {
    const { current, workforce } = this.dependencies;
    const binding = current.binding();
    const service = workforce.snapshot();
    if (!binding || service.kind !== "ready" || service.status !== "active")
      return;
    this.cancel();
    const generation = this.generation;
    const active = () =>
      generation === this.generation &&
      current.matches(binding) &&
      workforce.snapshot() === service;
    this.show({ kind: "loading" });
    const request = new AbortController();
    this.request = request;
    const unavailable = (result?: NumberResult) => {
      const limited =
        result?.kind === "failure" && result.code === "rate_limited";
      const delay =
        result?.kind === "failure" ? Math.max(5_000, result.retryAfter) : 5_000;
      const retryAt = Math.min(
        Number.MAX_SAFE_INTEGER,
        this.dependencies.now() + delay,
      );
      this.waiting = { ...binding.selection, at: retryAt, limited };
      this.show({ kind: limited ? "limited" : "unavailable", retryAt });
    };
    const timer = setTimeout(() => {
      if (active()) {
        this.cancel();
        unavailable();
      }
    }, 35_000);
    try {
      if (!(await this.dependencies.build().catch(() => false))) {
        if (active()) current.reload();
        return;
      }
      if (!active() || !(await current.confirm(binding)) || !active()) return;
      const waiting = this.waiting;
      if (
        waiting &&
        waiting.id === binding.selection.id &&
        waiting.revision === binding.selection.revision &&
        waiting.at > this.dependencies.now()
      ) {
        this.show({
          kind: waiting.limited ? "limited" : "unavailable",
          retryAt: waiting.at,
        });
        return;
      }
      const result = await this.dependencies.number(
        binding.selection.id,
        request.signal,
      );
      if (!active()) return;
      if (result.kind === "untrusted") {
        current.reload();
        return;
      }
      if (!(await current.confirm(binding)) || !active()) return;
      if (result.kind === "number") {
        this.waiting = undefined;
        this.show({ kind: "ready", number: result.number });
      } else if (
        result.kind === "failure" &&
        (result.status === 401 || result.status === 403)
      ) {
        await current.reject(binding, result.status);
      } else unavailable(result);
    } catch {
      if (active()) unavailable();
    } finally {
      clearTimeout(timer);
      if (this.request === request) this.request = undefined;
    }
  }
}
