import { Current, type CurrentBinding } from "../authentication/current";
import type { Correction, HistoryResult } from "../protocol/correction";

export type HistoryState =
  | { kind: "hidden" }
  | { kind: "loading"; page: number; size: number }
  | {
      kind: "ready";
      page: number;
      size: number;
      count: number;
      corrections: Correction[];
    }
  | {
      kind: "unavailable" | "limited";
      page: number;
      size: number;
      retryAt: number;
    };

type Dependencies = {
  current: Current;
  history(
    id: string,
    page: number,
    size: number,
    signal: AbortSignal,
  ): Promise<HistoryResult>;
  build(): Promise<boolean>;
  now(): number;
};

/** Owns only the visible account's read-only correction page. */
export class History {
  private state: HistoryState = { kind: "hidden" };
  private readonly listeners = new Set<() => void>();
  private generation = 0;
  private request?: AbortController;
  private binding?: CurrentBinding;
  private waiting?: {
    selection: CurrentBinding["selection"];
    at: number;
    limited: boolean;
  };
  private readonly unwatch: () => void;

  constructor(private readonly dependencies: Dependencies) {
    this.unwatch = dependencies.current.subscribe(this.changed);
    this.changed();
  }

  snapshot = (): HistoryState => this.state;
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  private show(state: HistoryState) {
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
    this.binding = this.dependencies.current.binding();
    this.show({ kind: "hidden" });
    if (this.binding) void this.load(1, 20);
  };

  close() {
    this.unwatch();
    this.cancel();
    this.binding = undefined;
    this.waiting = undefined;
    this.show({ kind: "hidden" });
  }

  refresh(size = this.state.kind === "hidden" ? 20 : this.state.size) {
    return this.load(1, size);
  }

  previous() {
    if (this.state.kind === "ready" && this.state.page > 1)
      return this.load(this.state.page - 1, this.state.size);
  }

  next() {
    if (
      this.state.kind === "ready" &&
      this.state.page < Math.ceil(this.state.count / this.state.size)
    )
      return this.load(this.state.page + 1, this.state.size);
  }

  private async load(page: number, size: number): Promise<void> {
    const binding = this.binding;
    const { current } = this.dependencies;
    if (
      !binding ||
      !current.matches(binding) ||
      !Number.isInteger(size) ||
      size < 1 ||
      size > 100
    )
      return;
    this.cancel();
    const generation = this.generation;
    const active = () =>
      generation === this.generation && current.matches(binding);
    this.show({ kind: "loading", page, size });
    const request = new AbortController();
    this.request = request;
    const timer = setTimeout(() => request.abort(), 35_000);
    try {
      const compatible = await this.dependencies.build().catch(() => false);
      if (!active()) return;
      if (!compatible) {
        current.reload();
        return;
      }
      if (!(await current.confirm(binding)) || !active()) return;
      const waiting = this.waiting;
      if (
        waiting &&
        waiting.selection.id === binding.selection.id &&
        waiting.selection.revision === binding.selection.revision &&
        waiting.at > this.dependencies.now()
      ) {
        this.show({
          kind: waiting.limited ? "limited" : "unavailable",
          page,
          size,
          retryAt: waiting.at,
        });
        return;
      }
      const result = await this.dependencies.history(
        binding.selection.id,
        page,
        size,
        request.signal,
      );
      if (!active()) return;
      if (result.kind === "untrusted") {
        current.reload();
        return;
      }
      if (!(await current.confirm(binding)) || !active()) return;
      if (result.kind === "history") {
        this.waiting = undefined;
        this.show({
          kind: "ready",
          page,
          size,
          count: result.count,
          corrections: result.corrections,
        });
      } else if (
        result.kind === "failure" &&
        (result.status === 401 || result.status === 403)
      ) {
        await current.reject(binding, result.status);
      } else {
        const limited =
          result.kind === "failure" && result.code === "rate_limited";
        const delay =
          result.kind === "failure"
            ? Math.max(5_000, result.retryAfter)
            : 5_000;
        const retryAt = Math.min(
          Number.MAX_SAFE_INTEGER,
          this.dependencies.now() + delay,
        );
        this.waiting = { selection: binding.selection, at: retryAt, limited };
        this.show({
          kind: limited ? "limited" : "unavailable",
          page,
          size,
          retryAt,
        });
      }
    } finally {
      clearTimeout(timer);
      if (this.request === request) this.request = undefined;
    }
  }
}
