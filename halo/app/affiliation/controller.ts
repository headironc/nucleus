import {
  type Current,
  type CurrentBinding,
  type CurrentState,
} from "../authentication/current";
import type {
  AffiliationCode,
  AffiliationValue,
  AffiliationValuesResult,
  AffiliationWriteResult,
  CurrentAffiliationResult,
  NamedAffiliation,
} from "../protocol/affiliation";

type Identity =
  | { kind: "identity-loading" }
  | { kind: "identity-anonymous" }
  | { kind: "identity-expired" }
  | { kind: "identity-denied" }
  | { kind: "identity-storage" }
  | { kind: "identity-reload" }
  | { kind: "identity-unavailable"; retryAt: number }
  | { kind: "identity-limited"; retryAt: number };

export type AffiliationRead =
  | { kind: "idle" }
  | { kind: "loading" }
  | { kind: "record"; value: AffiliationValue; name: string }
  | { kind: "unset" }
  | { kind: "error"; retryAt: number; limited: boolean }
  | { kind: "dictionary-error"; retryAt: number; limited: boolean };

export type AffiliationPanel =
  | { kind: "reading" }
  | { kind: "read-error"; retryAt: number; limited: boolean }
  | { kind: "record"; value: AffiliationValue; name: string }
  | { kind: "unset" }
  | { kind: "dictionary-error"; retryAt: number; limited: boolean }
  | { kind: "value-dictionary-error"; retryAt: number; limited: boolean }
  | { kind: "form"; choice: AffiliationCode | null }
  | { kind: "confirm"; choice: AffiliationCode }
  | { kind: "submitting"; choice: AffiliationCode }
  | { kind: "submit-limited"; retryAt: number }
  | { kind: "submit-conflict" }
  | { kind: "submit-invalid"; choice: AffiliationCode }
  | { kind: "submit-failed"; retryAt: number }
  | { kind: "success" }
  | { kind: "unknown"; retryAt: number };

export type AffiliationSnapshot =
  | Identity
  | {
      kind: "ready";
      person: string;
      options: readonly NamedAffiliation[];
      panel: AffiliationPanel;
      current: AffiliationRead;
    };

type Dependencies = {
  current: Current;
  read(id: string, signal: AbortSignal): Promise<CurrentAffiliationResult>;
  values(id: string, signal: AbortSignal): Promise<AffiliationValuesResult>;
  create(
    operation: string,
    id: string,
    code: AffiliationCode,
    signal: AbortSignal,
  ): Promise<AffiliationWriteResult>;
  build(): Promise<boolean>;
  now(): number;
  operation(): string;
};

type Submission = Readonly<{
  operation: string;
  code: AffiliationCode;
  binding: CurrentBinding;
}>;

type ReadTarget = "initial" | "current" | "replace";

function masked(state: CurrentState): Identity {
  switch (state.kind) {
    case "anonymous":
      return { kind: "identity-anonymous" };
    case "expired":
      return { kind: "identity-expired" };
    case "denied":
      return { kind: "identity-denied" };
    case "storage":
      return { kind: "identity-storage" };
    case "reload":
      return { kind: "identity-reload" };
    case "unavailable":
      return { kind: "identity-unavailable", retryAt: state.retryAt };
    case "limited":
      return { kind: "identity-limited", retryAt: state.retryAt };
    default:
      // checking and account-changed reveal no previous private state.
      return { kind: "identity-loading" };
  }
}

/**
 * Owns explicit first writes and independent current-state reads.
 * Unknown writes discard their input and are never replayed.
 */
export class Affiliation {
  private state: AffiliationSnapshot = { kind: "identity-loading" };
  private readonly listeners = new Set<() => void>();
  private generation = 0;
  private readSerial = 0;
  private writeSerial = 0;
  private readRequest?: AbortController;
  private writeRequest?: AbortController;
  private options: readonly NamedAffiliation[] = [];
  private readTarget: ReadTarget = "initial";
  private readonly unwatch: () => void;

  constructor(private readonly dependencies: Dependencies) {
    this.unwatch = dependencies.current.subscribe(this.changed);
    this.changed();
  }

  snapshot = (): AffiliationSnapshot => this.state;
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  private show(state: AffiliationSnapshot) {
    this.state = state;
    this.listeners.forEach((listener) => listener());
  }

  private cancel() {
    this.generation++;
    this.readSerial++;
    this.writeSerial++;
    this.readRequest?.abort();
    this.writeRequest?.abort();
    this.readRequest = undefined;
    this.writeRequest = undefined;
  }

  private clearPrivate() {
    this.options = [];
  }

  private changed = () => {
    const state = this.dependencies.current.snapshot();
    this.cancel();
    this.clearPrivate();
    if (state.kind !== "ready") {
      this.show(masked(state));
      return;
    }
    this.show({
      kind: "ready",
      person: state.account.name,
      options: [],
      panel: { kind: "reading" },
      current: { kind: "idle" },
    });
    void this.loadInitial();
  };

  private showPanel(panel: AffiliationPanel) {
    const state = this.dependencies.current.snapshot();
    if (state.kind !== "ready") return;
    this.show({
      kind: "ready",
      person: state.account.name,
      options: this.options,
      panel,
      current: { kind: "idle" },
    });
  }

  private showCurrent(current: AffiliationRead) {
    if (this.state.kind !== "ready") return;
    this.show({ ...this.state, options: this.options, current });
  }

  private wait(result: {
    kind: string;
    status?: number;
    code?: string;
    retryAfter?: number;
  }): { retryAt: number; limited: boolean } {
    const limited = result.kind === "failure" && result.code === "rate_limited";
    const delay =
      result.kind === "failure"
        ? Math.max(5_000, result.retryAfter ?? 0)
        : 5_000;
    return {
      limited,
      retryAt: Math.min(
        Number.MAX_SAFE_INTEGER,
        this.dependencies.now() + delay,
      ),
    };
  }

  private async loadInitial(): Promise<void> {
    const current = this.dependencies.current;
    const binding = current.binding();
    if (!binding) return;
    this.options = [];
    this.readTarget = "initial";
    this.showPanel({ kind: "reading" });
    const generation = this.generation;
    const serial = ++this.readSerial;
    this.readRequest?.abort();
    const request = new AbortController();
    this.readRequest = request;
    const active = () =>
      serial === this.readSerial &&
      generation === this.generation &&
      current.matches(binding);
    let retryAt = 0;
    let limited = false;
    const rememberWait = <
      T extends CurrentAffiliationResult | AffiliationValuesResult,
    >(
      result: T,
    ): T => {
      if (result.kind === "failure") {
        const wait = this.wait(result);
        retryAt = Math.max(retryAt, wait.retryAt);
        limited ||= wait.limited;
      }
      return result;
    };
    const combinedWait = () => ({
      retryAt: Math.max(retryAt, this.wait({ kind: "disconnected" }).retryAt),
      limited,
    });
    const timer = setTimeout(() => {
      if (!active()) return;
      this.readSerial++;
      request.abort();
      if (this.readRequest === request) this.readRequest = undefined;
      this.showPanel({
        kind: "read-error",
        ...combinedWait(),
      });
    }, 35_000);
    try {
      if (!(await this.dependencies.build().catch(() => false))) {
        if (active()) current.reload();
        return;
      }
      if (!active() || !(await current.confirm(binding)) || !active()) return;
      const [affiliation, values] = await Promise.all([
        this.dependencies
          .read(binding.selection.id, request.signal)
          .then(rememberWait),
        this.dependencies
          .values(binding.selection.id, request.signal)
          .then(rememberWait),
      ]);
      if (!active()) return;
      if (affiliation.kind === "untrusted" || values.kind === "untrusted") {
        current.reload();
        return;
      }
      if (
        affiliation.kind === "failure" &&
        (affiliation.status === 401 || affiliation.status === 403)
      ) {
        await current.reject(binding, affiliation.status);
        return;
      }
      if (
        values.kind === "failure" &&
        (values.status === 401 || values.status === 403)
      ) {
        await current.reject(binding, values.status);
        return;
      }
      if (!(await current.confirm(binding)) || !active()) return;
      if (
        affiliation.kind === "failure" ||
        affiliation.kind === "disconnected"
      ) {
        this.showPanel({
          kind: "read-error",
          ...combinedWait(),
        });
        return;
      }
      if (values.kind !== "values") {
        this.showPanel({
          kind:
            affiliation.kind === "unset"
              ? "dictionary-error"
              : "value-dictionary-error",
          ...combinedWait(),
        });
        return;
      }
      this.options = values.affiliations;
      if (affiliation.kind === "unset") {
        this.showPanel({ kind: "unset" });
        return;
      }
      const name = this.options.find(
        (entry) => entry.value === affiliation.affiliation.value,
      )?.name;
      this.showPanel(
        name
          ? {
              kind: "record",
              value: affiliation.affiliation,
              name,
            }
          : {
              kind: "value-dictionary-error",
              ...this.wait({ kind: "disconnected" }),
            },
      );
    } catch {
      if (active())
        this.showPanel({
          kind: "read-error",
          ...combinedWait(),
        });
    } finally {
      clearTimeout(timer);
      if (this.readRequest === request) this.readRequest = undefined;
    }
  }

  private async readCurrent(target: ReadTarget): Promise<void> {
    const current = this.dependencies.current;
    const binding = current.binding();
    if (!binding) return;
    this.readTarget = target;
    if (target === "replace") this.showPanel({ kind: "reading" });
    else this.showCurrent({ kind: "loading" });
    const generation = this.generation;
    const serial = ++this.readSerial;
    this.readRequest?.abort();
    const request = new AbortController();
    this.readRequest = request;
    const active = () =>
      serial === this.readSerial &&
      generation === this.generation &&
      current.matches(binding);
    const finish = (panel: AffiliationPanel, read: AffiliationRead) => {
      if (target === "replace") this.showPanel(panel);
      else this.showCurrent(read);
    };
    const timer = setTimeout(() => {
      if (!active()) return;
      this.readSerial++;
      request.abort();
      if (this.readRequest === request) this.readRequest = undefined;
      const wait = {
        retryAt: Math.min(
          Number.MAX_SAFE_INTEGER,
          this.dependencies.now() + 5_000,
        ),
        limited: false,
      };
      finish({ kind: "read-error", ...wait }, { kind: "error", ...wait });
    }, 35_000);
    try {
      if (!(await this.dependencies.build().catch(() => false))) {
        if (active()) current.reload();
        return;
      }
      if (!active() || !(await current.confirm(binding)) || !active()) return;
      const result = await this.dependencies.read(
        binding.selection.id,
        request.signal,
      );
      if (!active() || !(await current.confirm(binding)) || !active()) return;
      if (result.kind === "untrusted") {
        current.reload();
        return;
      }
      if (
        result.kind === "failure" &&
        (result.status === 401 || result.status === 403)
      ) {
        await current.reject(binding, result.status);
        return;
      }
      if (result.kind === "failure") {
        const wait = this.wait(result);
        finish({ kind: "read-error", ...wait }, { kind: "error", ...wait });
        return;
      }
      if (result.kind === "unset") {
        finish({ kind: "unset" }, { kind: "unset" });
        return;
      }
      if (result.kind === "disconnected") {
        const wait = {
          retryAt: Math.min(
            Number.MAX_SAFE_INTEGER,
            this.dependencies.now() + 5_000,
          ),
          limited: false,
        };
        finish({ kind: "read-error", ...wait }, { kind: "error", ...wait });
        return;
      }
      let name = this.options.find(
        (entry) => entry.value === result.affiliation.value,
      )?.name;
      if (!name) {
        const values = await this.dependencies.values(
          binding.selection.id,
          request.signal,
        );
        if (!active()) return;
        if (values.kind === "untrusted") {
          current.reload();
          return;
        }
        if (
          values.kind === "failure" &&
          (values.status === 401 || values.status === 403)
        ) {
          await current.reject(binding, values.status);
          return;
        }
        if (values.kind !== "values") {
          const wait = this.wait(values);
          finish(
            { kind: "value-dictionary-error", ...wait },
            { kind: "dictionary-error", ...wait },
          );
          return;
        }
        this.options = values.affiliations;
        name = this.options.find(
          (entry) => entry.value === result.affiliation.value,
        )?.name;
      }
      if (!(await current.confirm(binding)) || !active()) return;
      if (!name) {
        finish(
          {
            kind: "value-dictionary-error",
            ...this.wait({ kind: "disconnected" }),
          },
          {
            kind: "dictionary-error",
            retryAt: Math.min(
              Number.MAX_SAFE_INTEGER,
              this.dependencies.now() + 5_000,
            ),
            limited: false,
          },
        );
        return;
      }
      const record = {
        kind: "record" as const,
        value: result.affiliation,
        name,
      };
      finish(record, record);
    } catch {
      if (active()) {
        const wait = {
          retryAt: Math.min(
            Number.MAX_SAFE_INTEGER,
            this.dependencies.now() + 5_000,
          ),
          limited: false,
        };
        finish({ kind: "read-error", ...wait }, { kind: "error", ...wait });
      }
    } finally {
      clearTimeout(timer);
      if (this.readRequest === request) this.readRequest = undefined;
    }
  }

  async recheck(): Promise<void> {
    const state = this.dependencies.current.snapshot();
    if (state.kind === "reload") this.dependencies.current.reload();
    else if (
      ["storage", "unavailable", "limited", "changed"].includes(state.kind)
    )
      await this.dependencies.current.verify();
  }

  startForm() {
    if (this.state.kind !== "ready") return;
    if (this.state.panel.kind !== "unset") return;
    this.showPanel({ kind: "form", choice: null });
  }

  choose(code: AffiliationCode) {
    if (this.state.kind !== "ready") return;
    if (this.state.panel.kind !== "form") return;
    if (!this.options.some((entry) => entry.value === code)) return;
    this.showPanel({ kind: "form", choice: code });
  }

  continueToConfirm() {
    if (this.state.kind !== "ready") return;
    const panel = this.state.panel;
    if (panel.kind !== "form" || !panel.choice) return;
    this.showPanel({ kind: "confirm", choice: panel.choice });
  }

  backToForm() {
    if (this.state.kind !== "ready") return;
    const panel = this.state.panel;
    if (panel.kind !== "confirm" && panel.kind !== "submit-invalid") return;
    this.showPanel({ kind: "form", choice: panel.choice });
  }

  async submit(): Promise<void> {
    if (this.state.kind !== "ready") return;
    const panel = this.state.panel;
    if (panel.kind !== "confirm" || this.writeRequest) return;
    const current = this.dependencies.current;
    const binding = current.binding();
    if (!binding) return;
    const submission: Submission = {
      operation: this.dependencies.operation(),
      code: panel.choice,
      binding,
    };
    await this.send(submission);
  }
  private async send(submission: Submission): Promise<void> {
    const current = this.dependencies.current;
    if (!current.matches(submission.binding)) return;
    const generation = this.generation;
    const serial = ++this.writeSerial;
    this.writeRequest?.abort();
    const request = new AbortController();
    this.writeRequest = request;
    const active = () =>
      serial === this.writeSerial &&
      generation === this.generation &&
      current.matches(submission.binding);
    this.showPanel({ kind: "submitting", choice: submission.code });
    const timer = setTimeout(() => {
      if (!active()) return;
      this.writeSerial++;
      request.abort();
      if (this.writeRequest === request) this.writeRequest = undefined;
      this.unknown();
    }, 35_000);
    try {
      if (!(await this.dependencies.build().catch(() => false))) {
        if (active()) current.reload();
        return;
      }
      if (
        !active() ||
        !(await current.confirm(submission.binding)) ||
        !active()
      )
        return;
      const result = await this.dependencies.create(
        submission.operation,
        submission.binding.selection.id,
        submission.code,
        request.signal,
      );
      if (!active()) return;
      if (!(await current.confirm(submission.binding)) || !active()) return;
      await this.classify(result, submission);
    } catch {
      if (active()) this.unknown();
    } finally {
      clearTimeout(timer);
      if (this.writeRequest === request) this.writeRequest = undefined;
    }
  }

  private unknown(retryAfter = 0) {
    this.showPanel({
      kind: "unknown",
      retryAt: Math.min(
        Number.MAX_SAFE_INTEGER,
        this.dependencies.now() + Math.max(0, retryAfter),
      ),
    });
  }

  private async classify(
    result: AffiliationWriteResult,
    submission: Submission,
  ): Promise<void> {
    if (result.kind === "ok") {
      this.showPanel({ kind: "success" });
      return;
    }
    if (result.kind === "untrusted" || result.kind === "disconnected") {
      this.unknown();
      return;
    }
    if (result.status === 401 || result.status === 403) {
      await this.dependencies.current.reject(submission.binding, result.status);
      return;
    }
    const code = result.code;
    if (code === "rate_limited") {
      const { retryAt } = this.wait(result);
      this.showPanel({
        kind: "submit-limited",
        retryAt,
      });
      return;
    }
    if (code === "invalid_input") {
      this.showPanel({ kind: "submit-invalid", choice: submission.code });
      return;
    }
    if (code === "state_conflict") {
      this.showPanel({ kind: "submit-conflict" });
      return;
    }
    if (code === "dependency_unavailable") {
      const { retryAt } = this.wait(result);
      this.showPanel({
        kind: "submit-failed",
        retryAt,
      });
      return;
    }
    this.unknown(result.retryAfter);
  }

  private readBlocked(read: AffiliationRead): boolean {
    return (
      read.kind === "loading" ||
      ((read.kind === "error" || read.kind === "dictionary-error") &&
        read.retryAt > this.dependencies.now())
    );
  }

  viewCurrent() {
    if (this.state.kind !== "ready" || this.readBlocked(this.state.current))
      return;
    const panel = this.state.panel;
    if (
      ![
        "success",
        "unknown",
        "submit-limited",
        "submit-failed",
        "submit-conflict",
      ].includes(panel.kind)
    )
      return;
    if ("retryAt" in panel && panel.retryAt > this.dependencies.now()) return;
    void this.readCurrent(panel.kind === "success" ? "current" : "replace");
  }

  retryRead() {
    if (
      this.state.kind !== "ready" ||
      this.readRequest ||
      this.readBlocked(this.state.current)
    )
      return;
    const panel = this.state.panel;
    if (
      (panel.kind === "read-error" ||
        panel.kind === "dictionary-error" ||
        panel.kind === "value-dictionary-error") &&
      panel.retryAt > this.dependencies.now()
    )
      return;
    if (
      this.state.current.kind === "error" ||
      this.state.current.kind === "dictionary-error"
    ) {
      void this.readCurrent("current");
      return;
    }
    if (this.readTarget === "replace") {
      void this.readCurrent("replace");
      return;
    }
    this.options = [];
    this.readTarget = "initial";
    this.showPanel({ kind: "reading" });
    void this.loadInitial();
  }

  close() {
    this.unwatch();
    this.cancel();
    this.clearPrivate();
    this.show({ kind: "identity-loading" });
  }
}
