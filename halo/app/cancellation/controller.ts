import {
  Current,
  type CurrentState,
  type CurrentBinding,
} from "../authentication/current";
import {
  sameSelection,
  type BrowserSession,
  type Selection,
} from "../authentication/session";
import type { Account, CurrentResult } from "../protocol/session";
import type { CancelResult, Input, Result } from "../protocol/cancellation";
import { validInput } from "../protocol/cancellation";
import { UUID } from "../codes/material";
import {
  read,
  save,
  remove,
  type Material,
  type Saved,
  type Store,
} from "./material";

export type Marker = { preparing: string } | "submitted" | null;

export type UnknownNotice = "pending" | "mismatch" | "unknown" | "missing";

export type State =
  | {
      kind: "current";
      current: Exclude<CurrentState, { kind: "ready" }>;
      pending: boolean;
    }
  | { kind: "entry"; account: Account; selection: Selection }
  | { kind: "preparing" }
  | { kind: "submitting" }
  | { kind: "unsent" }
  | {
      kind: "refused";
      notice: Material["notice"];
      retryAt: number;
      canRestart: boolean;
    }
  | { kind: "unknown"; notice: UnknownNotice; persisted: boolean }
  | { kind: "success"; result: Result; persisted: boolean }
  | { kind: "hidden"; current: CurrentState }
  | { kind: "storage"; pending: boolean; unsubmitted: boolean }
  | { kind: "reload"; pending: boolean }
  | { kind: "random" };

export function hasPendingResult(state: State) {
  return (
    state.kind === "submitting" ||
    state.kind === "unknown" ||
    (state.kind === "current" && state.pending) ||
    ((state.kind === "storage" || state.kind === "reload") &&
      state.pending === true)
  );
}

/** Only an explicit navigation away from a confirmation or a possible send warns. */
export function leaveKind(state: State): "pending" | "unsubmitted" | null {
  if (hasPendingResult(state)) return "pending";
  if (state.kind === "entry" || state.kind === "preparing")
    return "unsubmitted";
  return null;
}

function equalSelection(left: Selection, right: Selection) {
  return left.id === right.id && left.revision === right.revision;
}

type Dependencies = {
  storage: Store;
  sessions: {
    read(): Promise<BrowserSession | null>;
    clear(expected: Selection): Promise<boolean>;
    release(expected: Selection): Promise<string | null>;
  };
  build(): Promise<boolean>;
  current(id: string, signal: AbortSignal): Promise<CurrentResult>;
  cancel(
    operation: string,
    session: string,
    input: Input,
    signal: AbortSignal,
  ): Promise<CancelResult>;
  now(): number;
  uuid(): string;
  marker(): Marker;
  mark(value?: Marker): void;
};

/** One tab's original self cancellation; foreground checks never resubmit it. */
export class Cancellation {
  private readonly current: Current;
  private readonly unsubscribe: () => void;
  private material: Saved = null;
  private state: State = {
    kind: "current",
    current: { kind: "checking" },
    pending: false,
  };
  private readonly listeners = new Set<() => void>();
  private readonly hiders = new Set<() => void>();
  private generation = 0;
  private request?: AbortController;
  private reloadNeeded = false;
  private phase: "idle" | "preparing" | "submitting" | "random" | "storage" =
    "idle";
  private unsubmitted = false;
  private unsaved = false;
  private checked = false;
  private checking = false;
  private verifying = false;
  private concealed = true;
  private concealedCurrent: CurrentState = { kind: "checking" };

  constructor(private readonly dependencies: Dependencies) {
    this.current = new Current({
      ...dependencies,
      sessions: {
        read: () => dependencies.sessions.read(),
        clear: async (selection) => (await this.release(selection)) !== null,
      },
    });
    this.unsubscribe = this.current.subscribe(() => this.render());
  }

  snapshot = () => this.state;
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };
  /** Registration lets lifecycle hiding close private dialogs synchronously. */
  onHide(listener: () => void) {
    this.hiders.add(listener);
    return () => this.hiders.delete(listener);
  }
  checkInfo() {
    return {
      visible: this.checked,
      checking: this.checking || this.verifying,
      current: this.checked
        ? this.current.snapshot()
        : { kind: "checking" as const },
    };
  }

  private show(state: State) {
    this.state = state;
    this.listeners.forEach((listener) => listener());
  }
  private hideAll() {
    this.hiders.forEach((listener) => {
      try {
        listener();
      } catch {
        // A dialog that is already gone must not block the controlled hide.
      }
    });
  }
  private persist(value: Material | "missing") {
    // A storage failure must not change what this page already knows.
    this.material = value;
    try {
      save(this.dependencies.storage, value);
      this.unsaved = false;
    } catch {
      this.unsaved = true;
    }
  }
  private pendingOutcome() {
    return (
      this.material === "missing" ||
      (!!this.material &&
        this.material.outcome === "pending" &&
        this.material.phase === "possibly-sent")
    );
  }
  private backoff(milliseconds = 0) {
    return Math.min(
      Number.MAX_SAFE_INTEGER,
      this.dependencies.now() + Math.max(5_000, milliseconds),
    );
  }
  private same(binding: CurrentBinding | undefined, selection: Selection) {
    return (
      !!binding &&
      binding.selection.id === selection.id &&
      binding.selection.revision === selection.revision
    );
  }
  private render() {
    if (this.concealed) {
      this.show({ kind: "hidden", current: this.concealedCurrent });
      return;
    }
    if (this.reloadNeeded) {
      this.show({ kind: "reload", pending: this.pendingOutcome() });
      return;
    }
    if (this.phase === "preparing") {
      this.show({ kind: "preparing" });
      return;
    }
    if (this.phase === "submitting") {
      this.show({ kind: "submitting" });
      return;
    }
    if (this.phase === "random") {
      this.show({ kind: "random" });
      return;
    }
    if (this.phase === "storage") {
      this.show({
        kind: "storage",
        pending: this.pendingOutcome(),
        unsubmitted: this.unsubmitted,
      });
      return;
    }
    const current = this.current.snapshot();
    const material = this.material;
    if (material && material !== "missing" && material.hidden) {
      this.show({ kind: "hidden", current });
      return;
    }
    if (this.unsubmitted) {
      this.show({ kind: "unsent" });
      return;
    }
    if (material === "missing") {
      this.show({
        kind: "unknown",
        notice: "missing",
        persisted: !this.unsaved,
      });
      return;
    }
    if (!material) {
      if (this.unsubmitted) {
        this.show({ kind: "unsent" });
        return;
      }
      if (current.kind === "ready") {
        this.show({
          kind: "entry",
          account: current.account,
          selection: current.selection,
        });
        return;
      }
      this.show({ kind: "current", current, pending: false });
      return;
    }
    if (material.outcome === "complete") {
      if (material.hidden) {
        this.show({ kind: "hidden", current });
        return;
      }
      if (
        current.kind === "ready" &&
        !equalSelection(current.selection, material.selection)
      ) {
        this.persist({ ...material, hidden: true });
        this.show({ kind: "hidden", current });
        return;
      }
      this.show({
        kind: "success",
        result: material.result!,
        persisted: !this.unsaved,
      });
      return;
    }
    if (material.outcome === "refused") {
      this.show({
        kind: "refused",
        notice: material.notice,
        retryAt: material.retryAt,
        canRestart:
          material.notice !== "authentication" &&
          material.notice !== "denied" &&
          material.retryAt <= this.dependencies.now(),
      });
      return;
    }
    if (material.phase === "prepared") {
      this.show({ kind: "unsent" });
      return;
    }
    if (material.hidden) {
      this.show({ kind: "hidden", current });
      return;
    }
    if (
      current.kind === "ready" &&
      !equalSelection(current.selection, material.selection)
    ) {
      this.persist({ ...material, hidden: true });
      this.show({ kind: "hidden", current });
      return;
    }
    this.show({
      kind: "unknown",
      notice: material.notice === "mismatch" ? "mismatch" : "unknown",
      persisted: !this.unsaved,
    });
  }

  private hide() {
    this.concealed = true;
    this.concealedCurrent = { kind: "checking" };
    this.hideAll();
    this.checked = false;
    this.checking = false;
    this.current.suspend();
  }

  suspend() {
    this.generation++;
    this.request?.abort();
    this.request = undefined;
    this.phase = "idle";
    this.verifying = false;
    this.hide();
    this.render();
  }

  reload() {
    const pending = !this.concealed && this.pendingOutcome();
    this.reloadNeeded = true;
    this.generation++;
    this.request?.abort();
    this.request = undefined;
    this.hideAll();
    this.current.reload();
    this.concealed = false;
    this.show({ kind: "reload", pending });
  }

  async verify() {
    if (this.reloadNeeded) {
      this.reload();
      return;
    }
    this.generation++;
    this.request?.abort();
    this.request = undefined;
    this.phase = "idle";
    this.hide();
    this.verifying = true;
    this.render();
    const generation = this.generation;
    try {
      await this.restore(generation);
    } finally {
      if (generation === this.generation) {
        this.verifying = false;
        this.render();
      }
    }
  }

  private async restore(generation: number) {
    let loaded: Saved;
    try {
      loaded = read(this.dependencies.storage);
    } catch {
      this.phase = "storage";
      if (this.material) await this.restorePermission(generation);
      else {
        this.concealed = false;
        this.render();
      }
      return;
    }
    if (generation !== this.generation) return;
    const known = this.material;
    const authoritative = !!known && known !== "missing";
    if (
      authoritative &&
      (loaded === null ||
        loaded === "missing" ||
        (loaded.outcome === "pending" && loaded.notice === "pending"))
    ) {
      // A failed save cannot downgrade a result this page already accepted.
      this.persist(known);
      loaded = known;
    } else if (
      authoritative &&
      loaded !== "missing" &&
      loaded !== null &&
      loaded.operation === known.operation
    ) {
      this.persist(known);
      loaded = known;
    }
    if (
      loaded === "missing" ||
      (loaded === null && this.dependencies.marker() !== null)
    ) {
      // A historical marker with no durable record can never become a new
      // confirmation, even though the original request may not have been sent.
      this.persist("missing");
      try {
        this.dependencies.mark("submitted");
      } catch {
        // The tombstone remains in memory and storage when history is unavailable.
      }
    } else {
      this.material = loaded;
      this.unsaved = false;
      if (loaded && this.dependencies.marker() === null) {
        try {
          this.dependencies.mark("submitted");
        } catch {
          // The material alone still preserves the original operation.
        }
      }
    }
    if (this.material) {
      await this.restorePermission(generation);
    } else {
      await this.current.verify();
      if (generation !== this.generation) return;
      this.concealed = false;
      this.render();
    }
  }

  private async restorePermission(generation: number) {
    const compatible = await this.dependencies.build().catch(() => false);
    if (generation !== this.generation) return;
    if (!compatible) {
      this.verifying = false;
      this.reload();
      return;
    }
    await this.permit(generation);
  }

  private async release(selection: Selection) {
    const original = this.material;
    const revision = await this.dependencies.sessions.release(selection);
    const material = this.material;
    if (
      revision &&
      material &&
      material !== "missing" &&
      original &&
      original !== "missing" &&
      original.operation === material.operation &&
      equalSelection(selection, material.selection)
    ) {
      this.persist({ ...material, clearedRevision: revision });
    }
    return revision;
  }

  /** Facts survive failures; displaying them requires a fresh selection check. */
  private async permit(generation: number) {
    try {
      const choice = await this.dependencies.sessions.read();
      if (generation !== this.generation) return;
      const material = this.material;
      if (material && material !== "missing") {
        const original = sameSelection(choice, material.selection);
        const cleared =
          !!choice &&
          choice.session === null &&
          choice.attempt === null &&
          material.clearedRevision !== null &&
          choice.revision === material.clearedRevision;
        if (!original && !cleared) {
          if (choice && material.phase === "prepared") {
            // No POST can precede the durable possibly-sent phase. Discard
            // this unsent confirmation before reading another identity.
            this.dependencies.mark(null);
            if (this.dependencies.marker() !== null)
              throw new Error("history unavailable");
            remove(this.dependencies.storage);
            this.material = null;
            this.unsaved = false;
            this.unsubmitted = false;
            await this.current.verify();
            if (generation !== this.generation) return;
            this.concealed = false;
            this.render();
            return;
          }
          if (choice && !material.hidden)
            this.persist({ ...material, hidden: true });
          this.concealed = true;
          this.concealedCurrent = { kind: choice ? "changed" : "anonymous" };
          this.checked = false;
          this.current.suspend();
          this.render();
          return;
        }
      }
      this.concealed = false;
    } catch {
      if (generation !== this.generation) return;
      this.concealed = true;
      this.concealedCurrent = { kind: "storage" };
    }
    this.render();
  }

  async check() {
    if (this.state.kind !== "unknown" || this.checking) return;
    this.hide();
    this.checking = true;
    this.render();
    const generation = this.generation;
    await this.current.verify();
    if (generation !== this.generation) return;
    this.checking = false;
    this.checked = true;
    await this.permit(generation);
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

  private revert() {
    try {
      this.dependencies.mark(null);
    } catch {
      // A marker left behind only causes a conservative unknown on reload.
    }
    try {
      remove(this.dependencies.storage);
    } catch {
      // A prepared record left behind remains visibly unsent.
    }
    this.material = null;
    this.unsaved = false;
  }

  async submit() {
    if (this.state.kind !== "entry" || this.material !== null) return;
    const { account, selection } = this.state;
    const generation = ++this.generation;
    this.phase = "preparing";
    this.render();
    let sent = false;
    let request: AbortController | undefined;
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      const compatible = await this.dependencies.build().catch(() => false);
      if (generation !== this.generation) return;
      if (!compatible) {
        this.reload();
        return;
      }
      const binding = this.current.binding();
      if (!binding || !this.same(binding, selection)) return;
      if (!(await this.current.confirm(binding))) return;
      if (generation !== this.generation) return;
      const operation = this.dependencies.uuid();
      if (!UUID.test(operation)) {
        this.phase = "random";
        this.render();
        return;
      }
      const target: Input = {
        accountId: account.id,
        personId: account.personId,
        revisionId: account.revisionId,
      };
      if (!validInput(target)) {
        this.unsubmitted = true;
        this.phase = "idle";
        this.render();
        return;
      }
      try {
        this.dependencies.mark({ preparing: operation });
      } catch {
        this.unsubmitted = true;
        this.phase = "idle";
        this.render();
        return;
      }
      const preparing = this.dependencies.marker();
      if (
        !preparing ||
        typeof preparing !== "object" ||
        preparing.preparing !== operation
      ) {
        this.unsubmitted = true;
        this.phase = "idle";
        this.render();
        return;
      }
      const prepared: Material = {
        version: 1,
        operation,
        target,
        selection,
        phase: "prepared",
        outcome: "pending",
        notice: "pending",
        retryAt: 0,
        result: null,
        hidden: false,
        clearedRevision: null,
      };
      this.persist(prepared);
      if (this.unsaved) {
        this.unsubmitted = true;
        this.phase = "idle";
        this.render();
        return;
      }
      const possiblySent: Material = { ...prepared, phase: "possibly-sent" };
      this.persist(possiblySent);
      if (this.unsaved) {
        this.unsubmitted = true;
        this.phase = "idle";
        this.render();
        return;
      }
      try {
        this.dependencies.mark("submitted");
      } catch {
        // No POST may start unless both the material and marker are durable.
        this.revert();
        this.unsubmitted = true;
        this.phase = "idle";
        this.render();
        return;
      }
      if (this.dependencies.marker() !== "submitted") {
        this.revert();
        this.unsubmitted = true;
        this.phase = "idle";
        this.render();
        return;
      }
      if (!(await this.selected(selection, generation))) return;
      if (generation !== this.generation) return;
      if (!(await this.current.confirm(binding))) {
        if (generation !== this.generation) return;
        this.revert();
        this.unsubmitted = true;
        await this.verify();
        return;
      }
      if (generation !== this.generation) return;
      this.phase = "submitting";
      this.render();
      const activeRequest = new AbortController();
      request = activeRequest;
      this.request = activeRequest;
      sent = true;
      timer = setTimeout(() => activeRequest.abort(), 35_000);
      const result = await this.dependencies.cancel(
        operation,
        selection.id,
        target,
        activeRequest.signal,
      );
      if (generation !== this.generation) return;
      await this.settle(result, possiblySent, generation);
    } catch {
      if (generation === this.generation) {
        this.phase = "idle";
        this.unsubmitted = !sent;
        this.render();
      }
    } finally {
      clearTimeout(timer);
      if (this.request === request) this.request = undefined;
      if (generation === this.generation && this.phase === "preparing") {
        this.phase = "idle";
        this.unsubmitted = true;
        this.render();
      }
    }
  }

  private async settle(
    result: CancelResult,
    material: Material,
    generation: number,
  ) {
    this.phase = "idle";
    this.hide();
    if (result.kind === "cancelled") {
      const complete: Material = {
        ...material,
        outcome: "complete",
        notice: "pending",
        retryAt: 0,
        result: result.result,
        hidden: false,
      };
      // A trusted success is preserved before any later read or cleanup can fail.
      this.persist(complete);
      try {
        this.dependencies.mark("submitted");
      } catch {
        // The stored result, not history, carries the completed fact.
      }
      if (generation !== this.generation) return;
      try {
        const selected = await this.dependencies.sessions.read();
        if (generation !== this.generation) return;
        if (sameSelection(selected, material.selection)) {
          try {
            // Compare-and-clear covers only the original session and revision.
            await this.release(material.selection);
          } catch {
            // A cleanup failure cannot change the accepted fact.
          }
        }
      } catch {
        // No cleanup or display is authorized by an unreadable selection.
      }
      if (generation !== this.generation) return;
      await this.permit(generation);
      return;
    }
    if (result.kind === "failure") {
      const refusal =
        result.code === "rate_limited"
          ? "limited"
          : result.code === "invalid_input"
            ? "invalid"
            : result.code === "authentication_invalid"
              ? "authentication"
              : result.code === "permission_denied"
                ? "denied"
                : result.code === "target_changed"
                  ? "target"
                  : result.code === "dependency_unavailable"
                    ? "dependency"
                    : null;
      if (refusal) {
        const refused: Material = {
          ...material,
          outcome: "refused",
          notice: refusal,
          retryAt: refusal === "limited" ? this.backoff(result.retryAfter) : 0,
          result: null,
          hidden: false,
        };
        this.persist(refused);
        try {
          this.dependencies.mark("submitted");
        } catch {
          // The refusal is already durable in its material.
        }
        if (generation !== this.generation) return;
        if (refusal === "authentication") {
          try {
            await this.release(material.selection);
          } catch {
            // A later login remains intact; this page still shows the refusal.
          }
        }
        if (generation !== this.generation) return;
        await this.permit(generation);
        return;
      }
    }
    const notice =
      result.kind === "failure" && result.code === "request_mismatch"
        ? "mismatch"
        : "unknown";
    const unknown: Material = { ...material, notice, hidden: false };
    // Unknown is preserved before the selection read; it can never be retried.
    this.persist(unknown);
    try {
      this.dependencies.mark("submitted");
    } catch {
      // The unknown fact never becomes a new confirmation on reload.
    }
    if (generation !== this.generation) return;
    await this.permit(generation);
  }

  async start() {
    const state = this.state;
    const allowed =
      state.kind === "unsent" ||
      (state.kind === "refused" &&
        state.notice !== "authentication" &&
        state.notice !== "denied" &&
        state.retryAt <= this.dependencies.now());
    if (!allowed) return;
    try {
      this.hide();
      this.dependencies.mark(null);
      remove(this.dependencies.storage);
      this.material = null;
      this.unsaved = false;
      this.unsubmitted = false;
      this.checked = false;
      this.phase = "idle";
      await this.verify();
    } catch {
      this.phase = "storage";
      this.concealed = false;
      this.render();
    }
  }

  close() {
    this.generation++;
    this.request?.abort();
    this.request = undefined;
    this.hideAll();
    this.current.suspend();
    this.unsubscribe();
  }
}
