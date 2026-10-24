import { Current, type CurrentState } from "../authentication/current";
import {
  sameSelection,
  type BrowserSession,
  type Selection,
} from "../authentication/session";
import type { Account, CurrentResult } from "../protocol/session";
import type { RevokeResult, ReadResult } from "../protocol/revocation";
import { matchesOriginal } from "../protocol/revocation";
import { UUID } from "../codes/material";
import {
  read,
  save,
  remove,
  type Material,
  type Saved,
  type Store,
} from "./material";

import * as all from "./material";

type AllMaterial = all.AllMaterial;
type AllSaved = all.AllSaved;

type Stored = Saved | AllSaved;
type Persisted = Material | AllMaterial;

export type State =
  | {
      kind: "current";
      current: Exclude<CurrentState, { kind: "ready" }>;
      pending: boolean;
      rejected?: boolean;
    }
  | { kind: "ready"; account: Account; selection: Selection }
  | { kind: "preparing" | "querying" }
  | { kind: "submitting"; pending: boolean }
  | {
      kind: "result";
      outcome: Material["outcome"];
      notice: Material["notice"];
      retryAt: number;
      canRead: boolean;
      canStart: boolean;
      newLogin: boolean;
    }
  | {
      kind: "other" | "missing" | "storage" | "reload" | "random";
      unsubmitted?: boolean;
      pending?: boolean;
    };

export type Marker = { preparing: string } | "submitted" | null;

export function hasPendingResult(state: State) {
  return (
    (state.kind === "submitting" && state.pending) ||
    state.kind === "querying" ||
    (state.kind === "result" && state.outcome === "pending") ||
    ((state.kind === "storage" || state.kind === "reload") &&
      state.pending === true) ||
    (state.kind === "current" && state.pending)
  );
}

type Dependencies = {
  storage: Store;
  sessions: {
    read(): Promise<BrowserSession | null>;
    clear(expected: Selection): Promise<boolean>;
  };
  build(): Promise<boolean>;
  current(id: string, signal: AbortSignal): Promise<CurrentResult>;
  revoke(
    operation: string,
    session: string,
    signal: AbortSignal,
  ): Promise<RevokeResult>;
  query(
    operation: string,
    original: string,
    session: string,
    signal: AbortSignal,
  ): Promise<ReadResult>;
  now(): number;
  uuid(): string;
  marker(): Marker;
  mark(value?: Marker): void;
};

/** Owns one tab's original logout; foreground checks never submit or query it. */
export class Logout {
  private readonly current: Current;
  private material: Stored = null;
  private readonly scope: `current` | `all`;
  private state: State = {
    kind: "current",
    current: { kind: "checking" },
    pending: false,
  };
  private readonly listeners = new Set<() => void>();
  private generation = 0;
  private request?: AbortController;
  private reloadNeeded = false;
  private preparation?: string;

  constructor(
    private readonly dependencies: Dependencies,
    scope: `current` | `all` = `current`,
  ) {
    this.scope = scope;
    this.current = new Current(dependencies);
    this.current.subscribe(() => this.render());
  }
  snapshot = () => this.state;
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };
  private show(state: State) {
    this.state = state;
    this.listeners.forEach((listener) => listener());
  }
  private load(): Stored {
    return this.scope === `all`
      ? all.readAll(this.dependencies.storage)
      : read(this.dependencies.storage);
  }
  private store(value: Persisted | `missing`) {
    if (this.scope === `all`)
      all.saveAll(this.dependencies.storage, value as AllMaterial);
    else save(this.dependencies.storage, value as Material);
  }
  private forget() {
    if (this.scope === `all`) all.removeAll(this.dependencies.storage);
    else remove(this.dependencies.storage);
  }
  detail() {
    const material = this.material;
    if (
      this.scope !== `all` ||
      ![`result`, `querying`].includes(this.state.kind)
    )
      return null;
    if (!material || material === `missing` || material.scope !== `all`)
      return null;
    return {
      count: material.count,
      binding: material.binding,
      record: material.record,
      revokedAt: material.revokedAt,
    };
  }
  private render() {
    if (this.reloadNeeded) {
      this.show({ kind: "reload", pending: false });
      return;
    }
    const current = this.current.snapshot();
    const material = this.material;
    if (current.kind !== "ready") {
      if (
        (current.kind === "anonymous" || current.kind === "expired") &&
        material === "missing"
      ) {
        this.show({ kind: "missing" });
      } else {
        const originalExpired =
          current.kind === "expired" &&
          !!material &&
          material !== "missing" &&
          current.selection.id === material.selection.id &&
          current.selection.revision === material.selection.revision;
        this.show({
          kind: "current",
          current,
          pending: originalExpired && material.outcome === "pending",
          rejected: originalExpired && material.outcome === "rejected",
        });
      }
      return;
    }
    if (material === "missing") {
      this.show({ kind: "missing" });
      return;
    }
    if (!material) {
      this.show({
        kind: "ready",
        account: current.account,
        selection: current.selection,
      });
      return;
    }
    if (current.account.id !== material.account) {
      this.show({ kind: "other" });
      return;
    }
    this.show({
      kind: "result",
      outcome: material.outcome,
      notice: material.notice,
      retryAt: material.retryAt,
      canRead: material.outcome === "pending" && material.notice !== "denied",
      canStart: material.outcome !== "pending",
      newLogin: current.selection.id !== material.selection.id,
    });
  }

  suspend() {
    this.generation++;
    this.request?.abort();
    this.request = undefined;
    this.current.suspend();
  }
  reload() {
    const pending = hasPendingResult(this.state) && this.pendingOutcome();
    this.reloadNeeded = true;
    this.suspend();
    // Only the visible, verified operation carries responsibility into reload.
    // A later suspension conceals it again before identity can change.
    this.show({ kind: "reload", pending });
  }
  async verify() {
    this.suspend();
    if (this.reloadNeeded) return;
    let unsubmitted = false;
    try {
      const marker = this.dependencies.marker();
      const preparation =
        this.preparation ??
        (marker && typeof marker === "object" ? marker.preparing : undefined);
      let material = this.load();
      const known = this.material;
      if (
        known &&
        known !== "missing" &&
        (known.outcome !== "pending" ||
          (known.scope === "all" && known.binding === "mismatch")) &&
        material &&
        material !== "missing" &&
        material.outcome === "pending" &&
        known.operation === material.operation &&
        known.account === material.account &&
        known.selection.id === material.selection.id &&
        known.selection.revision === material.selection.revision
      ) {
        // A failed save cannot undo a known result or a failed scope binding.
        this.persist(known);
        material = known;
      }
      if (
        preparation &&
        (!material ||
          (material !== "missing" && material.operation === preparation))
      ) {
        // An old history entry may only discard its own unsent preparation.
        unsubmitted = true;
        this.preparation = preparation;
        this.forget();
        this.dependencies.mark(null);
        this.material = null;
      } else this.material = material;
      this.preparation = undefined;
      if (
        this.material === "missing" ||
        (!this.material && marker === "submitted")
      ) {
        // Keep a tombstone so removing a damaged record cannot enable a new DELETE.
        this.store(`missing`);
        this.material = "missing";
      }
      if (this.material) this.dependencies.mark();
    } catch {
      this.show({ kind: "storage", unsubmitted });
      return;
    }
    await this.current.verify();
  }

  private persist(value: Persisted) {
    // Storage failure does not change the result already accepted by this page.
    this.material = value;
    this.store(value);
  }
  private pendingOutcome() {
    return (
      !!this.material &&
      this.material !== "missing" &&
      this.material.outcome === "pending"
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
      await this.verify();
      return false;
    }
    return true;
  }

  async submit() {
    if (this.state.kind !== "ready" || this.material !== null) return;
    const { account, selection } = this.state;
    const generation = ++this.generation;
    this.show({ kind: "preparing" });
    let sent = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      const compatible = await this.dependencies.build().catch(() => false);
      if (generation !== this.generation) return;
      if (!compatible) {
        this.reload();
        return;
      }
      if (!(await this.selected(selection, generation))) return;
      const operation = this.dependencies.uuid();
      if (!UUID.test(operation)) {
        this.show({ kind: "random" });
        return;
      }
      const material: Persisted =
        this.scope === `all`
          ? {
              version: 1,
              scope: `all`,
              operation,
              account: account.id,
              selection,
              outcome: `pending`,
              notice: `pending`,
              binding: "pending",
              count: null,
              record: null,
              retryAt: this.backoff(),
              revokedAt: null,
            }
          : {
              version: 1,
              operation,
              account: account.id,
              selection,
              outcome: "pending",
              notice: "pending",
              retryAt: this.backoff(),
              revokedAt: null,
            };
      this.preparation = operation;
      this.dependencies.mark({ preparing: operation });
      this.persist(material);
      const request = new AbortController();
      this.request = request;
      timer = setTimeout(() => request.abort(), 35_000);
      this.dependencies.mark("submitted");
      sent = true;
      this.preparation = undefined;
      this.show({ kind: "submitting", pending: true });
      const result = await this.dependencies.revoke(
        operation,
        selection.id,
        request.signal,
      );
      // A stopped local wait has no authority to turn the pending record into failure.
      if (generation !== this.generation) return;
      if (result.kind === "complete") {
        this.persist({
          ...(material.scope === "all"
            ? {
                ...material,
                count: result.completion.count,
                binding: "matched" as const,
                record: null,
              }
            : material),
          outcome: "complete",
          revokedAt: result.completion.revokedAt,
          retryAt: 0,
        });
        this.show({ kind: "submitting", pending: false });
        const cleared = await this.dependencies.sessions.clear(selection);
        if (generation !== this.generation) return;
        if (cleared) {
          // Only this accepted response and successful compare-and-clear authorize
          // the immediate receipt. Later entry must verify the original account.
          this.current.suspend();
          this.show({
            kind: "result",
            outcome: "complete",
            notice: "pending",
            retryAt: 0,
            canRead: false,
            canStart: false,
            newLogin: false,
          });
          return;
        }
      } else if (result.kind === "failure") {
        if (material.scope === `all` && result.code === `request_mismatch`) {
          // The server could not bind the DELETE to the requested scope. This
          // evidence is permanent and cannot be replaced by a later notice.
          const unverified: AllMaterial = {
            ...material,
            outcome: `pending`,
            notice: `mismatch`,
            retryAt: this.backoff(result.retryAfter),
            binding: "mismatch",
          };
          this.persist(unverified);
          if (generation === this.generation) await this.verify();
          return;
        }
        const rejection = [
          "invalid_input",
          "permission_denied",
          "authentication_invalid",
          "rate_limited",
        ].includes(result.code);
        this.persist({
          ...material,
          outcome: rejection ? "rejected" : "pending",
          notice:
            result.code === "rate_limited"
              ? "limited"
              : result.code === "permission_denied"
                ? "denied"
                : result.code === "authentication_invalid"
                  ? "authentication"
                  : result.code === "request_mismatch"
                    ? "mismatch"
                    : rejection
                      ? "invalid"
                      : "pending",
          retryAt: this.backoff(result.retryAfter),
        });
        if (rejection) this.show({ kind: "submitting", pending: false });
        if (result.code === "authentication_invalid") {
          const cleared = await this.dependencies.sessions.clear(selection);
          if (generation !== this.generation) return;
          if (cleared) {
            this.current.suspend();
            this.show({
              kind: "current",
              current: { kind: "expired", selection },
              pending: false,
              rejected: true,
            });
            return;
          }
        }
      } else {
        this.persist({
          ...material,
          retryAt: this.backoff(),
        });
        if (result.kind === "untrusted") {
          if (!(await this.selected(selection, generation))) return;
          this.reload();
          return;
        }
      }
      if (generation === this.generation) await this.verify();
    } catch {
      if (generation === this.generation)
        this.show({
          kind: "storage",
          unsubmitted: !sent,
          pending: sent && this.pendingOutcome(),
        });
    } finally {
      clearTimeout(timer);
    }
  }

  async query() {
    const material = this.material;
    if (
      this.state.kind !== "result" ||
      !this.state.canRead ||
      !material ||
      material === "missing" ||
      material.retryAt > this.dependencies.now()
    )
      return;
    // Reverify the current account before disclosing or requesting its old result.
    await this.verify();
    const current = this.current.snapshot();
    if (
      current.kind !== "ready" ||
      current.account.id !== material.account ||
      !this.material ||
      this.material === "missing" ||
      this.material.operation !== material.operation ||
      this.material.outcome !== "pending" ||
      this.material.notice === "denied"
    )
      return;
    const generation = ++this.generation;
    this.show({ kind: "querying" });
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      if (!(await this.selected(current.selection, generation))) return;
      this.persist({ ...material, retryAt: this.backoff() });
      const request = new AbortController();
      this.request = request;
      timer = setTimeout(() => request.abort(), 35_000);
      const result = await this.dependencies.query(
        material.operation,
        material.selection.id,
        current.selection.id,
        request.signal,
      );
      if (generation !== this.generation) return;
      if (!(await this.selected(current.selection, generation))) return;
      if (material.scope === `all` && result.kind === `collection`) {
        this.persist(
          result.collection.count === 0
            ? { ...material, notice: `empty`, retryAt: this.backoff() }
            : material.binding !== "mismatch" &&
                matchesOriginal(result.collection, "all", material.selection.id)
              ? {
                  ...material,
                  outcome: `complete`,
                  notice: `pending`,
                  retryAt: 0,
                  revokedAt: result.collection.revocations[0].revokedAt,
                  count: result.collection.count,
                  record: null,
                  binding: "matched" as const,
                }
              : {
                  // A mismatched binding describes a prior record, never this attempt.
                  ...material,
                  binding: "mismatch" as const,
                  notice: `mismatch`,
                  retryAt: this.backoff(),
                  record: {
                    count: result.collection.count,
                    revokedAt: result.collection.revocations[0].revokedAt,
                  },
                },
        );
        this.render();
        return;
      }
      if (result.kind === "untrusted") {
        this.reload();
        return;
      }
      if (result.kind === "collection") {
        this.persist(
          result.collection.count === 1
            ? {
                ...material,
                outcome: "complete",
                retryAt: 0,
                revokedAt: result.collection.revocations[0].revokedAt,
              }
            : { ...material, notice: "empty", retryAt: this.backoff() },
        );
      } else {
        this.persist({
          ...material,
          notice:
            result.kind === "failure" && result.code === "rate_limited"
              ? "limited"
              : result.kind === "failure" &&
                  ["permission_denied", "invalid_input"].includes(result.code)
                ? "denied"
                : "unavailable",
          retryAt: this.backoff(
            result.kind === "failure" ? result.retryAfter : 0,
          ),
        });
        if (
          result.kind === "failure" &&
          result.code === "authentication_invalid"
        ) {
          const cleared = await this.dependencies.sessions.clear(
            current.selection,
          );
          if (generation !== this.generation) return;
          if (cleared) {
            this.current.suspend();
            this.show({
              kind: "current",
              current: { kind: "expired", selection: current.selection },
              pending:
                current.selection.id === material.selection.id &&
                current.selection.revision === material.selection.revision,
            });
            return;
          }
          await this.verify();
          return;
        }
      }
      this.render();
    } catch {
      if (generation === this.generation)
        this.show({ kind: "storage", pending: this.pendingOutcome() });
    } finally {
      clearTimeout(timer);
    }
  }

  async start() {
    if (
      this.state.kind !== "result" ||
      !this.state.canStart ||
      this.state.retryAt > this.dependencies.now()
    )
      return;
    try {
      this.dependencies.mark(null);
      this.forget();
      this.material = null;
      // A new explicit confirmation uses a new history entry, without the old marker.
      this.current.suspend();
      await this.current.verify();
    } catch {
      this.show({ kind: "storage" });
    }
  }
}
