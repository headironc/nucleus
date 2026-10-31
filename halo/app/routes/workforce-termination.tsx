import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import type { RefObject } from "react";
import { flushSync } from "react-dom";
import { Button } from "@base-ui/react/button";
import { Header, Footer } from "../root";
import { Sessions } from "../authentication/session";
import { current } from "../protocol/session";
import { workforce, terminate } from "../protocol/workforce";
import { currentBuild } from "../protocol/build";
import { reloadRequired } from "../integrity";
import { UUID } from "../codes/material";
import { terminationReturn } from "../logout/return";
import {
  Termination,
  hasPendingResult,
  type State,
} from "../workforce/termination";
import type { WorkforceState } from "../workforce/status";

export function meta() {
  return [
    { title: "结束员工服务 · 司籍处" },
    { name: "robots", content: "noindex" },
  ];
}

function useTermination() {
  const [controller, setController] = useState<Termination>();
  useEffect(() => {
    const sessions = new Sessions();
    const recovery = new Termination({
      storage: {
        getItem: (key) => sessionStorage.getItem(key),
        setItem: (key, value) => sessionStorage.setItem(key, value),
        removeItem: (key) => sessionStorage.removeItem(key),
      },
      sessions,
      current,
      workforce,
      terminate,
      build: currentBuild,
      now: Date.now,
      uuid: () => crypto.randomUUID(),
      marker: () => {
        const marker = history.state?.workforceTermination;
        if (
          marker &&
          typeof marker === "object" &&
          typeof marker.preparing === "string" &&
          UUID.test(marker.preparing)
        )
          return { preparing: marker.preparing };
        return marker || terminationReturn(location.search)
          ? "submitted"
          : null;
      },
      mark: (value = "submitted") =>
        history.replaceState(
          { ...history.state, workforceTermination: value },
          "",
          value ? location.href : location.pathname,
        ),
    });
    setController(recovery);
    const hide = () => flushSync(() => recovery.suspend());
    const reload = () => flushSync(() => recovery.reload());
    const check = () => {
      hide();
      if (!document.hidden) void recovery.verify();
    };
    const unwatch = sessions.watch(check);
    window.addEventListener("pagehide", hide);
    window.addEventListener("pageshow", check);
    window.addEventListener("focus", check);
    document.addEventListener("visibilitychange", check);
    window.addEventListener("halo:reload-required", reload);
    if (reloadRequired()) recovery.reload();
    else void recovery.verify();
    return () => {
      recovery.close();
      unwatch();
      window.removeEventListener("pagehide", hide);
      window.removeEventListener("pageshow", check);
      window.removeEventListener("focus", check);
      document.removeEventListener("visibilitychange", check);
      window.removeEventListener("halo:reload-required", reload);
      void sessions.close().catch(() => {});
    };
  }, []);
  return controller;
}

function stamp(value: string) {
  const date = new Date(value);
  return Number.isFinite(date.valueOf())
    ? new Intl.DateTimeFormat("zh-CN", {
        dateStyle: "long",
        timeStyle: "short",
        timeZone: "Asia/Shanghai",
      }).format(date)
    : null;
}

function serviceLabel(service: WorkforceState): string | null {
  if (service.kind !== "ready") return null;
  return service.status === "active"
    ? "当前员工服务已开通"
    : service.status === "suspended"
      ? "当前员工服务已暂停"
      : service.status === "terminated"
        ? "当前员工服务已结束"
        : "当前员工服务未开通";
}

function currentService(service: WorkforceState, rejoined: boolean): string {
  if (service.kind === "loading")
    return "正在加载当前状态…";
  if (service.kind === "limited")
    return "请稍后重新加载当前状态。";
  if (service.kind === "unavailable")
    return "当前状态暂时无法加载，请重试。";
  if (service.kind === "hidden") return "正在加载当前状态…";
  if (service.kind !== "ready") return "当前员工服务状态暂时无法确认。";
  if (rejoined && service.status === "active")
    return "已有申请按当前状态办理。";
  return "";
}

function resultCopy(
  state: Extract<State, { kind: "result" }>,
): [string, string] {
  switch (state.outcome) {
    case "complete":
      return [
        "此前的员工服务已结束",
        "个人账户和已有工号保留。",
      ];
    case "rejected":
      if (state.notice === "limited")
        return [
          "本次结束申请未完成",
          "请稍后返回确认页面，再决定是否结束。",
        ];
      if (state.notice === "conflict")
        return [
          "本次结束申请未完成",
          state.canStart
            ? "当前已有新的员工服务。可返回确认页面，重新决定是否结束。"
            : "可先查看当前员工服务状态。",
        ];
      return [
        "本次结束申请未完成",
        "可返回确认页面，重新决定是否结束。",
      ];
    default:
      return [
        "结束结果尚不确定",
        state.canQuery
          ? "请稍后查看结束结果，暂时不要再次提交结束申请。"
          : "目前无法查看结果。可先返回个人账户，暂时不要再次提交结束申请。",
      ];
  }
}

function copy(state: State): [string, string] {
  switch (state.kind) {
    case "current":
      switch (state.current.kind) {
        case "checking":
        case "changed":
          return ["正在检查登录状态…", "请稍候。"];
        case "anonymous":
        case "expired":
          return [
            "请登录后继续",
            state.pending
              ? "结束结果仍未确认，请使用原账户重新登录。"
              : state.rejected
                ? "本次结束申请未完成。登录后可查看当前员工服务。"
                : "登录后可查看本人员工服务。",
          ];
        case "denied":
          return [
            "目前无法查看员工服务",
            "可返回个人账户或首页。",
          ];
        case "storage":
          return [
            "登录信息暂时无法使用",
            "请重新检查，或使用手机号登录。",
          ];
        case "reload":
          return ["请重新加载", "重新加载后，请按页面提示继续。"];
        case "limited":
          return ["请稍后再检查", "暂时无法检查登录状态，请稍后重试。"];
        default:
          return ["暂时无法检查登录状态", "请稍后重试。"];
      }
    case "other":
      return [
        "当前账户已变化",
        "可查看当前账户的员工服务或个人账户。",
      ];
    case "missing":
      return [
        "结束结果尚不确定",
        "本页无法查看原结束结果。可查看当前员工服务，暂时不要再次提交结束申请。",
      ];
    case "mismatch":
      return [
        "结束结果尚不确定",
        "原结束结果仍无法确认。可查看当前员工服务，暂时不要再次提交结束申请。",
      ];
    case "reading":
      return ["正在加载员工服务状态…", "请稍候。"];
    case "service-failed":
      return state.service.kind === "limited"
        ? ["员工服务状态暂时无法加载", "请稍后重试。"]
        : ["员工服务状态暂时无法加载", "请重试。"];
    case "unavailable":
      return state.service.kind === "ready" && state.service.status === "terminated"
        ? ["员工服务已结束", "当前没有可结束的员工服务。个人账户可继续使用。"]
        : ["尚未加入员工服务", "当前没有可结束的员工服务。个人账户可继续使用。"];
    case "entry":
      return [
        "结束员工服务",
        state.status === "suspended"
          ? "员工服务已暂停，仍可主动结束。"
          : "请先了解结束后的影响，再确认是否结束。",
      ];
    case "preparing":
      return [
        "正在准备结束申请",
        "本次结束申请尚未提交。",
      ];
    case "submitting":
      return [
        "正在提交结束申请",
        "请稍候。",
      ];
    case "querying":
      return [
        "正在查看结束结果…",
        "请稍候。",
      ];
    case "unsubmitted":
      return [
        "本次结束申请尚未提交",
        "请返回确认页面后再继续。",
      ];
    case "storage":
      return [
        state.unsubmitted ? "本次结束申请尚未提交" : "结束信息暂时无法使用",
        state.unsubmitted
          ? "请重新检查。"
          : state.pending
            ? "结束结果仍未确认。请重新检查。"
            : "请重新检查。",
      ];
    case "random":
      return [
        "本次结束申请尚未提交",
        "请重新加载后再试。",
      ];
    case "reload":
      return [
        "请重新加载",
        state.pending
          ? "结束结果仍未确认。重新加载后，请按页面提示继续。"
          : "重新加载后，请按页面提示继续。",
      ];
    case "result":
      return resultCopy(state);
  }
}

function EntryPanel({
  state,
  controller,
  onConfirm,
  trigger,
}: {
  state: Extract<State, { kind: "entry" }>;
  controller: Termination;
  onConfirm: () => void;
  trigger: RefObject<HTMLButtonElement | null>;
}) {
  return (
    <>
      <div className="termination-person">
        <strong>{state.account.name}</strong>
        <p>
          本人账户 · 当前员工服务
          {state.status === "suspended" ? "已暂停" : "已开通"}
        </p>
      </div>
      {state.known && (
        <p className="termination-note-copy">
          此前的结束申请已完成。下方为重新加入后的员工服务。
        </p>
      )}
      <h2>结束后会发生什么</h2>
      <ul className="termination-consequences">
        <li>结束后，将无法申请工号或继续办理工牌。</li>
        <li>个人账户和已有工号保留。</li>
      </ul>
      <div className="termination-actions">
        <Button ref={trigger} className="primary" onClick={onConfirm}>
          结束员工服务
        </Button>
        <a className="quiet-link" href="/workforce">
          返回员工服务
        </a>
      </div>
    </>
  );
}

function ServiceNote({
  service,
  rejoined,
  retry,
  seconds,
}: {
  service: WorkforceState;
  rejoined: boolean;
  retry: (() => void) | undefined;
  seconds: number;
}) {
  const description = currentService(service, rejoined);
  return (
    <section className="termination-guide" aria-label="当前员工服务">
      <h2>
        {rejoined
          ? "当前员工服务已重新开通"
          : (serviceLabel(service) ?? "当前员工服务")}
      </h2>
      {description && <p>{description}</p>}
      {retry && (service.kind === "limited" || service.kind === "unavailable") && (
        <Button
          className="quiet-link"
          disabled={seconds > 0}
          onClick={retry}
        >
          {seconds > 0 ? `${seconds} 秒后可刷新状态` : "刷新当前状态"}
        </Button>
      )}
    </section>
  );
}

function Content({ controller }: { controller: Termination }) {
  const state = useSyncExternalStore(
    controller.subscribe,
    controller.snapshot,
    controller.snapshot,
  );
  const [now, setNow] = useState(Date.now);
  const [confirming, setConfirming] = useState(false);
  const [leaving, setLeaving] = useState<string | null>(null);
  const heading = useRef<HTMLHeadingElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const confirmDialog = useRef<HTMLDialogElement>(null);
  const leaveDialog = useRef<HTMLDialogElement>(null);
  const cancelButton = useRef<HTMLButtonElement>(null);
  const leaveAllowed = useRef(false);
  const [title, description] = copy(state);
  const pending = hasPendingResult(state);
  const retryAt =
    state.kind === "result"
      ? state.retryAt
      : state.kind === "current" && "retryAt" in state.current
        ? state.current.retryAt
        : state.kind === "service-failed" && "retryAt" in state.service
          ? state.service.retryAt
          : 0;
  const seconds = Math.max(0, Math.ceil((retryAt - now) / 1000));
  const allowNew =
    state.kind === "result" &&
    state.outcome === "complete" &&
    state.allowNew;

  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, []);
  useEffect(() => {
    document.title = `${title} · 司籍处`;
    heading.current?.focus();
  }, [title]);
  useEffect(() => {
    const element = confirmDialog.current;
    if (!element) return;
    const allowed = state.kind === "entry" || allowNew;
    if (confirming && allowed && !element.open) {
      element.showModal();
      cancelButton.current?.focus();
    } else if ((!confirming || !allowed) && element.open) {
      element.close();
      if (!allowed) setConfirming(false);
    }
  }, [confirming, state.kind, allowNew]);
  useEffect(() => {
    if (!pending) setLeaving(null);
  }, [pending]);
  useEffect(() => {
    const element = leaveDialog.current;
    if (leaving && pending && element && !element.open) element.showModal();
  }, [leaving, pending]);
  useEffect(() => {
    if (!pending) return;
    leaveAllowed.current = false;
    const before = (event: BeforeUnloadEvent) => {
      if (leaveAllowed.current) return;
      event.preventDefault();
      event.returnValue = "";
    };
    const follow = (event: MouseEvent) => {
      if (
        event.defaultPrevented ||
        event.button !== 0 ||
        event.metaKey ||
        event.ctrlKey ||
        event.shiftKey ||
        event.altKey
      )
        return;
      const link =
        event.target instanceof Element
          ? event.target.closest<HTMLAnchorElement>("a[href]")
          : null;
      if (
        !link ||
        (link.target && link.target !== "_self") ||
        link.hasAttribute("download")
      )
        return;
      const target = new URL(link.href);
      if (
        target.pathname === location.pathname &&
        target.search === location.search &&
        target.hash
      )
        return;
      event.preventDefault();
      setLeaving(target.href);
    };
    window.addEventListener("beforeunload", before);
    document.addEventListener("click", follow);
    return () => {
      window.removeEventListener("beforeunload", before);
      document.removeEventListener("click", follow);
    };
  }, [pending]);

  const openConfirm = () => setConfirming(true);
  const closeConfirm = () => {
    setConfirming(false);
    (trigger.current ?? heading.current)?.focus();
  };
  const service =
    state.kind === "entry" ||
    state.kind === "result" ||
    state.kind === "missing" ||
    state.kind === "mismatch" ||
    state.kind === "other" ||
    state.kind === "unavailable" ||
    state.kind === "unsubmitted" ||
    state.kind === "service-failed"
      ? state.service
      : undefined;
  const account =
    state.kind === "entry" ||
    state.kind === "result" ||
    state.kind === "missing" ||
    state.kind === "mismatch" ||
    state.kind === "other" ||
    state.kind === "unavailable" ||
    state.kind === "unsubmitted" ||
    state.kind === "service-failed" ||
    state.kind === "reading"
      ? state.account
      : undefined;
  const rejoined =
    state.kind === "result" &&
    state.outcome === "complete" &&
    service?.kind === "ready" &&
    service.status === "active" &&
    service.grant !== null &&
    service.grant.id !== state.grant;
  const serviceSeconds =
    service && "retryAt" in service
      ? Math.max(0, Math.ceil((service.retryAt - now) / 1000))
      : 0;
  const serviceFailed =
    service?.kind === "limited" || service?.kind === "unavailable";

  return (
    <div className="termination-layout">
      <a className="termination-back" href="/workforce">
        ← 返回员工服务
      </a>
      <div className="termination-heading">
        <p className="eyebrow">员工服务</p>
        <h1 ref={heading} tabIndex={-1}>
          {title}
        </h1>
        <p>{description}</p>
      </div>
      <div className="termination-columns">
        <section
          className="termination-panel"
          aria-busy={
            state.kind === "preparing" ||
            state.kind === "submitting" ||
            state.kind === "querying"
          }
        >
          {state.kind === "entry" && (
            <EntryPanel
              state={state}
              controller={controller}
              onConfirm={openConfirm}
              trigger={trigger}
            />
          )}
          {state.kind === "result" && (
            <>
              {state.outcome === "complete" && state.terminatedAt && (
                <p className="termination-time">
                  结束时间：{stamp(state.terminatedAt) ?? "暂时无法显示"}
                </p>
              )}
              {state.outcome !== "complete" && state.previous && (
                <p className="field-hint">
                  此前结束时间：{stamp(state.previous) ?? "暂时无法显示"}
                </p>
              )}
              {state.outcome === "complete" && !state.persisted && (
                <p className="field-hint">
                  本页未能保存结束结果，离开后可能无法再次查看。
                </p>
              )}
            </>
          )}
          {state.kind === "result" && (
            <div className="termination-actions">
              {state.outcome === "pending" && state.canQuery && (
                <Button
                  className="primary"
                  disabled={seconds > 0}
                  onClick={() => void controller.query()}
                >
                  {seconds > 0 ? `${seconds} 秒后可查看结束结果` : "查看结束结果"}
                </Button>
              )}
              {state.outcome === "rejected" && state.canStart && (
                <Button
                  className="primary"
                  disabled={seconds > 0}
                  onClick={() => void controller.start()}
                >
                  {seconds > 0
                    ? `${seconds} 秒后可返回确认页面`
                    : "返回确认页面"}
                </Button>
              )}
              {state.outcome === "complete" &&
                (serviceFailed ? (
                  <Button
                    className="primary"
                    disabled={serviceSeconds > 0}
                    onClick={() => void controller.refreshService()}
                  >
                    {serviceSeconds > 0
                      ? `${serviceSeconds} 秒后可刷新状态`
                      : "刷新当前状态"}
                  </Button>
                ) : (
                  <a className="primary" href="/workforce">
                    查看当前员工服务
                  </a>
                ))}
              {state.outcome === "complete" && state.allowNew && (
                <Button
                  ref={trigger}
                  className="quiet-link"
                  onClick={openConfirm}
                >
                  结束当前员工服务
                </Button>
              )}
              {(state.outcome !== "complete" || serviceFailed) && (
                <a className="quiet-link" href="/workforce">
                  查看当前员工服务
                </a>
              )}
              <a className="quiet-link" href="/account">
                打开个人账户
              </a>
            </div>
          )}
          {state.kind === "unsubmitted" && (
            <div className="termination-actions">
              <Button
                className="primary"
                onClick={() => controller.resume()}
              >
                返回确认页面
              </Button>
              <a className="quiet-link" href="/workforce">
                查看当前员工服务
              </a>
              <a className="quiet-link" href="/">
                返回首页
              </a>
            </div>
          )}
          {(state.kind === "preparing" ||
            state.kind === "submitting" ||
            state.kind === "querying") && (
            <div className="termination-actions">
              <Button className="primary" disabled>
                {state.kind === "preparing"
                  ? "正在准备"
                  : state.kind === "submitting"
                    ? "正在提交"
                    : "正在查看"}
              </Button>
              <a className="quiet-link" href="/">
                返回首页
              </a>
            </div>
          )}
          {state.kind === "mismatch" && (
            <div className="termination-actions">
              <a className="primary" href="/workforce">
                查看当前员工服务
              </a>
              <a className="quiet-link" href="/">
                返回首页
              </a>
            </div>
          )}
          {state.kind === "missing" && (
            <div className="termination-actions">
              <a className="primary" href="/workforce">
                查看当前员工服务
              </a>
              <a className="quiet-link" href="/">
                返回首页
              </a>
            </div>
          )}
          {state.kind === "other" && (
            <div className="termination-actions">
              <a className="primary" href="/workforce">
                查看当前员工服务
              </a>
              <a className="quiet-link" href="/account">
                打开个人账户
              </a>
              <a className="quiet-link" href="/">
                返回首页
              </a>
            </div>
          )}
          {state.kind === "unavailable" && (
            <div className="termination-actions">
              <a className="primary" href="/workforce">
                查看当前员工服务
              </a>
              <a className="quiet-link" href="/account">
                打开个人账户
              </a>
            </div>
          )}
          {state.kind === "service-failed" && (
            <div className="termination-actions">
              <Button
                className="primary"
                disabled={seconds > 0}
                onClick={() => void controller.verify()}
              >
                {seconds > 0 ? `${seconds} 秒后可重新加载` : "重新加载"}
              </Button>
              <a className="quiet-link" href="/account">
                打开个人账户
              </a>
            </div>
          )}
          {state.kind === "storage" && (
            <div className="termination-actions">
              <Button
                className="primary"
                onClick={() => void controller.verify()}
              >
                重新检查
              </Button>
              <a className="quiet-link" href="/account">
                打开个人账户
              </a>
            </div>
          )}
          {state.kind === "reload" && (
            <div className="termination-actions">
              <Button className="primary" onClick={() => location.reload()}>
                重新加载
              </Button>
              <a className="quiet-link" href="/">
                返回首页
              </a>
            </div>
          )}
          {state.kind === "random" && (
            <div className="termination-actions">
              <Button className="primary" onClick={() => location.reload()}>
                重新加载
              </Button>
              <a className="quiet-link" href="/">
                返回首页
              </a>
            </div>
          )}
          {state.kind === "current" &&
            ["anonymous", "expired", "storage"].includes(
              state.current.kind,
            ) && (
              <div className="termination-actions">
                {state.current.kind === "storage" && (
                  <Button
                    className="primary"
                    onClick={() => void controller.verify()}
                  >
                    重新检查
                  </Button>
                )}
                <a
                  className="primary"
                  href={
                    state.pending
                      ? "/authentication/codes?return=service-termination"
                      : "/authentication/codes"
                  }
                >
                  手机号登录
                </a>
                <a className="quiet-link" href="/">
                  返回首页
                </a>
              </div>
            )}
          {state.kind === "current" &&
            ["limited", "unavailable"].includes(state.current.kind) && (
              <div className="termination-actions">
                <Button
                  className="primary"
                  disabled={seconds > 0}
                  onClick={() => void controller.verify()}
                >
                  {seconds > 0 ? `${seconds} 秒后可重新检查` : "重新检查"}
                </Button>
                <a className="quiet-link" href="/account">
                  打开个人账户
                </a>
                <a className="quiet-link" href="/">
                  返回首页
                </a>
              </div>
            )}
          {state.kind === "current" && state.current.kind === "denied" && (
            <div className="termination-actions">
              <a className="primary" href="/account">
                打开个人账户
              </a>
              <a className="quiet-link" href="/">
                返回首页
              </a>
            </div>
          )}
          {account && state.kind !== "entry" && (
            <p className="termination-account">
              当前账户：<strong>{account.name}</strong>
            </p>
          )}
        </section>
        {service && state.kind !== "entry" && (
          <ServiceNote
            service={service}
            rejoined={rejoined}
            retry={
              state.kind === "result" && state.outcome === "complete"
                ? undefined
                : () => void controller.refreshService()
            }
            seconds={serviceSeconds}
          />
        )}
        {state.kind === "entry" && (
          <aside className="termination-guide">
            <p className="eyebrow">重新加入后</p>
            <h2>已有申请按当时状态办理</h2>
          </aside>
        )}
      </div>
      <dialog
        ref={confirmDialog}
        className="termination-dialog"
        aria-labelledby="termination-confirm-title"
        aria-describedby="termination-confirm-description"
        onCancel={(event) => {
          event.preventDefault();
          closeConfirm();
        }}
      >
        <p className="eyebrow">本人确认</p>
        <h2 id="termination-confirm-title">结束员工服务？</h2>
        <p id="termination-confirm-description">
          结束后，将无法申请工号或继续办理工牌。
        </p>
        <p className="termination-dialog-note">
          个人账户和已有工号保留。
        </p>
        <div className="termination-actions">
          <Button
            ref={cancelButton}
            className="quiet-link"
            onClick={closeConfirm}
          >
            暂不结束
          </Button>
          <Button
            className="primary"
            onClick={() => {
              setConfirming(false);
              void controller.submit();
            }}
          >
            确认结束
          </Button>
        </div>
      </dialog>
      {pending && leaving && (
        <dialog
          ref={leaveDialog}
          className="termination-dialog"
          aria-labelledby="termination-leave-title"
          onCancel={(event) => {
            event.preventDefault();
            leaveDialog.current?.close();
          }}
          onClose={() => setLeaving(null)}
        >
          <h2 id="termination-leave-title">结束结果仍未确认</h2>
          <p>
            离开不会撤回已发出的结束申请，暂时不要重复提交。
          </p>
          <div className="termination-actions">
            <Button
              className="primary"
              onClick={() => {
                leaveDialog.current?.close();
                setLeaving(null);
              }}
            >
              留在此页
            </Button>
            <Button
              className="quiet-link"
              onClick={() => {
                const target = leaving;
                leaveAllowed.current = true;
                controller.suspend();
                setLeaving(null);
                location.assign(target);
              }}
            >
              仍然离开
            </Button>
          </div>
        </dialog>
      )}
    </div>
  );
}

export default function TerminationPage() {
  const controller = useTermination();
  return (
    <>
      <Header />
      <main id="main">
        {controller ? (
          <Content controller={controller} />
        ) : (
          <div className="loading-shell">
            <p>正在打开结束服务页面…</p>
          </div>
        )}
      </main>
      <Footer />
    </>
  );
}
