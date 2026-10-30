import type { Account, CurrentResult } from "../protocol/session";
import { sameSelection, type BrowserSession, type Selection } from "./session";

export type CurrentState =
  | { kind: "checking" }
  | { kind: "anonymous" }
  | { kind: "expired"; selection: Selection }
  | { kind: "changed" }
  | { kind: "storage" }
  | { kind: "reload" }
  | { kind: "unavailable"; retryAt: number }
  | { kind: "limited"; retryAt: number }
  | { kind: "denied" }
  | { kind: "ready"; selection: Selection; account: Account };

export const CHECKING: CurrentState = { kind: "checking" };

// One successful verification fixes the account, person and real-name revision.
// Consumers retain this non-private binding instead of retaining the projection.
export type CurrentBinding = Readonly<{
  generation: number;
  selection: Selection;
}>;

type Dependencies = {
  sessions: {
    read(): Promise<BrowserSession | null>;
    clear(expected: Selection): Promise<boolean>;
  };
  build(): Promise<boolean>;
  current(id: string, signal: AbortSignal): Promise<CurrentResult>;
  now(): number;
};

/** The account projection lives only in this visible page's verified state. */
export class Current {
  private state: CurrentState = CHECKING;
  private readonly listeners = new Set<() => void>();
  private generation = 0;
  private reloadNeeded = false;
  private request?: AbortController;
  private waiting?: { selection: Selection; at: number; limited: boolean };

  constructor(private readonly dependencies: Dependencies) {}

  snapshot = (): CurrentState => this.state;
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  private show(state: CurrentState) {
    this.state = state;
    this.listeners.forEach((listener) => listener());
  }

  suspend() {
    this.generation++;
    this.request?.abort();
    this.request = undefined;
    this.show(this.reloadNeeded ? { kind: "reload" } : CHECKING);
  }

  reload() {
    this.reloadNeeded = true;
    this.suspend();
  }

  binding(): CurrentBinding | undefined {
    return this.state.kind === "ready"
      ? { generation: this.generation, selection: this.state.selection }
      : undefined;
  }

  matches(binding: CurrentBinding): boolean {
    return (
      this.state.kind === "ready" && this.generation === binding.generation
    );
  }

  async confirm(binding: CurrentBinding): Promise<boolean> {
    if (!this.matches(binding)) return false;
    try {
      const selected = await this.dependencies.sessions.read();
      if (!this.matches(binding)) return false;
      if (sameSelection(selected, binding.selection)) return true;
      void this.verify();
    } catch {
      if (this.matches(binding)) {
        this.suspend();
        this.show({ kind: "storage" });
      }
    }
    return false;
  }

  async reject(binding: CurrentBinding, status: 401 | 403): Promise<void> {
    if (!this.matches(binding)) return;
    this.suspend();
    this.waiting = undefined;
    const generation = this.generation;
    if (status === 403) {
      this.show({ kind: "denied" });
      return;
    }
    try {
      const cleared = await this.dependencies.sessions.clear(binding.selection);
      if (generation !== this.generation) return;
      if (cleared) this.show({ kind: "expired", selection: binding.selection });
      else void this.verify();
    } catch {
      if (generation === this.generation) this.show({ kind: "storage" });
    }
  }

  async verify(): Promise<void> {
    this.suspend();
    if (this.reloadNeeded) return;
    const generation = this.generation;
    const request = new AbortController();
    this.request = request;
    const active = () => generation === this.generation;
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      const compatible = await this.dependencies.build().catch(() => false);
      if (!active()) return;
      if (!compatible) {
        this.reload();
        return;
      }
      const selected = await this.dependencies.sessions.read();
      if (!active()) return;
      if (!selected?.session) {
        this.waiting = undefined;
        this.show({ kind: "anonymous" });
        return;
      }
      const selection = {
        id: selected.session.id,
        revision: selected.revision,
      };
      if (
        this.waiting &&
        sameSelection(selected, this.waiting.selection) &&
        this.waiting.at > this.dependencies.now()
      ) {
        this.show({
          kind: this.waiting.limited ? "limited" : "unavailable",
          retryAt: this.waiting.at,
        });
        return;
      }
      timer = setTimeout(() => request.abort(), 35_000);
      const result = await this.dependencies.current(
        selection.id,
        request.signal,
      );
      if (!active()) return;
      if (result.kind === "untrusted") {
        this.reload();
        return;
      }
      const current = await this.dependencies.sessions.read();
      if (!active()) return;
      if (!sameSelection(current, selection)) {
        this.show({ kind: "changed" });
        void this.verify();
        return;
      }
      if (result.kind === "account") {
        this.waiting = undefined;
        this.show({ kind: "ready", selection, account: result.account });
      } else if (
        result.kind === "failure" &&
        result.status === 401 &&
        result.code === "authentication_invalid"
      ) {
        const cleared = await this.dependencies.sessions.clear(selection);
        if (!active()) return;
        if (cleared) {
          this.waiting = undefined;
          this.show({ kind: "expired", selection });
        } else {
          this.show({ kind: "changed" });
          void this.verify();
        }
      } else if (
        result.kind === "failure" &&
        result.status === 403 &&
        result.code === "permission_denied"
      ) {
        this.waiting = undefined;
        this.show({ kind: "denied" });
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
        this.waiting = { selection, at: retryAt, limited };
        this.show({ kind: limited ? "limited" : "unavailable", retryAt });
      }
    } catch {
      if (active()) this.show({ kind: "storage" });
    } finally {
      clearTimeout(timer);
      if (this.request === request) this.request = undefined;
    }
  }
}
