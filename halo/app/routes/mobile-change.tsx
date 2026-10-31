import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import type { ReactNode } from "react";
import { Button } from "@base-ui/react/button";
import { Header, Footer } from "../root";
import { Sessions } from "../authentication/session";
import { CodeInput } from "../authentication/code";
import { current } from "../protocol/session";
import {
  complete,
  establish,
  read,
  sendCurrentCode,
  sendNewCode,
  validSendInput,
} from "../protocol/mobile";
import { settings } from "../protocol/codes";
import { currentBuild } from "../protocol/build";
import { reloadRequired } from "../integrity";
import { mobileChangeReturn } from "../logout/return";
import { Turnstile } from "../codes/turnstile";
import {
  MobileChange,
  hasPendingResult,
  hasTransfer,
  type State,
} from "../mobile-change/recovery";
import type { Reason } from "../mobile-change/material";

export function meta() {
  return [
    { title: "更换手机号 · 司籍处" },
    { name: "robots", content: "noindex" },
    {
      name: "description",
      content: "验证当前手机号后更换本账户手机号。",
    },
  ];
}

function useMobileChange() {
  const [controller, setController] = useState<MobileChange>();
  useEffect(() => {
    const sessions = new Sessions();
    const recovery = new MobileChange({
      storage: {
        getItem: (key) => sessionStorage.getItem(key),
        setItem: (key, value) => sessionStorage.setItem(key, value),
        removeItem: (key) => sessionStorage.removeItem(key),
      },
      sessions,
      build: currentBuild,
      current,
      sendCurrent: sendCurrentCode,
      sendNew: sendNewCode,
      establish,
      complete,
      read,
      now: Date.now,
      uuid: () => crypto.randomUUID(),
      marker: () => Boolean(history.state?.haloMobileChange),
      mark: () =>
        history.replaceState({ ...history.state, haloMobileChange: true }, ""),
      unmark: () => {
        const state = { ...(history.state ?? {}) } as Record<string, unknown>;
        delete state.haloMobileChange;
        history.replaceState(state, "");
      },
      readonly: mobileChangeReturn(location.search),
    });
    setController(recovery);
    const hide = () => recovery.suspend();
    const reload = () => recovery.reload();
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

function stamp(value: string | number | null) {
  if (value === null) return null;
  const date = new Date(value);
  return Number.isFinite(date.valueOf())
    ? new Intl.DateTimeFormat("zh-CN", {
        dateStyle: "long",
        timeStyle: "short",
        timeZone: "Asia/Shanghai",
      }).format(date)
    : null;
}

const NOTICE: Record<Reason, string> = {
  limited: "暂时无法继续，请稍后再试。",
  invalid: "请检查填写内容。",
  refused: "填写内容暂时无法使用。",
  challenge: "请重新完成安全验证。",
  wrong_code: "请检查该手机号本次收到的验证码。",
  verification_required: "暂时无法继续本次验证。",
  mobile_taken:
    "本次更换时，新手机号已被其他账户使用。请重新填写新手机号并验证。",
  state_conflict: "本次更换时，新手机号与原手机号相同。请核对后重新填写。",
  mismatch: "暂时无法继续本次更换，请打开个人账户查看当前状态。",
  denied: "目前无法更换手机号。",
  unavailable: "暂时无法继续，请稍后再试。",
  expired: "本次验证已无法继续。",
};

const DELIVERY: Record<string, string> = {
  sent: "验证码已提交发送，请查看该手机号收到的短信。",
  sending: "验证码正在发送，请留意手机短信。",
  unknown: "验证码发送结果尚不确定，请先查看手机短信。",
  failed: "验证码未能发送，请重新获取。",
};

function title(state: State): [string, string] {
  switch (state.kind) {
    case "current":
      switch (state.current.kind) {
        case "checking":
        case "changed":
          return ["正在检查登录状态…", "请稍候。"];
        case "anonymous":
        case "expired":
          return [
            state.current.kind === "expired"
              ? "请重新登录"
              : "请使用手机号登录",
            state.transfer
              ? "请使用原账户的手机号登录，继续查看更换结果。"
              : state.pending
                ? "本次操作结果尚未确认。请登录后查看当前账户状态。"
                : "请验证手机号后继续。",
          ];
        case "denied":
          return ["目前无法更换手机号", "可以返回个人账户或首页。"];
        case "storage":
          return ["登录信息暂时无法使用", "请重新检查，或使用手机号登录。"];
        case "reload":
          return ["请重新加载", "页面暂时无法继续，请重新加载后再试。"];
        default:
          return ["暂时无法检查登录状态", "请稍后重新检查。"];
      }
    case "other":
      return ["当前账户已变化", "请打开个人账户，查看当前登录的账户资料。"];
    case "closed":
      if (state.known)
        return ["此前的手机号更换已完成", "可打开个人账户查看当前资料。"];
      if (state.why === "expired" && state.canRestart)
        return ["本次更换无法继续", "请重新验证当前手机号。"];
      if (state.why === "unknown" || state.why === "expired")
        return [
          "此前操作仍待确认",
          state.transfer
            ? "可查看此前的更换结果，或打开个人账户查看当前状态。"
            : "请打开个人账户，查看当前状态。",
        ];
      return [
        "无法查看更换结果",
        state.transfer
          ? "请查看此前的更换结果，或打开个人账户查看当前状态。"
          : "请打开个人账户，查看当前状态。",
      ];
    case "editing":
      return state.stage === "current"
        ? ["填写当前手机号", "填写此账户现在使用的手机号。"]
        : ["填写新手机号", "填写本人能接收短信的新手机号。"];
    case "limited":
      return ["请稍后重试", "暂时无法获取验证码，请稍后再试。"];
    case "coding":
      return [
        state.stage === "current"
          ? "输入当前手机号收到的验证码"
          : "输入新手机号收到的验证码",
        "请输入本次短信中的 6 位验证码。",
      ];
    case "confirm":
      return ["确认更换手机号", "请核对新手机号及更换后的影响。"];
    case "busy":
      return [
        state.action === "send"
          ? "正在获取验证码…"
          : state.action === "establish"
            ? "正在验证当前手机号…"
            : state.action === "complete"
              ? "正在更换手机号…"
              : "正在查看更换结果…",
        "请稍候。",
      ];
    case "unknown":
      if (state.step === "current-send" || state.step === "new-send")
        return [
          state.sending ? "验证码正在发送" : "验证码发送待确认",
          state.canRetry
            ? "请先查看手机短信，稍后可重试获取验证码。"
            : "请先查看手机短信。",
        ];
      if (state.step === "establish")
        return [
          "验证结果尚不确定",
          state.canRetry
            ? "可继续本次验证，暂时不要修改验证码或重新获取短信。"
            : state.canQuery
              ? "请查看更换结果。"
              : "请打开个人账户，查看当前状态。",
        ];
      return [
        "更换结果尚不确定",
        state.canQuery
          ? "请先查看更换结果。"
          : state.canRetry
            ? "可继续本次更换，暂时不要修改填写内容。"
            : "请打开个人账户，查看当前状态。",
      ];
    case "known":
      return state.showAccount
        ? ["此前的手机号更换已完成", "可打开个人账户查看当前资料。"]
        : ["手机号已更换", "请使用新手机号重新登录。"];
    case "query":
      switch (state.status) {
        case "idle":
          return ["查看更换结果", "本次更换无法在当前登录中继续。"];
        case "pending":
          return state.unknown
            ? ["更换结果尚不确定", "请稍后查看更换结果，暂时不要重新办理。"]
            : state.canContinue
              ? ["手机号尚未更换", "请继续验证新手机号。"]
              : [
                  "本次更换尚未完成",
                  "本次更换无法在当前登录中继续。可以查看更换结果，或返回个人账户。",
                ];
        case "expired":
          return state.unknown
            ? ["更换结果尚不确定", "请稍后查看更换结果。"]
            : ["本次更换无法继续", "请重新验证当前手机号。"];
        case "limited":
          return ["暂时无法查看更换结果", "请稍后再试。"];
        case "denied":
          return ["无法查看更换结果", "可以返回个人账户或首页。"];
        case "authentication":
          return ["请重新登录", "请使用原账户的手机号登录，继续查看更换结果。"];
        default:
          return [
            "暂时无法查看更换结果",
            "请稍后再试，或打开个人账户查看当前状态。",
          ];
      }
    case "storage":
      return [
        state.pending ? "本次结果尚不确定" : "暂时无法继续",
        "请重新检查。",
      ];
    case "reload":
      return [
        "请重新加载",
        state.pending
          ? "本次结果尚未确认，请重新加载后按页面提示继续。"
          : "页面暂时无法继续，请重新加载后再试。",
      ];
    case "random":
      return ["本次尚未提交", "请重新加载后再试。"];
  }
}

function Steps({ index }: { index: number }) {
  return (
    <ol className="mobile-change-steps" aria-label="办理步骤">
      {["1 验证当前手机号", "2 验证新手机号", "3 确认更换"].map(
        (label, position) => (
          <li
            key={label}
            aria-current={position === index ? "step" : undefined}
          >
            {label}
          </li>
        ),
      )}
    </ol>
  );
}

function useSiteKey(operation: string | undefined) {
  const [site, setSite] = useState<{ operation: string; key: string }>();
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    if (!operation) return;
    setFailed(false);
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), 10_000);
    let active = true;
    settings(controller.signal)
      .then((key) => {
        if (active) setSite({ operation, key });
      })
      .catch(() => {
        if (active) setFailed(true);
      });
    return () => {
      active = false;
      clearTimeout(timer);
      controller.abort();
    };
  }, [operation]);
  return {
    key: site && site.operation === operation ? site.key : undefined,
    failed,
  };
}

function Notice({ reason }: { reason: Reason | null }) {
  if (!reason) return null;
  return (
    <p
      className={reason === "limited" ? "field-hint" : "field-error"}
      role="status"
    >
      {NOTICE[reason]}
    </p>
  );
}

function AddressForm({
  state,
  controller,
  operation,
  siteKey,
  settingsFailed,
  now,
  onDirty,
}: {
  state: Extract<State, { kind: "editing" }>;
  controller: MobileChange;
  operation: string;
  siteKey: string | undefined;
  settingsFailed: boolean;
  now: number;
  onDirty: (dirty: boolean) => void;
}) {
  const [mobile, setMobile] = useState(state.mobile);
  const [token, setToken] = useState<string>();
  const [error, setError] = useState("");
  const input = useRef<HTMLInputElement>(null);
  const seconds = Math.max(0, Math.ceil((state.retryAt - now) / 1000));
  useEffect(() => setMobile(state.mobile), [state.mobile]);
  useEffect(() => onDirty(mobile.length > 0), [mobile, onDirty]);
  useEffect(() => {
    input.current?.focus();
  }, [state.stage]);
  const label = state.stage === "current" ? "当前手机号" : "新手机号";
  return (
    <>
      <Steps index={state.stage === "current" ? 0 : 1} />
      <h2>{state.stage === "current" ? "填写当前手机号" : "填写新手机号"}</h2>
      <p className="mobile-change-lead">
        {state.stage === "current"
          ? "填写此账户现在使用的手机号。"
          : "填写本人能接收短信的新手机号。"}
      </p>
      <Notice reason={state.notice} />
      <form
        noValidate
        autoComplete="off"
        onSubmit={(event) => {
          event.preventDefault();
          if (seconds > 0) return;
          if (!validSendInput({ mobile, token: "fixture" })) {
            setError("请输入 11 位中国大陆手机号，不含空格或区号。");
            input.current?.focus();
            return;
          }
          if (!token) {
            setError("请先完成安全验证。");
            return;
          }
          setError("");
          if (state.stage === "current")
            void controller.submitCurrent(mobile, token);
          else void controller.submitNew(mobile, token);
        }}
      >
        <label htmlFor="mobile-change-number">{label}</label>
        <input
          ref={input}
          id="mobile-change-number"
          name="mobile"
          type="tel"
          inputMode="numeric"
          autoComplete="off"
          maxLength={11}
          value={mobile}
          aria-invalid={!!error}
          aria-describedby="mobile-change-hint mobile-change-error"
          placeholder="请输入 11 位手机号"
          onChange={(event) => {
            setMobile(event.target.value);
            setError("");
          }}
        />
        <p id="mobile-change-hint" className="field-hint">
          {state.stage === "current"
            ? "当前手机号无法接收短信时，无法在此更换。"
            : "请填写 11 位中国大陆手机号，不含空格或区号。"}
        </p>
        <p id="mobile-change-error" className="field-error" role="alert">
          {error}
        </p>
        {state.stage === "new" && (
          <p className="field-hint">
            请在当前手机号验证后的 10 分钟有效期内完成更换。
          </p>
        )}
        {!siteKey && (
          <div className="notice" role="status">
            <p>
              {settingsFailed
                ? "安全验证暂时无法加载，请重新加载。"
                : "正在加载安全验证…"}
            </p>
          </div>
        )}
        {settingsFailed && (
          <Button className="text-button" onClick={() => location.reload()}>
            重新加载
          </Button>
        )}
        {siteKey && (
          <Turnstile
            siteKey={siteKey}
            operation={operation}
            action={
              state.stage === "current"
                ? "send_mobile_change_code"
                : "send_mobile_binding_code"
            }
            waiting={false}
            reset={0}
            onToken={setToken}
          />
        )}
        <Button
          className="primary send-button"
          type="submit"
          disabled={!siteKey || !token || seconds > 0}
        >
          {seconds > 0
            ? `${seconds} 秒后可获取`
            : state.stage === "current"
              ? "获取验证码"
              : "获取新手机号验证码"}
        </Button>
      </form>
      <div className="mobile-change-actions">
        <a className="quiet-link" href="/account">
          打开个人账户
        </a>
      </div>
    </>
  );
}

function CodeForm({
  state,
  controller,
  now,
  onDirty,
}: {
  state: Extract<State, { kind: "coding" }>;
  controller: MobileChange;
  now: number;
  onDirty: (dirty: boolean) => void;
}) {
  const [code, setCode] = useState("");
  const [incomplete, setIncomplete] = useState(false);
  const seconds = Math.max(0, Math.ceil((state.retryAt - now) / 1000));
  useEffect(() => onDirty(code.length > 0), [code, onDirty]);
  useEffect(() => {
    const input = document.querySelector<HTMLInputElement>(".code-slots input");
    input?.focus();
  }, [state.stage, state.notice]);
  return (
    <>
      <Steps index={state.stage === "current" ? 0 : 1} />
      <h2>
        {state.stage === "current"
          ? "输入当前手机号收到的验证码"
          : "输入新手机号收到的验证码"}
      </h2>
      <p className="mobile-change-lead">{DELIVERY[state.delivery]}</p>
      <p className="field-hint">请输入本次短信中的 6 位验证码。</p>
      {state.stage === "new" && (
        <p className="field-hint">
          请在当前手机号验证后的 10 分钟有效期内完成更换。
        </p>
      )}
      <Notice reason={state.notice} />
      <form
        autoComplete="off"
        onSubmit={(event) => {
          event.preventDefault();
          if (seconds > 0) return;
          if (!/^[0-9]{6}$/.test(code)) {
            setIncomplete(true);
            const inputs = Array.from(
              event.currentTarget.querySelectorAll<HTMLInputElement>(
                ".code-slots input",
              ),
            );
            inputs.find((input) => !input.value)?.focus();
            return;
          }
          setIncomplete(false);
          if (state.stage === "current")
            void controller.submitCurrentCode(code);
          else void controller.submitNewCode(code);
        }}
      >
        <CodeInput
          value={code}
          onChange={(next) => {
            setCode(next);
            setIncomplete(false);
          }}
          disabled={false}
          invalid={state.notice === "wrong_code"}
          incomplete={incomplete}
        />
        <Button className="primary send-button" type="submit">
          {state.stage === "current" ? "验证并继续" : "继续"}
        </Button>
      </form>
      <div className="mobile-change-actions">
        <Button
          className="text-button"
          disabled={seconds > 0}
          onClick={() => controller.resend()}
        >
          {seconds > 0 ? `${seconds} 秒后可重新获取` : "重新获取验证码"}
        </Button>
        <a className="quiet-link" href="/account">
          打开个人账户
        </a>
      </div>
    </>
  );
}

function ConfirmPanel({
  state,
  controller,
  now,
}: {
  state: Extract<State, { kind: "confirm" }>;
  controller: MobileChange;
  now: number;
}) {
  const seconds = Math.max(0, Math.ceil((state.retryAt - now) / 1000));
  const deadline = stamp(state.expiresAt);
  return (
    <>
      <Steps index={2} />
      <h2>确认更换手机号</h2>
      <p className="mobile-change-lead">本账户今后将使用以下号码登录：</p>
      <p className="mobile-change-number">{state.mobile}</p>
      <ul className="mobile-change-consequences">
        <li>更换后，网站和小程序的全部登录将退出。</li>
        <li>请使用新手机号重新登录，原账户资料和业务记录保留。</li>
      </ul>
      <Notice reason={state.notice} />
      <p className="field-hint">
        {deadline
          ? `请在 ${deadline}（北京时间）前完成。`
          : "请确认后完成更换。"}
      </p>
      <div className="mobile-change-actions">
        <Button
          className="primary"
          disabled={seconds > 0}
          onClick={() => void controller.confirm()}
        >
          {seconds > 0 ? `${seconds} 秒后可确认` : "确认更换手机号"}
        </Button>
        <Button className="quiet-link" onClick={() => controller.editCode()}>
          返回修改
        </Button>
      </div>
    </>
  );
}

function Content({ controller }: { controller: MobileChange }) {
  const state = useSyncExternalStore(
    controller.subscribe,
    controller.snapshot,
    controller.snapshot,
  );
  const [now, setNow] = useState(Date.now);
  const [leaving, setLeaving] = useState<string | null>(null);
  const [dirty, setDirty] = useState(false);
  const heading = useRef<HTMLHeadingElement>(null);
  const leaveDialog = useRef<HTMLDialogElement>(null);
  const stay = useRef<HTMLButtonElement>(null);
  const leaveAllowed = useRef(false);
  const [headingTitle, description] = title(state);
  const maskedPage = state.kind === "current" || state.kind === "other";
  const pageTitle = maskedPage
    ? state.kind === "other"
      ? "个人账户"
      : headingTitle
    : "更换手机号";
  const operation =
    state.kind === "editing"
      ? state.stage === "current"
        ? controller.currentOperation()
        : controller.newOperation()
      : undefined;
  const site = useSiteKey(operation);
  const pending = hasPendingResult(state);
  const transfer =
    hasTransfer(state) || (state.kind === "current" && state.transfer);
  const unknown =
    state.kind === "unknown" ||
    (state.kind === "query" && state.unknown) ||
    (state.kind === "closed" &&
      (state.why === "unknown" ||
        (state.why === "expired" && !state.canRestart))) ||
    (state.kind === "storage" && state.pending) ||
    (state.kind === "reload" && state.pending);
  const guarded =
    state.kind !== "current" &&
    state.kind !== "other" &&
    state.kind !== "known" &&
    (unknown ||
      transfer ||
      ((state.kind === "editing" || state.kind === "coding") && dirty));

  useEffect(() => {
    const timer = setInterval(() => {
      setNow(Date.now());
      controller.expire();
    }, 1000);
    return () => clearInterval(timer);
  }, [controller]);
  useEffect(() => {
    document.title = `${headingTitle} · 司籍处`;
    if (state.kind !== "editing" && state.kind !== "coding")
      heading.current?.focus();
  }, [headingTitle, state.kind]);
  useEffect(() => {
    if (!guarded) setLeaving(null);
  }, [guarded]);
  useEffect(() => {
    const element = leaveDialog.current;
    if (leaving && guarded && element && !element.open) {
      element.showModal();
      stay.current?.focus();
    }
  }, [leaving, guarded]);
  useEffect(() => {
    if (!guarded) return;
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
  }, [guarded]);

  const login = (primary: boolean) => (
    <a
      className={primary ? "primary" : "quiet-link"}
      href={
        state.kind !== "current" || pending || transfer || state.known !== null
          ? "/authentication/codes?return=mobile-change"
          : "/authentication/codes"
      }
    >
      {state.kind === "known" && !state.showAccount
        ? "使用新手机号登录"
        : "手机号登录"}
    </a>
  );
  const account = (primary = false) => (
    <a className={primary ? "primary" : "quiet-link"} href="/account">
      打开个人账户
    </a>
  );
  const home = () => (
    <a className="quiet-link" href="/">
      返回首页
    </a>
  );
  const actions = (nodes: ReactNode) => (
    <div className="mobile-change-actions">{nodes}</div>
  );
  const retryAt =
    state.kind === "current"
      ? "retryAt" in state.current
        ? state.current.retryAt
        : 0
      : "retryAt" in state
        ? state.retryAt
        : 0;
  const retrySeconds = Math.max(0, Math.ceil((retryAt - now) / 1000));

  return (
    <div className="mobile-change-layout">
      <a className="mobile-change-back" href="/account">
        ← 返回个人账户
      </a>
      <div className="mobile-change-heading">
        <p className="eyebrow">个人账户</p>
        <h1 ref={heading} tabIndex={-1}>
          {pageTitle}
        </h1>
        {(state.kind === "editing" ||
          state.kind === "coding" ||
          state.kind === "confirm") && (
          <p>先验证当前手机号，再验证新手机号。</p>
        )}
      </div>
      <div className="mobile-change-columns">
        <section
          className="mobile-change-panel"
          aria-busy={state.kind === "busy"}
        >
          {state.kind === "editing" && (
            <AddressForm
              state={state}
              controller={controller}
              operation={operation!}
              siteKey={site.key}
              settingsFailed={site.failed}
              now={now}
              onDirty={setDirty}
            />
          )}
          {state.kind === "coding" && (
            <CodeForm
              state={state}
              controller={controller}
              now={now}
              onDirty={setDirty}
            />
          )}
          {state.kind === "confirm" && (
            <ConfirmPanel state={state} controller={controller} now={now} />
          )}
          {state.kind === "busy" && (
            <>
              <h2>{headingTitle}</h2>
              <p className="mobile-change-lead">{description}</p>
              {actions(
                <>
                  <Button className="primary" disabled>
                    请稍候
                  </Button>
                  {home()}
                </>,
              )}
            </>
          )}
          {state.kind === "limited" && (
            <>
              <h2>{headingTitle}</h2>
              <p className="mobile-change-lead">{description}</p>
              {actions(
                <>
                  <Button
                    className="primary"
                    disabled={retrySeconds > 0}
                    onClick={() => void controller.retry()}
                  >
                    {retrySeconds > 0
                      ? `${retrySeconds} 秒后可重试`
                      : state.stage === "current"
                        ? "重试获取当前手机号验证码"
                        : "重试获取新手机号验证码"}
                  </Button>
                  {account()}
                  {home()}
                </>,
              )}
            </>
          )}
          {state.kind === "unknown" && (
            <>
              <h2>{headingTitle}</h2>
              <p className="mobile-change-lead">{description}</p>
              {state.notice === "limited" ? (
                <p className="field-hint" role="status">
                  暂时无法继续，请稍后再试。本次结果仍未确认。
                </p>
              ) : (
                <Notice reason={state.notice} />
              )}
              {!state.persisted && (
                <p className="field-hint">
                  本页暂时无法保存办理信息，离开后可能无法继续。
                </p>
              )}
              {actions(
                <>
                  {state.canRetry && (
                    <Button
                      className="primary"
                      disabled={retrySeconds > 0}
                      onClick={() => void controller.retry()}
                    >
                      {retrySeconds > 0
                        ? `${retrySeconds} 秒后可${state.step === "current-send" || state.step === "new-send" ? "重试" : "继续"}`
                        : state.step === "establish"
                          ? "继续本次验证"
                          : state.step === "complete"
                            ? "继续本次更换"
                            : state.step === "current-send"
                              ? "重试获取当前手机号验证码"
                              : "重试获取新手机号验证码"}
                    </Button>
                  )}
                  {state.canQuery && (
                    <Button
                      className={state.canRetry ? "quiet-link" : "primary"}
                      disabled={retrySeconds > 0}
                      onClick={() => void controller.query()}
                    >
                      {retrySeconds > 0
                        ? `${retrySeconds} 秒后可查看`
                        : "查看更换结果"}
                    </Button>
                  )}
                  {account(!state.canRetry && !state.canQuery)}
                  {home()}
                </>,
              )}
            </>
          )}
          {state.kind === "known" && (
            <>
              <h2>{headingTitle}</h2>
              <p className="mobile-change-lead">{description}</p>
              <p className="mobile-change-time">
                完成时间（北京时间）：
                {stamp(state.known.completedAt) ?? "暂时无法显示"}
              </p>
              {!state.persisted && (
                <p className="field-hint">
                  本页暂时无法保存结果，离开后可能无法再次查看本次结果。
                </p>
              )}
              {state.queryFailed && (
                <p className="field-hint">更换记录暂时无法加载。</p>
              )}
              {actions(
                <>
                  {state.showAccount ? account(true) : login(true)}
                  {state.canRestart && (
                    <Button
                      className="secondary"
                      onClick={() => controller.restart()}
                    >
                      再次更换手机号
                    </Button>
                  )}
                  {state.showAccount ? login(false) : account()}
                  {home()}
                </>,
              )}
            </>
          )}
          {state.kind === "query" && (
            <>
              <h2>{headingTitle}</h2>
              <p className="mobile-change-lead">{description}</p>
              {state.canContinue &&
                state.status === "pending" &&
                state.expiresAt && (
                  <p className="mobile-change-time">
                    请在 {stamp(state.expiresAt) ?? "有效期内"} 前完成。
                  </p>
                )}
              {actions(
                <>
                  {state.canContinue && (
                    <Button
                      className="primary"
                      onClick={() => controller.resume()}
                    >
                      {state.status === "pending" ? "继续验证新手机号" : "继续"}
                    </Button>
                  )}
                  {state.canContinue ||
                  state.unknown ||
                  state.status !== "expired" ? (
                    <Button
                      className={state.canContinue ? "quiet-link" : "primary"}
                      disabled={retrySeconds > 0}
                      onClick={() => void controller.query()}
                    >
                      {retrySeconds > 0
                        ? `${retrySeconds} 秒后可查看`
                        : "查看更换结果"}
                    </Button>
                  ) : null}
                  {state.canRetry && (
                    <Button
                      className={
                        state.canContinue ? "text-button" : "quiet-link"
                      }
                      disabled={retrySeconds > 0}
                      onClick={() => void controller.retry()}
                    >
                      {retrySeconds > 0
                        ? `${retrySeconds} 秒后可继续`
                        : "继续本次更换"}
                    </Button>
                  )}
                  {state.status === "expired" && !state.unknown && (
                    <Button
                      className="primary"
                      onClick={() => controller.restart()}
                    >
                      重新验证当前手机号
                    </Button>
                  )}
                  {account()}
                  {home()}
                </>,
              )}
            </>
          )}
          {state.kind === "closed" && (
            <>
              <h2>{headingTitle}</h2>
              <p className="mobile-change-lead">{description}</p>
              {actions(
                <>
                  {state.why === "expired" && state.canRestart && (
                    <Button
                      className={state.known ? "secondary" : "primary"}
                      onClick={() => controller.restart()}
                    >
                      重新验证当前手机号
                    </Button>
                  )}
                  {state.transfer && (
                    <Button
                      className={
                        state.canRestart || state.known
                          ? "quiet-link"
                          : "primary"
                      }
                      disabled={retrySeconds > 0}
                      onClick={() => void controller.query()}
                    >
                      {retrySeconds > 0
                        ? `${retrySeconds} 秒后可查看`
                        : "查看更换结果"}
                    </Button>
                  )}
                  {account(
                    !!state.known || (!state.transfer && !state.canRestart),
                  )}
                  {home()}
                </>,
              )}
            </>
          )}
          {state.kind === "other" && (
            <>
              <h2>{headingTitle}</h2>
              <p className="mobile-change-lead">{description}</p>
              {actions(
                <>
                  {account(true)}
                  {home()}
                </>,
              )}
            </>
          )}
          {state.kind === "current" && (
            <>
              <p className="mobile-change-lead">{description}</p>
              {actions(
                <>
                  {state.current.kind === "reload" ? (
                    <Button
                      className="primary"
                      onClick={() => location.reload()}
                    >
                      重新加载
                    </Button>
                  ) : ["checking", "changed"].includes(state.current.kind) ? (
                    <Button className="primary" disabled>
                      请稍候
                    </Button>
                  ) : (
                    <Button
                      className="primary"
                      disabled={retrySeconds > 0}
                      onClick={() => void controller.verify()}
                    >
                      {retrySeconds > 0
                        ? `${retrySeconds} 秒后重新检查`
                        : "检查登录状态"}
                    </Button>
                  )}
                  {["anonymous", "expired", "storage"].includes(
                    state.current.kind,
                  ) && login(false)}
                  {account()}
                  {home()}
                </>,
              )}
            </>
          )}
          {state.kind === "storage" && (
            <>
              <h2>{headingTitle}</h2>
              <p className="mobile-change-lead">{description}</p>
              {actions(
                <>
                  <Button
                    className="primary"
                    onClick={() => void controller.verify()}
                  >
                    重新检查
                  </Button>
                  {account()}
                  {home()}
                </>,
              )}
            </>
          )}
          {state.kind === "reload" && (
            <>
              <h2>{headingTitle}</h2>
              <p className="mobile-change-lead">{description}</p>
              {actions(
                <>
                  <Button className="primary" onClick={() => location.reload()}>
                    重新加载
                  </Button>
                  {home()}
                </>,
              )}
            </>
          )}
          {state.kind === "random" && (
            <>
              <h2>{headingTitle}</h2>
              <p className="mobile-change-lead">{description}</p>
              {actions(
                <>
                  <Button className="primary" onClick={() => location.reload()}>
                    重新加载
                  </Button>
                  {home()}
                </>,
              )}
            </>
          )}
          {"account" in state &&
            (state.kind !== "known" || state.showAccount) && (
              <p className="mobile-change-person">
                当前账户：<strong>{state.account.name}</strong>
              </p>
            )}
        </section>
      </div>
      {guarded && leaving && (
        <dialog
          ref={leaveDialog}
          className="mobile-change-dialog"
          aria-labelledby="mobile-change-leave-title"
          onCancel={(event) => {
            event.preventDefault();
            leaveDialog.current?.close();
          }}
          onClose={() => setLeaving(null)}
        >
          <h2 id="mobile-change-leave-title">
            {unknown
              ? "更换结果尚未确认"
              : transfer
                ? "暂时离开换号办理？"
                : "放弃当前填写？"}
          </h2>
          <p>
            {unknown
              ? "离开不会撤回已发出的请求。"
              : transfer
                ? "离开后有效期仍继续，请在本次登录中完成。"
                : "离开后需要重新填写。"}
          </p>
          <div className="mobile-change-actions">
            <Button
              ref={stay}
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
                if (target) location.assign(target);
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

export default function MobileChangePage() {
  const controller = useMobileChange();
  return (
    <>
      <Header />
      <main id="main">
        {controller ? (
          <Content controller={controller} />
        ) : (
          <div className="loading-shell">
            <p>正在打开更换手机号页面…</p>
          </div>
        )}
      </main>
      <Footer />
    </>
  );
}
