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
  limited: "本次申请尚未受理，请等待后再试。",
  invalid: "请检查填写内容。",
  refused: "本次请求未被接纳，请检查填写内容。",
  challenge: "安全验证未通过，请重新完成验证。",
  wrong_code: "验证码不正确，请查看该号码最新收到的短信。",
  verification_required: "验证码或本次办理已不再适用，请重新开始。",
  mobile_taken: "新手机号已被使用，请使用其他号码并重新获取新号验证码。",
  state_conflict: "新手机号与当前手机号相同，请填写其他号码。",
  mismatch: "本次请求与先前填写内容不一致，已停止继续提交。",
  denied: "你无权进行此操作。",
  unavailable: "暂时无法完成本次请求，请稍后再试。",
  expired: "本次验证已不能继续，请重新获取验证码。",
};

const DELIVERY: Record<string, string> = {
  sent: "短信已提交发送，请查看该号码收到的短信。受理不代表已送达。",
  sending: "短信仍在发送，请先留意手机短信。",
  unknown: "暂时无法确认短信是否已发送，请先查看手机短信。",
  failed: "本次短信发送已确认未成功，请重新获取验证码。",
};

function title(state: State): [string, string] {
  switch (state.kind) {
    case "current":
      switch (state.current.kind) {
        case "checking":
        case "changed":
          return ["正在检查登录状态", "请稍候，检查完成后再继续。"];
        case "anonymous":
        case "expired":
          return [
            "请先登录",
            state.pending
              ? "此前办理的结果仍待确认。请使用原账户登录，再查看原办理。"
              : "登录后再管理本人账户。",
          ];
        case "denied":
          return [
            "无法查看这项内容",
            "本次读取未能完成，你可以返回个人账户或首页。",
          ];
        case "storage":
          return [
            "暂时无法检查登录状态",
            "浏览器无法读取登录信息，请先重新检查；仍无法确认时可在新标签页重新登录。",
          ];
        case "reload":
          return ["请重新加载页面", "页面暂时无法继续，请重新加载。"];
        case "limited":
          return ["请稍后再检查", "暂时无法检查登录状态，请稍后重试。"];
        default:
          return ["暂时无法读取当前账户", "请稍后重新检查。"];
      }
    case "other":
      return [
        "当前账户已更新",
        "这里不再显示此前账户的办理信息。可以返回个人账户继续。",
      ];
    case "closed":
      if (state.known)
        return [
          "本次手机号更换已完成",
          "暂时无法读取本次办理记录，已确认的完成结果仍然有效。",
        ];
      if (state.why === "expired")
        return [
          "本次办理已不能继续",
          "这份办理已失效。重新更换时，请从验证当前手机号开始。",
        ];
      if (state.why === "missing")
        return [
          "暂时无法查看本次更换结果",
          "目前无法读取这项办理。这不表示更换已经失败。",
        ];
      return [
        "暂时无法查看本次更换结果",
        "本页保存的办理信息已不可用，结果仍不能确定。这不表示更换失败。",
      ];
    case "editing":
      return state.stage === "current"
        ? ["填写当前手机号", "请输入本账户目前使用的中国大陆手机号。"]
        : ["填写新手机号", "请输入准备使用的新手机号。"];
    case "limited":
      return ["请稍后重试发送", "本次申请尚未受理，请等待后再试。"];
    case "coding":
      return state.stage === "current"
        ? [
            "输入当前手机号验证码",
            "验证码仅用于本次更换手机号，登录验证码不能用于此处。",
          ]
        : [
            "输入新手机号验证码",
            "验证码仅用于绑定新手机号，登录验证码不能用于此处。",
          ];
    case "confirm":
      return ["确认更换手机号", "请核对新手机号及更换后的影响。"];
    case "busy":
      return state.action === "send"
        ? ["正在申请发送验证码", "请稍候，暂时不用重复操作。"]
        : state.action === "establish"
          ? ["正在验证当前手机号", "请稍候。"]
          : state.action === "complete"
            ? [
                "正在提交更换申请",
                "请稍候。离开页面或停止等待不会取消已提交的申请。",
              ]
            : ["正在查询本次更换", "只查询本次办理，不会重新提交。"];
    case "unknown":
      if (state.sending)
        return [
          "短信仍在发送",
          "请先留意手机短信，稍后查看本次发送结果。暂时不用重新获取验证码。",
        ];
      return state.step === "current-send" || state.step === "new-send"
        ? [
            "暂时无法确认验证码是否已发送",
            "请先查看手机短信。当前尚未取得可用于验证的发送结果，请勿再次获取验证码。",
          ]
        : state.step === "establish"
          ? [
              "暂时无法确认当前号验证结果",
              "请先查看本次验证结果，暂时不要修改验证码或重新获取短信。",
            ]
          : [
              "暂时无法确认更换结果",
              "请先查询本次办理。返回个人账户或离开页面不会取消已提交的申请。",
            ];
    case "known":
      return [
        "手机号已更换",
        "所有设备上的原登录已退出。请使用新手机号重新登录。",
      ];
    case "query":
      switch (state.status) {
        case "idle":
          return [
            "现在可以查询原办理",
            "登录成功不代表换号已经完成，请查询本次办理。",
          ];
        case "pending":
          return state.unknown
            ? [
                "本次更换状态尚未确认",
                "读取时办理仍未完成。这不表示此前的提交已经失败或不再执行，请不要重新提交。",
              ]
            : state.canContinue
              ? [
                  "本次更换尚未完成",
                  "请在有效期内，在本次登录中继续验证新手机号。",
                ]
              : [
                  "本次更换尚未完成",
                  "当前登录可以查询记录，无法继续此前登录建立的办理。",
                ];
        case "expired":
          return [
            "本次办理已不能继续",
            "这份办理已失效，不能据此认为更换已经完成。",
          ];
        case "limited":
          return [
            "暂时无法查询本次更换",
            "请稍后再查询，不要重新提交更换申请。",
          ];
        case "denied":
          return [
            "无法查看这项内容",
            "本次查询未能完成，你可以返回个人账户或首页。",
          ];
        case "authentication":
          return [
            "请重新登录后继续查询",
            "当前登录已失效，请使用原账户登录后再查询。",
          ];
        case "unavailable":
          return [
            "暂时无法查看本次更换结果",
            "目前无法读取这项办理，这不表示更换已经失败。",
          ];
        default:
          return ["更换结果查询失败", "暂时无法查询本次办理，请稍后再试。"];
      }
    case "storage":
      return [
        state.pending ? "暂时无法继续" : "本次申请尚未提交",
        state.pending
          ? "浏览器暂时无法读取或保存本页信息，请重新检查。"
          : "浏览器无法保存办理信息，因此没有发出申请。请重新检查。",
      ];
    case "reload":
      return [
        "请重新加载页面",
        state.pending
          ? "本次办理结果仍未确定，请重新加载后继续查询。"
          : "页面暂时无法继续，请重新加载。",
      ];
    case "random":
      return [
        "暂时无法安全地办理",
        "浏览器暂时无法准备本次申请，请重新加载页面。",
      ];
  }
}

function Steps({ index }: { index: number }) {
  return (
    <ol className="mobile-change-steps" aria-label="办理步骤">
      {["1 验证当前号", "2 验证新号", "3 确认更换"].map((label, position) => (
        <li key={label} aria-current={position === index ? "step" : undefined}>
          {label}
        </li>
      ))}
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
          ? "先验证本账户目前使用的手机号。"
          : "当前手机号已验证。请在有效期内验证新手机号。"}
      </p>
      <Notice reason={state.notice} />
      <form
        noValidate
        autoComplete="off"
        onSubmit={(event) => {
          event.preventDefault();
          if (seconds > 0) return;
          if (!validSendInput({ mobile, token: "fixture" })) {
            setError("请输入正确的中国大陆手机号，不含空格或区号。");
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
            ? "填写后，系统会核对是否为本账户的当前手机号。当前号码无法接收短信时，本人换号无法继续，请联系管理员。"
            : "请使用本人能接收短信的号码。新号验证码在最后确认时才校验。"}
        </p>
        <p id="mobile-change-error" className="field-error" role="alert">
          {error}
        </p>
        {!siteKey && (
          <div className="notice" role="status">
            <p>
              {settingsFailed
                ? "暂时无法加载安全验证设置，请重新加载页面。"
                : "正在加载安全验证设置…"}
            </p>
          </div>
        )}
        {settingsFailed && (
          <Button className="text-button" onClick={() => location.reload()}>
            重新加载页面
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
          {seconds > 0 ? `${seconds} 秒后可继续` : `获取${label}验证码`}
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
          ? "输入当前手机号验证码"
          : "输入新手机号验证码"}
      </h2>
      <p className="mobile-change-lead">{DELIVERY[state.delivery]}</p>
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
        <li>完成后，所有设备上的本账户登录都会退出。</li>
        <li>
          请用新手机号重新登录。姓名、头像、微信绑定和已有业务记录会保留。
        </li>
      </ul>
      <Notice reason={state.notice} />
      <p className="field-hint">
        {deadline
          ? `点击确认后，系统将验证新号验证码并办理更换。请在 ${deadline} 前完成。`
          : "点击确认后，系统将验证新号验证码并办理更换。"}
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
  const pageDescription = maskedPage
    ? description
    : "先验证当前手机号，再验证新手机号。";
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
  const masked = state.kind === "current" || state.kind === "other";

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
      手机号登录
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
        <p className="eyebrow">个人账户 · 本人办理</p>
        <h1 ref={heading} tabIndex={-1}>
          {pageTitle}
        </h1>
        <p>{pageDescription}</p>
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
              <p className="mobile-change-status">本次办理</p>
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
              <p className="mobile-change-status">本次申请 · 等待重试</p>
              <h2>{headingTitle}</h2>
              <p className="mobile-change-lead">{description}</p>
              <p className="field-hint">
                重试使用本次已填写的信息，无需再次完成安全验证。
              </p>
              {actions(
                <>
                  <Button
                    className="primary"
                    disabled={retrySeconds > 0}
                    onClick={() => void controller.retry()}
                  >
                    {retrySeconds > 0
                      ? `${retrySeconds} 秒后可重试`
                      : "重试发送验证码"}
                  </Button>
                  {account()}
                  {home()}
                </>,
              )}
            </>
          )}
          {state.kind === "unknown" && (
            <>
              <p className="mobile-change-status">本次办理 · 待确认</p>
              <h2>{headingTitle}</h2>
              <p className="mobile-change-lead">{description}</p>
              {state.notice === "limited" ? (
                <p className="field-hint" role="status">
                  本次查询过于频繁，请等待后再试。此前办理的结果仍待确认。
                </p>
              ) : (
                <Notice reason={state.notice} />
              )}
              {!state.persisted && (
                <p className="field-hint">
                  本次办理信息未能保存在本页，离开后可能无法再次查询。
                </p>
              )}
              <p className="field-hint">
                查看结果只处理本次申请，不会另外发送短信或再次提交更换。
              </p>
              {actions(
                <>
                  {state.canRetry && (
                    <Button
                      className="primary"
                      disabled={retrySeconds > 0}
                      onClick={() => void controller.retry()}
                    >
                      {retrySeconds > 0
                        ? `${retrySeconds} 秒后可查看`
                        : state.step === "establish"
                          ? "查看验证结果"
                          : state.step === "complete"
                            ? "查看提交结果"
                            : "查看发送结果"}
                    </Button>
                  )}
                  {state.canQuery && (
                    <Button
                      className={state.canRetry ? "quiet-link" : "primary"}
                      disabled={retrySeconds > 0}
                      onClick={() => void controller.query()}
                    >
                      {retrySeconds > 0
                        ? `${retrySeconds} 秒后可查询`
                        : "查询更换结果"}
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
              <p className="mobile-change-status">已确认完成</p>
              <h2>{headingTitle}</h2>
              <p className="mobile-change-lead">{description}</p>
              <p className="mobile-change-time">
                完成时间：{stamp(state.known.completedAt) ?? "已记录"}
              </p>
              <p className="field-hint">
                账户、实名和业务记录保留。重新登录或查看记录不会再次更换手机号，也不会改变本次完成时间。
              </p>
              {!state.persisted && (
                <p className="field-hint">
                  本次结果未能保存在本页，离开后可能无法再次查看。
                </p>
              )}
              {state.queryFailed && (
                <p className="field-hint">
                  暂时无法再次读取办理记录，已确认的完成结果仍然有效。
                </p>
              )}
              {actions(
                <>
                  {login(true)}
                  {state.canRestart && (
                    <Button
                      className="secondary"
                      onClick={() => controller.restart()}
                    >
                      再次更换手机号
                    </Button>
                  )}
                  {account()}
                  {home()}
                </>,
              )}
            </>
          )}
          {state.kind === "query" && (
            <>
              <p className="mobile-change-status">
                {state.unknown ? "此前办理 · 待确认" : "此前办理"}
              </p>
              <h2>{headingTitle}</h2>
              <p className="mobile-change-lead">{description}</p>
              {state.unknown && (
                <p className="field-hint">
                  此前提交的结果仍未确认。查询只读取办理状态，不会重新提交。
                </p>
              )}
              {state.status === "pending" && state.expiresAt && (
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
                        ? `${retrySeconds} 秒后可查询`
                        : "查询更换结果"}
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
                        ? `${retrySeconds} 秒后可查看`
                        : "查看提交结果"}
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
              <p className="mobile-change-status">本次办理</p>
              <h2>{headingTitle}</h2>
              <p className="mobile-change-lead">{description}</p>
              {actions(
                <>
                  {state.why === "expired" && state.canRestart && (
                    <Button
                      className="primary"
                      onClick={() => controller.restart()}
                    >
                      重新验证当前手机号
                    </Button>
                  )}
                  {state.transfer && (
                    <Button
                      className={state.canRestart ? "quiet-link" : "primary"}
                      disabled={retrySeconds > 0}
                      onClick={() => void controller.query()}
                    >
                      {retrySeconds > 0
                        ? `${retrySeconds} 秒后可查询`
                        : "查询更换结果"}
                    </Button>
                  )}
                  {account(!state.transfer)}
                  {home()}
                </>,
              )}
            </>
          )}
          {state.kind === "other" && (
            <>
              <p className="mobile-change-status">个人账户</p>
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
              <p className="mobile-change-status">个人账户</p>
              <p className="mobile-change-lead">{description}</p>
              {actions(
                <>
                  {state.current.kind === "reload" ? (
                    <Button
                      className="primary"
                      onClick={() => location.reload()}
                    >
                      重新加载页面
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
              <p className="mobile-change-status">本次办理</p>
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
              <p className="mobile-change-status">本次办理</p>
              <h2>{headingTitle}</h2>
              <p className="mobile-change-lead">{description}</p>
              {actions(
                <>
                  <Button className="primary" onClick={() => location.reload()}>
                    重新加载页面
                  </Button>
                  {home()}
                </>,
              )}
            </>
          )}
          {state.kind === "random" && (
            <>
              <p className="mobile-change-status">本次办理</p>
              <h2>{headingTitle}</h2>
              <p className="mobile-change-lead">{description}</p>
              {actions(
                <>
                  <Button className="primary" onClick={() => location.reload()}>
                    重新加载页面
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
        {!masked && (
          <aside className="mobile-change-guide">
            {state.kind === "known" ? (
              <>
                <h2>本次办理已完成</h2>
                <p>今后请使用新手机号登录原账户。</p>
                <p>重新登录或查看记录不会再次更换手机号。</p>
              </>
            ) : state.kind === "query" ? (
              <>
                <h2>本次登录只查询记录</h2>
                <p>此前登录建立的办理，不能在本次登录中继续。</p>
                <p>查询不会发送短信或再次更换手机号。</p>
              </>
            ) : (
              <>
                <h2>更换后，用新号登录</h2>
                <p>完成更换后，所有设备上的本账户登录都会退出。</p>
                <p>姓名、头像、微信绑定和已有业务记录会保留。</p>
                <hr />
                <h2>在本次登录中完成</h2>
                <p>
                  当前号验证通过后，请在 10
                  分钟内完成。中途重新登录时，只能查询原办理。
                </p>
              </>
            )}
          </aside>
        )}
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
              ? "离开前，请留意办理结果"
              : transfer
                ? "暂时离开换号办理？"
                : "放弃当前填写？"}
          </h2>
          <p>
            {unknown
              ? "本次结果尚未确认。离开不会取消已发送的申请，返回后请先查看原结果。"
              : transfer
                ? "离开不会撤销已经建立的办理，有效期仍继续计算。请在同一次登录中及时完成。"
                : "当前填写尚未提交，离开后需要重新填写。"}
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
              离开此页
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
            <p>正在打开换号页面…</p>
          </div>
        )}
      </main>
      <Footer />
    </>
  );
}
