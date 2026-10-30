import { Current, type CurrentBinding, type CurrentState } from "../authentication/current";
import {
  sameSelection,
  type BrowserSession,
  type Selection,
} from "../authentication/session";
import { UUID } from "../codes/material";
import type { Account, CurrentResult } from "../protocol/session";
import type {
  GrantLocator,
  TerminationResult,
  WorkforceResult,
} from "../protocol/workforce";
import { Workforce, type WorkforceState } from "./status";
import {
  read,
  remove,
  save,
  type Active,
  type Known,
  type Material,
  type Notice,
  type Saved,
  type Store,
} from "./termination-material";

export type Marker = { preparing: string } | "submitted" | null;
type Pending = { request: AbortController; done: () => void };

type Entry = Readonly<{
  account: Account;
  selection: Selection;
  status: "active" | "suspended";
  grant: GrantLocator;
  known: Known | null;
}>;

export type State =
  | {
      kind: "current";
      current: Exclude<CurrentState, { kind: "ready" }>;
      pending: boolean;
      rejected: boolean;
    }
  | { kind: "other"; account: Account; service: WorkforceState }
  | { kind: "missing"; account: Account; service: WorkforceState }
  | { kind: "mismatch"; account: Account; service: WorkforceState }
  | { kind: "reading"; account: Account }
  | {
      kind: "service-failed";
      account: Account;
      service: WorkforceState;
    }
  | { kind: "unavailable"; account: Account; service: WorkforceState }
  | ({ kind: "entry"; service: WorkforceState } & Entry)
  | { kind: "preparing" }
  | { kind: "submitting" }
  | { kind: "querying" }
  | { kind: "unsubmitted"; account: Account; service: WorkforceState }
  | {
      kind: "storage";
      unsubmitted: boolean;
      pending: boolean;
    }
  | { kind: "random" }
  | { kind: "reload"; pending: boolean }
  | {
      kind: "result";
      outcome: "pending" | "complete" | "rejected";
      notice: Notice;
      retryAt: number;
      account: Account;
      selection: Selection;
      service: WorkforceState;
      grant: string;
      terminatedAt: string | null;
      previous: string | null;
      persisted: boolean;
      canQuery: boolean;
      canStart: boolean;
      allowNew: boolean;
    };

type Dependencies = {
  storage: Store;
  sessions: {
    read(): Promise<BrowserSession | null>;
    clear(expected: Selection): Promise<boolean>;
  };
  build(): Promise<boolean>;
  current(id: string, signal: AbortSignal): Promise<CurrentResult>;
  workforce(id: string, signal: AbortSignal): Promise<WorkforceResult>;
  terminate(
    operation: string,
    grant: string,
    person: string,
    session: string,
    signal: AbortSignal,
  ): Promise<TerminationResult>;
  now(): number;
  uuid(): string;
  marker(): Marker;
  mark(value?: Marker): void;
};

export function hasPendingResult(state: State): boolean {
  return (
    state.kind === "submitting" ||
    state.kind === "querying" ||
    (state.kind === "result" &&
      state.outcome === "pending" &&
      state.notice !== "mismatch") ||
    ((state.kind === "storage" || state.kind === "reload") && state.pending) ||
    (state.kind === "current" && state.pending)
  );
}

const DEFINITE = new Set([400, 413, 415, 422]);

/** One tab's self termination bound to a trusted account, Grant and selection. */
export class Termination {
  private readonly current: Current;
  private readonly service: Workforce;
  private readonly listeners = new Set<() => void>();
  private state: State = {
    kind: "current",
    current: { kind: "checking" },
    pending: false,
    rejected: false,
  };
  private material: Saved = null;
  private unsaved = false;
  private generation = 0;
  private request?: AbortController;
  private reloadNeeded = false;
  private preparation?: string;
  private phase:
    | "idle"
    | "preparing"
    | "submitting"
    | "querying"
    | "unsubmitted"
    | "storage"
    | "random" = "idle";
  private storageUnsubmitted = false;
  private persistedComplete = true;

  constructor(private readonly dependencies: Dependencies) {
    this.current = new Current(dependencies);
    this.service = new Workforce({
      current: this.current,
      workforce: dependencies.workforce,
      build: dependencies.build,
      now: dependencies.now,
    });
    this.current.subscribe(this.changed);
    this.service.subscribe(this.changed);
    this.render();
  }

  snapshot = (): State => this.state;
  refreshService() {
    return this.service.refresh();
  }
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };
  private changed = () => this.render();
  private notify() {
    this.render();
  }
  private show(state: State) {
    this.state = state;
    this.listeners.forEach((listener) => listener());
  }
  close() {
    this.generation++;
    this.request?.abort();
    this.request = undefined;
    this.service.close();
    this.current.suspend();
    this.material = null;
    this.unsaved = false;
    this.show({
      kind: "current",
      current: { kind: "checking" },
      pending: false,
      rejected: false,
    });
  }

  suspend() {
    this.generation++;
    this.request?.abort();
    this.request = undefined;
    this.phase = "idle";
    this.current.suspend();
    this.render();
  }

  reload() {
    this.reloadNeeded = true;
    const pending = this.hasPending();
    this.generation++;
    this.request?.abort();
    this.request = undefined;
    this.phase = "idle";
    this.current.suspend();
    this.show({ kind: "reload", pending });
  }

  private hasPending() {
    const material = this.material;
    return (
      material === "missing" ||
      (!!material && material.active?.outcome === "pending" && material.active.sent)
    );
  }

  async verify() {
    this.generation++;
    this.request?.abort();
    this.request = undefined;
    this.phase = "idle";
    this.current.suspend();
    if (this.reloadNeeded) return;
    let unsubmitted = false;
    try {
      const marker = this.dependencies.marker();
      const preparation =
        this.preparation ??
        (marker && typeof marker === "object" ? marker.preparing : undefined);
      // A failed write leaves older browser data. Identity rechecks must not
      // replace a result already accepted by this page with that older state.
      let material = this.unsaved
        ? this.material
        : read(this.dependencies.storage);
      if (material !== null && material !== "missing") {
        const unsent =
          material.active && !material.active.sent
            ? material.active
            : undefined;
        if (unsent) {
          // A saved `sent: false` is durable proof that this attempt never
          // reached the server, even if an older marker remains.
          const next: Material | null = material.known.length
            ? { version: 1, known: material.known, active: null }
            : null;
          if (next) this.store(next);
          else this.clearMaterial();
          material = next;
          if (!next) this.dependencies.mark(null);
          unsubmitted = true;
        }
      }
      const record =
        material !== null && material !== "missing" ? material : null;
      const current = record?.active;
      if (
        preparation &&
        material !== "missing" &&
        (record === null ||
          !current ||
          (!current.sent && current.operation === preparation))
      ) {
        // A preparation marker may only discard its own unsent record.
        this.preparation = preparation;
        if (record === null || record.known.length === 0)
          this.clearMaterial();
        else
          this.store({
            version: 1,
            known: record.known,
            active: null,
          });
        this.dependencies.mark(null);
        material = record?.known.length
          ? { version: 1, known: record.known, active: null }
          : null;
        this.material = material;
        unsubmitted = true;
      } else this.material = material;
      this.preparation = undefined;
      if (this.material === "missing") {
        this.store("missing");
      } else if (
        this.material === null &&
        marker === "submitted" &&
        !unsubmitted
      ) {
        // A request may have been attempted after the record was lost.
        this.material = "missing";
        this.unsaved = true;
        this.store("missing");
      }
    } catch {
      this.phase = "storage";
      this.storageUnsubmitted = unsubmitted;
      this.render();
      return;
    }
    await this.current.verify();
  }

  private entry(): Entry | undefined {
    const current = this.current.snapshot();
    const service = this.service.snapshot();
    if (current.kind !== "ready") return;
    if (service.kind !== "ready" || !service.grant) return;
    if (service.status !== "active" && service.status !== "suspended") return;
    const material = this.material;
    if (material === "missing") return;
    if (material) {
      if (this.activeFor(current.account.id)) return;
      if (
        material.known.some(
          (result) =>
            result.account === current.account.id &&
            result.person !== current.account.personId,
        )
      )
        return;
      if (this.known()?.grant === service.grant.id) return;
    }
    return {
      account: current.account,
      selection: current.selection,
      status: service.status,
      grant: service.grant,
      known: this.known(),
    };
  }

  private activeFor(account: string): Active | null {
    const active =
      this.material && this.material !== "missing" ? this.material.active : null;
    // A definite rejection has no unresolved write to protect for another
    // account. Keep it stored until the current account explicitly starts work.
    return active?.outcome === "rejected" && active.account !== account
      ? null
      : active;
  }

  private hasNewGrant(previous: string): boolean {
    const service = this.service.snapshot();
    return (
      service.kind === "ready" &&
      service.grant !== null &&
      (service.status === "active" || service.status === "suspended") &&
      service.grant.id !== previous
    );
  }

  private known(): Known | null {
    const current = this.current.snapshot();
    if (current.kind !== "ready") return null;
    return (
      this.completed().find(
        (result) =>
          result.account === current.account.id &&
          result.person === current.account.personId,
      ) ?? null
    );
  }

  private completed(): readonly Known[] {
    return this.material && this.material !== "missing"
      ? this.material.known
      : [];
  }

  private render() {
    this.show(this.compute());
  }

  private compute(): State {
    const material = this.material;
    const pending = this.hasPending();
    const current = this.current.snapshot();
    if (this.reloadNeeded || current.kind === "reload")
      return { kind: "reload", pending };
    if (this.phase === "preparing") return { kind: "preparing" };
    if (this.phase === "submitting") return { kind: "submitting" };
    if (this.phase === "querying") return { kind: "querying" };
    if (this.phase === "random") return { kind: "random" };
    if (this.phase === "storage")
      return {
        kind: "storage",
        unsubmitted: this.storageUnsubmitted,
        pending,
      };
    const service = this.service.snapshot();
    if (this.phase === "unsubmitted" && current.kind === "ready")
      return { kind: "unsubmitted", account: current.account, service };
    if (current.kind !== "ready")
      return {
        kind: "current",
        current,
        pending,
        rejected: pending === false && this.wasRejected(material),
      };
    const account = current.account;
    if (material === "missing")
      return { kind: "missing", account, service };
    if (material) {
      const belongs = (value: { account: string; person: string }) =>
        value.account === account.id && value.person === account.personId;
      if (
        material.known.some(
          (result) => result.account === account.id && !belongs(result),
        )
      )
        return { kind: "other", account, service };
      const known = this.known();
      const active = this.activeFor(account.id);
      if (active) {
        if (!belongs(active)) return { kind: "other", account, service };
        if (active.sent) {
          if (active.notice === "mismatch")
            return { kind: "mismatch", account, service };
          if (active.outcome === "rejected")
            return {
              kind: "result",
              outcome: "rejected",
              notice: active.notice,
              retryAt: active.retryAt,
              account,
              selection: current.selection,
              service,
              grant: active.grant,
              terminatedAt: null,
              previous: known?.terminatedAt ?? null,
              persisted: true,
              canQuery: false,
              canStart:
                active.notice !== "conflict" || this.hasNewGrant(active.grant),
              allowNew: false,
            };
          return {
            kind: "result",
            outcome: "pending",
            notice: active.notice,
            retryAt: active.retryAt,
            account,
            selection: current.selection,
            service,
            grant: active.grant,
            terminatedAt: null,
            previous: known?.terminatedAt ?? null,
            persisted: true,
            canQuery: active.notice !== "denied",
            canStart: false,
            allowNew: false,
          };
        }
      }
      if (known) {
        return {
          kind: "result",
          outcome: "complete",
          notice: "pending",
          retryAt: 0,
          account,
          selection: current.selection,
          service,
          grant: known.grant,
          terminatedAt: known.terminatedAt,
          previous: null,
          persisted: this.persistedComplete,
          canQuery: false,
          canStart: false,
          allowNew: this.hasNewGrant(known.grant),
        };
      }
    }
    if (service.kind === "hidden" || service.kind === "loading")
      return { kind: "reading", account };
    if (service.kind === "limited" || service.kind === "unavailable")
      return { kind: "service-failed", account, service };
    if (
      service.kind === "ready" &&
      service.grant &&
      (service.status === "active" || service.status === "suspended")
    )
      return {
        kind: "entry",
        account,
        selection: current.selection,
        status: service.status,
        grant: service.grant,
        known: null,
        service,
      };
    return { kind: "unavailable", account, service };
  }

  private wasRejected(material: Saved) {
    return (
      !!material &&
      material !== "missing" &&
      material.active?.outcome === "rejected"
    );
  }

  private backoff(milliseconds = 0) {
    return Math.min(
      Number.MAX_SAFE_INTEGER,
      this.dependencies.now() + Math.max(5_000, milliseconds),
    );
  }

  private async selected(expected: Selection, generation: number) {
    const choice = await this.dependencies.sessions.read();
    if (generation !== this.generation) return false;
    if (!sameSelection(choice, expected)) {
      void this.verify();
      return false;
    }
    return true;
  }

  private abort() {
    const request = new AbortController();
    this.request = request;
    const timer = setTimeout(() => request.abort(), 35_000);
    return { request, done: () => clearTimeout(timer) };
  }

  private revert(known: readonly Known[]) {
    // No request has been sent on this path, so a best-effort removal is safe.
    try {
      if (known.length) {
        const next: Material = { version: 1, known, active: null };
        this.store(next);
      } else {
        this.clearMaterial();
      }
      this.dependencies.mark(null);
    } catch {
      // Keep the conservative in-memory record; no success is claimed.
    }
  }

  async submit() {
    const entry = this.entry();
    if (!entry) return;
    const known = this.completed();
    const generation = ++this.generation;
    this.phase = "preparing";
    this.notify();
    let active: Active | undefined;
    let attempted = false;
    let pending: Pending | undefined;
    try {
      if (!(await this.dependencies.build().catch(() => false))) {
        if (generation === this.generation) this.reload();
        return;
      }
      if (generation !== this.generation) return;
      const binding = this.current.binding();
      if (
        !binding ||
        binding.selection.id !== entry.selection.id ||
        binding.selection.revision !== entry.selection.revision
      ) {
        this.phase = "idle";
        this.render();
        return;
      }
      if (!(await this.current.confirm(binding)) || generation !== this.generation) {
        if (generation === this.generation) {
          this.phase = "idle";
          this.render();
        }
        return;
      }
      const operation = this.dependencies.uuid();
      if (!UUID.test(operation)) {
        this.phase = "random";
        this.notify();
        return;
      }
      active = {
        operation,
        account: entry.account.id,
        person: entry.account.personId,
        grant: entry.grant.id,
        selection: entry.selection,
        outcome: "pending",
        notice: "pending",
        sent: false,
        retryAt: this.backoff(),
      };
      const prepared: Material = {
        version: 1,
        known,
        active,
      };
      this.preparation = operation;
      this.dependencies.mark({ preparing: operation });
      if (
        (this.dependencies.marker() as { preparing?: string } | null)
          ?.preparing !== operation
      )
        throw new Error("termination_marker_unavailable");
      this.store(prepared);
      this.dependencies.mark("submitted");
      if (this.dependencies.marker() !== "submitted")
        throw new Error("termination_marker_unavailable");
      const sent: Material = {
        version: 1,
        known,
        active: { ...active, sent: true },
      };
      this.store(sent);
      this.preparation = undefined;
      if (
        generation !== this.generation ||
        !(await this.selected(entry.selection, generation)) ||
        !(await this.current.confirm(binding))
      ) {
        if (generation === this.generation) {
          this.revert(known);
          this.phase = "unsubmitted";
          this.storageUnsubmitted = true;
          this.notify();
        }
        return;
      }
      this.phase = "submitting";
      this.notify();
      attempted = true;
      pending = this.abort();
      const result = await this.dependencies.terminate(
        operation,
        entry.grant.id,
        entry.account.personId,
        entry.selection.id,
        pending.request.signal,
      );
      pending.done();
      pending = undefined;
      if (generation !== this.generation) return;
      if (!(await this.selected(entry.selection, generation))) return;
      await this.settle(result, { ...active, sent: true }, false);
    } catch {
      pending?.done();
      if (generation === this.generation) {
        this.phase = "storage";
        this.storageUnsubmitted = !attempted;
        this.notify();
      }
    }
  }

  async query() {
    const material = this.material;
    if (
      !material ||
      material === "missing" ||
      !material.active ||
      material.active.outcome !== "pending" ||
      !material.active.sent ||
      material.active.notice === "mismatch"
    )
      return;
    const active = material.active;
    const state = this.state;
    if (
      state.kind !== "result" ||
      state.outcome !== "pending" ||
      !state.canQuery ||
      state.retryAt > this.dependencies.now()
    )
      return;
    await this.current.verify();
    const current = this.current.snapshot();
    if (
      current.kind !== "ready" ||
      current.account.id !== active.account ||
      current.account.personId !== active.person
    )
      return;
    const generation = ++this.generation;
    this.phase = "querying";
    this.notify();
    let pending: Pending | undefined;
    try {
      if (!(await this.dependencies.build().catch(() => false))) {
        if (generation === this.generation) this.reload();
        return;
      }
      if (generation !== this.generation) return;
      const binding = this.current.binding();
      if (!binding || !(await this.current.confirm(binding))) {
        if (generation === this.generation) {
          this.phase = "idle";
          this.render();
        }
        return;
      }
      const stored = read(this.dependencies.storage);
      if (
        !stored ||
        stored === "missing" ||
        !stored.active ||
        stored.active.operation !== active.operation ||
        stored.active.account !== active.account ||
        stored.active.person !== active.person ||
        stored.active.grant !== active.grant ||
        stored.active.outcome !== "pending" ||
        !stored.active.sent ||
        stored.active.notice === "mismatch"
      ) {
        // Lost or replaced storage is not proof that this original was never
        // submitted. Preserve the unknown across rechecks and, when absent,
        // across reloads without overwriting another stored operation.
        this.material = "missing";
        this.unsaved = true;
        if (
          stored === null ||
          stored === "missing" ||
          stored.active?.operation === active.operation
        )
          this.store("missing");
        this.phase = "idle";
        this.render();
        return;
      }
      const next: Material = {
        version: 1,
        known: stored.known,
        active: {
          ...stored.active,
          retryAt: this.backoff(),
        },
      };
      this.store(next);
      if (!(await this.selected(current.selection, generation))) return;
      pending = this.abort();
      const result = await this.dependencies.terminate(
        active.operation,
        active.grant,
        active.person,
        current.selection.id,
        pending.request.signal,
      );
      pending.done();
      pending = undefined;
      if (generation !== this.generation) return;
      if (!(await this.selected(current.selection, generation))) return;
      await this.settle(result, next.active!, true);
    } catch {
      pending?.done();
      if (generation === this.generation) {
        this.phase = "storage";
        this.storageUnsubmitted = false;
        this.notify();
      }
    }
  }

  private clearMaterial() {
    remove(this.dependencies.storage);
    this.material = null;
    this.unsaved = false;
  }

  private store(next: Material | "missing") {
    save(this.dependencies.storage, next);
    this.material = next;
    this.unsaved = false;
    if (next !== "missing") this.persistedComplete = true;
  }

  private accept(next: Material) {
    // A response has already established this result. Local persistence
    // cannot roll it back, unlike a preparation that has not been sent.
    this.material = next;
    this.unsaved = true;
    this.store(next);
  }

  private async settle(
    result: TerminationResult,
    active: Active,
    replay: boolean,
  ) {
    this.phase = "idle";
    const now = this.dependencies.now();
    const known = this.completed();
    const nextActive = (outcome: Active["outcome"], notice: Notice, at = 0) => {
      const next: Material = {
        version: 1,
        known,
        active: { ...active, outcome, notice, retryAt: at },
      };
      try {
        this.accept(next);
      } catch {
        // The known rejection or unknown stays visible in this page.
        this.material = next;
      }
    };
    const keepPending = (notice: Notice, delay = 0) => {
      nextActive("pending", notice, this.backoff(delay));
    };
    if (result.kind === "termination") {
      const completion: Known = {
        operation: active.operation,
        account: active.account,
        person: active.person,
        grant: active.grant,
        terminatedAt: result.terminatedAt,
      };
      const next: Material = {
        version: 1,
        known: [
          ...known.filter((result) => result.account !== completion.account),
          completion,
        ],
        active: null,
      };
      this.persistedComplete = true;
      try {
        this.accept(next);
        this.dependencies.mark(null);
      } catch {
        // Keep the accepted in-memory result without claiming it was saved.
        this.material = next;
        this.persistedComplete = false;
      }
      this.render();
      void this.service.refresh();
      return;
    }
    if (result.kind === "failure") {
      if (result.status === 429) {
        if (replay) keepPending("limited", result.retryAfter);
        else {
          nextActive("rejected", "limited", this.backoff(result.retryAfter));
        }
        this.render();
        return;
      }
      if (result.status === 401) {
        keepPending("authentication");
        this.render();
        const binding = this.current.binding();
        if (binding) await this.current.reject(binding, 401);
        return;
      }
      if (result.status === 403) {
        keepPending("denied");
        this.render();
        const binding = this.current.binding();
        if (binding) await this.current.reject(binding, 403);
        return;
      }
      if (result.code === "request_mismatch") {
        nextActive("pending", "mismatch", this.backoff(result.retryAfter));
        this.render();
        return;
      }
      const definite =
        result.status === 404 ||
        result.status === 409 ||
        DEFINITE.has(result.status) ||
        ["target_unavailable", "state_conflict", "invalid_input"].includes(
          result.code,
        );
      if (definite) {
        if (replay) keepPending("refused", result.retryAfter);
        else {
          nextActive(
            "rejected",
            result.status === 404 || result.status === 409 || result.code === "state_conflict" || result.code === "target_unavailable"
              ? "conflict"
              : "invalid",
          );
        }
        this.render();
        return;
      }
    }
    // Network, timeout, untrusted and dependency failures keep the original.
    if (replay) keepPending("unavailable");
    else nextActive("pending", "pending", this.backoff());
    this.render();
  }

  resume() {
    this.phase = "idle";
    void this.verify();
  }

  async start() {
    const state = this.state;
    if (
      state.kind !== "result" ||
      state.outcome !== "rejected" ||
      !state.canStart ||
      state.retryAt > this.dependencies.now()
    )
      return;
    try {
      const known = this.completed();
      if (known.length) {
        const next: Material = { version: 1, known, active: null };
        this.store(next);
      } else {
        this.clearMaterial();
      }
      this.dependencies.mark(null);
      this.phase = "idle";
      await this.current.verify();
    } catch {
      this.phase = "storage";
      this.storageUnsubmitted = false;
      this.notify();
    }
  }
}
