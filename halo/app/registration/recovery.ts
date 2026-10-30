import { validInput, type Input, type Result } from "../protocol/registration";
import type { Session } from "../protocol/session";
import {
  expired,
  LIFETIME,
  LOGIN_KEY,
  remove,
  save,
  UUID,
  type Next,
  type Storage,
} from "../authentication/material";
import {
  sameAttempt,
  type Sessions,
  type Selection,
} from "../authentication/session";
import {
  KEY,
  parse,
  handoff,
  owns,
  retireLogin,
  receiptKey,
  receipts,
  maintain,
  REJECTIONS,
  type Proof,
  type Completed,
  type Rejection,
  type Refused,
  type Material,
} from "./material";

export type State =
  | { kind: "editing"; expiresAt: number; invalid?: true }
  | { kind: "preparing" | "submitting" }
  | { kind: "pending" | "limited" | "reload"; retryAt: number; sent: boolean }
  | {
      kind: "rejected";
      reason: Rejection;
      uncertain: boolean;
      editable?: boolean;
    }
  | { kind: "created"; selection?: Selection }
  | { kind: "created-storage" | "created-cleanup" }
  | {
      kind:
        | "storage-read"
        | "storage-write"
        | "storage-before"
        | "changed"
        | "missing"
        | "expired"
        | "credential-expired"
        | "random";
    };

type Dependencies = {
  storage: Storage;
  sessions: Pick<Sessions, "read" | "accept">;
  now(): number;
  uuid(): string;
  current(): Promise<boolean>;
  register(
    operation: string,
    input: Input,
    signal: AbortSignal,
  ): Promise<Result>;
};

export class Registration {
  private state: State = { kind: "preparing" };
  private next?: Next;
  private proof?: Proof;
  private completed?: Completed;
  private refused?: Refused;
  private pendingSave?: { operation: string; value: Material };
  private generation = 0;
  private request?: AbortController;
  private reloadNeeded = false;
  private listeners = new Set<() => void>();
  constructor(private readonly dependencies: Dependencies) {}
  snapshot = () => this.state;
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };
  private show(state: State) {
    this.state = state;
    this.listeners.forEach((listener) => listener());
  }
  private persist(proof: Proof) {
    save(this.dependencies.storage, KEY, proof);
  }
  private postpone(proof: Proof, delay = 5000) {
    proof.retryAt = Math.min(
      Number.MAX_SAFE_INTEGER,
      Math.max(proof.retryAt, this.dependencies.now() + Math.max(5000, delay)),
    );
    if (!owns(this.dependencies.storage, proof.operation))
      throw new Error("material_changed");
    this.saveOutcome(proof.operation, proof);
  }
  private saveOutcome(operation: string, value: Material) {
    this.pendingSave = { operation, value };
    this.flushOutcome();
  }
  private flushOutcome() {
    if (!this.pendingSave) return;
    const { operation, value } = this.pendingSave;
    const bytes = this.dependencies.storage.getItem(KEY);
    let saved = value;
    if (
      (value.kind === "registration" || value.kind === "refused") &&
      expired(value, this.dependencies.now())
    ) {
      saved =
        value.kind === "registration"
          ? {
              version: 1,
              kind: "closed",
              reason: "expired",
              operation: value.operation,
              attempt: value.attempt,
            }
          : {
              version: 1,
              kind: "closed",
              reason:
                value.reason === "rate_limited"
                  ? "verification_required"
                  : value.reason,
            };
    }
    if (
      !owns(this.dependencies.storage, operation) &&
      bytes !== JSON.stringify(value) &&
      bytes !== JSON.stringify(saved)
    )
      throw new Error("material_changed");
    save(this.dependencies.storage, KEY, saved);
    this.pendingSave = undefined;
  }
  private closeExpired() {
    const material = this.proof ?? this.next ?? this.refused;
    if (!material || !expired(material, this.dependencies.now())) return false;
    const refused = this.refused;
    const editing = !this.proof && !!this.next;
    this.suspend();
    this.proof = undefined;
    this.next = undefined;
    this.refused = undefined;
    const cleaned = maintain(
      this.dependencies.storage,
      this.dependencies.now(),
    );
    this.show(
      !cleaned
        ? { kind: "storage-write" }
        : refused
          ? {
              kind: "rejected",
              reason:
                refused.reason === "rate_limited"
                  ? "verification_required"
                  : refused.reason,
              uncertain: false,
              editable: false,
            }
          : { kind: editing ? "credential-expired" : "expired" },
    );
    return true;
  }
  private remember(proof: Proof, session: Session) {
    const completed: Completed = {
      version: 1,
      kind: "session",
      operation: proof.operation,
      attempt: { ...proof.attempt },
      session: { id: session.id, expiresAt: session.expiresAt },
      createdAt: proof.createdAt,
      expiresAt: proof.expiresAt,
    };
    // A cookie may already have arrived after cancellation or another login.
    // Keep the known fact without accepting it as the current selection.
    if (this.proof === proof || (!this.proof && !this.next && !this.refused)) {
      this.completed = completed;
      this.pendingSave = undefined;
      this.proof = undefined;
    }
    this.storeCompleted(completed);
    return completed;
  }
  private storeCompleted(completed: Completed) {
    let failed = false;
    try {
      save(
        this.dependencies.storage,
        receiptKey(completed.operation),
        completed,
      );
    } catch {
      failed = true;
    }
    try {
      if (owns(this.dependencies.storage, completed.operation))
        save(this.dependencies.storage, KEY, completed);
    } catch {
      failed = true;
    }
    try {
      retireLogin(this.dependencies.storage, completed.attempt);
    } catch {
      failed = true;
    }
    if (failed) throw new Error("cleanup_incomplete");
  }
  private async finish(completed: Completed, generation: number) {
    try {
      this.storeCompleted(completed);
    } catch {
      if (generation === this.generation)
        this.show({ kind: "created-cleanup" });
      return;
    }
    try {
      const selected = await this.dependencies.sessions.read();
      if (generation !== this.generation) return;
      if (
        selected?.attempt === null &&
        selected.session?.id === completed.session.id
      ) {
        this.show({
          kind: "created",
          selection: { id: completed.session.id, revision: selected.revision },
        });
        return;
      }
      if (
        !sameAttempt(selected, completed.attempt) ||
        expired(completed, this.dependencies.now()) ||
        Date.parse(completed.session.expiresAt) <= this.dependencies.now()
      ) {
        this.show({ kind: "created" });
        return;
      }
      const selection = await this.dependencies.sessions.accept(
        completed.attempt,
        completed.session,
      );
      if (generation === this.generation)
        this.show({ kind: "created", ...(selection ? { selection } : {}) });
    } catch {
      if (generation === this.generation)
        this.show({ kind: "created-storage" });
    }
  }
  async initialize() {
    if (this.reloadNeeded) return;
    const generation = ++this.generation;
    this.show({ kind: "preparing" });
    if (this.completed) {
      await this.finish(this.completed, generation);
      return;
    }
    try {
      this.flushOutcome();
      const bytes = this.dependencies.storage.getItem(KEY);
      let value = bytes === null ? undefined : parse(bytes);
      if (bytes !== null && !value) {
        save(this.dependencies.storage, KEY, {
          version: 1,
          kind: "closed",
          reason: "missing",
        });
        this.show({ kind: "missing" });
        return;
      }
      const next = handoff(this.dependencies.storage, this.dependencies.now());
      if (next && (!value || value.kind !== "registration")) {
        const selected = await this.dependencies.sessions.read();
        if (generation !== this.generation) return;
        if (
          sameAttempt(selected, next.attempt) &&
          (!value ||
            !("attempt" in value) ||
            value.attempt?.operation !== next.attempt.operation)
        ) {
          this.next = next;
          this.proof = undefined;
          this.show({ kind: "editing", expiresAt: next.expiresAt });
          return;
        }
      }
      const saved = receipts(this.dependencies.storage);
      if (
        value?.kind === "registration" ||
        (value?.kind === "closed" && value.operation && value.attempt)
      ) {
        const original = value;
        const found = saved.find(
          (receipt) =>
            receipt.operation === original.operation &&
            receipt.attempt.operation === original.attempt?.operation &&
            receipt.attempt.revision === original.attempt?.revision,
        );
        if (found) value = found;
      }
      if (value?.kind === "session") {
        this.completed = value;
        await this.finish(value, generation);
        return;
      }
      if (value?.kind === "closed") {
        this.show(
          value.reason === "missing" || value.reason === "expired"
            ? { kind: value.reason }
            : {
                kind: "rejected",
                reason: value.reason,
                uncertain: false,
                editable: false,
              },
        );
        return;
      }
      const selected = await this.dependencies.sessions.read();
      if (generation !== this.generation) return;
      if (value?.kind === "refused") {
        this.refused = value;
        this.proof = undefined;
        if (this.closeExpired()) return;
        if (!sameAttempt(selected, value.attempt))
          this.show({ kind: "changed" });
        else
          this.show(
            value.reason === "rate_limited"
              ? { kind: "limited", sent: false, retryAt: value.retryAt }
              : {
                  kind: "rejected",
                  reason: value.reason,
                  uncertain: false,
                  editable: true,
                },
          );
        return;
      }
      if (!value) {
        this.next = handoff(this.dependencies.storage, this.dependencies.now());
        if (!this.next) {
          this.show({ kind: "missing" });
          return;
        }
        this.show(
          sameAttempt(selected, this.next.attempt)
            ? { kind: "editing", expiresAt: this.next.expiresAt }
            : { kind: "changed" },
        );
        return;
      }
      this.proof = value;
      this.next = undefined;
      if (this.closeExpired()) return;
      retireLogin(this.dependencies.storage, value.attempt);
      if (!sameAttempt(selected, value.attempt)) this.show({ kind: "changed" });
      else if (value.reason)
        this.show({
          kind: "rejected",
          reason: value.reason,
          uncertain:
            [
              "authentication_invalid",
              "request_mismatch",
              "invalid_input",
              "permission_denied",
            ].includes(value.reason) && value.sent,
        });
      else
        this.show({
          kind: value.limited ? "limited" : "pending",
          retryAt: value.retryAt,
          sent: value.sent,
        });
    } catch {
      if (generation === this.generation) this.show({ kind: "storage-read" });
    }
  }
  async revise() {
    if (
      !this.refused ||
      this.closeExpired() ||
      this.dependencies.now() < this.refused.retryAt
    )
      return;
    const value = this.refused;
    const generation = ++this.generation;
    try {
      const selected = await this.dependencies.sessions.read();
      if (generation !== this.generation || this.closeExpired()) return;
      if (!sameAttempt(selected, value.attempt)) {
        this.show({ kind: "changed" });
        return;
      }
      this.next = {
        version: 1,
        kind: "registration",
        attempt: value.attempt,
        registration: value.registration,
        createdAt: value.createdAt,
        expiresAt: value.expiresAt,
      };
      this.show({ kind: "editing", expiresAt: value.expiresAt });
    } catch {
      this.show({ kind: "storage-read" });
    }
  }
  async retryLocal() {
    if (this.state.kind !== "storage-before") {
      await this.initialize();
      return;
    }
    const next = this.next;
    if (!next || this.closeExpired()) {
      await this.initialize();
      return;
    }
    const generation = ++this.generation;
    try {
      const selected = await this.dependencies.sessions.read();
      if (generation !== this.generation || this.closeExpired()) return;
      if (!sameAttempt(selected, next.attempt)) {
        this.show({ kind: "changed" });
        return;
      }
      // No POST was allowed past the failed preparation. Preserve the prior
      // credential before removing a possibly saved, unsubmitted R2 stage.
      save(this.dependencies.storage, LOGIN_KEY, next);
      remove(this.dependencies.storage, KEY);
      this.proof = undefined;
      this.show({ kind: "editing", expiresAt: next.expiresAt });
    } catch {
      this.show({ kind: "storage-before" });
    }
  }
  private refuse(proof: Proof, reason: Refused["reason"], delay = 0) {
    const refused: Refused = {
      version: 1,
      kind: "refused",
      attempt: proof.attempt,
      registration: {
        id: proof.input.id,
        token: proof.input.token,
        expiresAt: new Date(proof.credentialExpiresAt).toISOString(),
      },
      createdAt: proof.createdAt,
      expiresAt: Math.min(
        proof.createdAt + LIFETIME,
        proof.credentialExpiresAt,
      ),
      retryAt: Math.min(
        Number.MAX_SAFE_INTEGER,
        this.dependencies.now() + delay,
      ),
      reason,
    };
    this.refused = refused;
    this.proof = undefined;
    this.saveOutcome(proof.operation, refused);
  }
  async submit(name: string, idNumber: string) {
    if (this.state.kind !== "editing" || !this.next || this.closeExpired())
      return;
    const next = this.next;
    const input: Input = {
      id: next.registration.id,
      token: next.registration.token,
      name: name.trim().normalize("NFC"),
      idNumber: idNumber.replace(/x$/, "X"),
    };
    if (!validInput(input)) {
      this.show({ kind: "editing", expiresAt: next.expiresAt, invalid: true });
      return;
    }
    let operation: string;
    try {
      operation = this.dependencies.uuid();
      if (!UUID.test(operation) || operation === next.attempt.operation)
        throw new Error("invalid_random");
    } catch {
      this.show({ kind: "random" });
      return;
    }
    const generation = ++this.generation;
    this.show({ kind: "preparing" });
    try {
      const selected = await this.dependencies.sessions.read();
      if (generation !== this.generation || this.closeExpired()) return;
      if (!sameAttempt(selected, next.attempt)) {
        this.show({ kind: "changed" });
        return;
      }
      const createdAt = this.dependencies.now();
      const proof: Proof = {
        version: 1,
        kind: "registration",
        operation,
        attempt: next.attempt,
        input,
        createdAt,
        expiresAt: Math.min(createdAt + LIFETIME, next.expiresAt),
        credentialExpiresAt: next.expiresAt,
        retryAt: 0,
        sent: false,
      };
      this.persist(proof);
      this.proof = proof;
      this.refused = undefined;
      retireLogin(this.dependencies.storage, proof.attempt);
      this.next = undefined;
    } catch {
      if (generation === this.generation) this.show({ kind: "storage-before" });
      return;
    }
    await this.confirm();
  }
  async confirm() {
    if (
      !["preparing", "pending", "limited"].includes(this.state.kind) ||
      this.reloadNeeded ||
      this.request ||
      this.closeExpired() ||
      !this.proof
    )
      return;
    const proof = this.proof;
    if (proof.reason || this.dependencies.now() < proof.retryAt) return;
    const generation = ++this.generation;
    const request = new AbortController();
    this.request = request;
    const active = () =>
      generation === this.generation &&
      this.proof === proof &&
      !this.closeExpired();
    const timer = setTimeout(() => request.abort(), 35_000);
    this.show({ kind: "submitting" });
    try {
      const selected = await this.dependencies.sessions.read();
      if (!active()) return;
      if (!sameAttempt(selected, proof.attempt)) {
        this.show({ kind: "changed" });
        return;
      }
      if (!(await this.dependencies.current().catch(() => false))) {
        if (active()) this.reload();
        return;
      }
      if (!active()) return;
      if (request.signal.aborted) throw new Error("wait_ended");
      const before = await this.dependencies.sessions.read();
      if (!active()) return;
      if (!sameAttempt(before, proof.attempt)) {
        this.show({ kind: "changed" });
        return;
      }
      const first = !proof.sent;
      if (!owns(this.dependencies.storage, proof.operation))
        throw new Error("material_changed");
      const submitted = {
        ...proof,
        sent: true,
        expiresAt: proof.createdAt + LIFETIME,
      };
      delete submitted.limited;
      this.persist(submitted);
      Object.assign(proof, submitted);
      delete proof.limited;
      const result = await this.dependencies.register(
        proof.operation,
        proof.input,
        request.signal,
      );
      if (result.kind === "session") {
        const withinLifetime = !expired(proof, this.dependencies.now());
        let completed: Completed;
        try {
          completed = this.remember(proof, result.session);
        } catch {
          if (generation === this.generation)
            this.show({ kind: "created-cleanup" });
          return;
        }
        if (generation === this.generation) {
          if (withinLifetime) await this.finish(completed, generation);
          else this.show({ kind: "created" });
        }
        return;
      }
      if (!active()) return;
      const latest = await this.dependencies.sessions.read();
      if (!active()) return;
      if (!sameAttempt(latest, proof.attempt)) {
        this.show({ kind: "changed" });
        return;
      }
      if (result.kind === "untrusted") {
        this.reload();
        return;
      }
      if (
        result.kind === "failure" &&
        REJECTIONS.includes(result.code as Rejection)
      ) {
        const reason = result.code as Rejection;
        if (reason === "verification_required") {
          this.saveOutcome(proof.operation, {
            version: 1,
            kind: "closed",
            reason,
          });
          this.proof = undefined;
        } else if (
          ["mobile_taken", "id_number_taken"].includes(reason) ||
          (first && ["invalid_input", "permission_denied"].includes(reason))
        ) {
          this.refuse(proof, reason as Refused["reason"]);
        } else {
          proof.reason = reason;
          this.saveOutcome(proof.operation, proof);
        }
        this.show({
          kind: "rejected",
          reason,
          editable: !!this.refused,
          uncertain:
            ["authentication_invalid", "request_mismatch"].includes(reason) ||
            (!first && ["invalid_input", "permission_denied"].includes(reason)),
        });
        return;
      }
      if (result.kind === "failure" && result.code === "rate_limited") {
        if (first) {
          this.refuse(proof, "rate_limited", Math.max(5000, result.retryAfter));
          this.show({
            kind: "limited",
            retryAt: this.refused!.retryAt,
            sent: false,
          });
          return;
        }
        proof.limited = true;
      }
      this.postpone(
        proof,
        result.kind === "failure" ? result.retryAfter : 5000,
      );
      this.show({
        kind: proof.limited ? "limited" : "pending",
        retryAt: proof.retryAt,
        sent: proof.sent,
      });
    } catch {
      if (generation === this.generation && !this.closeExpired())
        this.show({ kind: "storage-write" });
    } finally {
      clearTimeout(timer);
      if (this.request === request) this.request = undefined;
    }
  }
  reload() {
    this.reloadNeeded = true;
    this.suspend();
    if (this.closeExpired()) return;
    try {
      if (this.proof) this.postpone(this.proof);
      this.show({
        kind: "reload",
        retryAt: this.proof?.retryAt ?? 0,
        sent: this.proof?.sent ?? false,
      });
    } catch {
      this.show({ kind: "storage-write" });
    }
  }
  suspend() {
    ++this.generation;
    this.request?.abort();
    this.request = undefined;
    if (this.state.kind === "submitting" && this.proof) {
      try {
        this.postpone(this.proof);
        this.show({
          kind: "pending",
          retryAt: this.proof.retryAt,
          sent: this.proof.sent,
        });
      } catch {
        this.show({ kind: "storage-write" });
      }
    }
  }
  async selectionChanged() {
    this.suspend();
    await this.initialize();
  }
  expire() {
    if (!this.completed) this.closeExpired();
  }
}
