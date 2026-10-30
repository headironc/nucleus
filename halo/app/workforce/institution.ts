import { Current, type CurrentBinding } from "../authentication/current";
import type { RoleResult } from "../protocol/admission";

export type InstitutionState =
  | { kind: "hidden" | "loading" | "allowed" | "denied" }
  | { kind: "unavailable" | "limited"; retryAt: number };

/** Current institution permission, independent of Workforce membership. */
export class Institution {
  private state: InstitutionState = { kind: "hidden" };
  private readonly listeners = new Set<() => void>();
  private generation = 0;
  private request?: AbortController;
  private binding?: CurrentBinding;
  private waiting?: {
    selection: CurrentBinding["selection"];
    state: Extract<InstitutionState, { retryAt: number }>;
  };
  private readonly unwatch: () => void;
  constructor(
    private readonly dependencies: {
      current: Current;
      role(session: string, signal: AbortSignal): Promise<RoleResult>;
      build(): Promise<boolean>;
      now(): number;
    },
  ) {
    this.unwatch = dependencies.current.subscribe(this.changed);
    this.changed();
  }
  snapshot = () => this.state;
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };
  private show(state: InstitutionState) {
    this.state = state;
    this.listeners.forEach((listener) => listener());
  }
  private changed = () => {
    this.generation++;
    this.request?.abort();
    this.binding = this.dependencies.current.binding();
    this.show({ kind: "hidden" });
    if (this.binding) void this.refresh();
  };
  async refresh(): Promise<void> {
    const { current } = this.dependencies;
    const binding = this.binding;
    if (!binding || !current.matches(binding)) return;
    if (
      this.waiting &&
      this.waiting.selection.id === binding.selection.id &&
      this.waiting.selection.revision === binding.selection.revision &&
      this.waiting.state.retryAt > this.dependencies.now()
    ) {
      this.show(this.waiting.state);
      return;
    }
    if ("retryAt" in this.state && this.state.retryAt > this.dependencies.now())
      return;
    this.request?.abort();
    const generation = ++this.generation;
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
      const result = await this.dependencies.role(
        binding.selection.id,
        request.signal,
      );
      if (!active() || !(await current.confirm(binding)) || !active()) return;
      if (result.kind === "role") {
        this.waiting = undefined;
        this.show({
          kind: result.role.permissions.includes("institution.workforce")
            ? "allowed"
            : "denied",
        });
      } else if (result.kind === "untrusted") current.reload();
      else if (result.kind === "failure" && [401, 403].includes(result.status))
        await current.reject(binding, result.status as 401 | 403);
      else {
        const state = {
          kind:
            result.kind === "failure" && result.code === "rate_limited"
              ? ("limited" as const)
              : ("unavailable" as const),
          retryAt:
            this.dependencies.now() +
            Math.max(5_000, result.kind === "failure" ? result.retryAfter : 0),
        };
        this.waiting = { selection: binding.selection, state };
        this.show(state);
      }
    } finally {
      clearTimeout(timer);
    }
  }
  close() {
    this.unwatch();
    this.generation++;
    this.request?.abort();
    this.show({ kind: "hidden" });
  }
}
