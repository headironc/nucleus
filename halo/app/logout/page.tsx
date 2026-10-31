import * as React from "react";
import * as ui from "@base-ui/react/button";
import * as icons from "lucide-react";
import * as returns from "./return";
import * as recovery from "./recovery";

export type Scope = `current` | `all`;
export type Detail = {
  count: number | null;
  binding: "pending" | "matched" | "mismatch";
  record: { count: number; revokedAt: string } | null;
  revokedAt: string | null;
};
type PageController = recovery.Logout;

function stamp(value: string) {
  const date = new Date(value);
  return Number.isFinite(date.valueOf())
    ? new Intl.DateTimeFormat(`zh-CN`, {
        dateStyle: `long`,
        timeStyle: `short`,
        timeZone: `Asia/Shanghai`,
      }).format(date)
    : value;
}

function message(
  state: recovery.State,
  scope: Scope,
  detail: Detail | null,
): [string, string] {
  const all = scope === `all`;
  switch (state.kind) {
    case `ready`:
      return all
        ? [`退出全部登录`, `将退出网站和小程序的全部登录，包括本次登录。`]
        : [
            `退出登录？`,
            `退出后，再次进入个人账户需要重新登录。其他登录不受影响。`,
          ];
    case `preparing`:
      return [`正在准备退出…`, `请稍候。`];
    case `submitting`:
      return [
        state.pending
          ? all
            ? `正在退出全部登录…`
            : `正在退出登录…`
          : `正在更新登录状态…`,
        `请稍候。`,
      ];
    case `querying`:
      return [`正在查看退出结果…`, `请稍候。`];
    case `other`:
      return [`当前账户已变化`, `请打开个人账户，查看当前登录的账户资料。`];
    case `missing`:
      return [
        `暂时无法查看此前的退出结果`,
        `请打开个人账户，查看当前登录状态，或返回首页。`,
      ];
    case `storage`:
      return state.unsubmitted
        ? [`本次尚未提交退出`, `请重新检查。`]
        : [
            state.pending ? `退出结果尚不确定` : `登录信息暂时无法使用`,
            `请重新检查。`,
          ];
    case `random`:
      return [`本次尚未提交退出`, `请重新加载后再试。`];
    case `reload`:
      return [`请重新加载`, `页面暂时无法继续，请重新加载后再试。`];
    case `result`:
      if (state.outcome === `complete`)
        return state.newLogin
          ? [`此前的退出已完成`, `请打开个人账户，查看当前登录状态。`]
          : [
              all ? `已退出全部登录` : `已退出登录`,
              `账户资料和已提交的申请保留。`,
            ];
      if (state.outcome === `rejected`)
        return [
          `本次尚未退出`,
          state.notice === `limited`
            ? `请稍后返回退出页面再试。`
            : `请先打开个人账户，重新查看当前状态。`,
        ];
      if (state.notice === `denied`)
        return [`暂时无法查看此前的退出结果`, `可以返回首页。`];
      if (all && detail?.record && state.notice === `mismatch`)
        return [`已查到此前退出记录`, `此前记录不能确认本次全部退出的结果。`];
      if (state.notice === `mismatch` || detail?.binding === `mismatch`)
        return [
          `暂时无法确认此前的退出结果`,
          `请稍后查看退出结果，暂时不要重复退出。`,
        ];
      if (state.notice === `limited` || state.notice === `unavailable`)
        return [`退出结果尚不确定`, `暂时无法查看退出结果，请稍后再试。`];
      return [`退出结果尚不确定`, `请稍后查看退出结果，暂时不要重复退出。`];
    case `current`:
      switch (state.current.kind) {
        case `checking`:
        case `changed`:
          return [`正在检查登录状态…`, `请稍候。`];
        case `anonymous`:
        case `expired`:
          return [
            state.current.kind === `expired`
              ? `请重新登录`
              : `请使用手机号登录`,
            state.pending
              ? `退出结果尚未确认。请使用原账户的手机号登录，继续查看退出结果。`
              : state.rejected
                ? `本次尚未退出，请重新登录后继续。`
                : `请验证手机号后继续。`,
          ];
        case `denied`:
          return [`暂时无法检查登录状态`, `可以返回首页。`];
        case `reload`:
          return [`请重新加载`, `页面暂时无法继续，请重新加载后再试。`];
        case `storage`:
          return [`登录信息暂时无法使用`, `请重新检查，或使用手机号登录。`];
        default:
          return [`暂时无法检查登录状态`, `请稍后重新检查。`];
      }
  }
}

export function Content({
  controller,
  scope = `current`,
}: {
  controller: PageController;
  scope?: Scope;
}) {
  const state = React.useSyncExternalStore(
    controller.subscribe,
    controller.snapshot,
    controller.snapshot,
  );
  const detail = scope === `all` ? (controller.detail?.() ?? null) : null;
  const all = scope === `all`;
  const [now, setNow] = React.useState(Date.now);
  const [leaving, setLeaving] = React.useState<string | null>(null);
  const [confirming, setConfirming] = React.useState(false);
  const [failedAvatar, setFailedAvatar] = React.useState<string | null>(null);
  const dialog = React.useRef<HTMLDialogElement>(null);
  const confirmDialog = React.useRef<HTMLDialogElement>(null);
  const cancelButton = React.useRef<HTMLButtonElement>(null);
  const trigger = React.useRef<HTMLButtonElement>(null);
  const leaveAllowed = React.useRef(false);
  const heading = React.useRef<HTMLHeadingElement>(null);
  const [title, copy] = message(state, scope, detail);
  const neutral =
    state.kind === `other` ||
    state.kind === `reload` ||
    (state.kind === `storage` && !state.unsubmitted) ||
    state.kind === `current`;
  const pageTitle = neutral
    ? `账户管理`
    : state.kind === `ready` || state.kind === `preparing`
      ? all
        ? `退出全部登录`
        : `退出登录`
      : `退出结果`;
  const avatarHref =
    state.kind === `ready` ? state.account.avatar.href : undefined;
  React.useEffect(() => {
    setFailedAvatar(null);
  }, [avatarHref]);
  React.useEffect(() => {
    document.title = `${pageTitle} · 司籍处`;
  }, [pageTitle]);
  React.useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, []);
  React.useEffect(() => {
    heading.current?.focus();
  }, [title]);
  React.useEffect(() => {
    if (state.kind !== "ready") setConfirming(false);
  }, [state.kind]);
  React.useEffect(() => {
    const element = confirmDialog.current;
    if (!element) return;
    if (confirming && !element.open) {
      element.showModal();
      cancelButton.current?.focus();
    } else if (!confirming && element.open) element.close();
  }, [confirming]);
  const retryAt =
    state.kind === `result`
      ? state.retryAt
      : state.kind === `current` && `retryAt` in state.current
        ? state.current.retryAt
        : 0;
  const seconds = Math.max(0, Math.ceil((retryAt - now) / 1000));
  const loading =
    state.kind === `preparing` ||
    state.kind === `submitting` ||
    state.kind === `querying`;
  const needsReload =
    state.kind === `reload` ||
    state.kind === `random` ||
    (state.kind === `current` && state.current.kind === `reload`);
  const needsLogin =
    state.kind === `missing` ||
    (state.kind === `current` &&
      [`anonymous`, `expired`, `storage`].includes(state.current.kind));
  const loginHref =
    state.kind === `current` &&
    state.pending &&
    [`anonymous`, `expired`].includes(state.current.kind)
      ? returns.loginPath(
          `/authentication/codes`,
          all ? `?return=all-logout-result` : `?return=logout-result`,
        )
      : `/authentication/codes`;
  const canCheck =
    state.kind === `storage` ||
    (state.kind === `current` &&
      [`storage`, `unavailable`, `limited`].includes(state.current.kind));
  const pending = recovery.hasPendingResult(state);
  const queryLabel = seconds ? `${seconds} 秒后可查看` : `查看退出结果`;

  React.useEffect(() => {
    if (!pending) {
      setLeaving(null);
      return;
    }
    leaveAllowed.current = false;
    const before = (event: BeforeUnloadEvent) => {
      if (leaveAllowed.current) return;
      event.preventDefault();
      event.returnValue = ``;
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
          ? event.target.closest<HTMLAnchorElement>(`a[href]`)
          : null;
      if (
        !link ||
        (link.target && link.target !== `_self`) ||
        link.hasAttribute(`download`)
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
    window.addEventListener(`beforeunload`, before);
    document.addEventListener(`click`, follow);
    return () => {
      window.removeEventListener(`beforeunload`, before);
      document.removeEventListener(`click`, follow);
    };
  }, [pending]);
  React.useEffect(() => {
    if (leaving && pending && !dialog.current?.open)
      dialog.current?.showModal();
  }, [leaving, pending]);

  return (
    <div className={`logout-layout`}>
      <div className={`logout-heading`}>
        <p className={`eyebrow`}>{all ? `本人账户` : `账户管理`}</p>
        <h1>{pageTitle}</h1>
      </div>
      <div className={`logout-columns`}>
        <section
          className={`logout-panel`}
          aria-busy={loading}
          aria-labelledby={`logout-stage`}
        >
          <div className={`result-icon`} aria-hidden={`true`}>
            {state.kind === `ready` ? <icons.LogOut /> : <icons.Info />}
          </div>
          <h2 id={`logout-stage`} ref={heading} tabIndex={-1}>
            {title}
          </h2>
          <p className={`result-copy`} aria-live={`polite`}>
            {copy}
          </p>
          {state.kind === `ready` &&
            (all ? (
              <>
                <div className={`logout-person`}>
                  {avatarHref && failedAvatar !== avatarHref ? (
                    <img
                      className={`logout-avatar-image`}
                      src={avatarHref}
                      width={56}
                      height={56}
                      alt={`当前账户头像`}
                      onError={() => setFailedAvatar(avatarHref)}
                    />
                  ) : (
                    <span
                      className={`logout-avatar`}
                      role={`img`}
                      aria-label={`头像暂时无法显示`}
                    >
                      头像
                    </span>
                  )}
                  <div>
                    <strong>{state.account.name}</strong>
                    <p>{`身份证尾号 ${state.account.idNumber.suffix}`}</p>
                  </div>
                </div>
                <div className={`logout-consequences`}>
                  <strong>再次进入个人账户需要重新登录。</strong>
                  <p>账户资料和已提交的申请保留。</p>
                </div>
              </>
            ) : (
              <>
                <dl className={`current-summary`}>
                  <div>
                    <dt>姓名</dt>
                    <dd>{state.account.name}</dd>
                  </div>
                  <div>
                    <dt>身份证尾号</dt>
                    <dd>{state.account.idNumber.suffix}</dd>
                  </div>
                </dl>
                <p className={`scope-note`}>账户资料和已提交的申请保留。</p>
              </>
            ))}
          {all && state.kind === `result` && state.outcome === `complete` && (
            <>
              <dl className={`result-meta`}>
                <div>
                  <dt>已退出的登录</dt>
                  <dd>
                    {detail?.count === null || detail?.count === undefined
                      ? `—`
                      : `${detail.count} 条`}
                  </dd>
                </div>
                {detail?.revokedAt && (
                  <div>
                    <dt>退出时间（北京时间）</dt>
                    <dd>{stamp(detail.revokedAt)}</dd>
                  </div>
                )}
              </dl>
            </>
          )}
          {all &&
            state.kind === `result` &&
            state.outcome === `pending` &&
            state.notice !== "denied" &&
            detail?.record && (
              <>
                <dl className={`result-meta`}>
                  <div>
                    <dt>此前退出的登录</dt>
                    <dd>{`${detail.record.count} 条`}</dd>
                  </div>
                  <div>
                    <dt>退出时间（北京时间）</dt>
                    <dd>{stamp(detail.record.revokedAt)}</dd>
                  </div>
                </dl>
              </>
            )}
          <div className={`result-actions`}>
            {all && state.kind === `ready` && (
              <>
                <ui.Button
                  ref={trigger}
                  className={`primary`}
                  onClick={() => setConfirming(true)}
                >
                  退出全部登录
                </ui.Button>
                <a className={`quiet-link`} href={`/account`}>
                  返回个人账户
                </a>
              </>
            )}
            {!all && state.kind === `ready` && (
              <>
                <ui.Button
                  className={`primary`}
                  onClick={() => void controller.submit()}
                >
                  退出登录
                </ui.Button>
                <a className={`quiet-link`} href={`/account`}>
                  返回个人账户
                </a>
              </>
            )}
            {state.kind === `result` && state.canRead && (
              <ui.Button
                className={`primary`}
                disabled={seconds > 0}
                onClick={() => void controller.query()}
              >
                {queryLabel}
              </ui.Button>
            )}
            {state.kind === `result` && state.canStart && (
              <ui.Button
                className={`quiet-link`}
                disabled={seconds > 0}
                onClick={() => void controller.start()}
              >
                {seconds
                  ? `${seconds} 秒后可重新确认`
                  : all
                    ? `重新确认全部退出`
                    : `重新确认退出`}
              </ui.Button>
            )}
            {all &&
              state.kind === `result` &&
              state.outcome === `complete` &&
              !state.newLogin && (
                <a className={`quiet-link`} href={`/authentication/codes`}>
                  重新登录
                </a>
              )}
            {canCheck && (
              <ui.Button
                className={`primary`}
                disabled={seconds > 0}
                onClick={() => void controller.verify()}
              >
                {seconds ? `${seconds} 秒后可重新检查` : `重新检查`}
              </ui.Button>
            )}
            {needsReload && (
              <ui.Button
                className={`primary`}
                onClick={() => location.reload()}
              >
                重新加载
              </ui.Button>
            )}
            {needsLogin && (
              <a className={`primary`} href={loginHref}>
                手机号登录
              </a>
            )}
            {loading && (
              <ui.Button
                className={`quiet-link`}
                onClick={() => {
                  controller.suspend();
                  void controller.verify();
                }}
              >
                检查登录状态
              </ui.Button>
            )}
            {state.kind !== `ready` &&
              !(
                all &&
                state.kind === "result" &&
                state.notice === "denied"
              ) && (
                <a
                  className={
                    state.kind === `result` &&
                    state.outcome === `complete` &&
                    state.newLogin
                      ? `primary`
                      : `quiet-link`
                  }
                  href={`/account`}
                >
                  打开个人账户
                </a>
              )}
            <a
              className={
                state.kind === `result` &&
                state.outcome === `complete` &&
                !state.newLogin
                  ? `primary`
                  : `quiet-link`
              }
              href={`/`}
            >
              返回首页
            </a>
          </div>
        </section>
      </div>
      {all && state.kind === `ready` && (
        <dialog
          ref={confirmDialog}
          className={`logout-leave`}
          aria-labelledby={`logout-confirm-title`}
          aria-describedby={`logout-confirm-description`}
          onCancel={(event) => {
            event.preventDefault();
            setConfirming(false);
            trigger.current?.focus();
          }}
        >
          <p className={`eyebrow`}>本人确认</p>
          <button
            type={`button`}
            className={`dialog-close`}
            aria-label={`关闭`}
            onClick={() => {
              setConfirming(false);
              trigger.current?.focus();
            }}
          >
            ×
          </button>
          <h2 id={`logout-confirm-title`}>退出全部登录？</h2>
          <p id={`logout-confirm-description`}>
            将退出网站和小程序的全部登录，包括本次登录。再次进入个人账户需要重新登录。
          </p>
          <p className={`minor`}>账户资料和已提交的申请保留。</p>
          <div className={`result-actions`}>
            <ui.Button
              ref={cancelButton}
              className={`quiet-link`}
              onClick={() => {
                setConfirming(false);
                trigger.current?.focus();
              }}
            >
              暂不退出
            </ui.Button>
            <ui.Button
              className={`primary`}
              onClick={() => {
                setConfirming(false);
                void controller.submit();
              }}
            >
              全部退出
            </ui.Button>
          </div>
        </dialog>
      )}
      {pending && leaving && (
        <dialog
          ref={dialog}
          className={`logout-leave`}
          aria-labelledby={`logout-leave-title`}
          onCancel={(event) => {
            event.preventDefault();
            dialog.current?.close();
          }}
          onClose={() => setLeaving(null)}
        >
          <h2 id={`logout-leave-title`}>退出结果尚未确认</h2>
          <p>离开不会撤回已经发出的退出请求。</p>
          <div className={`result-actions`}>
            <ui.Button
              className={`primary`}
              onClick={() => {
                dialog.current?.close();
                setLeaving(null);
              }}
            >
              留在此页
            </ui.Button>
            <ui.Button
              className={`quiet-link`}
              onClick={() => {
                const target = leaving;
                leaveAllowed.current = true;
                controller.suspend();
                setLeaving(null);
                location.assign(target);
              }}
            >
              仍然离开
            </ui.Button>
          </div>
        </dialog>
      )}
    </div>
  );
}
