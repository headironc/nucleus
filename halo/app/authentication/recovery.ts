import {
  validInput,
  type Input,
  type LoginResult,
  type Session,
} from "../protocol/session";
import {
  clearCode,
  discardSend,
  LIFETIME,
  MESSAGE_KEY,
  readCode,
  save,
  type Code,
  type Storage,
} from "./material";
import {
  sameAttempt,
  SessionStorageError,
  type Attempt,
  type Selection,
  type Sessions,
} from "./session";
import {
  LOGIN_KEY,
  UUID,
  REJECTIONS,
  parseMaterial,
  expired,
  receiptKey,
  receipts,
  matches,
  maintainMaterial,
  cleanHandoff,
  type Proof,
  type Next,
  type Closed,
  type Completed,
} from "./material";
export { LOGIN_KEY } from "./material";

export type LoginState =
  | { kind: "editing"; code: Code; invalid: boolean }
  | { kind: "preparing" }
  | { kind: "submitting" | "reload"; retryAt: number }
  | { kind: "pending" | "limited"; retryAt: number; sent: boolean }
  | { kind: "registration"; expiresAt: number }
  | { kind: "session"; selection?: Selection }
  | { kind: "rejected"; reason: string; uncertain?: boolean }
  | {
      kind:
        | "missing"
        | "expired"
        | "storage"
        | "session-storage"
        | "random"
        | "changed"
        | "delivery-failed";
    };

type Dependencies = {
  storage: Storage;
  sessions: Pick<Sessions, "read" | "begin" | "accept">;
  now(): number;
  uuid(): string;
  secret(): string;
  current(): Promise<boolean>;
  marked(): boolean;
  mark(): void;
  login(
    operation: string,
    input: Input,
    signal: AbortSignal,
  ): Promise<LoginResult>;
};

export function secret(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(32));
  return btoa(String.fromCharCode(...bytes))
    .replaceAll("+", "-")
    .replaceAll("/", "_")
    .replaceAll("=", "");
}

/** One original login per tab; only a determined wrong-code result permits editing. */
export class Login {
  private state: LoginState = { kind: "preparing" };
  private material?: Proof | Next | Completed;
  // A known server result survives local storage/selection failures. Its
  // per-operation key also preserves it when another flow replaces LOGIN_KEY.
  private completed = new Map<string, Completed>();
  private request?: AbortController;
  private generation = 0;
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
  private show(state: LoginState) {
    this.state = state;
    this.listeners.forEach((listener) => listener());
  }
  private storageFailure() {
    this.show({
      kind:
        this.material?.kind === "session" || this.completed.size > 0
          ? "session-storage"
          : "storage",
    });
  }
  private persist(value: Proof | Next | Closed | Completed) {
    save(this.dependencies.storage, LOGIN_KEY, value);
  }
  private owns(attempt: Attempt) {
    const bytes = this.dependencies.storage.getItem(LOGIN_KEY);
    if (!bytes) return false;
    try {
      const value = JSON.parse(bytes);
      return (
        value?.attempt?.operation === attempt.operation &&
        value?.attempt?.revision === attempt.revision
      );
    } catch {
      return false;
    }
  }
  private cleanup(completed: Completed) {
    if (completed.cleanup === undefined) return;
    clearCode(this.dependencies.storage, completed.cleanup);
    const cleaned = { ...completed };
    delete cleaned.cleanup;
    save(
      this.dependencies.storage,
      receiptKey(completed.attempt.operation),
      cleaned,
    );
    if (this.owns(completed.attempt)) this.persist(cleaned);
    delete completed.cleanup;
  }
  private remember(proof: Proof, session: Session) {
    const completed: Completed = {
      version: 1,
      kind: "session",
      attempt: {
        operation: proof.attempt.operation,
        revision: proof.attempt.revision,
      },
      session: { id: session.id, expiresAt: session.expiresAt },
      cleanup: { message: proof.input.credential.id, send: proof.send },
    };
    this.completed.set(completed.attempt.operation, completed);
    if (
      this.material?.attempt.operation === completed.attempt.operation &&
      this.material.attempt.revision === completed.attempt.revision
    )
      this.material = completed;

    let failed = false;
    try {
      save(
        this.dependencies.storage,
        `halo.r1.session.${completed.attempt.operation}`,
        completed,
      );
    } catch {
      failed = true;
    }
    try {
      if (this.owns(completed.attempt)) {
        try {
          this.persist(completed);
        } catch {
          failed = true;
          if (this.owns(completed.attempt)) {
            this.dependencies.storage.removeItem(LOGIN_KEY);
            if (this.dependencies.storage.getItem(LOGIN_KEY) === null)
              this.persist(completed);
          }
        }
      }
    } catch {
      failed = true;
    }
    try {
      this.cleanup(completed);
    } catch {
      failed = true;
    }
    if (failed) throw new SessionStorageError();
    return completed;
  }
  private async finish(completed: Completed, generation: number) {
    this.cleanup(completed);
    const active = () =>
      generation === this.generation && this.material === completed;
    const selected = await this.dependencies.sessions.read();
    if (!active()) return;
    if (Date.parse(completed.session.expiresAt) <= this.dependencies.now()) {
      this.show({ kind: "expired" });
      return;
    }
    if (
      selected?.attempt === null &&
      selected.session?.id === completed.session.id
    ) {
      if (this.owns(completed.attempt))
        this.persist({ version: 1, kind: "closed", reason: "session" });
      this.show({
        kind: "session",
        selection: { id: completed.session.id, revision: selected.revision },
      });
      return;
    }
    if (!sameAttempt(selected, completed.attempt)) {
      this.show({ kind: "changed" });
      return;
    }
    const selection = await this.dependencies.sessions.accept(
      completed.attempt,
      completed.session,
    );
    if (!active()) return;
    if (selection && this.owns(completed.attempt))
      this.persist({ version: 1, kind: "closed", reason: "session" });
    this.show(selection ? { kind: "session", selection } : { kind: "changed" });
  }
  private expired() {
    if (!this.material || this.material.kind === "session") return false;
    const now = this.dependencies.now();
    if (!expired(this.material, now)) return false;
    ++this.generation;
    this.request?.abort();
    this.request = undefined;
    this.material = undefined;
    const cleaned = maintainMaterial(this.dependencies.storage, now);
    this.show({ kind: cleaned ? "expired" : "storage" });
    return true;
  }
  private edit(invalid = false) {
    const code = readCode(this.dependencies.storage, this.dependencies.now());
    this.show(
      code?.message.delivery === "failed"
        ? { kind: "delivery-failed" }
        : code
          ? { kind: "editing", code, invalid }
          : { kind: "missing" },
    );
  }
  private closeMissing() {
    this.material = undefined;
    try {
      this.dependencies.mark();
    } finally {
      this.persist({ version: 1, kind: "closed", reason: "missing" });
    }
    this.show({ kind: "missing" });
  }

  async initialize() {
    if (this.reloadNeeded) return;
    const generation = ++this.generation;
    this.show({ kind: "preparing" });
    try {
      for (const completed of this.completed.values()) {
        save(
          this.dependencies.storage,
          receiptKey(completed.attempt.operation),
          completed,
        );
        this.cleanup(completed);
      }
      let bytes = this.dependencies.storage.getItem(LOGIN_KEY);
      if (bytes === null && this.material?.kind === "session") {
        this.persist(this.material);
        bytes = JSON.stringify(this.material);
      }
      let value = bytes === null ? undefined : parseMaterial(bytes);
      if (bytes !== null && !value) {
        this.closeMissing();
        return;
      }
      if (bytes === null) {
        const saved = receipts(this.dependencies.storage);
        for (const completed of saved)
          this.completed.set(completed.attempt.operation, completed);
        if (saved.length > 0) {
          const selected = await this.dependencies.sessions.read();
          if (generation !== this.generation) return;
          const candidates = saved.filter((completed) =>
            matches(completed, selected),
          );
          const code = readCode(
            this.dependencies.storage,
            this.dependencies.now(),
          );
          const original = candidates.filter(
            (completed) =>
              selected?.attempt === completed.attempt.operation &&
              selected.revision === completed.attempt.revision,
          );
          const awaitingCleanup =
            code &&
            candidates.some(
              (completed) =>
                completed.cleanup?.message === code.message.id &&
                completed.cleanup.send === code.operation,
            );
          if (code && original.length === 0 && !awaitingCleanup) {
            discardSend(this.dependencies.storage, code);
            this.edit();
            return;
          }
          if (candidates.length > 1) throw new SessionStorageError("invalid");
          if (candidates.length === 1) {
            value = candidates[0];
            this.persist(value);
            bytes = JSON.stringify(value);
          } else {
            this.show({ kind: "changed" });
            return;
          }
        }
      }
      if (value?.kind === "session") {
        this.material = value;
        this.completed.set(value.attempt.operation, value);
        this.dependencies.mark();
        save(
          this.dependencies.storage,
          receiptKey(value.attempt.operation),
          value,
        );
        await this.finish(value, generation);
        return;
      }
      if (value?.kind === "login" || value?.kind === "registration") {
        this.material = value;
        if (value.kind === "login") {
          const receipt = this.dependencies.storage.getItem(
            receiptKey(value.attempt.operation),
          );
          if (receipt !== null) {
            const completed = parseMaterial(receipt);
            if (
              completed?.kind !== "session" ||
              completed.attempt.operation !== value.attempt.operation ||
              completed.attempt.revision !== value.attempt.revision
            )
              throw new SessionStorageError("invalid");
            const restored = this.remember(value, completed.session);
            this.dependencies.mark();
            await this.finish(restored, generation);
            return;
          }
        }
        this.dependencies.mark();
        if (this.expired()) return;
        if (value.kind === "registration") {
          value = cleanHandoff(this.dependencies.storage, LOGIN_KEY, value);
          this.material = value;
        } else {
          const code = readCode(
            this.dependencies.storage,
            this.dependencies.now(),
          );
          if (code && code.message.id === value.input.credential.id)
            discardSend(this.dependencies.storage, code);
        }
      } else if (value?.kind === "closed") {
        value = cleanHandoff(this.dependencies.storage, LOGIN_KEY, value);
        this.material = undefined;
      } else {
        const code = readCode(
          this.dependencies.storage,
          this.dependencies.now(),
        );
        if (code) discardSend(this.dependencies.storage, code);
      }
      let selected;
      try {
        selected = await this.dependencies.sessions.read();
      } catch (error) {
        if (generation !== this.generation) return;
        if (
          error instanceof SessionStorageError &&
          error.kind === "invalid" &&
          this.dependencies.storage.getItem(LOGIN_KEY) === null &&
          !this.dependencies.marked()
        ) {
          this.edit();
          return;
        }
        throw error;
      }
      if (generation !== this.generation) return;
      if (!value) {
        if (this.dependencies.marked()) this.show({ kind: "missing" });
        else this.edit();
        return;
      }
      if (value.kind === "closed") {
        this.dependencies.mark();
        if (value.reason === "session") this.show({ kind: "session" });
        else if (value.reason === "registration")
          this.show({ kind: "registration", expiresAt: 0 });
        else if (value.reason === "verification_invalid") this.edit(true);
        else if (value.reason === "expired") this.show({ kind: "expired" });
        else if (
          REJECTIONS.includes(value.reason as (typeof REJECTIONS)[number])
        )
          this.show({ kind: "rejected", reason: value.reason });
        else this.show({ kind: "missing" });
        return;
      }
      if (this.expired()) return;
      if (!sameAttempt(selected, value.attempt)) this.show({ kind: "changed" });
      else if (value.kind === "registration")
        this.show({ kind: "registration", expiresAt: value.expiresAt });
      else if (value.reason)
        this.show({ kind: "rejected", reason: value.reason, uncertain: true });
      else
        this.show({
          kind: value.limited ? "limited" : "pending",
          retryAt: value.retryAt,
          sent: value.sent,
        });
    } catch {
      if (generation === this.generation) this.storageFailure();
    }
  }
  async submit(code: string) {
    if (this.state.kind !== "editing" || !/^[0-9]{6}$/.test(code)) return;
    const message = this.state.code;
    if (this.dependencies.now() >= message.expiresAt) {
      this.show({ kind: "expired" });
      return;
    }
    const generation = ++this.generation;
    this.show({ kind: "preparing" });
    let operation: string;
    let input: Input;
    try {
      operation = this.dependencies.uuid();
      input = {
        secret: this.dependencies.secret(),
        credential: {
          type: "message",
          id: message.message.id,
          mobile: message.mobile,
          code,
        },
      };
      if (!UUID.test(operation) || !validInput(input))
        throw new Error("invalid_random");
    } catch {
      this.show({ kind: "random" });
      return;
    }
    try {
      const attempt = await this.dependencies.sessions.begin(operation);
      if (generation !== this.generation) return;
      const createdAt = this.dependencies.now();
      if (createdAt < message.createdAt || createdAt >= message.expiresAt) {
        this.dependencies.storage.removeItem(MESSAGE_KEY);
        this.show({ kind: "expired" });
        return;
      }
      const proof: Proof = {
        version: 1,
        kind: "login",
        attempt,
        input,
        send: message.operation,
        createdAt,
        expiresAt: Math.min(createdAt + LIFETIME, message.expiresAt),
        retryAt: 0,
        sent: false,
      };
      this.persist(proof);
      this.material = proof;
      this.dependencies.mark();
    } catch {
      if (generation === this.generation) this.show({ kind: "storage" });
      return;
    }
    await this.confirm();
  }

  private postpone(proof: Proof, delay = 5000) {
    proof.retryAt = Math.min(
      Number.MAX_SAFE_INTEGER,
      Math.max(proof.retryAt, this.dependencies.now() + Math.max(5000, delay)),
    );
    if (!this.owns(proof.attempt)) throw new SessionStorageError("invalid");
    this.persist(proof);
    return proof.retryAt;
  }

  async confirm() {
    if (
      !["preparing", "pending", "limited"].includes(this.state.kind) ||
      this.reloadNeeded ||
      this.request ||
      this.expired() ||
      this.material?.kind !== "login"
    )
      return;
    const proof = this.material;
    if (proof.reason || this.dependencies.now() < proof.retryAt) return;
    const generation = ++this.generation;
    const request = new AbortController();
    this.request = request;
    const active = () =>
      generation === this.generation &&
      this.material === proof &&
      !this.expired();
    const timer = setTimeout(() => request.abort(), 35_000);
    this.show({ kind: "submitting", retryAt: proof.retryAt });
    try {
      const selected = await this.dependencies.sessions.read();
      if (!active()) return;
      if (!sameAttempt(selected, proof.attempt)) {
        this.show({ kind: "changed" });
        return;
      }
      const current = await this.dependencies.current();
      if (!active()) return;
      if (!current) {
        this.reload();
        return;
      }
      if (request.signal.aborted) {
        this.show({
          kind: "pending",
          retryAt: this.postpone(proof),
          sent: proof.sent,
        });
        return;
      }
      const beforeWrite = await this.dependencies.sessions.read();
      if (!active()) return;
      if (!sameAttempt(beforeWrite, proof.attempt)) {
        this.show({ kind: "changed" });
        return;
      }
      const first = !proof.sent;
      const unsubmittedExpiry = proof.expiresAt;
      const expiresAt = proof.createdAt + LIFETIME;
      if (!this.owns(proof.attempt)) throw new SessionStorageError("invalid");
      const submitted = { ...proof, sent: true, expiresAt };
      delete submitted.limited;
      this.persist(submitted);
      proof.sent = true;
      proof.expiresAt = expiresAt;
      delete proof.limited;
      // The original operation, secret and credential are never rebuilt here.
      const result = await this.dependencies.login(
        proof.attempt.operation,
        proof.input,
        request.signal,
      );
      if (result.kind === "session") {
        // Cookie delivery is a server fact even after this page stops waiting.
        // Record it before deciding whether it may affect the current UI.
        const completed = this.remember(proof, result.session);
        if (generation === this.generation)
          await this.finish(completed, generation);
        return;
      }
      if (!active()) return;
      const latest = await this.dependencies.sessions.read();
      if (!active()) return;
      if (!sameAttempt(latest, proof.attempt)) {
        this.show({ kind: "changed" });
        return;
      }
      if (result.kind === "registration") {
        const next: Next = {
          version: 1,
          kind: "registration",
          attempt: proof.attempt,
          registration: result.registration,
          cleanup: { message: proof.input.credential.id, send: proof.send },
          createdAt: proof.createdAt,
          expiresAt: Math.min(
            proof.expiresAt,
            Date.parse(result.registration.expiresAt),
          ),
        };
        // Atomic replacement preserves R1 until the complete R2 handoff is saved.
        this.persist(next);
        this.material = next;
        if (this.expired()) return;
        this.material = cleanHandoff(
          this.dependencies.storage,
          LOGIN_KEY,
          next,
        );
        const currentSelection = await this.dependencies.sessions.read();
        if (generation !== this.generation || this.expired()) return;
        this.show(
          sameAttempt(currentSelection, next.attempt)
            ? { kind: "registration", expiresAt: next.expiresAt }
            : { kind: "changed" },
        );
      } else if (
        result.kind === "failure" &&
        (["request_mismatch", "authentication_invalid"].includes(result.code) ||
          (!first &&
            ["permission_denied", "invalid_input"].includes(result.code)))
      ) {
        proof.reason = result.code;
        this.persist(proof);
        this.show({ kind: "rejected", reason: result.code, uncertain: true });
      } else if (
        result.kind === "failure" &&
        REJECTIONS.includes(result.code as (typeof REJECTIONS)[number])
      ) {
        this.persist({ version: 1, kind: "closed", reason: result.code });
        this.material = undefined;
        if (result.code === "verification_invalid") this.edit(true);
        else this.show({ kind: "rejected", reason: result.code });
      } else {
        if (result.kind === "untrusted") {
          this.reload();
          return;
        }
        if (result.kind === "failure" && result.code === "rate_limited")
          proof.limited = true;
        if (proof.limited && first) {
          // A trusted first admission refusal cannot have created a login.
          proof.sent = false;
          proof.expiresAt = unsubmittedExpiry;
        }
        const retryAt = this.postpone(
          proof,
          result.kind === "failure" ? result.retryAfter : 5000,
        );
        this.show({
          kind: proof.limited ? "limited" : "pending",
          retryAt,
          sent: proof.sent,
        });
      }
    } catch {
      if (generation === this.generation) {
        if (this.material !== proof) this.storageFailure();
        else if (!this.expired()) {
          try {
            this.show({
              kind: "pending",
              retryAt: this.postpone(proof),
              sent: proof.sent,
            });
          } catch {
            this.show({ kind: "storage" });
          }
        }
      }
    } finally {
      clearTimeout(timer);
      if (this.request === request) this.request = undefined;
    }
  }

  reload() {
    this.reloadNeeded = true;
    ++this.generation;
    this.request?.abort();
    this.request = undefined;
    if (this.expired()) return;
    try {
      const retryAt =
        this.material?.kind === "login"
          ? this.postpone(this.material)
          : this.dependencies.now() + 5000;
      this.show({ kind: "reload", retryAt });
    } catch {
      this.show({ kind: "storage" });
    }
  }

  suspend() {
    ++this.generation;
    this.request?.abort();
    this.request = undefined;
    if (
      this.state.kind === "submitting" &&
      this.material?.kind === "login" &&
      !this.expired()
    ) {
      try {
        this.show({
          kind: "pending",
          retryAt: this.postpone(this.material),
          sent: this.material.sent,
        });
      } catch {
        this.show({ kind: "storage" });
      }
    }
  }

  async selectionChanged() {
    if (this.reloadNeeded) return;
    this.suspend();
    if (this.state.kind !== "editing") {
      // Initialization hides the previous stage before reading storage.
      await this.initialize();
    }
  }

  expire() {
    if (this.material?.kind === "session") {
      try {
        this.cleanup(this.material);
        if (
          Date.parse(this.material.session.expiresAt) <= this.dependencies.now()
        )
          this.show({ kind: "expired" });
      } catch {
        this.storageFailure();
      }
      return;
    }
    if (this.state.kind === "editing") {
      const now = this.dependencies.now();
      if (now < this.state.code.createdAt || now >= this.state.code.expiresAt) {
        try {
          maintainMaterial(this.dependencies.storage, now);
          this.show({ kind: "expired" });
        } catch {
          this.show({ kind: "storage" });
        }
      }
      return;
    }
    this.expired();
  }
}
