import type { Current, CurrentBinding } from "../authentication/current";

export type AvatarState =
  | { kind: "hidden" }
  | { kind: "loading"; href: string; attempt: number }
  | { kind: "ready"; href: string; attempt: number }
  | { kind: "failed"; retry: boolean };

const HIDDEN: AvatarState = { kind: "hidden" };

type Request = Readonly<{
  attempt: number;
  binding: CurrentBinding;
  href: string;
}>;

type Dependencies = {
  current: Current;
  wait?: number;
  now?: () => number;
};

/**
 * Owns the one private image selected by the visible Current projection.
 * Pixels are exposed only after the original selection is confirmed again.
 */
export class Avatar {
  private state: AvatarState = HIDDEN;
  private readonly listeners = new Set<() => void>();
  private readonly wait: number;
  private readonly unwatch: () => void;
  private attempt = 0;
  private retries = true;
  private failed = false;
  private identity?: string;
  private link?: { href: string; expiresAt: number };
  private request?: Request;
  private timer?: ReturnType<typeof setTimeout>;

  constructor(private readonly dependencies: Dependencies) {
    this.wait = dependencies.wait ?? 35_000;
    this.unwatch = dependencies.current.subscribe(this.changed);
    this.changed();
  }

  snapshot = (): AvatarState => this.state;
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  /** Re-read the whole projection; the retry budget survives this transition. */
  retry(): Promise<void> | undefined {
    if (this.state.kind !== "failed" || !this.state.retry) return;
    if (this.dependencies.current.snapshot().kind !== "ready") return;
    this.retries = false;
    this.failed = false;
    this.link = undefined;
    this.cancel();
    this.show(HIDDEN);
    return this.dependencies.current.verify();
  }

  loaded(attempt: number) {
    const request = this.request;
    if (!request || request.attempt !== attempt) return;
    void this.confirm(request);
  }

  error(attempt: number) {
    const request = this.request;
    if (!request || request.attempt !== attempt) return;
    this.fail();
  }

  close() {
    this.unwatch();
    this.cancel();
    this.link = undefined;
    this.show(HIDDEN);
    this.listeners.clear();
  }

  private changed = () => {
    this.cancel();
    const current = this.dependencies.current;
    const state = current.snapshot();
    const binding = state.kind === "ready" ? current.binding() : undefined;
    if (state.kind !== "ready" || !binding) {
      // A concealed candidate is useful only after another trusted projection
      // confirms the same selection/revision. Failed verification discards it.
      if (state.kind !== "checking") this.link = undefined;
      this.show(HIDDEN);
      return;
    }
    const identity = [
      state.selection.id,
      state.selection.revision,
      state.account.id,
      state.account.avatar.revision,
    ].join("\n");
    if (identity !== this.identity) {
      this.identity = identity;
      this.retries = true;
      this.failed = false;
      this.link = undefined;
    }
    if (this.failed) {
      this.show({ kind: "failed", retry: this.retries });
      return;
    }
    if (
      !this.link ||
      this.link.expiresAt <= (this.dependencies.now ?? Date.now)()
    )
      this.link = {
        href: state.account.avatar.href,
        expiresAt: Date.parse(state.account.avatar.expiresAt),
      };
    const href = this.link.href;
    const attempt = this.attempt;
    this.request = { attempt, binding, href };
    this.timer = setTimeout(() => {
      if (this.request?.attempt === attempt) this.fail();
    }, this.wait);
    this.show({
      kind: "loading",
      href,
      attempt,
    });
  };

  private async confirm(request: Request) {
    let confirmed = false;
    try {
      confirmed = await this.dependencies.current.confirm(request.binding);
    } catch {
      confirmed = false;
    }
    if (
      !confirmed ||
      this.request !== request ||
      request.attempt !== this.attempt
    )
      return;
    this.clearTimer();
    this.request = undefined;
    this.retries = true;
    this.show({
      kind: "ready",
      href: request.href,
      attempt: request.attempt,
    });
  }

  private fail() {
    this.clearTimer();
    this.request = undefined;
    this.link = undefined;
    this.failed = true;
    this.show({ kind: "failed", retry: this.retries });
  }

  private cancel() {
    this.attempt++;
    this.clearTimer();
    this.request = undefined;
  }

  private clearTimer() {
    clearTimeout(this.timer);
    this.timer = undefined;
  }

  private show(state: AvatarState) {
    this.state = state;
    this.listeners.forEach((listener) => listener());
  }
}
