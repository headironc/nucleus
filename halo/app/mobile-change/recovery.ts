import {
  Current,
  type CurrentBinding,
  type CurrentState,
} from "../authentication/current";
import type { BrowserSession, Selection } from "../authentication/session";
import type { Account, CurrentResult } from "../protocol/session";
import type {
  Completion,
  CompleteInput,
  EstablishInput,
  Message,
  Result,
  SendInput,
  State as TransferState,
  Transfer,
} from "../protocol/mobile";
import {
  LIFETIME,
  expired,
  expireProof,
  messageDeadline,
  read,
  save,
  transferDeadline,
  type Active,
  type Known,
  type Material,
  type Reason,
  type Step,
  type Store,
} from "./material";

type NonReady = Exclude<CurrentState, { kind: "ready" }>;
type Window = "current" | "new";
type FailureResult = Exclude<Result<unknown>, { kind: "ok" }>;

export type QueryStatus =
  | "idle"
  | "pending"
  | "expired"
  | "unavailable"
  | "failed"
  | "limited"
  | "denied"
  | "authentication";

export type State =
  | {
      kind: "current";
      current: NonReady;
      pending: boolean;
      transfer: boolean;
      known: Known | null;
    }
  | { kind: "other"; account: Account }
  | {
      kind: "closed";
      why: "missing" | "expired" | "unknown";
      account: Account;
      known: Known | null;
      transfer: string | null;
      canRestart: boolean;
    }
  | {
      kind: "editing";
      stage: Window;
      account: Account;
      mobile: string;
      notice: Reason | null;
      retryAt: number;
    }
  | {
      kind: "limited";
      stage: Window;
      account: Account;
      retryAt: number;
    }
  | {
      kind: "coding";
      stage: Window;
      account: Account;
      mobile: string;
      delivery: Message["delivery"];
      notice: Reason | null;
      retryAt: number;
    }
  | {
      kind: "confirm";
      account: Account;
      mobile: string;
      notice: Reason | null;
      retryAt: number;
      expiresAt: number | null;
    }
  | {
      kind: "busy";
      action: "send" | "establish" | "complete" | "query";
      stage: Window | null;
    }
  | {
      kind: "unknown";
      step: Step;
      account: Account;
      notice: Reason | null;
      retryAt: number;
      canRetry: boolean;
      canQuery: boolean;
      persisted: boolean;
    }
  | {
      kind: "known";
      account: Account;
      known: Known;
      persisted: boolean;
      queryFailed: boolean;
      retryAt: number;
      showAccount: boolean;
      canRestart: boolean;
    }
  | {
      kind: "query";
      account: Account;
      transfer: string;
      status: QueryStatus;
      retryAt: number;
      expiresAt: string | null;
      canContinue: boolean;
      unknown: boolean;
      canRetry: boolean;
    }
  | { kind: "storage"; pending: boolean }
  | { kind: "reload"; pending: boolean }
  | { kind: "random" };

type Dependencies = {
  storage: Store;
  sessions: {
    read(): Promise<BrowserSession | null>;
    clear(expected: Selection): Promise<boolean>;
  };
  build(): Promise<boolean>;
  current(id: string, signal: AbortSignal): Promise<CurrentResult>;
  sendCurrent(
    operation: string,
    session: string,
    input: SendInput,
    signal: AbortSignal,
  ): Promise<Result<Message>>;
  sendNew(
    operation: string,
    session: string,
    transfer: string,
    input: SendInput,
    signal: AbortSignal,
  ): Promise<Result<Message>>;
  establish(
    operation: string,
    session: string,
    input: EstablishInput,
    signal: AbortSignal,
  ): Promise<Result<Transfer>>;
  complete(
    operation: string,
    session: string,
    transfer: string,
    input: CompleteInput,
    signal: AbortSignal,
  ): Promise<Result<Completion>>;
  read(
    session: string,
    transfer: string,
    signal: AbortSignal,
  ): Promise<Result<TransferState>>;
  now(): number;
  uuid(): string;
  marker(): boolean;
  mark(): void;
  unmark(): void;
  readonly: boolean;
};

const TRANSFER_STEPS = new Set<Step>([
  "transfer",
  "new-send",
  "new-code",
  "complete",
]);
const RETRYABLE = new Set<Reason | null>([null, "limited", "unavailable"]);

export function hasPendingResult(state: State): boolean {
  if (state.kind === "busy" || state.kind === "unknown") return true;
  if (state.kind === "storage" || state.kind === "reload") return state.pending;
  if (state.kind === "current") return state.pending;
  if (state.kind === "query")
    return (
      state.unknown || state.status === "idle" || state.status === "pending"
    );
  return false;
}

/** Any established transfer still governs the leave prompt and queries. */
export function hasTransfer(state: State): boolean {
  return (
    (state.kind === "unknown" && TRANSFER_STEPS.has(state.step)) ||
    (state.kind === "query" && state.status !== "idle") ||
    (state.kind === "confirm" && state.expiresAt !== null) ||
    (state.kind === "editing" && state.stage === "new") ||
    (state.kind === "limited" && state.stage === "new") ||
    (state.kind === "coding" && state.stage === "new")
  );
}

type Query = Readonly<{
  status: QueryStatus;
  retryAt: number;
  expiresAt: string | null;
  unknown: boolean;
}>;

/** One tab's self mobile change bound to a trusted account and selection. */
export class MobileChange {
  private readonly current: Current;
  private readonly listeners = new Set<() => void>();
  private state: State = {
    kind: "current",
    current: { kind: "checking" },
    pending: false,
    transfer: false,
    known: null,
  };
  private material: Material | null = null;
  private unsaved = false;
  private persistedKnown = true;
  private readonly unstoredKnown = new Map<string, Known>();
  private generation = 0;
  private pendingRequest?: AbortController;
  private reloadNeeded = false;
  private phase:
    | "idle"
    | "sending"
    | "establishing"
    | "completing"
    | "querying"
    | "storage"
    | "random" = "idle";
  private phaseStage: Window | null = null;
  private storagePending = false;
  private queryResult: Query | null = null;
  private flash: Reason | null = null;
  private readonly drafts = new Map<Window, string>();
  private sentBinding?: CurrentBinding;
  private confirmedKnown: Known | null = null;
  private lastAccount: Account | null = null;

  constructor(private readonly dependencies: Dependencies) {
    this.current = new Current(dependencies);
    this.current.subscribe(this.changed);
    this.render();
  }

  snapshot = (): State => this.state;
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };
  private changed = () => this.render();
  private show(state: State) {
    this.state = state;
    this.listeners.forEach((listener) => listener());
  }

  close() {
    this.generation++;
    this.stopRequest();
    this.current.suspend();
    if (!this.unsaved) this.material = null;
    this.queryResult = null;
    this.sentBinding = undefined;
    this.confirmedKnown = null;
    this.lastAccount = null;
    this.show({
      kind: "current",
      current: { kind: "checking" },
      pending: false,
      transfer: false,
      known: null,
    });
  }

  suspend() {
    this.generation++;
    this.confirmedKnown = null;
    this.lastAccount = null;
    this.stopRequest();
    if (this.phase !== "storage") this.phase = "idle";
    this.phaseStage = null;
    this.current.suspend();
    this.render();
  }

  reload() {
    this.reloadNeeded = true;
    this.generation++;
    this.stopRequest();
    this.phase = "idle";
    this.current.suspend();
    this.show({ kind: "reload", pending: this.pending() });
  }

  private pending() {
    return (
      !!this.material?.active?.attempted || (this.queryResult?.unknown ?? false)
    );
  }

  private belongs(
    account: Account,
    value: { account: string; person: string },
  ) {
    return value.account === account.id && value.person === account.personId;
  }

  private knownFor(account: Account): Known | null {
    return (
      this.material?.known.findLast((result) =>
        this.belongs(account, result),
      ) ?? null
    );
  }

  private backoff(milliseconds = 0) {
    return Math.min(
      Number.MAX_SAFE_INTEGER,
      this.dependencies.now() + Math.max(5_000, milliseconds),
    );
  }

  private stopRequest() {
    const active = this.material?.active;
    if (
      this.phase === "sending" &&
      active &&
      (active.step === "current-send" || active.step === "new-send")
    ) {
      const retryAt = Math.max(active.retryAt, this.backoff());
      try {
        const saved = read(this.dependencies.storage);
        if (
          saved &&
          saved !== "missing" &&
          saved.active &&
          saved.active.operation === active.operation &&
          saved.active.step === active.step &&
          saved.active.account === active.account &&
          saved.active.person === active.person &&
          this.sameSelection(saved.active.selection, active)
        ) {
          if (
            !this.store({
              ...saved,
              active: {
                ...saved.active,
                retryAt: Math.max(saved.active.retryAt, retryAt),
              },
            })
          ) {
            this.phase = "storage";
            this.storagePending = active.attempted;
          }
        }
      } catch {
        this.material = { ...this.material!, active: { ...active, retryAt } };
        this.unsaved = true;
        this.phase = "storage";
        this.storagePending = active.attempted;
      }
    }
    this.pendingRequest?.abort();
    this.pendingRequest = undefined;
  }

  private store(value: Material): boolean {
    this.material = value;
    try {
      save(this.dependencies.storage, value);
      this.unsaved = false;
      return true;
    } catch {
      this.unsaved = true;
      return false;
    }
  }

  private remove() {
    this.material = null;
    try {
      this.dependencies.storage.removeItem("halo.mobile-change");
      this.unsaved = false;
    } catch {
      this.unsaved = true;
    }
  }

  /** Expiry closes secret use; it never turns an unknown write into a new one. */
  private normalize(value: Material, now: number): Material | null {
    const active = value.active;
    if (!active || !expired(active, now)) return value;
    if (active.transfer) {
      const next = expireProof(active, now);
      return { version: 1, known: value.known, active: next, closed: null };
    }
    if (active.attempted)
      return {
        version: 1,
        known: value.known,
        active: null,
        closed: "unknown",
      };
    return value.known.length
      ? { version: 1, known: value.known, active: null, closed: null }
      : null;
  }

  expire() {
    if (!this.material) return;
    const next = this.normalize(this.material, this.dependencies.now());
    if (next === this.material) return;
    if (next) this.store(next);
    else this.remove();
    this.render();
  }

  async verify() {
    this.generation++;
    this.confirmedKnown = null;
    this.lastAccount = null;
    this.stopRequest();
    this.phase = "idle";
    this.phaseStage = null;
    this.queryResult = null;
    this.current.suspend();
    if (this.reloadNeeded) return;
    try {
      let material: Material | null;
      if (this.unstoredKnown.size) {
        for (const known of [...this.unstoredKnown.values()])
          this.remember(known);
        material = this.material;
      } else if (this.unsaved) {
        material = this.material;
      } else {
        const saved = read(this.dependencies.storage);
        const damaged = saved === "missing";
        const marked = saved === null && this.dependencies.marker();
        material =
          damaged || marked
            ? { version: 1, known: [], active: null, closed: "unknown" }
            : saved;
        this.material = material;
        if (material && (damaged || marked)) this.store(material);
      }
      if (material?.active || material?.closed) {
        this.dependencies.mark();
        if (!this.dependencies.marker())
          throw new Error("mobile_change_marker_unavailable");
      }
      if (material && this.unstoredKnown.size === 0) {
        const next = this.normalize(material, this.dependencies.now());
        if (next !== material) {
          if (next) this.store(next);
          else this.remove();
          material = next;
        }
      }
      // A save that failed earlier is retried before any new write.
      if (this.unsaved && material && this.unstoredKnown.size === 0)
        this.store(material);
    } catch {
      this.phase = "storage";
      this.storagePending = this.pending();
      this.render();
      return;
    }
    await this.current.verify();
    this.render();
  }

  private binding(): CurrentBinding | undefined {
    return this.current.binding();
  }

  private selection(): Selection | null {
    const state = this.current.snapshot();
    return state.kind === "ready" ? state.selection : null;
  }

  private account(): Account | null {
    const state = this.current.snapshot();
    return state.kind === "ready" ? state.account : null;
  }

  private sameSelection(selection: Selection, active: Active) {
    return (
      selection.id === active.selection.id &&
      selection.revision === active.selection.revision
    );
  }

  private operation(stage: Window): string {
    const active = this.material?.active;
    const reusable =
      stage === "current"
        ? active?.step === "current-send" || active?.step === "establish"
        : active?.step === "new-send" || active?.step === "complete";
    if (active && reusable && active.operation) return active.operation;
    return this.draft(stage);
  }

  private draft(stage: Window): string {
    const existing = this.drafts.get(stage);
    if (existing) return existing;
    const operation = this.dependencies.uuid();
    this.drafts.set(stage, operation);
    return operation;
  }

  private fresh(stage: Window): string {
    this.drafts.delete(stage);
    const operation = this.dependencies.uuid();
    this.drafts.set(stage, operation);
    return operation;
  }

  currentOperation(): string {
    return this.operation("current");
  }

  newOperation(): string {
    return this.operation("new");
  }

  private render() {
    this.show(this.compute());
  }

  private compute(): State {
    const pending = this.pending();
    if (this.reloadNeeded || this.current.snapshot().kind === "reload")
      return { kind: "reload", pending };
    if (this.phase === "storage")
      return { kind: "storage", pending: this.storagePending };
    if (this.phase === "random") return { kind: "random" };
    if (this.phase !== "idle")
      return {
        kind: "busy",
        action:
          this.phase === "sending"
            ? "send"
            : this.phase === "establishing"
              ? "establish"
              : this.phase === "completing"
                ? "complete"
                : "query",
        stage: this.phaseStage,
      };
    const current = this.current.snapshot();
    if (current.kind !== "ready") {
      if (
        this.confirmedKnown &&
        this.lastAccount &&
        (current.kind === "anonymous" ||
          current.kind === "expired" ||
          current.kind === "checking")
      )
        return {
          kind: "known",
          account: this.lastAccount,
          known: this.confirmedKnown,
          persisted: this.persistedKnown,
          queryFailed: false,
          retryAt: 0,
          showAccount: false,
          canRestart: false,
        };
      return {
        kind: "current",
        current,
        pending,
        transfer: !!this.material?.active?.transfer,
        known: this.material?.known[0] ?? null,
      };
    }
    const account = current.account;
    const material = this.material;
    if (material) {
      const active = material.active;
      if (active && !this.belongs(account, active))
        return { kind: "other", account };
      if (this.queryResult) return this.queryState(account);
      if (active) return this.flowState(account, current.selection, active);
      if (material.closed)
        return {
          kind: "closed",
          why: material.closed,
          account,
          known: null,
          transfer: null,
          canRestart: false,
        };
      const known = this.knownFor(account);
      if (known)
        return {
          kind: "known",
          account,
          known,
          persisted: this.persistedKnown,
          queryFailed: false,
          retryAt: 0,
          showAccount: true,
          canRestart: !this.dependencies.readonly,
        };
    }
    if (this.dependencies.readonly)
      return {
        kind: "closed",
        why: "missing",
        account,
        known: null,
        transfer: null,
        canRestart: false,
      };
    return {
      kind: "editing",
      stage: "current",
      account,
      mobile: "",
      notice: this.flash,
      retryAt: 0,
    };
  }

  private flowState(
    account: Account,
    selection: Selection,
    active: Active,
  ): State {
    const known =
      this.material?.known.find(
        (result) =>
          result.transfer === active.transfer && this.belongs(account, result),
      ) ?? null;
    if (this.dependencies.readonly || !this.sameSelection(selection, active)) {
      if (!active.transfer)
        // No transfer locator exists, so this session cannot read a result and
        // must not present a query action it cannot perform.
        return {
          kind: "closed",
          why: active.attempted ? "unknown" : "missing",
          account,
          known,
          transfer: null,
          canRestart: false,
        };
      return {
        kind: "query",
        account,
        transfer: active.transfer,
        status: "idle",
        retryAt: active.retryAt,
        expiresAt: null,
        canContinue: false,
        unknown: active.attempted,
        canRetry: false,
      };
    }
    const notice = active.reason ?? this.flash;
    if (
      !active.attempted &&
      active.reason === "limited" &&
      (active.step === "current-send" || active.step === "new-send")
    )
      return {
        kind: "limited",
        stage: active.step === "current-send" ? "current" : "new",
        account,
        retryAt: active.retryAt,
      };
    switch (active.step) {
      case "current-send":
        if (active.attempted) return this.unknownState(account, active);
        return {
          kind: "editing",
          stage: "current",
          account,
          mobile: active.currentMobile ?? "",
          notice,
          retryAt: active.retryAt,
        };
      case "current-code":
        return {
          kind: "coding",
          stage: "current",
          account,
          mobile: active.currentMobile ?? "",
          delivery: active.delivery ?? "unknown",
          notice: null,
          retryAt: active.retryAt,
        };
      case "establish":
        if (active.attempted) return this.unknownState(account, active);
        if (
          active.reason === "verification_required" ||
          active.reason === "expired"
        )
          return {
            kind: "closed",
            why: "expired",
            account,
            known,
            transfer: active.transfer ?? null,
            canRestart: !active.attempted,
          };
        return {
          kind: "coding",
          stage: "current",
          account,
          mobile: active.currentMobile ?? "",
          delivery: active.delivery ?? "unknown",
          notice,
          retryAt: active.retryAt,
        };
      case "transfer":
        if (
          active.reason === "verification_required" ||
          active.reason === "expired"
        )
          return {
            kind: "closed",
            why: "expired",
            account,
            known,
            transfer: active.transfer ?? null,
            canRestart: !active.attempted,
          };
        if (active.reason === "unavailable" || active.reason === "denied")
          return {
            kind: "closed",
            why: "missing",
            account,
            known,
            transfer: active.transfer ?? null,
            canRestart: false,
          };
        return {
          kind: "editing",
          stage: "new",
          account,
          mobile: active.newMobile ?? "",
          notice,
          retryAt: active.retryAt,
        };
      case "new-send":
        if (active.attempted) return this.unknownState(account, active);
        return {
          kind: "editing",
          stage: "new",
          account,
          mobile: active.newMobile ?? "",
          notice,
          retryAt: active.retryAt,
        };
      case "new-code":
        return {
          kind: "coding",
          stage: "new",
          account,
          mobile: active.newMobile ?? "",
          delivery: active.delivery ?? "unknown",
          notice,
          retryAt: active.retryAt,
        };
      case "complete":
        if (active.attempted) return this.unknownState(account, active);
        return {
          kind: "confirm",
          account,
          mobile: active.newMobile ?? "",
          notice,
          retryAt: active.retryAt,
          expiresAt: active.transferExpiresAt ?? null,
        };
    }
  }

  private queryState(account: Account): State {
    const query = this.queryResult!;
    const active = this.material?.active ?? null;
    const known = active
      ? this.material?.known.find(
          (result) =>
            result.transfer === active.transfer &&
            this.belongs(account, result),
        )
      : this.knownFor(account);
    if (known)
      return {
        kind: "known",
        account,
        known,
        persisted: this.persistedKnown,
        queryFailed:
          query.status === "unavailable" ||
          query.status === "failed" ||
          query.status === "limited",
        retryAt: query.retryAt,
        showAccount: true,
        canRestart: !this.dependencies.readonly && !active?.attempted,
      };
    if (query.status === "denied") return { kind: "other", account };
    const selection = this.selection();
    const transfer = active?.transfer ?? "";
    const canContinue =
      query.status === "pending" &&
      !query.unknown &&
      !!selection &&
      !!active &&
      !this.dependencies.readonly &&
      !expired(active, this.dependencies.now()) &&
      this.sameSelection(selection, active) &&
      (active.step === "transfer" || active.step === "new-code");
    return {
      kind: "query",
      account,
      transfer,
      status: query.status,
      retryAt: query.retryAt,
      expiresAt: query.expiresAt,
      canContinue,
      unknown: query.unknown,
      canRetry:
        !!active &&
        !!selection &&
        !this.dependencies.readonly &&
        this.sameSelection(selection, active) &&
        !expired(active, this.dependencies.now()) &&
        active.attempted &&
        RETRYABLE.has(active.reason ?? null),
    };
  }

  private unknownState(account: Account, active: Active): State {
    return {
      kind: "unknown",
      step: active.step,
      account,
      notice: active.reason ?? null,
      retryAt: active.retryAt,
      canRetry: RETRYABLE.has(active.reason ?? null),
      canQuery: !!active.transfer,
      persisted: !this.unsaved,
    };
  }

  private record(): Material | null {
    return this.material;
  }

  private handoff(next: Material): boolean {
    if (this.store(next)) return true;
    this.phase = "storage";
    this.storagePending = true;
    this.render();
    return false;
  }

  private nextBase(
    active: Active,
    step: Step,
    createdAt = active.createdAt,
    expiresAt = active.expiresAt,
  ): Active {
    return {
      step,
      account: active.account,
      person: active.person,
      selection: active.selection,
      createdAt,
      expiresAt,
      retryAt: 0,
      attempted: false,
    };
  }

  private async prepare(active: Active): Promise<boolean> {
    if (expired(active, this.dependencies.now())) {
      this.expire();
      return false;
    }
    const binding = this.binding();
    if (!binding) return false;
    if (active.step === "current-send" || active.step === "new-send") {
      if (
        !this.store({
          version: 1,
          known: this.record()?.known ?? [],
          active: { ...active, attempted: false },
          closed: null,
        })
      ) {
        this.phase = "storage";
        this.storagePending = false;
        this.render();
        return false;
      }
      if (!(await this.checkBuild(active))) return false;
    }
    try {
      this.dependencies.mark();
      if (!this.dependencies.marker())
        throw new Error("mobile_change_marker_unavailable");
    } catch {
      this.phase = "random";
      this.render();
      return false;
    }
    const known = this.record()?.known ?? [];
    const next: Material = { version: 1, known, active, closed: null };
    if (!this.handoff(next)) {
      const memory = this.record();
      if (memory?.active)
        this.material = {
          version: 1,
          known: memory.known,
          active: { ...memory.active, attempted: false },
          closed: null,
        };
      this.phase = "storage";
      this.storagePending = false;
      try {
        this.dependencies.unmark();
      } catch {
        // A stale marker only causes a conservative unknown on a lost record.
      }
      this.render();
      return false;
    }
    try {
      if (
        !this.current.matches(binding) ||
        !(await this.current.confirm(binding))
      ) {
        // The request was never sent; it must not become an unknown outcome.
        this.store({
          version: 1,
          known,
          active: { ...active, attempted: false },
          closed: null,
        });
        try {
          this.dependencies.unmark();
        } catch {
          // A stored unsubmitted record does not need the marker.
        }
        this.render();
        return false;
      }
      this.sentBinding = binding;
    } catch {
      this.phase = "storage";
      this.storagePending = true;
      this.render();
      return false;
    }
    return true;
  }

  private async request<T>(
    action: (signal: AbortSignal) => Promise<Result<T>>,
    on: (result: Result<T>, generation: number) => Promise<void> | void,
  ) {
    const generation = ++this.generation;
    const binding = this.binding();
    const controller = new AbortController();
    this.pendingRequest = controller;
    const timer = setTimeout(() => controller.abort(), 35_000);
    try {
      const result = await action(controller.signal);
      if (
        generation === this.generation &&
        (!binding || !(await this.current.confirm(binding)))
      )
        await this.verify();
      await on(result, generation);
    } catch {
      if (generation === this.generation) {
        this.phase = "storage";
        this.storagePending = this.pending();
      }
    } finally {
      clearTimeout(timer);
      if (this.pendingRequest === controller) this.pendingRequest = undefined;
      if (generation === this.generation) {
        if (this.phase !== "storage" && this.phase !== "random") {
          this.phase = "idle";
          this.phaseStage = null;
        }
        this.render();
      }
    }
  }

  private begin(
    action: "sending" | "establishing" | "completing",
    stage: Window | null,
  ) {
    this.phase = action;
    this.phaseStage = stage;
    this.flash = null;
    this.render();
  }

  private setActive(active: Active) {
    const known = this.record()?.known ?? [];
    if (!this.store({ version: 1, known, active, closed: null })) {
      this.phase = "storage";
      this.storagePending = active.attempted;
    }
  }

  async submitCurrent(mobile: string, token: string) {
    const view = this.compute();
    if (
      (view.kind !== "editing" && view.kind !== "limited") ||
      view.stage !== "current" ||
      view.retryAt > this.dependencies.now()
    )
      return;
    const account = this.account();
    const selection = this.selection();
    if (!account || !selection) return;
    const record = this.record();
    const previous = record?.active;
    if (
      view.kind === "limited" &&
      (previous?.currentMobile !== mobile || previous?.token !== token)
    )
      return;
    const retrying = previous?.step === "current-send" && !previous.attempted;
    const createdAt = retrying ? previous.createdAt : this.dependencies.now();
    const expiresAt = retrying
      ? previous.expiresAt
      : Math.min(Number.MAX_SAFE_INTEGER, createdAt + LIFETIME);
    const active: Active = {
      step: "current-send",
      account: account.id,
      person: account.personId,
      selection,
      createdAt,
      expiresAt,
      retryAt: 0,
      attempted: true,
      operation: this.operation("current"),
      currentMobile: mobile,
      token,
      ...(view.kind === "limited" ? { reason: "limited" as const } : {}),
    };
    if (!(await this.prepare(active))) return;
    this.begin("sending", "current");
    await this.request(
      (signal) =>
        this.dependencies.sendCurrent(
          active.operation!,
          selection.id,
          { mobile, token },
          signal,
        ),
      async (result, generation) => {
        if (generation !== this.generation) return;
        if (result.kind === "ok")
          this.messageHandoff(active, result.value, "current");
        else await this.failedWrite(result, active, "send", false);
      },
    );
  }

  async submitCurrentCode(code: string) {
    const view = this.compute();
    if (
      view.kind !== "coding" ||
      view.stage !== "current" ||
      view.retryAt > this.dependencies.now()
    )
      return;
    const account = this.account();
    const record = this.record();
    const active = record?.active;
    if (!account || !active || !active.messageId) return;
    const next: Active = {
      ...this.nextBase(active, "establish"),
      attempted: true,
      operation: this.operation("current"),
      ...(active.currentMobile ? { currentMobile: active.currentMobile } : {}),
      messageId: active.messageId,
      ...(active.messageExpiresAt !== undefined
        ? { messageExpiresAt: active.messageExpiresAt }
        : {}),
      ...(active.delivery ? { delivery: active.delivery } : {}),
      code,
    };
    if (!(await this.prepare(next))) return;
    this.begin("establishing", "current");
    await this.request(
      (signal) =>
        this.dependencies.establish(
          next.operation!,
          next.selection.id,
          { messageId: next.messageId!, code },
          signal,
        ),
      async (result, generation) => {
        if (generation !== this.generation) return;
        if (result.kind === "ok") this.transferHandoff(next, result.value);
        else await this.failedWrite(result, next, "establish", false);
      },
    );
  }

  async submitNew(mobile: string, token: string) {
    const view = this.compute();
    if (
      (view.kind !== "editing" && view.kind !== "limited") ||
      view.stage !== "new" ||
      view.retryAt > this.dependencies.now()
    )
      return;
    const account = this.account();
    const active = this.record()?.active;
    if (!account || !active || !active.transfer) return;
    if (
      view.kind === "limited" &&
      (active.newMobile !== mobile || active.token !== token)
    )
      return;
    if (active.currentMobile && active.currentMobile === mobile) {
      this.flash = "state_conflict";
      this.render();
      return;
    }
    const next: Active = {
      ...this.nextBase(active, "new-send"),
      attempted: true,
      operation: this.operation("new"),
      transfer: active.transfer,
      ...(active.transferExpiresAt !== undefined
        ? { transferExpiresAt: active.transferExpiresAt }
        : {}),
      ...(active.currentMobile ? { currentMobile: active.currentMobile } : {}),
      newMobile: mobile,
      token,
      ...(view.kind === "limited" ? { reason: "limited" as const } : {}),
    };
    if (!(await this.prepare(next))) return;
    this.begin("sending", "new");
    await this.request(
      (signal) =>
        this.dependencies.sendNew(
          next.operation!,
          next.selection.id,
          next.transfer!,
          { mobile, token },
          signal,
        ),
      async (result, generation) => {
        if (generation !== this.generation) return;
        if (result.kind === "ok")
          this.messageHandoff(next, result.value, "new");
        else await this.failedWrite(result, next, "send", false);
      },
    );
  }

  async submitNewCode(code: string) {
    const view = this.compute();
    if (
      view.kind !== "coding" ||
      view.stage !== "new" ||
      view.retryAt > this.dependencies.now()
    )
      return;
    const record = this.record();
    const active = record?.active;
    if (
      !record ||
      !active ||
      !active.messageId ||
      !active.newMobile ||
      !active.transfer
    )
      return;
    const next: Active = {
      ...this.nextBase(active, "complete"),
      ...(active.transfer ? { transfer: active.transfer } : {}),
      ...(active.transferExpiresAt !== undefined
        ? { transferExpiresAt: active.transferExpiresAt }
        : {}),
      ...(active.currentMobile ? { currentMobile: active.currentMobile } : {}),
      newMobile: active.newMobile,
      messageId: active.messageId,
      ...(active.messageExpiresAt !== undefined
        ? { messageExpiresAt: active.messageExpiresAt }
        : {}),
      code,
      operation: this.fresh("new"),
    };
    if (
      !this.store({
        version: 1,
        known: record.known,
        active: next,
        closed: null,
      })
    ) {
      this.phase = "storage";
      this.storagePending = false;
      this.render();
      return;
    }
    this.flash = null;
    this.render();
  }

  async confirm() {
    const view = this.compute();
    if (view.kind !== "confirm" || view.retryAt > this.dependencies.now())
      return;
    const active = this.record()?.active;
    if (
      !active ||
      active.step !== "complete" ||
      !active.messageId ||
      !active.newMobile ||
      !active.transfer ||
      !active.code
    )
      return;
    const next: Active = {
      ...active,
      operation: this.operation("new"),
      attempted: true,
    };
    if (!(await this.prepare(next))) return;
    this.begin("completing", "new");
    await this.request(
      (signal) =>
        this.dependencies.complete(
          next.operation!,
          next.selection.id,
          next.transfer!,
          {
            mobile: next.newMobile!,
            messageId: next.messageId!,
            code: next.code!,
          },
          signal,
        ),
      async (result, generation) => {
        if (result.kind === "ok")
          await this.completed(next, result.value, generation);
        else if (generation === this.generation)
          await this.failedWrite(result, next, "complete", false);
      },
    );
  }

  private messageHandoff(previous: Active, message: Message, stage: Window) {
    const record = this.record();
    if (!record) return;
    const deadline = messageDeadline(previous.createdAt, message.expiresAt);
    const next: Active = {
      ...this.nextBase(
        previous,
        stage === "current" ? "current-code" : "new-code",
        previous.createdAt,
        Math.min(previous.expiresAt, deadline),
      ),
      operation: previous.operation,
      ...(stage === "new" && previous.transfer
        ? { transfer: previous.transfer }
        : {}),
      ...(stage === "new" && previous.transferExpiresAt !== undefined
        ? { transferExpiresAt: previous.transferExpiresAt }
        : {}),
      ...(previous.currentMobile
        ? { currentMobile: previous.currentMobile }
        : {}),
      ...(stage === "new" && previous.newMobile
        ? { newMobile: previous.newMobile }
        : {}),
      messageId: message.id,
      messageExpiresAt: deadline,
      delivery: message.delivery,
    };
    this.flash = null;
    this.drafts.delete(stage);
    this.handoff({
      version: 1,
      known: record.known,
      active: next,
      closed: null,
    });
  }

  private transferHandoff(previous: Active, transfer: Transfer) {
    const record = this.record();
    if (!record) return;
    const createdAt = this.dependencies.now();
    const deadline = transferDeadline(createdAt, transfer.expiresAt);
    const next: Active = {
      ...this.nextBase(previous, "transfer", createdAt, deadline),
      transfer: transfer.id,
      transferExpiresAt: deadline,
    };
    this.flash = null;
    this.drafts.delete("current");
    this.handoff({
      version: 1,
      known: record.known,
      active: next,
      closed: null,
    });
  }

  private async completed(
    previous: Active,
    completion: Completion,
    generation: number,
  ) {
    const visible = generation === this.generation;
    const known: Known = {
      account: previous.account,
      person: previous.person,
      transfer: completion.id,
      completedAt: completion.completedAt,
    };
    this.remember(known);
    if (visible) {
      this.flash = null;
      this.queryResult = null;
      this.drafts.delete("current");
      this.drafts.delete("new");
      const current = this.current.snapshot();
      if (current.kind === "ready" && this.belongs(current.account, previous))
        this.lastAccount = current.account;
      this.confirmedKnown = known;
    }
    try {
      const cleared = await this.dependencies.sessions.clear(
        previous.selection,
      );
      if (generation === this.generation) {
        if (cleared) {
          this.current.suspend();
          void this.current.verify();
        } else await this.verify();
      }
    } catch {
      // Retain the completion, but recheck identity when cleanup is uncertain.
      if (generation === this.generation) await this.verify();
    }
    if (generation === this.generation) this.render();
  }

  private remember(known: Known) {
    // A late response may outlive this page. Merge with the current stored
    // record so it cannot erase a later page's transfer or completed facts.
    const saved = read(this.dependencies.storage);
    const record = saved === "missing" ? this.record() : saved;
    const list = [
      ...(record?.known ?? []).filter(
        (item) => item.transfer !== known.transfer,
      ),
      known,
    ];
    const active = record?.active;
    const ownsActive =
      active &&
      active.transfer === known.transfer &&
      active.account === known.account &&
      active.person === known.person;
    const next: Material = {
      version: 1,
      known: list,
      active: ownsActive ? null : (record?.active ?? null),
      closed: ownsActive ? null : (record?.closed ?? null),
    };
    this.material = next;
    try {
      if (saved === "missing")
        throw new Error("mobile_change_storage_unavailable");
      save(this.dependencies.storage, next);
      this.unstoredKnown.delete(known.transfer);
      this.persistedKnown = true;
      this.unsaved = false;
    } catch {
      this.unstoredKnown.set(known.transfer, known);
      this.persistedKnown = false;
      this.unsaved = true;
    }
  }

  private async failedWrite(
    result: FailureResult,
    active: Active,
    action: "send" | "establish" | "complete",
    replay: boolean,
  ) {
    if (result.kind === "untrusted") {
      this.setActive({
        ...active,
        attempted: true,
        reason: "unavailable",
        retryAt: Math.max(active.retryAt, this.backoff()),
      });
      this.reload();
      return;
    }
    if (result.kind === "failure" && result.status === 401) {
      const binding = this.sentBinding;
      this.setActive({
        ...active,
        attempted: true,
        reason: "denied",
        retryAt: 0,
      });
      // Persist before yielding so a later page's material cannot be overwritten.
      // C1 also uses 401 for an invalid token proof; verify Session authority separately.
      if (action === "send") await this.current.verify();
      else if (binding) await this.current.reject(binding, 401);
      this.render();
      return;
    }
    if (
      result.kind === "failure" &&
      result.status === 403 &&
      result.code === "permission_denied"
    ) {
      const binding = this.sentBinding;
      this.setActive({
        ...active,
        attempted: true,
        reason: "denied",
        retryAt: 0,
      });
      if (binding) await this.current.reject(binding, 403);
      this.render();
      return;
    }
    if (replay) {
      // A rejection of a recovery check cannot convert an unknown original
      // write into a definite failure.
      const limited = result.kind === "failure" && result.status === 429;
      const mismatch =
        result.kind === "failure" && result.code === "request_mismatch";
      const refused =
        result.kind === "failure" && result.status < 500 && !limited;
      const reason: Reason = mismatch
        ? "mismatch"
        : limited
          ? "limited"
          : refused
            ? "refused"
            : "unavailable";
      this.setActive({
        ...active,
        attempted: true,
        reason,
        retryAt:
          limited || !refused
            ? this.backoff(result.kind === "failure" ? result.retryAfter : 0)
            : 0,
      });
      this.render();
      return;
    }
    if (result.kind === "failure" && result.status === 429) {
      this.setActive({
        ...active,
        attempted: false,
        reason: "limited",
        retryAt: this.backoff(result.retryAfter),
      });
      this.render();
      return;
    }
    if (result.kind === "failure" && result.status < 500) {
      this.definiteFailure(result, active, action);
      this.render();
      return;
    }
    this.setActive({
      ...active,
      attempted: true,
      reason: "unavailable",
      retryAt: this.backoff(),
    });
    this.render();
  }

  private definiteFailure(
    result: Extract<FailureResult, { kind: "failure" }>,
    active: Active,
    action: "send" | "establish" | "complete",
  ) {
    const code = result.code;
    const base = this.nextBase(active, active.step);
    const carried = {
      ...(active.transfer ? { transfer: active.transfer } : {}),
      ...(active.transferExpiresAt !== undefined
        ? { transferExpiresAt: active.transferExpiresAt }
        : {}),
      ...(active.currentMobile ? { currentMobile: active.currentMobile } : {}),
    };
    if (action === "send") {
      // The request was not accepted; it never became an unknown write.
      if (active.step === "current-send") {
        this.flash =
          code === "verification_invalid"
            ? "challenge"
            : code === "verification_required"
              ? "expired"
              : code === "request_mismatch"
                ? "mismatch"
                : "invalid";
        this.drafts.delete("current");
        this.setActive({
          ...base,
          ...(active.currentMobile
            ? { currentMobile: active.currentMobile }
            : {}),
        });
        return;
      }
      if (code === "request_mismatch") {
        this.setActive({
          ...active,
          attempted: true,
          reason: "mismatch",
          retryAt: 0,
        });
        return;
      }
      if (code === "state_conflict") {
        this.setActive({
          ...base,
          step: "transfer",
          ...carried,
          reason: "state_conflict",
        });
        return;
      }
      if (code === "verification_required") {
        this.setActive({
          ...base,
          step: "transfer",
          ...carried,
          reason: "verification_required",
        });
        return;
      }
      if (result.status === 404) {
        this.setActive({
          ...base,
          step: "transfer",
          ...carried,
          reason: "unavailable",
        });
        return;
      }
      this.flash =
        code === "verification_invalid"
          ? "challenge"
          : code === "invalid_input"
            ? "invalid"
            : "refused";
      this.drafts.delete("new");
      this.setActive({
        ...base,
        ...carried,
        ...(active.newMobile ? { newMobile: active.newMobile } : {}),
      });
      return;
    }
    if (action === "establish") {
      if (code === "request_mismatch") {
        this.setActive({
          ...active,
          attempted: true,
          reason: "mismatch",
          retryAt: 0,
        });
        return;
      }
      if (code === "verification_required") {
        this.setActive({
          ...base,
          messageId: active.messageId,
          reason: "verification_required",
        });
        return;
      }
      if (code === "verification_invalid") this.flash = "wrong_code";
      else if (code === "invalid_input") this.flash = "invalid";
      else this.flash = "refused";
      this.drafts.delete("current");
      this.setActive({
        ...base,
        ...(active.currentMobile
          ? { currentMobile: active.currentMobile }
          : {}),
        messageId: active.messageId,
        ...(active.messageExpiresAt !== undefined
          ? { messageExpiresAt: active.messageExpiresAt }
          : {}),
        ...(active.delivery ? { delivery: active.delivery } : {}),
      });
      return;
    }
    if (code === "request_mismatch") {
      this.setActive({
        ...active,
        attempted: true,
        reason: "mismatch",
        retryAt: 0,
      });
      return;
    }
    if (code === "mobile_taken" || code === "state_conflict") {
      this.setActive({
        ...base,
        step: "transfer",
        expiresAt: active.transferExpiresAt ?? active.expiresAt,
        ...carried,
        reason: code,
      });
      return;
    }
    if (code === "verification_required") {
      this.setActive({
        ...base,
        step: "transfer",
        ...carried,
        reason: "verification_required",
      });
      return;
    }
    if (result.status === 404) {
      this.setActive({
        ...base,
        step: "transfer",
        ...carried,
        reason: "unavailable",
      });
      return;
    }
    this.drafts.delete("new");
    this.flash =
      code === "verification_invalid"
        ? "wrong_code"
        : code === "invalid_input"
          ? "invalid"
          : "refused";
    this.setActive({
      ...base,
      step: "new-code",
      ...carried,
      ...(active.newMobile ? { newMobile: active.newMobile } : {}),
      messageId: active.messageId,
      ...(active.messageExpiresAt !== undefined
        ? { messageExpiresAt: active.messageExpiresAt }
        : {}),
      ...(active.delivery ? { delivery: active.delivery } : {}),
    });
  }

  private async checkBuild(active: Active): Promise<boolean> {
    const generation = this.generation;
    this.begin("sending", active.step === "current-send" ? "current" : "new");
    const compatible = await this.dependencies.build().catch(() => false);
    if (generation !== this.generation) return false;
    this.phase = "idle";
    if (!compatible) {
      const record = this.record()?.active;
      if (record) this.setActive({ ...record, retryAt: this.backoff() });
      this.reload();
      return false;
    }
    if (expired(active, this.dependencies.now())) {
      this.expire();
      return false;
    }
    return true;
  }

  private async ready(): Promise<boolean> {
    const binding = this.binding();
    if (!binding) return false;
    try {
      const ready = await this.current.confirm(binding);
      if (ready) this.sentBinding = binding;
      return ready;
    } catch {
      this.phase = "storage";
      this.storagePending = true;
      return false;
    }
  }

  /** An explicit replay of the exact original write, or the first retry. */
  async retry() {
    const active = this.record()?.active;
    if (!active) return;
    const selection = this.selection();
    const account = this.account();
    if (
      this.phase !== "idle" ||
      this.dependencies.readonly ||
      !selection ||
      !account ||
      !this.belongs(account, active) ||
      !this.sameSelection(selection, active) ||
      expired(active, this.dependencies.now()) ||
      !RETRYABLE.has(active.reason ?? null)
    )
      return;
    if (active.retryAt > this.dependencies.now()) return;
    if (!active.attempted) {
      switch (active.step) {
        case "current-send":
          if (active.currentMobile && active.token)
            await this.submitCurrent(active.currentMobile, active.token);
          return;
        case "establish":
          if (active.code) await this.submitCurrentCode(active.code);
          return;
        case "new-send":
          if (active.newMobile && active.token)
            await this.submitNew(active.newMobile, active.token);
          return;
        case "complete":
          await this.confirm();
          return;
        default:
          return;
      }
    }
    if (
      (active.step === "current-send" || active.step === "new-send") &&
      !(await this.checkBuild(active))
    )
      return;
    if (!(await this.ready())) return;
    if (active.step === "current-send" || active.step === "new-send") {
      if (!active.operation || !active.token) return;
      if (active.step === "current-send") {
        if (!active.currentMobile) return;
        this.begin("sending", "current");
        await this.request(
          (signal) =>
            this.dependencies.sendCurrent(
              active.operation!,
              active.selection.id,
              { mobile: active.currentMobile!, token: active.token! },
              signal,
            ),
          async (result, generation) => {
            if (generation !== this.generation) return;
            if (result.kind === "ok")
              this.messageHandoff(active, result.value, "current");
            else await this.failedWrite(result, active, "send", true);
          },
        );
      } else {
        if (!active.newMobile || !active.transfer) return;
        this.begin("sending", "new");
        await this.request(
          (signal) =>
            this.dependencies.sendNew(
              active.operation!,
              active.selection.id,
              active.transfer!,
              { mobile: active.newMobile!, token: active.token! },
              signal,
            ),
          async (result, generation) => {
            if (generation !== this.generation) return;
            if (result.kind === "ok")
              this.messageHandoff(active, result.value, "new");
            else await this.failedWrite(result, active, "send", true);
          },
        );
      }
      return;
    }
    if (active.step === "establish") {
      if (!active.operation || !active.messageId || !active.code) return;
      this.begin("establishing", "current");
      await this.request(
        (signal) =>
          this.dependencies.establish(
            active.operation!,
            active.selection.id,
            { messageId: active.messageId!, code: active.code! },
            signal,
          ),
        async (result, generation) => {
          if (generation !== this.generation) return;
          if (result.kind === "ok") this.transferHandoff(active, result.value);
          else await this.failedWrite(result, active, "establish", true);
        },
      );
      return;
    }
    if (active.step === "complete") {
      if (
        !active.operation ||
        !active.newMobile ||
        !active.messageId ||
        !active.code ||
        !active.transfer
      )
        return;
      this.begin("completing", "new");
      await this.request(
        (signal) =>
          this.dependencies.complete(
            active.operation!,
            active.selection.id,
            active.transfer!,
            {
              mobile: active.newMobile!,
              messageId: active.messageId!,
              code: active.code!,
            },
            signal,
          ),
        async (result, generation) => {
          if (result.kind === "ok")
            await this.completed(active, result.value, generation);
          else if (generation === this.generation)
            await this.failedWrite(result, active, "complete", true);
          if (generation === this.generation) this.render();
        },
      );
    }
  }

  async query() {
    const account = this.account();
    if (this.phase !== "idle" || !account || !this.material) return;
    const active = this.material.active;
    if (active && !this.belongs(account, active)) return;
    const known = this.knownFor(account);
    const transfer = active?.transfer ?? known?.transfer;
    if (!transfer) return;
    if (
      Math.max(this.queryResult?.retryAt ?? 0, active?.retryAt ?? 0) >
      this.dependencies.now()
    )
      return;
    const binding = this.binding();
    const selection = this.selection();
    if (!binding || !selection) return;
    if (!(await this.ready())) return;
    this.phase = "querying";
    this.render();
    const generation = ++this.generation;
    const controller = new AbortController();
    this.pendingRequest = controller;
    const timer = setTimeout(() => controller.abort(), 35_000);
    try {
      const result = await this.dependencies.read(
        selection.id,
        transfer,
        controller.signal,
      );
      if (
        generation === this.generation &&
        !(await this.current.confirm(binding))
      )
        await this.verify();
      if (result.kind === "ok" && result.value.status === "completed") {
        this.remember({
          account: account.id,
          person: account.personId,
          transfer,
          completedAt: result.value.completedAt,
        });
        if (generation === this.generation) {
          this.phase = "idle";
          this.queryResult = null;
          this.render();
        }
        return;
      }
      if (generation !== this.generation) return;
      this.phase = "idle";
      if (result.kind === "ok") {
        if (result.value.status === "pending") {
          this.queryResult = {
            status: "pending",
            retryAt: 0,
            expiresAt: result.value.expiresAt,
            unknown: active?.attempted ?? false,
          };
        } else {
          if (active) {
            const next: Active = {
              ...this.nextBase(active, "transfer"),
              ...(active.currentMobile
                ? { currentMobile: active.currentMobile }
                : {}),
              ...(active.transfer ? { transfer: active.transfer } : {}),
              ...(active.transferExpiresAt !== undefined
                ? { transferExpiresAt: active.transferExpiresAt }
                : {}),
              reason: "expired",
              attempted: active.attempted,
              retryAt: active.retryAt,
            };
            this.setActive(next);
          }
          this.queryResult = {
            status: "expired",
            retryAt: 0,
            expiresAt: null,
            unknown: active?.attempted ?? false,
          };
        }
        this.render();
        return;
      }
      if (result.kind === "untrusted") {
        this.reload();
        return;
      }
      const unknown = active?.attempted ?? false;
      if (result.kind === "disconnected") {
        const retryAt = this.backoff();
        if (active) this.setActive({ ...active, retryAt });
        this.queryResult = {
          status: "failed",
          retryAt,
          expiresAt: null,
          unknown,
        };
      } else if (result.status === 401) {
        await this.current.reject(binding, 401);
        this.queryResult = {
          status: "authentication",
          retryAt: 0,
          expiresAt: null,
          unknown,
        };
      } else if (result.status === 403) {
        await this.current.reject(binding, 403);
        this.queryResult = {
          status: "denied",
          retryAt: 0,
          expiresAt: null,
          unknown,
        };
      } else if (result.status === 429) {
        const retryAt = this.backoff(result.retryAfter);
        if (active) this.setActive({ ...active, retryAt });
        this.queryResult = {
          status: "limited",
          retryAt,
          expiresAt: null,
          unknown,
        };
      } else if (result.status === 404) {
        this.queryResult = {
          status: "unavailable",
          retryAt: 0,
          expiresAt: null,
          unknown,
        };
      } else {
        const retryAt = this.backoff(result.retryAfter);
        if (active) this.setActive({ ...active, retryAt });
        this.queryResult = {
          status: "failed",
          retryAt,
          expiresAt: null,
          unknown,
        };
      }
      this.render();
    } catch {
      if (generation === this.generation) {
        this.phase = "idle";
        const retryAt = this.backoff();
        if (active) this.setActive({ ...active, retryAt });
        this.queryResult = {
          status: "failed",
          retryAt,
          expiresAt: null,
          unknown: active?.attempted ?? false,
        };
        this.render();
      }
    } finally {
      clearTimeout(timer);
      if (this.pendingRequest === controller) this.pendingRequest = undefined;
    }
  }

  /** Return from a pending query to the step that can still continue. */
  resume() {
    if (this.queryResult?.status !== "pending") return;
    this.queryResult = null;
    this.render();
  }

  /** Start a new application only after a definite end or explicit restart. */
  restart() {
    if (this.phase !== "idle" || this.dependencies.readonly) return;
    const account = this.account();
    const selection = this.selection();
    if (!account || !selection) return;
    const record = this.record();
    if (record?.closed === "unknown") return;
    if (record?.active?.attempted) return;
    if (record?.active && !this.belongs(account, record.active)) return;
    const known = record?.known ?? [];
    const createdAt = this.dependencies.now();
    const active: Active = {
      step: "current-send",
      account: account.id,
      person: account.personId,
      selection,
      createdAt,
      expiresAt: Math.min(Number.MAX_SAFE_INTEGER, createdAt + LIFETIME),
      attempted: false,
      retryAt: 0,
    };
    if (!this.store({ version: 1, known, active, closed: null })) {
      this.phase = "storage";
      this.storagePending = false;
      this.render();
      return;
    }
    this.queryResult = null;
    this.flash = null;
    this.confirmedKnown = null;
    this.lastAccount = null;
    this.sentBinding = undefined;
    try {
      this.dependencies.unmark();
    } catch {
      // A stale marker only causes a conservative unknown on a lost record.
    }
    this.drafts.clear();
    this.render();
  }

  /** Return from the confirmation panel to new-code entry. */
  editCode() {
    if (this.compute().kind !== "confirm") return;
    const record = this.record();
    const active = record?.active;
    if (!record || !active || active.step !== "complete") return;
    const next: Active = {
      ...active,
      step: "new-code",
      code: undefined,
      operation: undefined,
      attempted: false,
      reason: undefined,
      retryAt: 0,
    };
    if (
      !this.store({
        version: 1,
        known: record.known,
        active: next,
        closed: null,
      })
    ) {
      this.phase = "storage";
      this.storagePending = false;
      this.render();
      return;
    }
    this.flash = null;
    this.render();
  }

  /** Request another code for the same stage; never replaces an unknown write. */
  resend() {
    const record = this.record();
    const active = record?.active;
    if (!record || !active || active.attempted) return;
    if (active.step === "current-code" || active.step === "establish") {
      const now = this.dependencies.now();
      const next: Active = {
        ...this.nextBase(
          active,
          "current-send",
          now,
          Math.min(Number.MAX_SAFE_INTEGER, now + LIFETIME),
        ),
        ...(active.currentMobile
          ? { currentMobile: active.currentMobile }
          : {}),
      };
      if (
        !this.store({
          version: 1,
          known: record.known,
          active: next,
          closed: null,
        })
      ) {
        this.phase = "storage";
        this.storagePending = false;
        this.render();
        return;
      }
      this.drafts.delete("current");
      this.flash = null;
      try {
        this.dependencies.unmark();
      } catch {
        // A stale marker only causes a conservative unknown on a lost record.
      }
      this.render();
      return;
    }
    if (active.step === "new-code" || active.step === "complete") {
      const next: Active = {
        ...this.nextBase(
          active,
          "new-send",
          active.createdAt,
          active.transferExpiresAt ?? active.expiresAt,
        ),
        ...(active.transfer ? { transfer: active.transfer } : {}),
        ...(active.transferExpiresAt !== undefined
          ? { transferExpiresAt: active.transferExpiresAt }
          : {}),
        ...(active.currentMobile
          ? { currentMobile: active.currentMobile }
          : {}),
        ...(active.newMobile ? { newMobile: active.newMobile } : {}),
      };
      if (
        !this.store({
          version: 1,
          known: record.known,
          active: next,
          closed: null,
        })
      ) {
        this.phase = "storage";
        this.storagePending = false;
        this.render();
        return;
      }
      this.drafts.delete("new");
      this.flash = null;
      this.render();
    }
  }
}
