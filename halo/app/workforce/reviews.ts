import type { Current, CurrentBinding } from "../authentication/current";
import {
  managedApplications,
  managedApplication,
  reviewApplication,
  cancelApplication,
  applicationResource,
  sameApplicationContent,
  type ApplicationStatus,
  type ManagedApplicationDetail,
  type ManagedApplicationList,
  type ReviewInput,
} from "../protocol/applications";
import {
  loadTemplateArtwork,
  type ApplicationArtwork,
  type TemplateArtwork,
} from "./artwork";

export type ReviewMode = { kind: "list" } | { kind: "detail"; id: string };
type Media =
  | { kind: "loading" }
  | { kind: "failed"; retryAt: number }
  | { kind: "ready"; artwork: ApplicationArtwork };
export type ReviewDetail = {
  kind: "detail";
  result: ManagedApplicationDetail;
  media: Media;
  confirmation?: ReviewInput["decision"] | "cancel";
  pending: boolean;
  notice?: string;
};
export type ReviewsState =
  | { kind: "hidden" | "loading" }
  | { kind: "error" | "missing"; message: string; retryAt: number }
  | {
      kind: "unknown";
      operation: "review" | "cancellation";
      message: string;
      retryAt: number;
    }
  | { kind: "cancelled" }
  | {
      kind: "list";
      page: number;
      status: ApplicationStatus;
      result: ManagedApplicationList;
    }
  | ReviewDetail;
const API = {
  list: managedApplications,
  detail: managedApplication,
  review: reviewApplication,
  cancellation: cancelApplication,
  resource: applicationResource,
  artwork: loadTemplateArtwork,
};
const READ_ERROR = "工牌申请暂时无法加载，请重试。";
const UNKNOWN_COPY = "请刷新详情查看，暂时不要重复提交。";

/** One visible management page owns its fixed application and current access. */
export class Reviews {
  private state: ReviewsState = { kind: "hidden" };
  private readonly listeners = new Set<() => void>();
  private readonly unwatch: () => void;
  private readonly api: typeof API;
  private generation = 0;
  private request?: AbortController;
  private timer?: ReturnType<typeof setTimeout>;
  private template?: TemplateArtwork;
  private artwork?: ApplicationArtwork;
  private closed = false;
  private retryAt = 0;
  private waitingSelection?: string;
  private uncertainAccount?: string;
  private writingAccount?: string;
  private cancellationUnknown = false;
  private page = 1;
  private filter: ApplicationStatus = "submitted";
  constructor(
    readonly mode: ReviewMode,
    private readonly dependencies: {
      current: Current;
      build(): Promise<boolean>;
      now(): number;
      api?: typeof API;
    },
  ) {
    this.api = dependencies.api ?? API;
    this.unwatch = dependencies.current.subscribe(this.changed);
    this.changed();
  }
  snapshot = () => this.state;
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };
  private show(state: ReviewsState) {
    if (this.closed) return;
    this.state = state;
    this.listeners.forEach((listener) => listener());
  }
  private unknown(retryAt = 0, message?: string) {
    this.show({
      kind: "unknown",
      operation: this.cancellationUnknown ? "cancellation" : "review",
      message:
        message ??
        (this.cancellationUnknown
          ? "请刷新详情查看当前状态，暂时不要重复提交。"
          : UNKNOWN_COPY),
      retryAt,
    });
  }
  private account() {
    const identity = this.dependencies.current.snapshot();
    return identity.kind === "ready" ? identity.account.id : undefined;
  }
  private cancel() {
    if (this.writingAccount) this.uncertainAccount = this.writingAccount;
    this.writingAccount = undefined;
    this.generation++;
    this.request?.abort();
    this.request = undefined;
    clearTimeout(this.timer);
    this.timer = undefined;
  }
  private clearMedia() {
    this.artwork?.close();
    this.artwork = undefined;
    this.template?.close();
    this.template = undefined;
  }
  private changed = () => {
    this.cancel();
    this.clearMedia();
    this.show({ kind: "hidden" });
    const binding = this.dependencies.current.binding();
    if (!binding) return;
    if (
      this.waitingSelection !==
      `${binding.selection.id}/${binding.selection.revision}`
    )
      this.retryAt = 0;
    void this.load(this.page, this.filter);
  };
  close() {
    this.cancel();
    this.clearMedia();
    this.unwatch();
    this.closed = true;
    this.state = { kind: "hidden" };
    this.listeners.clear();
  }
  private scope() {
    const binding = this.dependencies.current.binding();
    if (this.closed || !binding) return;
    this.cancel();
    const generation = this.generation;
    const request = new AbortController();
    this.request = request;
    const active = () =>
      !this.closed &&
      generation === this.generation &&
      this.dependencies.current.matches(binding);
    const guard = async () => {
      if (!active()) return false;
      if (!(await this.dependencies.build().catch(() => false))) {
        if (active()) this.dependencies.current.reload();
        return false;
      }
      return (
        active() &&
        (await this.dependencies.current.confirm(binding)) &&
        active()
      );
    };
    this.timer = setTimeout(() => {
      if (!active()) return;
      const writing = !!this.writingAccount;
      const state = this.state;
      this.cancel();
      this.clearMedia();
      this.retryAt = writing ? 0 : this.dependencies.now() + 5000;
      this.waitingSelection = `${binding.selection.id}/${binding.selection.revision}`;
      if (
        !writing &&
        state.kind === "detail" &&
        !state.pending &&
        state.media.kind === "loading"
      ) {
        this.show({
          ...state,
          media: { kind: "failed", retryAt: this.retryAt },
        });
        return;
      }
      if (writing || this.uncertainAccount === this.account())
        this.unknown(this.retryAt);
      else
        this.show({
          kind: "error",
          message: READ_ERROR,
          retryAt: this.retryAt,
        });
    }, 35000);
    return {
      binding,
      signal: request.signal,
      active,
      guard,
      finish: () => {
        if (active()) {
          clearTimeout(this.timer);
          this.timer = undefined;
          this.request = undefined;
        }
      },
    };
  }
  private async failure(
    result: {
      kind: string;
      status?: number;
      code?: string;
      retryAfter?: number;
    },
    binding: CurrentBinding,
  ) {
    this.clearMedia();
    if (
      result.kind === "failure" &&
      (result.status === 401 || result.status === 403)
    ) {
      await this.dependencies.current.reject(binding, result.status);
      return;
    }
    this.retryAt =
      this.dependencies.now() + Math.max(5000, result.retryAfter ?? 0);
    this.waitingSelection = `${binding.selection.id}/${binding.selection.revision}`;
    const uncertain =
      this.mode.kind === "detail" &&
      this.uncertainAccount === this.account() &&
      (result.status !== 404 || this.cancellationUnknown);
    if (uncertain) {
      this.unknown(
        this.retryAt,
        result.status === 404
          ? "目前无法查看这份申请，仍无法确认刚才的取消是否成功。"
          : undefined,
      );
      return;
    }
    this.show({
      kind: result.status === 404 ? "missing" : "error",
      message:
        result.status === 404
          ? "目前无法查看这份申请。"
          : result.status === 429
            ? "暂时无法加载申请，请稍后再试。"
            : READ_ERROR,
      retryAt: this.retryAt,
    });
  }
  async load(
    page = this.mode.kind === "list" ? 1 : this.page,
    status = this.filter,
    notice?: string,
  ) {
    if (this.dependencies.now() < this.retryAt) {
      if (
        this.dependencies.current.binding() &&
        (this.state.kind !== "detail" || this.state.pending)
      ) {
        this.clearMedia();
        const uncertain = this.uncertainAccount === this.account();
        if (uncertain) this.unknown(this.retryAt);
        else
          this.show({
            kind: "error",
            message: READ_ERROR,
            retryAt: this.retryAt,
          });
      }
      return;
    }
    const scope = this.scope();
    if (!scope) return;
    this.clearMedia();
    this.show({ kind: "loading" });
    this.page = page;
    this.filter = status;
    try {
      if (!(await scope.guard())) return;
      if (this.mode.kind === "list") {
        const result = await this.api.list(
          scope.binding.selection.id,
          page,
          status,
          scope.signal,
        );
        if (!(await scope.guard())) return;
        if (result.kind !== "ok") return this.failure(result, scope.binding);
        this.retryAt = 0;
        this.show({ kind: "list", page, status, result: result.value });
      } else {
        const result = await this.api.detail(
          scope.binding.selection.id,
          this.mode.id,
          scope.signal,
        );
        if (!(await scope.guard())) return;
        if (result.kind !== "ok") return this.failure(result, scope.binding);
        const uncertain = this.uncertainAccount === this.account();
        this.uncertainAccount = undefined;
        this.cancellationUnknown = false;
        this.retryAt = 0;
        this.show({
          kind: "detail",
          result: result.value,
          media: { kind: "loading" },
          pending: false,
          notice:
            notice ??
            (uncertain && result.value.application.status === "submitted"
              ? "当前仍待审核，请查看申请内容后再作决定。"
              : undefined),
        });
        await this.media(result.value, scope);
      }
    } catch {
      if (scope.active())
        await this.failure({ kind: "disconnected" }, scope.binding);
    } finally {
      scope.finish();
    }
  }
  private async media(
    result: ManagedApplicationDetail,
    scope: NonNullable<ReturnType<Reviews["scope"]>>,
  ) {
    const session = scope.binding.selection.id,
      item = result.application;
    const template = await this.api.artwork(
      session,
      item.snapshot.template,
      scope.signal,
    );
    if (!(await scope.guard())) {
      if (template.kind === "ok") template.value.close();
      return;
    }
    if (template.kind !== "ok")
      return this.mediaFailure(template, scope.binding);
    this.template = template.value;
    const photo = await this.api.resource(
      session,
      item.photo.resourceId,
      scope.signal,
    );
    if (!(await scope.guard())) return;
    if (photo.kind !== "ok") return this.mediaFailure(photo, scope.binding);
    const artwork = await template.value.preview(
      item.snapshot,
      item.photo,
      photo.value.blob,
    );
    if (!(await scope.guard())) {
      if (artwork.kind === "ok") artwork.value.close();
      return;
    }
    if (artwork.kind !== "ok") return this.mediaFailure(artwork, scope.binding);
    this.artwork = artwork.value;
    if (this.state.kind === "detail")
      this.show({
        ...this.state,
        media: { kind: "ready", artwork: artwork.value },
      });
  }
  private async mediaFailure(
    result: { kind: string; status?: number; retryAfter?: number },
    binding: CurrentBinding,
  ) {
    this.clearMedia();
    if (
      result.kind === "failure" &&
      (result.status === 401 || result.status === 403)
    ) {
      await this.dependencies.current.reject(binding, result.status);
      return;
    }
    if (this.state.kind === "detail") {
      this.retryAt =
        this.dependencies.now() + Math.max(5000, result.retryAfter ?? 0);
      this.waitingSelection = `${binding.selection.id}/${binding.selection.revision}`;
      this.show({
        ...this.state,
        media: {
          kind: "failed",
          retryAt: this.retryAt,
        },
      });
    }
  }
  mediaFailed() {
    if (this.state.kind !== "detail" || this.state.pending) return;
    const binding = this.dependencies.current.binding();
    if (!binding) return;
    this.clearMedia();
    this.retryAt = this.dependencies.now() + 5000;
    this.waitingSelection = `${binding.selection.id}/${binding.selection.revision}`;
    this.show({
      ...this.state,
      confirmation: undefined,
      media: { kind: "failed", retryAt: this.retryAt },
    });
  }
  confirm(decision: ReviewInput["decision"] | "cancel") {
    const state = this.state;
    if (
      state.kind !== "detail" ||
      state.pending ||
      (decision === "cancel"
        ? !["submitted", "approved"].includes(state.result.application.status)
        : state.result.application.status !== "submitted")
    )
      return;
    if (
      decision === "approve" &&
      (state.media.kind !== "ready" ||
        !state.result.current.accountActive ||
        !state.result.current.participationActive)
    )
      return;
    this.show({ ...state, confirmation: decision });
  }
  dismiss() {
    if (this.state.kind === "detail" && !this.state.pending)
      this.show({ ...this.state, confirmation: undefined });
  }
  async submit(reason: string) {
    const state = this.state;
    if (state.kind === "detail" && state.confirmation === "cancel")
      return this.submitCancellation(reason);
    if (
      state.kind !== "detail" ||
      state.pending ||
      !state.confirmation ||
      state.result.application.status !== "submitted"
    )
      return;
    if (
      state.confirmation === "approve" &&
      (state.media.kind !== "ready" ||
        !state.result.current.accountActive ||
        !state.result.current.participationActive)
    )
      return;
    if (
      state.confirmation === "reject" &&
      (!reason.trim() || reason.includes("\0"))
    )
      return;
    const input: ReviewInput =
      state.confirmation === "approve"
        ? { decision: "approve" }
        : { decision: "reject", reason };
    if (new TextEncoder().encode(JSON.stringify(input)).byteLength > 8192)
      return;
    const scope = this.scope();
    if (!scope) return;
    this.show({ ...state, pending: true });
    try {
      if (!(await scope.guard())) return;
      this.writingAccount = this.account();
      const result = await this.api.review(
        scope.binding.selection.id,
        state.result.application.id,
        input,
        scope.signal,
      );
      if (!(await scope.guard())) return;
      this.writingAccount = undefined;
      if (result.kind === "ok") {
        const item = result.value,
          original = state.result.application;
        const identity = this.dependencies.current.snapshot();
        if (
          item.status === "submitted" ||
          identity.kind !== "ready" ||
          item.reviewerId !== identity.account.personId ||
          item.kind !== original.kind ||
          item.submittedAt !== original.submittedAt ||
          !sameApplicationContent(item.snapshot, original.snapshot) ||
          !sameApplicationContent(item.photo, original.photo)
        ) {
          this.clearMedia();
          this.uncertainAccount = this.account();
          this.unknown();
          return;
        }
        if (state.media.kind === "loading") this.clearMedia();
        this.show({
          ...state,
          result: { ...state.result, application: item },
          media:
            state.media.kind === "loading"
              ? { kind: "failed", retryAt: 0 }
              : state.media,
          pending: false,
          confirmation: undefined,
          notice: input.decision === "approve" ? "审核通过。" : "申请已驳回。",
        });
      } else if (result.kind !== "failure" || result.status >= 500) {
        this.uncertainAccount = this.account();
        this.clearMedia();
        this.retryAt =
          result.kind === "failure"
            ? this.dependencies.now() + result.retryAfter
            : 0;
        this.waitingSelection = `${scope.binding.selection.id}/${scope.binding.selection.revision}`;
        this.unknown(this.retryAt);
      } else if (
        result.code === "state_changed" ||
        result.code === "applicant_unavailable"
      ) {
        await this.load(
          this.page,
          this.filter,
          result.code === "state_changed"
            ? "申请状态已更新，请查看当前结果。"
            : "申请人目前不能继续办理工牌，无法批准。可填写理由驳回申请。",
        );
      } else await this.failure(result, scope.binding);
    } catch {
      if (scope.active()) {
        this.uncertainAccount = this.account();
        this.clearMedia();
        this.unknown();
      }
    } finally {
      scope.finish();
    }
  }
  private async submitCancellation(reason: string) {
    const state = this.state;
    if (
      state.kind !== "detail" ||
      state.pending ||
      state.confirmation !== "cancel" ||
      !reason.trim() ||
      reason.includes("\0")
    )
      return;
    const expectedStatus = state.result.application.status;
    if (expectedStatus !== "submitted" && expectedStatus !== "approved") return;
    const input = { mode: "manager", expectedStatus, reason } as const;
    if (new TextEncoder().encode(JSON.stringify(input)).byteLength > 8192)
      return;
    const scope = this.scope();
    if (!scope) return;
    this.show({ ...state, pending: true });
    try {
      if (!(await scope.guard())) return;
      this.writingAccount = this.account();
      this.cancellationUnknown = true;
      const result = await this.api.cancellation(
        scope.binding.selection.id,
        state.result.application.id,
        input,
        scope.signal,
      );
      if (!(await scope.guard())) return;
      this.writingAccount = undefined;
      if (result.kind === "ok") {
        this.uncertainAccount = undefined;
        this.cancellationUnknown = false;
        this.clearMedia();
        this.show({ kind: "cancelled" });
      } else if (result.kind !== "failure" || result.status >= 500) {
        this.uncertainAccount = this.account();
        this.clearMedia();
        this.retryAt =
          result.kind === "failure"
            ? this.dependencies.now() + result.retryAfter
            : 0;
        this.waitingSelection = `${scope.binding.selection.id}/${scope.binding.selection.revision}`;
        this.unknown(this.retryAt);
      } else {
        this.cancellationUnknown = false;
        if (result.code === "state_changed")
          await this.load(
            this.page,
            this.filter,
            "申请状态已更新。请核对当前状态后重新确认取消。",
          );
        else await this.failure(result, scope.binding);
      }
    } catch {
      if (scope.active()) {
        this.cancel();
        this.clearMedia();
        this.unknown();
      }
    } finally {
      scope.finish();
    }
  }
}
