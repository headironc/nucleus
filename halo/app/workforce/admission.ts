import {
  Current,
  type CurrentBinding,
  type CurrentState,
} from "../authentication/current";
import type { BrowserSession, Selection } from "../authentication/session";
import type { CurrentResult } from "../protocol/session";
import type {
  AdmissionInput,
  AdmissionResult,
  Candidate,
  CandidateResult,
  Receipt,
  Role,
  RoleResult,
} from "../protocol/admission";
import { UUID } from "../codes/material";
import { Institution, type InstitutionState } from "./institution";
import {
  readAdmission,
  saveAdmission,
  removeAdmission,
  type Material,
} from "./admission-material";
import type { Store } from "./termination-material";

type Confirmation = {
  candidate: Candidate;
  mobile: string;
  page: number;
  selected: Role | null;
  defaultRole: Role | null;
};
export type Content =
  | { kind: "lookup" | "finding" | "storage" | "missing"; message?: string }
  | ({ kind: "confirm" } & Confirmation)
  | { kind: "submitting" | "querying" }
  | { kind: "pending"; message: string; retryAt: number; blocked: boolean }
  | { kind: "rejected"; message: string; retryAt?: number }
  | { kind: "complete"; receipt: Receipt; canStart: boolean };
export type AdmissionState =
  | { kind: "current"; current: CurrentState }
  | { kind: "permission"; permission: InstitutionState }
  | { kind: "content"; content: Content };
type Dependencies = {
  storage: Store;
  sessions: {
    read(): Promise<BrowserSession | null>;
    clear(expected: Selection): Promise<boolean>;
  };
  build(): Promise<boolean>;
  current(session: string, signal: AbortSignal): Promise<CurrentResult>;
  role(session: string, signal: AbortSignal): Promise<RoleResult>;
  candidate(
    session: string,
    mobile: string,
    page: number,
    signal: AbortSignal,
  ): Promise<CandidateResult>;
  admit(
    operation: string,
    input: AdmissionInput,
    operator: string,
    session: string,
    signal: AbortSignal,
  ): Promise<AdmissionResult>;
  now(): number;
  uuid(): string;
  marker(account: string): boolean;
  mark(account: string, pending: boolean): void;
};
function target(
  candidate: Candidate,
  mobile: string,
): AdmissionInput["target"] {
  const { identity, assignmentId, suspensionId, grant } = candidate;
  return {
    accountId: identity.accountId,
    personId: identity.personId,
    revisionId: identity.revisionId,
    mobile,
    assignmentId,
    suspensionId,
    grant,
  };
}
function sameTarget(a: Candidate, b: Candidate, mobile: string) {
  return (
    JSON.stringify(target(a, mobile)) === JSON.stringify(target(b, mobile)) &&
    a.count === b.count
  );
}
function rejection(code: string): string {
  switch (code) {
    case "target_changed":
      return "本次开通时，同事资料或员工服务状态已变化。请重新查找并核对。";
    case "role_changed":
      return "本次开通时，所选角色已变化。请重新查找并选择角色。";
    case "already_authorized":
      return "本次开通时，该同事已有未结束的员工服务。请重新查找并查看当前状态。";
    case "account_not_ready":
      return "本次开通时，该账户处于暂停状态。请重新查找并查看当前状态。";
    case "target_unavailable":
      return "暂时无法为该同事办理。请核对手机号，或查找其他同事。";
    default:
      return "本次开通未完成。请重新查找并核对。";
  }
}

/** One institution admission, with explicit original-input replay after uncertainty. */
export class Admission {
  readonly current: Current;
  private readonly access: Institution;
  private readonly listeners = new Set<() => void>();
  private state: AdmissionState = {
    kind: "current",
    current: { kind: "checking" },
  };
  private content: Content = { kind: "lookup" };
  private material: Material | "missing" | null = null;
  private binding?: CurrentBinding;
  private generation = 0;
  private request?: AbortController;
  private readonly candidateWait = new Map<string, number>();
  private readonly unwatch: (() => void)[];
  constructor(private readonly dependencies: Dependencies) {
    this.current = new Current(dependencies);
    this.access = new Institution({
      current: this.current,
      role: dependencies.role,
      build: dependencies.build,
      now: dependencies.now,
    });
    this.unwatch = [
      this.current.subscribe(this.changed),
      this.access.subscribe(this.permissionChanged),
    ];
  }
  snapshot = () => this.state;
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };
  private render() {
    const current = this.current.snapshot();
    const permission = this.access.snapshot();
    this.state =
      current.kind !== "ready"
        ? { kind: "current", current }
        : permission.kind !== "allowed"
          ? { kind: "permission", permission }
          : { kind: "content", content: this.content };
    this.listeners.forEach((listener) => listener());
  }
  private cancel() {
    this.generation++;
    this.request?.abort();
    this.request = undefined;
  }
  private changed = () => {
    this.cancel();
    this.binding = this.current.binding();
    this.content = { kind: "lookup" };
    this.material = null;
    const current = this.current.snapshot();
    if (current.kind === "ready") {
      try {
        this.material = readAdmission(
          this.dependencies.storage,
          current.account.id,
        );
        if (!this.material && this.dependencies.marker(current.account.id))
          this.material = "missing";
        if (
          this.material === "missing" ||
          (this.material && this.material.operator !== current.account.personId)
        )
          this.content = { kind: "missing" };
        else if (this.material)
          this.content = {
            kind: "pending",
            message: this.material.blocked
              ? "暂时无法确认此前的开通结果。可返回个人账户，暂时不要重复办理。"
              : "此前的开通结果尚不确定。请查看开通结果，暂时不要重复办理。",
            retryAt: this.material.retryAt,
            blocked: this.material.blocked,
          };
        else if (
          (this.candidateWait.get(current.account.id) ?? 0) >
          this.dependencies.now()
        )
          this.content = {
            kind: "rejected",
            message: "暂时无法查找同事，请稍后再试。",
            retryAt: this.candidateWait.get(current.account.id),
          };
      } catch {
        this.content = {
          kind: "storage",
          message: "办理信息暂时无法使用，请重新检查。",
        };
      }
    }
    this.render();
  };
  private permissionChanged = () => {
    if (this.access.snapshot().kind !== "allowed") {
      this.cancel();
      if (this.content.kind === "confirm" || this.content.kind === "finding")
        this.content = { kind: "lookup" };
    }
    this.render();
  };
  verify = () => this.current.verify();
  refreshPermission = () => this.access.refresh();
  suspend = () => this.current.suspend();
  reload = () => this.current.reload();
  close() {
    this.current.suspend();
    this.unwatch.forEach((unwatch) => unwatch());
    this.access.close();
    this.cancel();
  }
  private allowed() {
    return (
      this.current.snapshot().kind === "ready" &&
      this.access.snapshot().kind === "allowed"
    );
  }
  private async run<T>(
    work: (
      binding: CurrentBinding,
      signal: AbortSignal,
      active: () => boolean,
    ) => Promise<T>,
  ) {
    const binding = this.binding;
    if (!binding || !this.allowed()) return;
    this.cancel();
    const generation = this.generation;
    const request = new AbortController();
    this.request = request;
    const active = () =>
      generation === this.generation &&
      this.current.matches(binding) &&
      this.allowed();
    const timer = setTimeout(() => request.abort(), 35_000);
    try {
      if (!(await this.dependencies.build().catch(() => false))) {
        if (active()) this.current.reload();
        return;
      }
      if (!active() || !(await this.current.confirm(binding)) || !active())
        return;
      await work(binding, request.signal, active);
    } finally {
      clearTimeout(timer);
      if (this.request === request) this.request = undefined;
    }
  }
  async find(mobile: string, page = 1): Promise<void> {
    if (!this.allowed() || this.material || !/^1[0-9]{10}$/.test(mobile))
      return;
    const account = this.current.snapshot();
    if (
      account.kind !== "ready" ||
      (this.candidateWait.get(account.account.id) ?? 0) >
        this.dependencies.now()
    )
      return;
    const previous = this.content.kind === "confirm" ? this.content : undefined;
    if (!["lookup", "confirm", "rejected"].includes(this.content.kind)) return;
    this.content = { kind: "finding" };
    this.render();
    await this.run(async (binding, signal, active) => {
      const result = await this.dependencies.candidate(
        binding.selection.id,
        mobile,
        page,
        signal,
      );
      if (!active() || !(await this.current.confirm(binding)) || !active())
        return;
      if (result.kind !== "candidate") {
        await this.findFailure(result, binding);
        return;
      }
      const candidate = result.candidate;
      const current = this.current.snapshot();
      if (current.kind !== "ready") return;
      if (
        candidate.identity.personId === current.account.personId ||
        candidate.identity.accountId === current.account.id
      ) {
        this.content = {
          kind: "rejected",
          message: "请查找其他同事，不能为本人开通。",
        };
        this.render();
        return;
      }
      if (previous && !sameTarget(previous.candidate, candidate, mobile)) {
        this.content = {
          kind: "rejected",
          message: "同事资料或可选角色已变化，请重新查找并核对。",
        };
        this.render();
        return;
      }
      let defaultRole =
        previous?.defaultRole ??
        candidate.roles.find((role) => role.kind === "default") ??
        null;
      if (!previous && !defaultRole && candidate.count > 100) {
        const last = await this.dependencies.candidate(
          binding.selection.id,
          mobile,
          Math.ceil(candidate.count / 100),
          signal,
        );
        if (!active() || !(await this.current.confirm(binding)) || !active())
          return;
        if (last.kind !== "candidate") {
          await this.findFailure(last, binding);
          return;
        }
        if (!sameTarget(candidate, last.candidate, mobile)) {
          this.content = {
            kind: "rejected",
            message: "同事资料或可选角色已变化，请重新查找并核对。",
          };
          this.render();
          return;
        }
        defaultRole =
          last.candidate.roles.find((role) => role.kind === "default") ?? null;
      }
      this.content = {
        kind: "confirm",
        candidate,
        mobile,
        page,
        defaultRole,
        selected: previous?.selected ?? defaultRole,
      };
      this.render();
    });
  }
  private async findFailure(
    result: Exclude<CandidateResult, { kind: "candidate" }>,
    binding: CurrentBinding,
  ) {
    if (result.kind === "failure" && [401, 403].includes(result.status))
      await this.current.reject(binding, result.status as 401 | 403);
    else if (result.kind === "untrusted") this.current.reload();
    else {
      const retryAt =
        this.dependencies.now() +
        Math.max(5_000, result.kind === "failure" ? result.retryAfter : 0);
      const current = this.current.snapshot();
      if (current.kind === "ready")
        this.candidateWait.set(current.account.id, retryAt);
      this.content = {
        kind: "rejected",
        retryAt,
        message:
          result.kind === "failure" && result.code === "target_unavailable"
            ? rejection(result.code)
            : "暂时无法查找同事，请稍后再试。",
      };
      this.render();
    }
  }
  select(id: string) {
    if (!this.allowed() || this.content.kind !== "confirm") return;
    const selected = [
      this.content.defaultRole,
      ...this.content.candidate.roles,
    ].find((role) => role?.id === id);
    if (selected) {
      this.content = { ...this.content, selected };
      this.render();
    }
  }
  reset() {
    if (
      !this.allowed() ||
      this.material ||
      !["lookup", "confirm", "rejected", "complete", "storage"].includes(
        this.content.kind,
      )
    )
      return;
    const current = this.current.snapshot();
    if (
      current.kind !== "ready" ||
      (this.candidateWait.get(current.account.id) ?? 0) >
        this.dependencies.now()
    )
      return;
    this.cancel();
    this.content = { kind: "lookup" };
    this.render();
  }
  async submit(): Promise<void> {
    const state = this.content;
    const current = this.current.snapshot();
    if (
      !this.allowed() ||
      state.kind !== "confirm" ||
      !state.selected ||
      this.material ||
      current.kind !== "ready" ||
      state.candidate.suspensionId ||
      (state.candidate.grant && state.candidate.grant.status !== "terminated")
    )
      return;
    try {
      const operation = this.dependencies.uuid();
      if (!UUID.test(operation)) throw Error("random_unavailable");
      const material: Material = {
        version: 1,
        operation,
        account: current.account.id,
        operator: current.account.personId,
        input: {
          entry: "institution",
          target: target(state.candidate, state.mobile),
          role: { id: state.selected.id, changeId: state.selected.changeId },
        },
        retryAt: 0,
        blocked: false,
      };
      saveAdmission(this.dependencies.storage, material);
      this.material = material;
      this.dependencies.mark(material.account, true);
    } catch {
      this.content = {
        kind: "storage",
        message: "本次开通尚未提交。请重新检查。",
      };
      this.render();
      return;
    }
    await this.send(false);
  }
  async check(): Promise<void> {
    if (
      this.content.kind !== "pending" ||
      this.content.blocked ||
      this.content.retryAt > this.dependencies.now()
    )
      return;
    await this.send(true);
  }
  private async send(replay: boolean) {
    const material = this.material;
    if (!this.allowed() || !material || material === "missing") return;
    this.content = { kind: replay ? "querying" : "submitting" };
    this.render();
    await this.run(async (binding, signal, active) => {
      const result = await this.dependencies.admit(
        material.operation,
        material.input,
        material.operator,
        binding.selection.id,
        signal,
      );
      if (!active() || !(await this.current.confirm(binding)) || !active())
        return;
      if (result.kind === "complete") {
        this.clear(material);
        this.content = {
          kind: "complete",
          receipt: result.receipt,
          canStart: this.material === null,
        };
        this.render();
        return;
      }
      if (result.kind === "failure" && [401, 403].includes(result.status)) {
        await this.current.reject(binding, result.status as 401 | 403);
        return;
      }
      if (
        result.kind === "failure" &&
        [400, 404, 409, 413, 415, 422].includes(result.status) &&
        result.code !== "request_mismatch"
      ) {
        this.clear(material);
        this.content = { kind: "rejected", message: rejection(result.code) };
        this.render();
        return;
      }
      const retryAt =
        this.dependencies.now() +
        Math.max(5_000, result.kind === "failure" ? result.retryAfter : 0);
      const blocked =
        result.kind === "failure" && result.code === "request_mismatch";
      this.material = { ...material, retryAt, blocked };
      try {
        saveAdmission(this.dependencies.storage, this.material);
      } catch {
        /* Original saved input remains the recovery authority. */
      }
      this.content = {
        kind: "pending",
        retryAt,
        blocked,
        message: blocked
          ? "暂时无法确认此前的开通结果。可返回个人账户，暂时不要重复办理。"
          : "开通结果尚不确定。请稍后查看结果，暂时不要重复办理。",
      };
      this.render();
    });
  }
  private clear(material: Material) {
    try {
      removeAdmission(this.dependencies.storage, material.account);
      this.dependencies.mark(material.account, false);
      this.material = null;
    } catch {
      /* A retained original may only replay; it never permits a new operation. */
    }
  }
}
