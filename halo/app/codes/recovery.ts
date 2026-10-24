import {
  validInput,
  type Input,
  type Message,
  type Result,
} from "../protocol/codes";
import {
  STORAGE_KEY,
  LIFETIME,
  UUID,
  readMaterial,
  persistMaterial,
  expireMaterial,
  expired,
  type Proof,
  type Closed,
} from "./material";
export { STORAGE_KEY } from "./material";
export type State =
  | { kind: "editing"; operation: string }
  | { kind: "submitting" | "pending" | "sending" | "limited"; retryAt: number }
  | { kind: "message"; message: Pick<Message, "delivery"> }
  | { kind: "rejected"; reason: string; uncertain?: boolean }
  | { kind: "missing" | "expired" | "storage" | "reload" | "random" };

type Dependencies = {
  storage: Pick<Storage, "getItem" | "setItem" | "removeItem">;
  marked: () => boolean;
  blocked: () => boolean;
  mark: (blocked?: boolean) => void;
  now: () => number;
  uuid: () => string;
  current: () => Promise<boolean>;
  received?: (operation: string, mobile: string, message: Message) => void;
  send: (
    operation: string,
    input: Input,
    signal: AbortSignal,
  ) => Promise<Result>;
};

// C1 owns one active request in this tab. A pending operation cannot be edited or replaced.
export class Recovery {
  state: State = { kind: "missing" };
  private proof?: Proof;
  private request?: AbortController;
  private generation = 0;
  private listeners = new Set<() => void>();
  constructor(private readonly dependencies: Dependencies) {}
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };
  snapshot = () => this.state;
  get unsubmitted() {
    return this.proof?.attempted === false;
  }
  private show(state: State) {
    this.state = state;
    this.listeners.forEach((listener) => listener());
  }
  private persist(value: Proof | Closed) {
    persistMaterial(this.dependencies.storage, value);
  }
  initialize() {
    let restored = false;
    try {
      if (this.dependencies.blocked()) {
        this.discard();
        this.show({ kind: "missing" });
        return;
      }
      const bytes = this.dependencies.storage.getItem(STORAGE_KEY);
      if (bytes === null) {
        if (this.dependencies.marked()) this.show({ kind: "missing" });
        else this.begin();
        return;
      }
      restored = true;
      const material = readMaterial(bytes);
      if (material.kind === "closed") {
        const value = material.value;
        this.dependencies.mark();
        if (
          value.delivery &&
          ["sent", "failed", "unknown"].includes(value.delivery)
        ) {
          this.show({ kind: "message", message: { delivery: value.delivery } });
        } else if (typeof value.reason === "string") {
          this.show({ kind: "rejected", reason: value.reason });
        } else this.show({ kind: "missing" });
        return;
      }
      if (material.kind === "invalid") {
        expireMaterial(this.dependencies.storage, this.dependencies.now());
        this.dependencies.mark();
        this.show({ kind: "missing" });
        return;
      }
      const value = material.proof;
      this.proof = value;
      this.dependencies.mark();
      if (this.expired()) return;
      if (value.reason) {
        this.show({ kind: "rejected", reason: value.reason, uncertain: true });
        return;
      }
      this.show({
        kind: value.delivery === "sending" ? "sending" : "pending",
        retryAt: value.retryAt,
      });
    } catch {
      if (this.proof) {
        this.show({ kind: "missing" });
        return;
      }
      if (restored)
        expireMaterial(this.dependencies.storage, this.dependencies.now());
      else this.discard();
      this.show({
        kind: restored || this.dependencies.marked() ? "missing" : "storage",
      });
    }
  }
  private expired() {
    if (!this.proof) return true;
    const now = this.dependencies.now();
    if (expired(this.proof, now)) {
      this.proof = undefined;
      this.request?.abort();
      expireMaterial(this.dependencies.storage, now);
      this.show({ kind: "expired" });
      return true;
    }
    return false;
  }
  begin() {
    if (this.proof || this.request) return;
    try {
      const operation = this.dependencies.uuid();
      if (!UUID.test(operation)) throw new Error("random_unavailable");
      this.show({ kind: "editing", operation });
    } catch {
      this.show({ kind: "random" });
    }
  }
  async submit(input: Input) {
    if (this.state.kind !== "editing" || !validInput(input)) return;
    const createdAt = this.dependencies.now();
    const proof: Proof = {
      version: 1,
      operation: this.state.operation,
      input: { ...input },
      createdAt,
      expiresAt: createdAt + LIFETIME,
      retryAt: 0,
      attempted: false,
    };
    try {
      // Readback and the non-secret history marker both precede the first network write.
      this.persist(proof);
      this.dependencies.mark();
    } catch {
      this.discard();
      this.show({ kind: "storage" });
      return;
    }
    this.proof = proof;
    await this.confirm();
  }
  async confirm() {
    if (this.request || this.expired() || !this.proof) return;
    if (this.proof.reason) return;
    if (this.dependencies.now() < this.proof.retryAt) return;
    const proof = this.proof;
    const generation = ++this.generation;
    const request = new AbortController();
    this.request = request;
    this.show({ kind: "submitting", retryAt: proof.retryAt });
    const timer = setTimeout(() => request.abort(), 35_000);
    try {
      const current = await this.dependencies.current();
      if (
        generation !== this.generation ||
        this.proof !== proof ||
        this.expired()
      )
        return;
      if (!current) {
        if (this.postpone(proof)) this.show({ kind: "reload" });
        return;
      }
      if (request.signal.aborted) {
        if (this.postpone(proof))
          this.show({ kind: "pending", retryAt: proof.retryAt });
        return;
      }
      // Missing markers may come from an earlier page that already sent the request.
      const firstAttempt = proof.attempted === false;
      proof.attempted = true;
      if (!this.saveUpdate(proof)) {
        if (firstAttempt) this.show({ kind: "storage" });
        return;
      }
      const result = await this.dependencies.send(
        proof.operation,
        proof.input,
        request.signal,
      );
      if (
        generation !== this.generation ||
        this.proof !== proof ||
        this.expired()
      )
        return;
      if (result.kind === "untrusted") {
        if (this.postpone(proof)) this.show({ kind: "reload" });
      } else if (result.kind === "message") {
        const expiresAt = Date.parse(result.message.expiresAt);
        proof.expiresAt = Math.min(proof.expiresAt, expiresAt);
        this.dependencies.received?.(
          proof.operation,
          proof.input.mobile,
          result.message,
        );
        if (result.message.delivery === "sending") {
          proof.delivery = "sending";
          if (!this.postpone(proof)) return;
          this.show({ kind: "sending", retryAt: proof.retryAt });
        } else {
          this.finish({
            version: 1,
            closed: true,
            delivery: result.message.delivery,
          });
          this.show({ kind: "message", message: result.message });
        }
      } else if (result.kind === "failure" && result.code === "rate_limited") {
        if (!this.postpone(proof, Math.max(1000, result.retryAfter))) return;
        this.show({ kind: "limited", retryAt: proof.retryAt });
      } else if (
        result.kind === "failure" &&
        (result.code === "request_mismatch" ||
          result.code === "authentication_invalid" ||
          (!firstAttempt &&
            (result.code === "invalid_input" ||
              result.code === "permission_denied")))
      ) {
        // A rejected replay does not determine the original request's outcome.
        proof.reason = result.code;
        if (!this.saveUpdate(proof)) return;
        this.show({ kind: "rejected", reason: result.code, uncertain: true });
      } else if (
        result.kind === "failure" &&
        [
          "verification_invalid",
          "verification_required",
          "permission_denied",
          "invalid_input",
        ].includes(result.code)
      ) {
        this.finish({ version: 1, closed: true, reason: result.code });
        this.show({ kind: "rejected", reason: result.code });
      } else {
        if (
          !this.postpone(
            proof,
            result.kind === "failure"
              ? Math.max(5000, result.retryAfter)
              : 5000,
          )
        )
          return;
        this.show({ kind: "pending", retryAt: proof.retryAt });
      }
    } catch {
      if (
        generation === this.generation &&
        this.proof === proof &&
        !this.expired() &&
        this.postpone(proof)
      )
        this.show({ kind: "pending", retryAt: proof.retryAt });
    } finally {
      clearTimeout(timer);
      if (this.request === request) this.request = undefined;
    }
  }
  private finish(value: Closed) {
    this.proof = undefined;
    try {
      this.dependencies.mark();
    } catch {
      // Still save the closed record when history is unavailable.
    }
    try {
      this.persist(value);
    } catch {
      this.discard();
    }
  }
  private discard() {
    this.proof = undefined;
    try {
      this.dependencies.storage.removeItem(STORAGE_KEY);
    } catch {
      // A non-secret history marker also prevents replay if storage becomes readable later.
      try {
        this.dependencies.mark(true);
      } catch {
        /* Both browser stores are inaccessible. */
      }
    }
  }
  private saveUpdate(proof: Proof) {
    try {
      this.persist(proof);
      return true;
    } catch {
      this.discard();
      this.show({ kind: "missing" });
      return false;
    }
  }
  private postpone(proof: Proof, delay = 5000) {
    proof.retryAt = Math.max(proof.retryAt, this.dependencies.now() + delay);
    return this.saveUpdate(proof);
  }
  reload() {
    ++this.generation;
    this.request?.abort();
    this.request = undefined;
    if (this.proof && (this.expired() || !this.postpone(this.proof))) return;
    this.show({ kind: "reload" });
  }
  suspend() {
    ++this.generation;
    this.request?.abort();
    this.request = undefined;
    if (
      this.state.kind === "submitting" &&
      this.proof &&
      !this.expired() &&
      this.postpone(this.proof)
    ) {
      this.show({ kind: "pending", retryAt: this.proof.retryAt });
    }
  }
  expire() {
    if (this.proof) this.expired();
  }
}
