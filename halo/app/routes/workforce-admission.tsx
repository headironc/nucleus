import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { flushSync } from "react-dom";
import { Button } from "@base-ui/react/button";
import { Header, Footer } from "../root";
import { Sessions } from "../authentication/session";
import { current } from "../protocol/session";
import {
  admissionCandidate,
  admit,
  institutionRole,
} from "../protocol/admission";
import { currentBuild } from "../protocol/build";
import { reloadRequired } from "../integrity";
import { Admission, type AdmissionState } from "../workforce/admission";

export function meta() {
  return [
    { title: "开通员工服务 · 司籍处" },
    { name: "robots", content: "noindex" },
  ];
}
function useAdmission() {
  const [controller, setController] = useState<Admission>();
  useEffect(() => {
    const sessions = new Sessions();
    const admission = new Admission({
      storage: {
        getItem: (key) => sessionStorage.getItem(key),
        setItem: (key, value) => sessionStorage.setItem(key, value),
        removeItem: (key) => sessionStorage.removeItem(key),
      },
      sessions,
      current,
      role: institutionRole,
      candidate: admissionCandidate,
      admit,
      build: currentBuild,
      now: Date.now,
      uuid: () => crypto.randomUUID(),
      marker: (account) => history.state?.workforceAdmission === account,
      mark: (account, pending) =>
        history.replaceState(
          { ...history.state, workforceAdmission: pending ? account : null },
          "",
        ),
    });
    setController(admission);
    const hide = () => flushSync(() => admission.suspend());
    const check = () => {
      hide();
      if (!document.hidden) void admission.verify();
    };
    const reload = () => flushSync(() => admission.reload());
    const unwatch = sessions.watch(check);
    document.addEventListener("visibilitychange", check);
    window.addEventListener("focus", check);
    window.addEventListener("pageshow", check);
    window.addEventListener("pagehide", hide);
    window.addEventListener("halo:reload-required", reload);
    if (reloadRequired()) admission.reload();
    else if (!document.hidden) void admission.verify();
    return () => {
      admission.close();
      unwatch();
      document.removeEventListener("visibilitychange", check);
      window.removeEventListener("focus", check);
      window.removeEventListener("pageshow", check);
      window.removeEventListener("pagehide", hide);
      window.removeEventListener("halo:reload-required", reload);
      void sessions.close().catch(() => {});
    };
  }, []);
  return controller;
}
function message(state: AdmissionState): string {
  if (state.kind === "current") {
    switch (state.current.kind) {
      case "anonymous":
        return "请先登录，再检查是否可以办理。";
      case "expired":
        return "当前登录已失效。请使用原办理账户重新登录，原结果仍需确认。";
      case "reload":
        return "请重新加载页面后继续检查。";
      case "denied":
        return "当前无法办理。此前已提交的开通不会因本次检查被取消。";
      case "checking":
      case "changed":
        return "正在检查当前登录，请稍候。";
      default:
        return "暂时无法检查当前登录，请稍后重新检查。";
    }
  }
  if (state.kind === "permission") {
    switch (state.permission.kind) {
      case "denied":
        return "当前账户没有开通员工服务的权限。此前办理结果仍需由原账户恢复权限后检查。";
      case "unavailable":
      case "limited":
        return "暂时无法检查办理权限，请稍后重试。";
      default:
        return "正在检查办理权限，请稍候。";
    }
  }
  const content = state.content;
  switch (content.kind) {
    case "lookup":
      return "为已注册的同事开通员工服务。请先用完整手机号查找。";
    case "finding":
      return "正在查找同事和可选角色，请稍候。";
    case "confirm":
      return "请核对同事资料与所选角色，再明确确认开通。";
    case "submitting":
      return "正在提交开通。离开页面或停止等待不会取消已提交的办理。";
    case "querying":
      return "正在检查原开通结果，只核对此前办理。";
    case "pending":
    case "rejected":
      return content.message;
    case "storage":
      return content.message ?? "本页办理信息暂时无法使用，请重新检查。";
    case "missing":
      return "原办理信息不完整，暂时无法确认开通结果。请联系管理人员，暂时不要重复办理。";
    case "complete":
      return "本次开通记录已确认。同事可刷新员工服务页面，查看当前服务状态。";
  }
}
function Content({ controller }: { controller: Admission }) {
  const state = useSyncExternalStore(
    controller.subscribe,
    controller.snapshot,
    controller.snapshot,
  );
  const content = state.kind === "content" ? state.content : undefined;
  const [mobile, setMobile] = useState("");
  const [invalid, setInvalid] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [now, setNow] = useState(Date.now);
  const heading = useRef<HTMLHeadingElement>(null);
  const input = useRef<HTMLInputElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const dialog = useRef<HTMLDialogElement>(null);
  const cancel = useRef<HTMLButtonElement>(null);
  const confirmation = content?.kind === "confirm" ? content : undefined;
  const selected = confirmation?.selected;
  const eligible =
    !!confirmation &&
    !confirmation.candidate.suspensionId &&
    (!confirmation.candidate.grant ||
      confirmation.candidate.grant.status === "terminated");
  const pending =
    content?.kind === "pending" ||
    content?.kind === "submitting" ||
    content?.kind === "querying";
  const retryAt =
    content && "retryAt" in content
      ? (content.retryAt ?? 0)
      : state.kind === "current" && "retryAt" in state.current
        ? state.current.retryAt
        : state.kind === "permission" && "retryAt" in state.permission
          ? state.permission.retryAt
          : 0;
  const seconds = Math.max(0, Math.ceil((retryAt - now) / 1000));
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, []);
  useEffect(() => {
    heading.current?.focus();
  }, [state.kind, content?.kind]);
  useEffect(() => {
    if (state.kind !== "content") {
      setMobile("");
      setInvalid(false);
    }
  }, [state.kind]);
  useEffect(() => {
    const element = dialog.current;
    if (!element) return;
    if (confirming && confirmation && selected && eligible) {
      if (!element.open) {
        element.showModal();
        cancel.current?.focus();
      }
    } else {
      element.close();
      setConfirming(false);
    }
  }, [confirming, confirmation, selected, eligible]);
  useEffect(() => {
    if (!pending) return;
    const before = (event: BeforeUnloadEvent) => {
      event.preventDefault();
      event.returnValue = "";
    };
    window.addEventListener("beforeunload", before);
    return () => window.removeEventListener("beforeunload", before);
  }, [pending]);
  const closeDialog = () => {
    setConfirming(false);
    trigger.current?.focus();
  };
  const stamp =
    content?.kind === "complete"
      ? new Intl.DateTimeFormat("zh-CN", {
          dateStyle: "long",
          timeStyle: "short",
          timeZone: "Asia/Shanghai",
        }).format(new Date(content.receipt.grant.grantedAt))
      : "";
  return (
    <div className="admission-layout">
      <a className="termination-back" href="/account">
        ← 返回个人账户
      </a>
      <header className="termination-heading">
        <p className="eyebrow">机构管理 · 同事办理</p>
        <h1 ref={heading} tabIndex={-1}>
          开通员工服务
        </h1>
        <p role="status">{message(state)}</p>
      </header>
      <section
        className="termination-panel admission-panel"
        aria-busy={["finding", "submitting", "querying"].includes(
          content?.kind ?? "",
        )}
      >
        {content?.kind === "lookup" && (
          <form
            className="admission-form"
            onSubmit={(event) => {
              event.preventDefault();
              if (!/^1[0-9]{10}$/.test(mobile)) {
                setInvalid(true);
                input.current?.focus();
                return;
              }
              setInvalid(false);
              void controller.find(mobile);
            }}
          >
            <label htmlFor="colleague-mobile">同事的手机号</label>
            <input
              ref={input}
              id="colleague-mobile"
              type="tel"
              inputMode="numeric"
              autoComplete="off"
              maxLength={11}
              value={mobile}
              aria-invalid={invalid}
              aria-describedby="mobile-help"
              onChange={(event) => {
                setMobile(event.target.value);
                setInvalid(false);
              }}
            />
            <p
              id="mobile-help"
              className={invalid ? "field-error" : "field-hint"}
            >
              {invalid
                ? "请填写 11 位完整手机号。"
                : "使用同事注册时的完整手机号。"}
            </p>
            <Button type="submit" className="primary">
              查找同事
            </Button>
          </form>
        )}
        {confirmation && (
          <>
            <div className="admission-person">
              <p className="eyebrow">请核对同事</p>
              <h2>{confirmation.candidate.identity.name}</h2>
              <p>
                身份证尾号{" "}
                <span className="suffix">
                  {confirmation.candidate.identity.idNumber.suffix}
                </span>
              </p>
            </div>
            <dl className="current-summary">
              <div>
                <dt>账户状态</dt>
                <dd>
                  {confirmation.candidate.suspensionId ? "已暂停" : "正常"}
                </dd>
              </div>
              <div>
                <dt>员工服务</dt>
                <dd>
                  {!confirmation.candidate.grant
                    ? "尚未开通"
                    : confirmation.candidate.grant.status === "terminated"
                      ? "此前服务已结束"
                      : confirmation.candidate.grant.status === "suspended"
                        ? "已暂停"
                        : "已开通"}
                </dd>
              </div>
            </dl>
            {!eligible && (
              <p className="admission-notice" role="status">
                {confirmation.candidate.suspensionId
                  ? "该账户已暂停，暂时无法开通员工服务。"
                  : "该同事已有未结束的员工服务，无需再次开通。"}
              </p>
            )}
            {eligible && (
              <fieldset className="admission-roles">
                <legend>选择员工服务角色</legend>
                <p className="field-hint">
                  普通成员不含管理权限。其他角色请按实际职责明确选择。
                </p>
                {confirmation.defaultRole &&
                  !confirmation.candidate.roles.some(
                    (role) => role.id === confirmation.defaultRole!.id,
                  ) && (
                    <label className="admission-role">
                      <input
                        type="radio"
                        name="role"
                        checked={selected?.id === confirmation.defaultRole.id}
                        onChange={() =>
                          controller.select(confirmation.defaultRole!.id)
                        }
                      />
                      <span>
                        {confirmation.defaultRole.name}
                        <small>普通成员 · 默认选择</small>
                      </span>
                    </label>
                  )}
                {confirmation.candidate.roles.map((role) => (
                  <label className="admission-role" key={role.id}>
                    <input
                      type="radio"
                      name="role"
                      checked={selected?.id === role.id}
                      onChange={() => controller.select(role.id)}
                    />
                    <span>
                      {role.name}
                      <small>
                        {role.kind === "default"
                          ? "普通成员 · 默认选择"
                          : "包含角色所授予的职责，请谨慎选择"}
                      </small>
                    </span>
                  </label>
                ))}
                {confirmation.candidate.roles.length === 0 && (
                  <p>本页没有可选角色，请返回上一页。</p>
                )}
                {confirmation.candidate.count > 100 && (
                  <nav className="admission-pagination" aria-label="角色分页">
                    <Button
                      disabled={confirmation.page <= 1}
                      onClick={() =>
                        void controller.find(
                          confirmation.mobile,
                          confirmation.page - 1,
                        )
                      }
                    >
                      上一页
                    </Button>
                    <span>
                      第 {confirmation.page} /{" "}
                      {Math.ceil(confirmation.candidate.count / 100)} 页 · 共{" "}
                      {confirmation.candidate.count} 个角色
                    </span>
                    <Button
                      disabled={
                        confirmation.page * 100 >= confirmation.candidate.count
                      }
                      onClick={() =>
                        void controller.find(
                          confirmation.mobile,
                          confirmation.page + 1,
                        )
                      }
                    >
                      下一页
                    </Button>
                  </nav>
                )}
                <p
                  className={
                    selected?.kind !== "default"
                      ? "admission-notice"
                      : "field-hint"
                  }
                >
                  {selected
                    ? `已选角色：${selected.name}${selected.kind !== "default" ? "。请确认确实需要授予该角色。" : ""}`
                    : "请选择一个实际角色后再继续。"}
                </p>
              </fieldset>
            )}
            <div className="termination-actions">
              <Button
                ref={trigger}
                className="primary"
                disabled={!eligible || !selected}
                onClick={() => setConfirming(true)}
              >
                确认开通
              </Button>
              <Button
                className="quiet-link"
                onClick={() => {
                  controller.reset();
                  setMobile("");
                }}
              >
                查找另一位同事
              </Button>
            </div>
          </>
        )}
        {content?.kind === "pending" && (
          <Button
            className="primary"
            disabled={content.blocked || seconds > 0}
            onClick={() => void controller.check()}
          >
            {seconds > 0 ? `${seconds} 秒后可检查开通结果` : "检查开通结果"}
          </Button>
        )}
        {content?.kind === "rejected" && (
          <Button
            className="primary"
            disabled={seconds > 0}
            onClick={() => {
              controller.reset();
              setMobile("");
            }}
          >
            {seconds > 0 ? `${seconds} 秒后可重新查找` : "重新查找"}
          </Button>
        )}
        {content?.kind === "complete" && (
          <>
            <div className="admission-person">
              <span className="binding-state">本次员工服务已开通</span>
              <h2>{content.receipt.grant.subject.name}</h2>
              <p>角色：{content.receipt.assignment.role.name}</p>
              <p className="field-hint">开通时间：{stamp}</p>
            </div>
            <p className="admission-notice">
              这是本次开通的原记录。之后的暂停、结束或角色变化，以同事刷新后读到的当前状态为准。工号需另行办理。
            </p>
            {content.canStart ? (
              <Button
                className="primary"
                onClick={() => {
                  controller.reset();
                  setMobile("");
                }}
              >
                继续办理另一人
              </Button>
            ) : (
              <p>浏览器尚未清理本次办理信息，请重新检查后再继续。</p>
            )}
          </>
        )}
        {state.kind === "permission" &&
          ["unavailable", "limited"].includes(state.permission.kind) && (
            <Button
              className="primary"
              disabled={seconds > 0}
              onClick={() => void controller.refreshPermission()}
            >
              {seconds > 0 ? `${seconds} 秒后可重查` : "重新检查权限"}
            </Button>
          )}
        {(content?.kind === "storage" ||
          (state.kind === "current" &&
            ["unavailable", "limited", "storage"].includes(
              state.current.kind,
            ))) && (
          <Button
            className="primary"
            disabled={seconds > 0}
            onClick={() => void controller.verify()}
          >
            重新检查
          </Button>
        )}
        {state.kind === "current" &&
          ["anonymous", "expired", "storage"].includes(state.current.kind) && (
            <a
              className="quiet-link"
              href="/authentication/codes"
              target="_blank"
              rel="noopener"
            >
              手机号登录（新标签页）
            </a>
          )}
        {state.kind === "current" && state.current.kind === "reload" && (
          <Button className="primary" onClick={() => location.reload()}>
            重新加载页面
          </Button>
        )}
        {pending && (
          <p className="field-hint admission-leave">
            可返回个人账户；离开不会取消本次办理。请使用原办理账户回来检查结果。
          </p>
        )}
      </section>
      <aside className="admission-guide">
        <h2>开通前请确认</h2>
        <p>
          同事已完成实名注册，且属于你可以办理的范围。开通员工服务不代表人事入职，也不会自动取得工号或工牌。
        </p>
      </aside>
      <dialog
        ref={dialog}
        className="termination-dialog"
        aria-labelledby="admission-confirm-title"
        onCancel={(event) => {
          event.preventDefault();
          closeDialog();
        }}
      >
        <h2 id="admission-confirm-title">为这位同事开通员工服务？</h2>
        {confirming && confirmation && selected && (
          <>
            <p>
              {confirmation.candidate.identity.name} · 身份证尾号{" "}
              {confirmation.candidate.identity.idNumber.suffix}
            </p>
            <p
              className={selected.kind !== "default" ? "admission-notice" : ""}
            >
              将授予角色：<strong>{selected.name}</strong>
            </p>
            <p>
              开通后，同事可刷新员工服务页面继续办理。工号和工牌需另行申请。
            </p>
          </>
        )}
        <div className="termination-actions">
          <Button ref={cancel} className="secondary" onClick={closeDialog}>
            取消
          </Button>
          <Button
            className="primary"
            onClick={() => {
              setConfirming(false);
              void controller.submit();
            }}
          >
            开通员工服务
          </Button>
        </div>
      </dialog>
    </div>
  );
}
export default function AdmissionPage() {
  const controller = useAdmission();
  return (
    <>
      <Header />
      <main id="main">
        {controller ? (
          <Content controller={controller} />
        ) : (
          <div className="loading-shell">
            <p>正在打开开通员工服务…</p>
          </div>
        )}
      </main>
      <Footer />
    </>
  );
}
