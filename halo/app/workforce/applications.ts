import type { Current, CurrentBinding } from "../authentication/current";
import type { Workforce } from "./status";
import { ApplicationPhoto as LocalPhoto, type ConfirmedPhoto } from "./photo";
import { directories, type DirectorySelection } from "../protocol/directories";
import { currentNumberValue, type FixedNumber } from "../protocol/number";
import {
  applicationTemplate,
  prepareApplicationPhoto,
  previewApplication,
  submitApplication,
  cancelApplication,
  application,
  applications,
  applicationResource,
  sameApplicationContent,
  type Application,
  type ApplicationDetail,
  type ApplicationList,
  type ApplicationPreview,
  type ApplicationTemplate,
  type PhotoMetadata,
  type PreviewInput,
} from "../protocol/applications";
import {
  loadTemplateArtwork,
  type TemplateArtwork,
  type ApplicationArtwork,
} from "./artwork";
import type { ApplicationChoices } from "./application-fields";
import { applicationBadge } from "../protocol/applications";
import { loadBadgeImage, type BadgeImage } from "./badge-image";
import { BadgeDownloads } from "./badge-downloads";
import type { BadgeMedia, BadgeDownload } from "./badge-display";

export type ApplicationMode =
  { kind: "new" } | { kind: "list" } | { kind: "detail"; id: string };
export type ApplicationContext = Readonly<{
  directories: DirectorySelection;
  template: ApplicationTemplate;
  number: FixedNumber;
  choices: ApplicationChoices;
  artwork: TemplateArtwork;
}>;
export type ApplicationState =
  | { kind: "hidden" | "loading"; notice?: string }
  | {
      kind: "error" | "blocked";
      message: string;
      retryAt: number;
      notice?: string;
    }
  | {
      kind: "edit";
      context: ApplicationContext;
      message?: string;
      retryAt: number;
    }
  | { kind: "working"; context: ApplicationContext }
  | {
      kind: "preview";
      context: ApplicationContext;
      preview: ApplicationPreview;
      artwork: ApplicationArtwork;
    }
  | {
      kind: "submitting";
      context: ApplicationContext;
      preview: ApplicationPreview;
      artwork: ApplicationArtwork;
    }
  | { kind: "unknown" }
  | { kind: "cancellation-unknown"; retryAt: number; message: string }
  | { kind: "cancelled" }
  | {
      kind: "created";
      application: Pick<Application, "id" | "kind" | "status" | "submittedAt">;
    }
  | { kind: "list"; page: number; result: ApplicationList }
  | {
      kind: "detail";
      result: ApplicationDetail;
      artwork?: ApplicationArtwork;
      media?: "loading" | "failed";
      retryAt?: number;
      cancellation?: boolean;
      pending?: boolean;
      notice?: string;
      badge?: BadgeMedia;
      download?: BadgeDownload;
    };

const READ_ERROR = "暂时无法加载，请刷新后重试。";
export const APPLICATION_FAILURE_COPY: Record<string, string> = {
  data_changed: "工牌内容已变化，请重新加载申请表后预览并确认。",
  number_required: "当前没有可用工号，暂时无法申请工牌。",
  selection_unavailable:
    "所选科室、职务、职称或领取地点已不可用，请重新加载申请表后选择。",
  missing_glyph: "工牌文字无法完整显示，请联系工牌管理人员处理。",
  text_overflow: "工牌文字无法完整显示，请联系工牌管理人员处理。",
  target_unavailable: "无法使用这项内容，请刷新后重新准备。",
  invalid_input: "请检查填写内容和照片后重试。",
  rate_limited: "暂时无法处理，请稍后再试。",
};

const DEFAULT_API = {
  directories,
  number: currentNumberValue,
  template: applicationTemplate,
  photo: prepareApplicationPhoto,
  preview: previewApplication,
  submit: submitApplication,
  cancellation: cancelApplication,
  list: applications,
  detail: application,
  resource: applicationResource,
  artwork: loadTemplateArtwork,
  badge: loadBadgeImage,
};
type Dependencies = {
  current: Current;
  workforce: Workforce;
  build(): Promise<boolean>;
  now(): number;
  api?: Omit<typeof DEFAULT_API, "badge"> &
    Partial<Pick<typeof DEFAULT_API, "badge">>;
};

/** Owns one visible application page. No business material is persisted or replayed. */
export class Applications {
  readonly photo = new LocalPhoto();
  private state: ApplicationState = { kind: "hidden" };
  private readonly listeners = new Set<() => void>();
  private readonly unwatch: (() => void)[];
  private readonly api: typeof DEFAULT_API;
  private context?: ApplicationContext;
  private templateArtwork?: TemplateArtwork;
  private previewArtwork?: ApplicationArtwork;
  private uploaded?: { confirmed: ConfirmedPhoto; photo: PhotoMetadata };
  private generation = 0;
  private request?: AbortController;
  private timer?: ReturnType<typeof setTimeout>;
  private uncertainAccount?: string;
  private submittingAccount?: string;
  private cancellingAccount?: string;
  private uncertainCancellation?: string;
  private page = 1;
  private retryAt = 0;
  private waitingSelection?: string;
  private completed?: {
    account: string;
    application: Pick<Application, "id" | "kind" | "status" | "submittedAt">;
  };
  private closed = false;
  private confirmedCancellationAccount?: string;
  private badgeImage?: BadgeImage;
  private readonly downloads: BadgeDownloads;
  constructor(
    readonly mode: ApplicationMode,
    private readonly dependencies: Dependencies,
  ) {
    this.api = { ...DEFAULT_API, ...dependencies.api };
    this.downloads = new BadgeDownloads({
      ...dependencies,
      read: this.api.badge,
      unavailable: () => this.badgeUnavailable(),
      changed: (download) => {
        if (this.state.kind === "detail")
          this.show({ ...this.state, download });
      },
    });
    this.unwatch = [
      dependencies.current.subscribe(this.changed),
      dependencies.workforce.subscribe(this.changed),
      this.photo.subscribe(this.photoChanged),
    ];
    this.changed();
  }
  snapshot = () => this.state;
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };
  private show(state: ApplicationState) {
    if (!this.closed) {
      this.state = state;
      for (const listener of this.listeners) listener();
    }
  }
  private account() {
    const state = this.dependencies.current.snapshot();
    return state.kind === "ready" ? state.account.id : undefined;
  }
  private cancel() {
    this.downloads.clear();
    if (this.cancellingAccount)
      this.uncertainCancellation = this.cancellingAccount;
    this.cancellingAccount = undefined;
    if (this.submittingAccount) this.uncertainAccount = this.submittingAccount;
    this.submittingAccount = undefined;
    this.generation++;
    this.request?.abort();
    this.request = undefined;
    clearTimeout(this.timer);
    this.timer = undefined;
  }
  private clear() {
    this.clearPreview();
    this.badgeImage?.close();
    this.badgeImage = undefined;
    this.downloads.clear();
  }
  private clearPreview() {
    this.previewArtwork?.close();
    this.previewArtwork = undefined;
    this.templateArtwork?.close();
    this.templateArtwork = undefined;
    this.context = undefined;
    this.uploaded = undefined;
    this.photo.clear();
  }
  private changed = () => {
    this.confirmedCancellationAccount = undefined;
    this.cancel();
    this.clear();
    this.show({ kind: "hidden" });
    const identity = this.dependencies.current.binding();
    const service = this.dependencies.workforce.snapshot();
    if (!identity) return;
    if (
      this.waitingSelection !==
      `${identity.selection.id}/${identity.selection.revision}`
    )
      this.retryAt = 0;
    const completed = this.completed;
    if (
      completed &&
      completed.account === this.account() &&
      this.mode.kind === "new"
    ) {
      this.show({ kind: "created", application: completed.application });
      return;
    }
    if (this.uncertainAccount === this.account() && this.mode.kind === "new") {
      this.show({ kind: "unknown" });
      return;
    }
    if (this.mode.kind !== "new" || service.kind === "ready")
      void this.load(this.page);
    else if (service.kind === "unavailable" || service.kind === "limited")
      this.show({
        kind: "error",
        message: READ_ERROR,
        retryAt: service.retryAt,
      });
    else this.show({ kind: "loading" });
  };
  private photoChanged = () => {
    if (!this.context) return;
    const state = this.photo.snapshot();
    if (
      state.kind !== "editing" ||
      state.confirmed !== this.uploaded?.confirmed
    )
      this.uploaded = undefined;
    if (this.state.kind === "preview") this.edit(this.context.choices);
  };
  close() {
    if (this.closed) return;
    this.cancel();
    this.clear();
    this.unwatch.forEach((fn) => fn());
    this.photo.close();
    this.closed = true;
    this.state = { kind: "hidden" };
    this.listeners.clear();
  }
  private scope() {
    const binding = this.dependencies.current.binding();
    if (!binding || this.closed) return;
    this.cancel();
    const generation = this.generation;
    const service = this.dependencies.workforce.snapshot();
    const request = new AbortController();
    this.request = request;
    const active = () =>
      !this.closed &&
      generation === this.generation &&
      this.dependencies.current.matches(binding) &&
      this.dependencies.workforce.snapshot() === service;
    this.timer = setTimeout(() => {
      if (!active()) return;
      const unknown = !!this.submittingAccount;
      const cancelling = !!this.cancellingAccount;
      const state = this.state;
      this.cancel();
      const retryAt = this.dependencies.now() + 5000;
      if (this.mode.kind !== "new") {
        this.retryAt = retryAt;
        this.waitingSelection = `${binding.selection.id}/${binding.selection.revision}`;
      }
      if (cancelling || this.uncertainCancellation === this.account()) {
        this.clear();
        this.show({
          kind: "cancellation-unknown",
          retryAt,
          message: "请刷新详情查看当前状态，暂时不要重复提交。",
        });
        return;
      }
      if (
        !unknown &&
        state.kind === "detail" &&
        state.result.access === "full" &&
        (state.media === "loading" || state.badge?.kind === "loading")
      ) {
        if (state.media === "loading") this.clearPreview();
        this.show({
          ...state,
          media: state.media === "loading" ? "failed" : state.media,
          retryAt: state.media === "loading" ? retryAt : state.retryAt,
          badge:
            state.badge?.kind === "loading"
              ? { kind: "failed", retryAt }
              : state.badge,
          download: { kind: "idle" },
        });
        return;
      }
      this.clear();
      this.show(
        unknown
          ? { kind: "unknown" }
          : {
              kind: "error",
              message: READ_ERROR,
              retryAt,
              notice: this.cancellationNotice(),
            },
      );
    }, 35000);
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
    editing = false,
    displayMessage?: string,
  ) {
    if (result.status === 401 || result.status === 403 || result.status === 404)
      this.confirmedCancellationAccount = undefined;
    if (
      result.kind === "failure" &&
      (result.status === 401 || result.status === 403)
    ) {
      await this.dependencies.current.reject(binding, result.status);
      return;
    }
    const retryAt =
      this.dependencies.now() + Math.max(5000, result.retryAfter ?? 0);
    this.retryAt = retryAt;
    this.waitingSelection = `${binding.selection.id}/${binding.selection.revision}`;
    const notice = this.cancellationNotice();
    const message = notice
      ? "详情暂时无法加载，请刷新后重试。"
      : (displayMessage ??
        (result.kind === "artwork-error"
          ? "工牌预览无法完整显示，请联系工牌管理人员处理。"
          : (APPLICATION_FAILURE_COPY[result.code ?? ""] ?? READ_ERROR)));
    if (editing && this.context)
      this.show({ kind: "edit", context: this.context, message, retryAt });
    else {
      this.clear();
      this.show(
        this.uncertainCancellation === this.account()
          ? {
              kind: "cancellation-unknown",
              retryAt,
              message:
                result.status === 404
                  ? "目前无法查看这份申请，仍无法确认刚才的取消是否成功。可返回列表查看当前允许访问的记录。"
                  : "请刷新详情查看当前状态，暂时不要重复提交。",
            }
          : { kind: "error", message, retryAt, notice },
      );
    }
  }
  async load(
    page = 1,
    notice?: string,
    badgeUnavailable = false,
  ): Promise<void> {
    if (this.dependencies.now() < this.retryAt) {
      this.show(
        this.uncertainCancellation === this.account()
          ? {
              kind: "cancellation-unknown",
              message: "请刷新详情查看当前状态，暂时不要重复提交。",
              retryAt: this.retryAt,
            }
          : {
              kind: "error",
              message: READ_ERROR,
              retryAt: this.retryAt,
              notice: this.cancellationNotice(),
            },
      );
      return;
    }
    const scope = this.scope();
    if (!scope) return;
    this.clear();
    this.show({ kind: "loading", notice: this.cancellationNotice() });
    this.page = page;
    try {
      if (!(await scope.guard())) return;
      const session = scope.binding.selection.id;
      if (this.mode.kind === "new") {
        const service = this.dependencies.workforce.snapshot();
        if (service.kind !== "ready" || service.status !== "active") {
          this.show({
            kind: "blocked",
            message: "目前无法提交新的工牌申请。可在“我的申请”查看记录。",
            retryAt: 0,
          });
          return;
        }
        if (this.uncertainAccount === this.account()) {
          this.show({ kind: "unknown" });
          return;
        }
        const [directory, template, number] = await Promise.all([
          this.api.directories(session, scope.signal),
          this.api.template(session, scope.signal),
          this.api.number(session, scope.signal),
        ]);
        if (!(await scope.guard())) return;
        if (directory.kind !== "directories")
          return this.failure(directory, scope.binding);
        if (template.kind !== "ok")
          return this.failure(template, scope.binding);
        if (number.kind !== "value") return this.failure(number, scope.binding);
        if (number.number === null) {
          this.show({
            kind: "blocked",
            message: APPLICATION_FAILURE_COPY.number_required,
            retryAt: 0,
          });
          return;
        }
        const rule = directory.selection.rules.find(
          (item) => item.id === number.number!.subtype,
        );
        if (
          !rule ||
          directory.selection.missing.length ||
          !template.value.pickups.length
        )
          return this.failure(
            { kind: "failure", code: "dependency_unavailable" },
            scope.binding,
          );
        const artwork = await this.api.artwork(
          session,
          template.value.template,
          scope.signal,
        );
        if (!scope.active()) {
          if (artwork.kind === "ok") artwork.value.close();
          return;
        }
        if (!(await scope.guard())) {
          if (artwork.kind === "ok") artwork.value.close();
          return;
        }
        if (artwork.kind !== "ok") return this.failure(artwork, scope.binding);
        this.templateArtwork = artwork.value;
        this.context = {
          directories: directory.selection,
          template: template.value,
          number: number.number,
          artwork: artwork.value,
          choices: {
            kind: "issuance",
            departmentId: rule.selectable ? rule.departmentId : "",
            departmentConfirmed: false,
            rosterId: "",
            titleId: "",
            pickup: "",
          },
        };
        this.retryAt = 0;
        this.show({ kind: "edit", context: this.context, retryAt: 0 });
      } else if (this.mode.kind === "list") {
        const result = await this.api.list(session, page, scope.signal);
        if (!(await scope.guard())) return;
        if (result.kind !== "ok") return this.failure(result, scope.binding);
        this.retryAt = 0;
        this.show({
          kind: "list",
          page,
          result: result.value,
        });
      } else {
        const result = await this.api.detail(
          session,
          this.mode.id,
          scope.signal,
        );
        if (!(await scope.guard())) return;
        if (result.kind !== "ok")
          return this.failure(
            result,
            scope.binding,
            false,
            result.kind === "failure" && result.status === 404
              ? "目前无法查看这份申请。"
              : undefined,
          );
        this.retryAt = 0;
        const uncertain = this.uncertainCancellation === this.account();
        this.uncertainCancellation = undefined;
        notice ??= uncertain
          ? "已读取当前状态。请核对申请后再决定是否取消。"
          : undefined;
        if (result.value.access === "restricted") {
          this.confirmedCancellationAccount = undefined;
          this.show({ kind: "detail", result: result.value, notice });
          return;
        }
        const item = result.value.application;
        this.confirmedCancellationAccount = undefined;
        this.show({
          kind: "detail",
          result: result.value,
          media: "loading",
          badge: applicationBadge(item)
            ? badgeUnavailable
              ? { kind: "failed", retryAt: this.dependencies.now() + 5000 }
              : { kind: "loading" }
            : undefined,
          download: { kind: "idle" },
          notice,
        });
        await Promise.all([
          this.detailPreview(result.value, scope),
          badgeUnavailable ? undefined : this.detailBadge(result.value, scope),
        ]);
      }
    } catch {
      if (scope.active()) {
        if (this.state.kind === "detail")
          await this.detailMediaFailure(
            { kind: "disconnected" },
            scope.binding,
            this.state.result,
          );
        else await this.failure({ kind: "disconnected" }, scope.binding);
      }
    } finally {
      scope.finish();
    }
  }
  private async detailPreview(
    detail: ApplicationDetail,
    scope: NonNullable<ReturnType<Applications["scope"]>>,
  ) {
    if (detail.access !== "full") return;
    const session = scope.binding.selection.id;
    const item = detail.application;
    try {
      const resources = await this.api.artwork(
        session,
        item.snapshot.template,
        scope.signal,
      );
      if (!scope.active()) {
        if (resources.kind === "ok") resources.value.close();
        return;
      }
      if (!(await scope.guard())) {
        if (resources.kind === "ok") resources.value.close();
        return;
      }
      if (resources.kind !== "ok")
        return this.detailMediaFailure(resources, scope.binding, detail);
      this.templateArtwork = resources.value;
      const photo = await this.api.resource(
        session,
        item.photo.resourceId,
        scope.signal,
      );
      if (!(await scope.guard())) return;
      if (photo.kind !== "ok")
        return this.detailMediaFailure(photo, scope.binding, detail);
      const artwork = await resources.value.preview(
        item.snapshot,
        item.photo,
        photo.value.blob,
      );
      if (!scope.active()) {
        if (artwork.kind === "ok") artwork.value.close();
        return;
      }
      if (!(await scope.guard())) {
        if (artwork.kind === "ok") artwork.value.close();
        return;
      }
      if (artwork.kind !== "ok")
        return this.detailMediaFailure(artwork, scope.binding, detail);
      this.previewArtwork = artwork.value;
      this.show({
        ...(this.state.kind === "detail" ? this.state : {}),
        kind: "detail",
        result: detail,
        artwork: artwork.value,
        media: undefined,
      });
    } catch {
      if (scope.active()) {
        if (this.state.kind === "detail")
          await this.detailMediaFailure(
            { kind: "disconnected" },
            scope.binding,
            this.state.result,
          );
        else await this.failure({ kind: "disconnected" }, scope.binding);
      }
    }
  }
  private async detailBadge(
    detail: ApplicationDetail,
    scope: NonNullable<ReturnType<Applications["scope"]>>,
  ) {
    if (detail.access !== "full") return;
    const badge = applicationBadge(detail.application);
    if (!badge) return;
    const image = await this.api
      .badge(scope.binding.selection.id, badge.id, scope.signal)
      .catch(() => ({ kind: "disconnected" }) as const);
    if (!(await scope.guard())) {
      if (image.kind === "ok") image.value.close();
      return;
    }
    if (image.kind !== "ok") {
      if (image.kind === "failure" && image.status === 404) {
        this.badgeUnavailable();
        return;
      }
      if (
        image.kind === "failure" &&
        (image.status === 401 || image.status === 403)
      ) {
        await this.dependencies.current.reject(scope.binding, image.status);
        return;
      }
      this.badgeFailed(image.kind === "failure" ? image.retryAfter : 0);
      return;
    }
    this.badgeImage = image.value;
    if (this.state.kind === "detail")
      this.show({
        ...this.state,
        badge: { kind: "ready", image: image.value },
      });
  }
  private badgeUnavailable() {
    this.cancel();
    this.clear();
    this.retryAt = 0;
    this.show({ kind: "loading" });
    void this.load(this.page, undefined, true);
  }
  badgeFailed(delay = 0) {
    if (this.state.kind !== "detail" || !this.state.badge) return;
    this.badgeImage?.close();
    this.badgeImage = undefined;
    this.show({
      ...this.state,
      badge: {
        kind: "failed",
        retryAt: this.dependencies.now() + Math.max(5000, delay),
      },
    });
  }
  downloadBadge() {
    const state = this.state;
    if (
      state.kind !== "detail" ||
      state.pending ||
      state.result.access !== "full" ||
      state.badge?.kind !== "ready"
    )
      return;
    const badge = applicationBadge(state.result.application);
    if (badge) void this.downloads.start(badge.id);
  }
  confirmCancellation() {
    const state = this.state;
    if (
      state.kind !== "detail" ||
      state.pending ||
      !["submitted", "approved", "ready"].includes(
        state.result.application.status,
      )
    )
      return;
    this.show({ ...state, cancellation: true });
  }
  dismissCancellation() {
    if (this.state.kind === "detail" && !this.state.pending)
      this.show({ ...this.state, cancellation: false });
  }
  async cancelApplication() {
    const state = this.state;
    if (state.kind !== "detail" || !state.cancellation || state.pending) return;
    const expectedStatus = state.result.application.status;
    if (
      expectedStatus !== "submitted" &&
      expectedStatus !== "approved" &&
      expectedStatus !== "ready"
    )
      return;
    const scope = this.scope();
    if (!scope) return;
    this.show({ ...state, pending: true });
    try {
      if (!(await scope.guard())) return;
      this.cancellingAccount = this.account();
      const result = await this.api.cancellation(
        scope.binding.selection.id,
        state.result.application.id,
        { mode: "self", expectedStatus },
        scope.signal,
      );
      if (!(await scope.guard())) return;
      this.cancellingAccount = undefined;
      if (result.kind === "ok") {
        this.uncertainCancellation = undefined;
        this.clear();
        if (expectedStatus === "ready" && state.result.access === "full") {
          this.confirmedCancellationAccount = this.account();
          await this.load(1, "申请已取消，已生成的工牌图片仍保留。");
        } else this.show({ kind: "cancelled" });
      } else if (result.kind !== "failure" || result.status >= 500) {
        this.uncertainCancellation = this.account();
        this.clear();
        this.retryAt =
          result.kind === "failure"
            ? this.dependencies.now() + result.retryAfter
            : 0;
        this.waitingSelection = `${scope.binding.selection.id}/${scope.binding.selection.revision}`;
        this.show({
          kind: "cancellation-unknown",
          retryAt: this.retryAt,
          message: "请刷新详情查看当前状态，暂时不要重复提交。",
        });
      } else if (result.code === "state_changed") {
        await this.load(1, "申请状态已更新。请核对当前状态后重新确认取消。");
      } else await this.failure(result, scope.binding);
    } catch {
      if (scope.active()) {
        this.cancel();
        this.clear();
        this.show({
          kind: "cancellation-unknown",
          retryAt: 0,
          message: "请刷新详情查看当前状态，暂时不要重复提交。",
        });
      }
    } finally {
      scope.finish();
    }
  }
  private cancellationNotice() {
    return this.confirmedCancellationAccount &&
      this.confirmedCancellationAccount === this.account()
      ? "申请已取消，已生成的工牌图片仍保留。"
      : undefined;
  }
  private async detailMediaFailure(
    result: { kind: string; status?: number; retryAfter?: number },
    binding: CurrentBinding,
    detail: ApplicationDetail,
  ) {
    if (
      result.kind === "failure" &&
      (result.status === 401 || result.status === 403)
    )
      return this.failure(result, binding);
    this.clearPreview();
    this.retryAt =
      this.dependencies.now() + Math.max(5000, result.retryAfter ?? 0);
    this.waitingSelection = `${binding.selection.id}/${binding.selection.revision}`;
    this.show({
      ...(this.state.kind === "detail" ? this.state : {}),
      kind: "detail",
      result: detail,
      media: "failed",
      artwork: undefined,
      retryAt: this.retryAt,
    });
  }
  edit(choices: ApplicationChoices) {
    if (!this.context || this.submittingAccount || this.closed) return;
    this.cancel();
    this.previewArtwork?.close();
    this.previewArtwork = undefined;
    this.context = { ...this.context, choices };
    this.show({ kind: "edit", context: this.context, retryAt: this.retryAt });
  }
  async preview(): Promise<void> {
    const context = this.context,
      selectedPhoto = this.photo.snapshot();
    if (
      !context ||
      this.state.kind !== "edit" ||
      selectedPhoto.kind !== "editing" ||
      !selectedPhoto.confirmed ||
      this.dependencies.now() < this.retryAt
    )
      return;
    const choices = context.choices;
    if (
      !choices.departmentConfirmed ||
      !choices.departmentId ||
      !choices.pickup
    )
      return;
    const scope = this.scope();
    if (!scope) return;
    const confirmed = selectedPhoto.confirmed;
    this.show({ kind: "working", context });
    try {
      if (!(await scope.guard())) return;
      const session = scope.binding.selection.id;
      if (this.uploaded?.confirmed !== confirmed) {
        const uploaded = await this.api.photo(
          session,
          confirmed.file,
          scope.signal,
        );
        if (!(await scope.guard())) return;
        if (uploaded.kind !== "ok")
          return this.failure(uploaded, scope.binding, true);
        this.uploaded = { confirmed, photo: uploaded.value };
      }
      const input: PreviewInput = {
        kind: choices.kind,
        selections: {
          departmentId: choices.departmentId,
          rosterId: choices.rosterId || null,
          titleId: choices.titleId || null,
          pickup: choices.pickup,
          photoId: this.uploaded.photo.photoId,
        },
      };
      const result = await this.api.preview(session, input, scope.signal);
      if (!(await scope.guard())) return;
      if (result.kind !== "ok")
        return this.failure(result, scope.binding, true);
      if (
        !sameApplicationContent(
          result.value.snapshot.template,
          context.template.template,
        )
      ) {
        this.uploaded = undefined;
        this.photo.clear();
        return this.failure(
          { kind: "failure", code: "data_changed" },
          scope.binding,
          true,
        );
      }
      const resource = await this.api.resource(
        session,
        result.value.photo.resourceId,
        scope.signal,
      );
      if (!(await scope.guard())) return;
      if (resource.kind !== "ok")
        return this.failure(resource, scope.binding, true);
      const artwork = await context.artwork.preview(
        result.value.snapshot,
        result.value.photo,
        resource.value.blob,
      );
      if (!scope.active()) {
        if (artwork.kind === "ok") artwork.value.close();
        return;
      }
      if (!(await scope.guard())) {
        if (artwork.kind === "ok") artwork.value.close();
        return;
      }
      if (artwork.kind !== "ok")
        return this.failure(artwork, scope.binding, true);
      this.previewArtwork = artwork.value;
      this.show({
        kind: "preview",
        context,
        preview: result.value,
        artwork: artwork.value,
      });
    } catch {
      if (scope.active())
        await this.failure({ kind: "disconnected" }, scope.binding, true);
    } finally {
      scope.finish();
    }
  }
  async submit(): Promise<void> {
    const state = this.state;
    if (state.kind !== "preview" || !this.uploaded) return;
    const scope = this.scope();
    if (!scope) return;
    const choices = state.context.choices;
    this.show({ ...state, kind: "submitting" });
    try {
      if (!(await scope.guard())) return;
      this.submittingAccount = this.account();
      const result = await this.api.submit(
        scope.binding.selection.id,
        {
          kind: state.preview.kind,
          selections: {
            departmentId: choices.departmentId,
            rosterId: choices.rosterId || null,
            titleId: choices.titleId || null,
            pickup: choices.pickup,
            photoId: this.uploaded.photo.photoId,
          },
          expected: state.preview.snapshot,
        },
        scope.signal,
      );
      if (!scope.active()) return;
      if (!(await scope.guard())) return;
      if (result.kind === "ok") {
        this.submittingAccount = undefined;
        this.uncertainAccount = undefined;
        const { id, kind, status, submittedAt } = result.value;
        const summary = { id, kind, status, submittedAt };
        this.completed = { account: this.account()!, application: summary };
        this.clear();
        this.show({ kind: "created", application: summary });
      } else if (
        result.kind === "untrusted" ||
        result.kind === "disconnected" ||
        (result.kind === "failure" && result.status >= 500)
      ) {
        this.uncertainAccount = this.submittingAccount;
        this.submittingAccount = undefined;
        this.clear();
        this.show({ kind: "unknown" });
      } else {
        this.submittingAccount = undefined;
        this.previewArtwork?.close();
        this.previewArtwork = undefined;
        await this.failure(result, scope.binding, true);
      }
    } catch {
      if (scope.active()) {
        this.uncertainAccount = this.submittingAccount;
        this.submittingAccount = undefined;
        this.clear();
        this.show({ kind: "unknown" });
      }
    } finally {
      scope.finish();
    }
  }
  previewFailed() {
    if (this.state.kind === "detail" && this.state.pending) return;
    if (this.state.kind === "detail") {
      this.clearPreview();
      this.show({
        ...this.state,
        artwork: undefined,
        media: "failed",
        retryAt: this.dependencies.now() + 5000,
      });
    } else if (this.state.kind === "preview" && this.context) {
      this.previewArtwork?.close();
      this.previewArtwork = undefined;
      this.show({
        kind: "edit",
        context: this.context,
        message: "工牌预览无法完整显示，请重新预览后再提交。",
        retryAt: 0,
      });
    } else if (this.state.kind === "submitting") {
      const unknown = !!this.submittingAccount;
      this.cancel();
      this.clear();
      this.show(
        unknown
          ? { kind: "unknown" }
          : {
              kind: "error",
              message: "工牌预览无法完整显示，请重新加载申请表后重试。",
              retryAt: 0,
            },
      );
    }
  }
}
