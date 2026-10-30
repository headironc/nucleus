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

function allMessage(
  state: recovery.State,
  detail: Detail | null,
): [string, string] {
  const unverified = detail?.binding === "mismatch";
  const prefix = unverified
    ? `此前`
    : detail?.binding === "pending"
      ? ""
      : `本次`;
  switch (state.kind) {
    case `ready`:
      return [`退出全部设备`, `如果希望结束其他设备上的登录，可在此统一退出。`];
    case `preparing`:
      return [`正在准备退出`, `正在核对当前登录，本次退出尚未提交。`];
    case `submitting`:
      return state.pending
        ? [`正在提交退出请求`, `请稍候，正在等待本次退出的结果。`]
        : [
            `正在更新登录状态`,
            `退出请求已有确定结果，正在更新本页的登录状态。`,
          ];
    case `querying`:
      return [
        `正在查看${prefix}退出的结果`,
        `只读取${prefix}退出的记录，不会再次退出。`,
      ];
    case `other`:
      return [`当前登录的账户`, `请打开个人账户，查看当前登录的账户资料。`];
    case `missing`:
      return [
        `暂时无法查看此前退出结果`,
        `暂时无法继续查看此前退出结果。这不表示退出失败。你可以检查当前登录、打开个人账户或返回首页。`,
      ];
    case `storage`:
      return [
        `暂时无法继续`,
        state.unsubmitted
          ? `浏览器无法保存退出信息，本次退出尚未提交。请重新检查。`
          : `浏览器暂时无法读取或保存本页信息。请重新检查。`,
      ];
    case `random`:
      return [`本次退出尚未提交`, `暂时无法发起退出，请重新加载页面后再试。`];
    case `reload`:
      return [`请重新加载页面`, `页面暂时无法继续，请重新加载。`];
    case `result`:
      if (state.outcome === `complete`)
        return [
          `本次全部退出已完成`,
          `已结束当时网站和小程序中尚未退出的全部登录，包括发起本次操作的登录。`,
        ];
      if (state.outcome === `rejected`)
        return [
          `本次退出请求未获允许`,
          state.notice === `limited`
            ? `本次请求暂未受理。请稍后返回退出页面。`
            : `本次请求已被拒绝。返回个人账户后，可重新查看当前情况。`,
        ];
      if (detail?.record && state.notice === "mismatch")
        return [
          `已查到此前退出记录`,
          `以下是此前那次退出的结果，不表示本次退出全部设备已完成。`,
        ];
      if (state.notice === `empty`)
        return [
          `暂未查到${prefix}退出结果`,
          `目前仍无法确认此前退出是否完成。请稍后再查看，暂时不要再次退出。`,
        ];
      if (state.notice === `limited`)
        return [
          `暂时无法查看${prefix}退出结果`,
          `请稍后再查看。此前退出结果仍无法确认。`,
        ];
      if (state.notice === `unavailable`)
        return [
          `暂时无法查看${prefix}退出结果`,
          `此前退出是否完成仍无法确认。可重新查看，不会再次退出。`,
        ];
      if (state.notice === `denied`)
        return [
          `暂时无法查看${prefix}退出结果`,
          `此前是否已退出仍无法确认。你可以先返回首页浏览公开内容。`,
        ];
      if (state.notice === `mismatch`)
        return [
          `请查询此前退出的结果`,
          `本次请求与此前退出不一致。请先检查当前登录，再查看此前退出的结果，暂时不要再次退出。`,
        ];
      if (unverified)
        return [
          "请查询此前退出的结果",
          "本次退出的范围尚未核实。可以查看此前退出记录，暂时不要再次退出。",
        ];
      return [
        `暂时无法确认退出结果`,
        `请先检查登录状态，再查看${prefix}退出结果。暂时不要再次退出。`,
      ];
    case `current`:
      switch (state.current.kind) {
        case `checking`:
        case `changed`:
          return [`正在检查登录状态`, `请稍候，检查完成后再继续。`];
        case `anonymous`:
        case `expired`:
          return [
            `请登录后继续`,
            state.pending
              ? `此前退出的结果仍待确认。请使用此前账户登录，再查看退出结果。`
              : state.rejected
                ? `本次退出未受理。请重新登录后再继续。`
                : `请先登录，再打开个人账户。`,
          ];
        case `denied`:
          return [`暂时无法检查登录状态`, `本次读取未完成，你可以返回首页。`];
        case `reload`:
          return [`请重新加载页面`, `页面暂时无法继续。`];
        case `storage`:
          return [
            `暂时无法检查登录状态`,
            `请重新检查；仍无法读取时，可重新登录。`,
          ];
        default:
          return [`暂时无法检查登录状态`, `请稍后重新检查。`];
      }
  }
}

function currentMessage(state: recovery.State): [string, string] {
  switch (state.kind) {
    case `ready`:
      return [
        `确认退出本次登录？`,
        `退出后，在这里查看本人资料或办理事务时，需要重新登录。`,
      ];
    case `preparing`:
      return [`正在准备退出`, `正在核对当前登录，本次退出尚未提交。`];
    case `submitting`:
      if (!state.pending)
        return [
          `正在更新登录状态`,
          `退出请求已有确定结果，正在更新本页的登录状态。`,
        ];
      return [
        `正在退出本次登录`,
        `请稍候。离开页面或停止等待不会撤销已提交的退出。`,
      ];
    case `querying`:
      return [`正在查看退出结果`, `只查看此前退出的结果，不会退出当前登录。`];
    case `other`:
      return [`当前账户已变化`, `请打开个人账户，查看当前登录的账户资料。`];
    case `missing`:
      return [
        `暂时无法查看本次退出结果`,
        `本页已无法查询此前退出，结果仍不能确定。你可以返回首页或前往登录。`,
      ];
    case `storage`:
      return [
        `暂时无法继续`,
        state.unsubmitted
          ? `浏览器无法保存退出信息，本次退出尚未提交。请重新检查。`
          : `浏览器暂时无法读取或保存本页信息。请重新检查。`,
      ];
    case `random`:
      return [`本次退出尚未提交`, `暂时无法发起退出，请重新加载页面后再试。`];
    case `reload`:
      return [`请重新加载页面`, `页面暂时无法继续，请重新加载。`];
    case `result`:
      if (state.outcome === `complete`)
        return [
          state.newLogin ? `此前登录已退出` : `本次登录已退出`,
          state.newLogin
            ? `此前退出已完成。当前的新登录仍然有效，没有因此退出。`
            : `本次退出已完成。其他登录和已提交的办理不受影响，账户及本人资料仍会保留。`,
        ];
      if (state.outcome === `rejected`)
        return [
          `本次退出未受理`,
          state.notice === `limited`
            ? `暂时无法处理，请稍后再试。`
            : `本次退出请求未被接受。你可以检查当前登录状态后再决定是否退出。`,
        ];
      if (state.notice === `empty`)
        return [
          `暂未查到本次退出结果`,
          `这不表示退出失败。请稍后再查看，暂时不要重复退出。`,
        ];
      if (state.notice === `limited`)
        return [
          `请稍后查看退出结果`,
          `暂时无法处理。本次退出是否完成仍待确认。`,
        ];
      if (state.notice === `unavailable`)
        return [
          `暂时无法读取退出结果`,
          `本次查询未能完成，退出结果仍未确定。请稍后再查看，不要重复退出。`,
        ];
      if (state.notice === `denied`)
        return [
          `暂时无法查看退出结果`,
          `本次查询未能完成，不表示此前退出失败。你可以返回首页。`,
        ];
      if (state.notice === `mismatch`)
        return [
          `请查询此前退出的结果`,
          `请求与此前信息不一致。请只查询此前退出，不要再次提交退出。`,
        ];
      return [
        `本次退出结果待确认`,
        `请查看本次退出的结果。查询不会退出当前登录，请勿重复提交退出。`,
      ];
    case `current`:
      switch (state.current.kind) {
        case `checking`:
        case `changed`:
          return [`正在检查登录状态`, `请稍候，检查完成后再继续。`];
        case `anonymous`:
        case `expired`:
          return [
            `请登录后继续`,
            state.pending
              ? `此前退出的结果仍待确认。请使用此前账户登录，再查看退出结果。`
              : state.rejected
                ? `本次退出未受理。请重新登录后再继续。`
                : `请先登录，再打开个人账户。`,
          ];
        case `denied`:
          return [`暂时无法检查登录状态`, `本次读取未完成，你可以返回首页。`];
        case `reload`:
          return [`请重新加载页面`, `页面暂时无法继续。`];
        case `storage`:
          return [
            `暂时无法检查登录状态`,
            `请重新检查；仍无法读取时，可重新登录。`,
          ];
        default:
          return [`暂时无法检查登录状态`, `请稍后重新检查。`];
      }
  }
}

function message(
  state: recovery.State,
  scope: Scope,
  detail: Detail | null,
): [string, string] {
  return scope === `all` ? allMessage(state, detail) : currentMessage(state);
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
  const unverified = all && detail?.binding === "mismatch";
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
  const pageTitle = all
    ? neutral
      ? `账户管理`
      : state.kind === `ready` || state.kind === `preparing`
        ? `退出全部设备`
        : unverified
          ? `此前退出结果`
          : detail?.binding === "pending"
            ? "退出结果"
            : `本次退出结果`
    : neutral
      ? `账户管理`
      : state.kind === `ready` || state.kind === `preparing`
        ? `退出本次登录`
        : state.kind === `submitting` ||
            (state.kind === `result` &&
              state.outcome !== `pending` &&
              !state.newLogin) ||
            (state.kind === `storage` && state.unsubmitted)
          ? `本次退出结果`
          : `此前退出结果`;
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
  const queryLabel = seconds
    ? `${seconds} 秒后可查看`
    : unverified
      ? `查看此前退出结果`
      : all && detail?.binding === "pending"
        ? "查看退出结果"
        : `查看本次退出结果`;

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
                      aria-label={`头像暂时无法加载`}
                    >
                      头像
                    </span>
                  )}
                  <div>
                    <strong>{state.account.name}</strong>
                    <p>{`本人账户 · 身份证尾号 ${state.account.idNumber.suffix}`}</p>
                  </div>
                </div>
                <div className={`logout-consequences`}>
                  <strong>当前登录也会退出</strong>
                  <p>网站和小程序均需重新登录后才能继续使用。</p>
                </div>
                <p className={`logout-preserved`}>
                  账户、实名资料、微信绑定和员工服务均保留。
                </p>
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
                <p className={`scope-note`}>
                  仅结束本次登录，不退出其他设备，也不取消已提交的办理。
                </p>
              </>
            ))}
          {all && state.kind === `result` && state.outcome === `complete` && (
            <>
              <dl className={`result-meta`}>
                <div>
                  <dt>本次退出的登录</dt>
                  <dd>
                    {detail?.count === null || detail?.count === undefined
                      ? `—`
                      : `${detail.count} 个`}
                  </dd>
                </div>
                {detail?.revokedAt && (
                  <div>
                    <dt>退出时间</dt>
                    <dd>{stamp(detail.revokedAt)}</dd>
                  </div>
                )}
              </dl>
              {state.newLogin ? (
                <div className={`current-login`}>
                  <strong>当前的新登录继续保留</strong>
                  <p>
                    本次结果仅对应当时的退出，之后建立的新登录不在本次结果范围内。
                  </p>
                </div>
              ) : (
                <p className={`minor`}>
                  账户、实名资料、微信绑定和员工服务均保留。
                </p>
              )}
            </>
          )}
          {all &&
            state.kind === `result` &&
            state.outcome === `pending` &&
            state.notice !== "denied" &&
            detail?.record && (
              <>
                <p className="minor">
                  以下是此前已读到的记录，不表示本次退出全部设备已完成。
                </p>
                <dl className={`result-meta`}>
                  <div>
                    <dt>此前退出的登录</dt>
                    <dd>{`${detail.record.count} 个`}</dd>
                  </div>
                  <div>
                    <dt>退出时间</dt>
                    <dd>{stamp(detail.record.revokedAt)}</dd>
                  </div>
                </dl>
                <div className={`current-login`}>
                  <strong>当前登录继续保留</strong>
                  <p>读取此前记录不会再次退出。</p>
                </div>
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
                  退出全部设备
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
                  确认退出本次登录
                </ui.Button>
                <a className={`quiet-link`} href={`/account`}>
                  取消退出
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
                    ? `重新确认退出全部设备`
                    : `重新确认退出当前登录`}
              </ui.Button>
            )}
            {all &&
              state.kind === `result` &&
              state.outcome === `complete` &&
              !state.newLogin && (
                <a className={`primary`} href={`/authentication/codes`}>
                  手机号登录
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
                重新加载页面
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
                停止等待并检查登录状态
              </ui.Button>
            )}
            {state.kind !== `ready` &&
              !(
                all &&
                state.kind === "result" &&
                state.notice === "denied"
              ) && (
                <a className={`quiet-link`} href={`/account`}>
                  打开个人账户
                </a>
              )}
            <a className={`quiet-link`} href={`/`}>
              返回首页
            </a>
          </div>
        </section>

        {!all && state.kind === `ready` && (
          <aside className={`logout-guide`}>
            <p className={`eyebrow`}>本次退出的范围</p>
            <h2>账户仍会保留</h2>
            <ul>
              <li>不会注销账户或删除本人资料。</li>
              <li>不会取消已提交的申请和办理。</li>
              <li>不会退出其他设备上的登录。</li>
            </ul>
          </aside>
        )}
        {all && state.kind === `ready` && (
          <aside className={`logout-guide`}>
            <p className={`eyebrow`}>本次退出的范围</p>
            <h2>网站和小程序都会退出</h2>
            <ul>
              <li>当前登录也会退出，需要重新登录。</li>
              <li>不会注销账户或删除本人资料。</li>
              <li>账户、实名资料、微信绑定和员工服务均保留。</li>
            </ul>
          </aside>
        )}
        {(state.kind === `querying` ||
          (state.kind === `result` &&
            state.outcome === `pending` &&
            state.canRead)) && (
          <aside className={`logout-guide`}>
            <p className={`eyebrow`}>
              {all
                ? unverified
                  ? `此前退出的记录`
                  : "退出结果"
                : `此前退出的结果`}
            </p>
            <h2>查询不会再次退出</h2>
            <ul>
              <li>暂未查到不表示退出失败。</li>
              <li>重新登录同一账户后，仍可查看此前退出结果。</li>
              <li>查询只读取结果，不会提交新的退出。</li>
            </ul>
          </aside>
        )}
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
          <h2 id={`logout-confirm-title`}>退出全部设备？</h2>
          <p id={`logout-confirm-description`}>
            当前登录也将退出。网站和小程序均需重新登录后才能继续使用。
          </p>
          <p className={`minor`}>账户、实名资料、微信绑定和员工服务均保留。</p>
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
              确认退出全部设备
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
          <h2 id={`logout-leave-title`}>
            {all
              ? `离开前请留意${unverified ? `此前` : detail?.binding === "pending" ? "" : `本次`}退出结果`
              : `离开前请留意退出结果`}
          </h2>
          <p>
            {all
              ? `${unverified ? `此前` : detail?.binding === "pending" ? "" : `本次`}退出结果仍未确定。离开不会取消已经发出的请求。`
              : `此前退出的结果仍未确定。离开不会取消已提交的退出，也不会再次提交退出。`}
          </p>
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
