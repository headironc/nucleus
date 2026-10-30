import { Current, type CurrentBinding } from "../authentication/current";
import type {
  GrantLocator,
  ServiceStatus,
  WorkforceResult,
} from "../protocol/workforce";

export type WorkforceState =
  | { kind: "hidden" }
  | { kind: "loading" }
  | { kind: "ready"; status: ServiceStatus; grant: GrantLocator | null }
  | { kind: "unavailable" | "limited"; retryAt: number };

type Dependencies = {
  current: Current;
  workforce(id: string, signal: AbortSignal): Promise<WorkforceResult>;
  build(): Promise<boolean>;
  now(): number;
};

/** The visible page owns one status bound to a trusted Current generation. */
export class Workforce {
  private state: WorkforceState = { kind: "hidden" };
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

  snapshot = (): WorkforceState => this.state;
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };
  private show(state: WorkforceState) {
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
    if (this.binding) void this.refresh();
  };
  close() {
    this.unwatch();
    this.cancel();
    this.binding = undefined;
    this.waiting = undefined;
    this.show({ kind: "hidden" });
  }
  async refresh(): Promise<void> {
    const binding = this.binding;
    const { current } = this.dependencies;
    if (!binding || !current.matches(binding)) return;
    this.cancel();
    const generation = this.generation;
    const active = () =>
      generation === this.generation && current.matches(binding);
    this.show({ kind: "loading" });
    const request = new AbortController();
    this.request = request;
    const timer = setTimeout(() => request.abort(), 35_000);
    try {
      if (!(await this.dependencies.build().catch(() => false))) {
        if (active()) current.reload();
        return;
      }
      if (!active() || !(await current.confirm(binding)) || !active()) return;
      const waiting = this.waiting;
      if (
        waiting &&
        waiting.selection.id === binding.selection.id &&
        waiting.selection.revision === binding.selection.revision &&
        waiting.at > this.dependencies.now()
      ) {
        this.show({
          kind: waiting.limited ? "limited" : "unavailable",
          retryAt: waiting.at,
        });
        return;
      }
      const result = await this.dependencies.workforce(
        binding.selection.id,
        request.signal,
      );
      if (!active()) return;
      if (result.kind === "untrusted") {
        current.reload();
        return;
      }
      if (!(await current.confirm(binding)) || !active()) return;
      if (result.kind === "status") {
        this.waiting = undefined;
        this.show({
          kind: "ready",
          status: result.status,
          grant: result.grant,
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
        this.show({ kind: limited ? "limited" : "unavailable", retryAt });
      }
    } finally {
      clearTimeout(timer);
      if (this.request === request) this.request = undefined;
    }
  }
}
