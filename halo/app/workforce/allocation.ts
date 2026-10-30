import { Current, type CurrentState } from "../authentication/current";
import type { BrowserSession, Selection } from "../authentication/session";
import type { Account, CurrentResult } from "../protocol/session";
import type {
  AllocationInput,
  AllocationResult,
  NumberResult,
} from "../protocol/number";
import type { WorkforceResult } from "../protocol/workforce";
import { registry } from "../protocol/response";
import { UUID } from "../codes/material";
import { Workforce } from "./status";
import { Numbers } from "./numbers";
import {
  read,
  save,
  remove,
  type Store,
  type Allocation,
} from "./allocation-material";

export type State =
  | { kind: "current"; current: Exclude<CurrentState, { kind: "ready" }> }
  | {
      kind: "reading" | "entry" | "other" | "ineligible" | "present";
      account: Account;
    }
  | { kind: "unavailable"; account: Account; retryAt: number }
  | {
      kind: "result";
      account: Account;
      allocation: Allocation;
      persisted: boolean;
    }
  | { kind: "busy" | "storage" | "missing" | "reload" };

type Dependencies = {
  storage: Store;
  sessions: {
    read(): Promise<BrowserSession | null>;
    clear(expected: Selection): Promise<boolean>;
  };
  current(id: string, signal: AbortSignal): Promise<CurrentResult>;
  workforce(id: string, signal: AbortSignal): Promise<WorkforceResult>;
  number(id: string, signal: AbortSignal): Promise<NumberResult>;
  allocate(
    operation: string,
    id: string,
    input: AllocationInput,
    signal: AbortSignal,
  ): Promise<AllocationResult>;
  build(): Promise<boolean>;
  now(): number;
  uuid(): string;
  marker(): string | null;
  mark(operation: string | null): void;
};

/** The first allocation retains its complete original input until a known result. */
export class AllocationController {
  private readonly current: Current;
  private readonly service: Workforce;
  private readonly numbers: Numbers;
  private readonly listeners = new Set<() => void>();
  private state: State = { kind: "current", current: { kind: "checking" } };
  private material: Allocation | "missing" | null = null;
  private generation = 0;
  private request?: AbortController;
  private phase: "idle" | "busy" | "storage" | "reload" = "idle";
  private unsaved = false;

  constructor(private readonly dependencies: Dependencies) {
    this.current = new Current(dependencies);
    this.service = new Workforce({ ...dependencies, current: this.current });
    this.numbers = new Numbers({
      ...dependencies,
      current: this.current,
      workforce: this.service,
    });
    this.current.subscribe(this.render);
    this.service.subscribe(this.render);
    this.numbers.subscribe(this.render);
  }
  snapshot = (): State => this.state;
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };
  private render = () => {
    const identity = this.current.snapshot();
    let state: State;
    if (this.phase === "reload") state = { kind: "reload" };
    else if (identity.kind !== "ready")
      state = { kind: "current", current: identity };
    else if (this.phase === "storage") state = { kind: "storage" };
    else if (this.phase === "busy") state = { kind: "busy" };
    else if (this.material === "missing") state = { kind: "missing" };
    else if (this.material) {
      state =
        this.material.account === identity.account.id &&
        this.material.person === identity.account.personId
          ? {
              kind: "result",
              account: identity.account,
              allocation: this.material,
              persisted: !this.unsaved,
            }
          : { kind: "other", account: identity.account };
    } else {
      const service = this.service.snapshot();
      const numbers = this.numbers.snapshot();
      const account = identity.account;
      if (service.kind === "ready" && service.status !== "active")
        state = { kind: "ineligible", account };
      else if (service.kind === "unavailable" || service.kind === "limited")
        state = { kind: "unavailable", account, retryAt: service.retryAt };
      else if (numbers.kind === "unavailable" || numbers.kind === "limited")
        state = { kind: "unavailable", account, retryAt: numbers.retryAt };
      else if (service.kind !== "ready" || numbers.kind !== "ready")
        state = { kind: "reading", account };
      else state = { kind: numbers.number ? "present" : "entry", account };
    }
    this.state = state;
    this.listeners.forEach((listener) => listener());
  };
  private retryAt(delay = 0) {
    return Math.min(
      Number.MAX_SAFE_INTEGER,
      this.dependencies.now() + Math.max(5_000, delay),
    );
  }
  private store(value: Allocation) {
    this.material = value;
    this.unsaved = true;
    save(this.dependencies.storage, value);
    this.unsaved = false;
  }
  suspend() {
    this.generation++;
    this.request?.abort();
    this.request = undefined;
    if (
      this.phase === "busy" &&
      this.material &&
      this.material !== "missing" &&
      this.material.outcome === "pending"
    ) {
      try {
        this.store({
          ...this.material,
          retryAt: Math.max(this.material.retryAt, this.retryAt()),
        });
      } catch {
        this.phase = "storage";
      }
    }
    if (this.phase === "busy") this.phase = "idle";
    this.current.suspend();
  }
  reload() {
    this.suspend();
    this.phase = "reload";
    this.current.reload();
    this.render();
  }
  close() {
    this.suspend();
    this.numbers.close();
    this.service.close();
  }
  async verify() {
    this.suspend();
    if (this.phase === "reload") return;
    try {
      // A known but unsaved receipt must not be replaced by an older pending record.
      if (!this.unsaved) this.material = read(this.dependencies.storage);
      const marker = this.dependencies.marker();
      if (
        marker &&
        (!this.material ||
          this.material === "missing" ||
          this.material.operation !== marker)
      )
        this.material = "missing";
      this.phase = "idle";
    } catch {
      this.phase = "storage";
    }
    await this.current.verify();
    this.render();
  }
  async refresh() {
    if (this.phase !== "idle") return;
    await this.service.refresh();
    // Service refresh causes Numbers to read against the new trusted status.
  }
  async submit(subtype: number) {
    if (
      this.state.kind !== "entry" ||
      this.material ||
      !registry.allocateWorkforceNumber_input({ subtype })
    )
      return;
    const identity = this.current.snapshot();
    if (identity.kind !== "ready") return;
    const operation = this.dependencies.uuid();
    if (!UUID.test(operation)) {
      this.phase = "storage";
      this.render();
      return;
    }
    const allocation: Allocation = {
      version: 1,
      operation,
      account: identity.account.id,
      person: identity.account.personId,
      input: { subtype } as AllocationInput,
      retryAt: this.retryAt(),
      outcome: "pending",
      code: "pending",
      number: null,
    };
    try {
      this.store(allocation);
      this.dependencies.mark(operation);
      if (this.dependencies.marker() !== operation)
        throw Error("allocation_marker_unavailable");
    } catch {
      this.phase = "storage";
      this.render();
      return;
    }
    await this.send(allocation);
  }
  async query() {
    if (
      this.state.kind !== "result" ||
      this.state.allocation.outcome !== "pending" ||
      this.state.allocation.retryAt > this.dependencies.now() ||
      this.phase !== "idle"
    )
      return;
    const allocation = this.state.allocation;
    await this.verify();
    if (
      this.state.kind !== "result" ||
      this.state.allocation.operation !== allocation.operation ||
      this.state.allocation.outcome !== "pending" ||
      this.state.allocation.retryAt > this.dependencies.now()
    )
      return;
    const pending = this.state.allocation;
    try {
      const stored = read(this.dependencies.storage);
      if (
        !stored ||
        stored === "missing" ||
        JSON.stringify(stored) !== JSON.stringify(this.material)
      ) {
        this.material = "missing";
        this.render();
        return;
      }
      this.dependencies.mark(allocation.operation);
      if (this.dependencies.marker() !== allocation.operation)
        throw Error("allocation_marker_unavailable");
    } catch {
      this.phase = "storage";
      this.render();
      return;
    }
    await this.send(pending);
  }
  private async send(original: Allocation) {
    const binding = this.current.binding();
    if (!binding) return;
    const generation = ++this.generation;
    this.phase = "busy";
    this.render();
    const request = new AbortController();
    this.request = request;
    const timer = setTimeout(() => request.abort(), 35_000);
    const active = () =>
      generation === this.generation && this.current.matches(binding);
    const pending = () =>
      this.material &&
      this.material !== "missing" &&
      this.material.operation === original.operation &&
      this.material.outcome === "pending";
    try {
      const next = { ...original, retryAt: this.retryAt() };
      this.store(next);
      if (!(await this.dependencies.build().catch(() => false))) {
        if (active()) this.reload();
        return;
      }
      if (
        !active() ||
        !(await this.current.confirm(binding)) ||
        !active() ||
        !pending()
      )
        return;
      const result = await this.dependencies.allocate(
        original.operation,
        binding.selection.id,
        original.input,
        request.signal,
      );
      // A late confirmed success may preserve only this original receipt. It
      // never restores a hidden projection or changes the selected session.
      if (result.kind === "allocated") {
        if (
          this.material &&
          this.material !== "missing" &&
          this.material.operation === original.operation
        ) {
          try {
            this.store({
              ...next,
              outcome: "complete",
              code: "ok",
              number: result.number,
              retryAt: 0,
            });
          } catch {
            // store retains the confirmed receipt before attempting persistence.
            // Continue selection confirmation and show its unsaved warning.
          }
        }
        if (active()) await this.current.confirm(binding);
      } else if (active()) {
        if (!(await this.current.confirm(binding)) || !active()) return;
        if (result.kind === "untrusted") {
          try {
            if (pending()) this.store({ ...next, code: "untrusted" });
          } finally {
            this.reload();
          }
          return;
        }
        if (!pending()) return;
        if (result.kind === "failure") {
          const outcome =
            result.code === "request_mismatch"
              ? "mismatch"
              : result.code === "number_present" ||
                  result.code === "number_pool_exhausted" ||
                  [400, 413, 415, 422].includes(result.status)
                ? "refused"
                : "pending";
          this.store({
            ...next,
            outcome,
            code: result.code,
            retryAt: this.retryAt(result.retryAfter),
          });
          if (result.status === 401 && result.code === "authentication_invalid")
            await this.current.reject(binding, 401);
        } else
          this.store({
            ...next,
            code: "disconnected",
            retryAt: this.retryAt(),
          });
      }
    } catch {
      if (active()) this.phase = "storage";
    } finally {
      clearTimeout(timer);
      if (this.request === request) this.request = undefined;
      if (generation === this.generation && this.phase === "busy")
        this.phase = "idle";
      this.render();
    }
  }
  async chooseAgain() {
    if (
      this.state.kind !== "result" ||
      this.state.allocation.outcome !== "refused" ||
      this.state.allocation.code !== "number_pool_exhausted"
    )
      return;
    try {
      remove(this.dependencies.storage);
      this.dependencies.mark(null);
      if (this.dependencies.marker() !== null)
        throw Error("allocation_marker_unavailable");
      this.material = null;
      this.unsaved = false;
    } catch {
      this.phase = "storage";
      this.render();
      return;
    }
    await this.verify();
  }
}
