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
  | { kind: "hidden" | "loading" }
  | { kind: "error" | "blocked"; message: string; retryAt: number }
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
    };

const READ_ERROR = "暂时无法加载，请刷新后重试。";
export const APPLICATION_FAILURE_COPY: Record<string, string> = {
  data_changed: "工牌内容已变化，请刷新办理内容后重新预览并确认。",
  number_required: "当前没有可用工号，请先申请工号。",
  selection_unavailable:
    "所选科室、职务、职称或领取地点已不可用，请刷新后重新选择。",
  missing_glyph: "工牌字体无法显示全部文字，请联系管理员调整字体后再办理。",
  text_overflow: "完整文字无法放入当前工牌，请联系管理员调整模板后再办理。",
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
};
type Dependencies = {
  current: Current;
  workforce: Workforce;
  build(): Promise<boolean>;
  now(): number;
  api?: typeof DEFAULT_API;
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
  constructor(
    readonly mode: ApplicationMode,
    private readonly dependencies: Dependencies,
  ) {
    this.api = dependencies.api ?? DEFAULT_API;
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
    this.previewArtwork?.close();
    this.previewArtwork = undefined;
    this.templateArtwork?.close();
    this.templateArtwork = undefined;
    this.context = undefined;
    this.uploaded = undefined;
    this.photo.clear();
  }
  private changed = () => {
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
      this.clear();
      const retryAt = this.dependencies.now() + 5000;
      if (this.mode.kind !== "new") {
        this.retryAt = retryAt;
        this.waitingSelection = `${binding.selection.id}/${binding.selection.revision}`;
      }
      if (cancelling || this.uncertainCancellation === this.account()) {
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
        state.media === "loading"
      ) {
        this.show({
          kind: "detail",
          result: state.result,
          media: "failed",
          retryAt,
        });
        return;
      }
      this.show(
        unknown
          ? { kind: "unknown" }
          : {
              kind: "error",
              message: READ_ERROR,
              retryAt,
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
  ) {
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
    const message =
      result.kind === "artwork-error"
        ? "工牌预览无法完整显示，请联系管理员检查字体、照片和模板。"
        : (APPLICATION_FAILURE_COPY[result.code ?? ""] ?? READ_ERROR);
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
          : { kind: "error", message, retryAt },
      );
    }
  }
  async load(page = 1, notice?: string): Promise<void> {
    if (this.dependencies.now() < this.retryAt) {
      this.show({ kind: "error", message: READ_ERROR, retryAt: this.retryAt });
      return;
    }
    const scope = this.scope();
    if (!scope) return;
    this.clear();
    this.show({ kind: "loading" });
    this.page = page;
    try {
      if (!(await scope.guard())) return;
      const session = scope.binding.selection.id;
      if (this.mode.kind === "new") {
        const service = this.dependencies.workforce.snapshot();
        if (service.kind !== "ready" || service.status !== "active") {
          this.show({
            kind: "blocked",
            message:
              "当前员工服务不可用于新申请。你仍可查看允许访问的申请记录。",
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
        if (result.kind !== "ok") return this.failure(result, scope.binding);
        this.retryAt = 0;
        const uncertain = this.uncertainCancellation === this.account();
        this.uncertainCancellation = undefined;
        notice ??= uncertain
          ? "已读取当前状态。请核对申请后再决定是否取消。"
          : undefined;
        if (result.value.access === "restricted") {
          this.show({ kind: "detail", result: result.value, notice });
          return;
        }
        const item = result.value.application;
        this.show({
          kind: "detail",
          result: result.value,
          media: "loading",
          notice,
        });
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
          return this.detailMediaFailure(
            resources,
            scope.binding,
            result.value,
          );
        this.templateArtwork = resources.value;
        const photo = await this.api.resource(
          session,
          item.photo.resourceId,
          scope.signal,
        );
        if (!(await scope.guard())) return;
        if (photo.kind !== "ok")
          return this.detailMediaFailure(photo, scope.binding, result.value);
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
          return this.detailMediaFailure(artwork, scope.binding, result.value);
        this.previewArtwork = artwork.value;
        this.show({
          ...(this.state.kind === "detail" ? this.state : {}),
          kind: "detail",
          result: result.value,
          artwork: artwork.value,
          media: undefined,
        });
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
  confirmCancellation() {
    const state = this.state;
    if (
      state.kind !== "detail" ||
      state.pending ||
      !["submitted", "approved"].includes(state.result.application.status)
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
    if (expectedStatus !== "submitted" && expectedStatus !== "approved") return;
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
        this.show({ kind: "cancelled" });
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
    this.clear();
    this.retryAt =
      this.dependencies.now() + Math.max(5000, result.retryAfter ?? 0);
    this.waitingSelection = `${binding.selection.id}/${binding.selection.revision}`;
    this.show({
      kind: "detail",
      result: detail,
      media: "failed",
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
      const result = this.state.result;
      this.cancel();
      this.clear();
      this.show({ kind: "detail", result, media: "failed", retryAt: 0 });
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
              message: "工牌预览无法完整显示，请刷新后重试。",
              retryAt: 0,
            },
      );
    }
  }
}
