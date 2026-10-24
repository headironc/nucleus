import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { Button } from "@base-ui/react/button";
import { useLocation } from "react-router";
import { ArrowRight, Check, Clock3, Info, ShieldCheck } from "lucide-react";
import { Header, Footer } from "../root";
import { Result } from "../components/result";
import { Recovery, type State } from "../codes/recovery";
import { STORAGE_KEY as C1_KEY } from "../codes/material";
import {
  availableCode,
  blocksSend,
  receiveCode,
} from "../authentication/material";
import { blocksSend as blocksRegistration } from "../registration/material";
import { send, settings, validInput } from "../protocol/codes";
import { currentBuild } from "../protocol/build";
import { Turnstile } from "../codes/turnstile";
import { reloadRequired, requestReload } from "../integrity";
import botanical from "../../../design/assets/halo-botanical.svg";
import { loginPath } from "../logout/return";

export function meta() {
  return [
    { title: "申请短信验证码 · 司籍处" },
    {
      name: "description",
      content: "申请短信验证码，查看本次发送的处理结果。",
    },
    { name: "robots", content: "noindex" },
  ];
}

function useRecovery() {
  const [recovery, setRecovery] = useState<Recovery>();
  useEffect(() => {
    try {
      if (blocksRegistration(sessionStorage)) {
        location.replace("/authentication/register");
        return;
      }
      if (blocksSend(sessionStorage)) {
        location.replace(loginPath("/authentication/login", location.search));
        return;
      }
      if (
        sessionStorage.getItem(C1_KEY) === null &&
        availableCode(sessionStorage, Date.now())
      )
        history.replaceState({ ...history.state, haloC1: true }, "");
    } catch {
      // The login page owns damaged login material; C1 cannot infer its outcome.
      location.replace(loginPath("/authentication/login", location.search));
      return;
    }
    const controller = new Recovery({
      storage: {
        getItem: (key) => sessionStorage.getItem(key),
        setItem: (key, value) => sessionStorage.setItem(key, value),
        removeItem: (key) => sessionStorage.removeItem(key),
      },
      marked: () => Boolean(history.state?.haloC1),
      blocked: () => history.state?.haloC1 === "blocked",
      mark: (blocked = false) =>
        history.replaceState(
          { ...history.state, haloC1: blocked ? "blocked" : true },
          "",
        ),
      now: Date.now,
      uuid: () => crypto.randomUUID(),
      current: currentBuild,
      received: (operation, mobile, message) => {
        receiveCode(sessionStorage, operation, mobile, message, Date.now());
        history.replaceState({ ...history.state, haloR1: false }, "");
      },
      send,
    });
    const reload = () => controller.reload();
    window.addEventListener("halo:reload-required", reload);
    controller.initialize();
    if (reloadRequired()) controller.reload();
    setRecovery(controller);
    return () => {
      window.removeEventListener("halo:reload-required", reload);
      controller.suspend();
    };
  }, []);
  return recovery;
}

const labels: Record<string, [string, string]> = {
  verification_invalid: [
    "安全验证未通过",
    "本次安全验证未通过。请重新完成安全验证。",
  ],
  verification_required: [
    "需要重新进行安全验证",
    "本次验证已无法继续使用。请先查看手机短信，再决定是否重新申请。",
  ],
  authentication_invalid: [
    "本次发送结果暂时无法确认",
    "本页保存的验证信息暂时无法用于查看结果。请先查看手机短信。",
  ],
  request_mismatch: [
    "发送信息与此前不一致",
    "本次发送信息与此前的申请不一致，暂时无法查看结果。请先查看手机短信。",
  ],
  permission_denied: [
    "暂时无法办理",
    "本次申请未被接纳。请稍后重新填写手机号并完成安全验证。",
  ],
  invalid_input: [
    "请检查填写内容",
    "本次申请未被接纳。请重新填写手机号并完成安全验证。",
  ],
};

function ResultView({
  state,
  recovery,
  seconds,
  now,
}: {
  state: State;
  recovery: Recovery;
  seconds: number;
  now: number;
}) {
  const { search } = useLocation();
  const [hasCode, setHasCode] = useState(false);
  useEffect(() => {
    try {
      setHasCode(!!availableCode(sessionStorage, Date.now()));
    } catch {
      setHasCode(false);
    }
  }, [state, now]);
  const unsubmitted = recovery.unsubmitted;
  let title = "发送结果待确认";
  let copy =
    "暂时无法确定验证码是否已发送。请先查看手机短信，也可以查看本次发送的处理结果。";
  let note = "查看结果只核对本次申请，不会另外发送一条短信。";
  let successful = false;
  if (state.kind === "submitting") {
    title = "正在申请发送验证码";
    copy = "正在处理本次申请，请稍候。";
    note = "请勿重复申请，请等待本次处理结果。";
  } else if (state.kind === "pending" && unsubmitted) {
    title = "本次申请尚未发出";
    copy = "申请信息已保存，尚未申请发送验证码。你可以继续提交本次申请。";
    note = "只有点击继续申请，才会发起本次短信请求。";
  } else if (state.kind === "sending") {
    title = "短信仍在发送";
    copy = "短信服务商仍在处理本次发送，请先留意手机短信。";
  } else if (state.kind === "message") {
    const delivery = state.message.delivery;
    successful = delivery === "sent";
    title =
      delivery === "sent"
        ? "短信已提交发送"
        : delivery === "failed"
          ? "短信发送未成功"
          : "短信发送情况未确认";
    copy =
      delivery === "sent"
        ? "短信服务商已受理本次发送，请留意手机短信。"
        : delivery === "failed"
          ? "本次发送已确认未成功。"
          : "目前还不能确认短信是否发送成功，请先查看手机短信。";
    note =
      delivery === "sent"
        ? "受理不代表短信已经送达。验证码请妥善保管，勿向他人透露。"
        : "本页不会自动重新发送短信。";
  } else if (state.kind === "limited") {
    title = "暂时无法处理";
    copy = "服务暂时繁忙，请稍后重试本次申请。";
    note = "重试只处理本次申请，不会另外发送一条短信。";
  } else if (state.kind === "rejected") {
    [title, copy] = labels[state.reason] ?? [
      "暂时无法确认发送情况",
      "请先查看手机短信。",
    ];
    if (
      state.uncertain &&
      ["invalid_input", "permission_denied"].includes(state.reason)
    ) {
      title = "本次发送结果暂时无法确认";
      copy = "本次查询未被接纳，仍不能确认先前的发送结果。请先查看手机短信。";
    }
    note = "本页不会自动重新发送短信。";
  } else if (state.kind === "expired" || state.kind === "missing") {
    title = "暂时无法查看本次发送结果";
    copy = "这不表示短信没有发送。请先查看手机短信，避免反复申请。";
    note =
      state.kind === "expired"
        ? "本页保存的申请信息已超过可查询期限。"
        : "本页保存的申请信息不完整。";
  } else if (state.kind === "reload") {
    title = "请重新加载页面";
    copy = unsubmitted
      ? "本次申请尚未发出。请重新加载页面后继续申请。"
      : "暂时无法查看本次发送结果。若已申请发送，请先查看手机短信。";
    note = "请保留当前标签页。重新加载不会自动发送短信。";
  } else if (state.kind === "storage") {
    title = "无法保存本次申请信息";
    copy =
      "浏览器暂时无法保存申请信息，本页尚未申请发送验证码。请检查浏览器是否限制了本站保存信息，再重新加载。";
    note = "申请前需先保存信息，便于断网或刷新后查看本次发送结果。";
  } else if (state.kind === "random") {
    title = "暂时无法安全地办理";
    copy = "浏览器暂时无法准备本次申请。请重新加载页面后再试。";
    note = "本页尚未申请发送验证码。";
  }
  const continueAllowed =
    state.kind === "pending" ||
    state.kind === "sending" ||
    state.kind === "limited";
  const action = unsubmitted
    ? "继续申请"
    : state.kind === "limited"
      ? "重试本次申请"
      : "查看发送结果";
  const newAllowed =
    state.kind === "rejected" &&
    !state.uncertain &&
    [
      "verification_invalid",
      "verification_required",
      "invalid_input",
      "permission_denied",
    ].includes(state.reason);
  return (
    <Result
      title={title}
      copy={copy}
      note={note}
      icon={
        <div
          className={`result-icon ${successful ? "success" : ""}`}
          aria-hidden="true"
        >
          {successful ? (
            <Check size={30} />
          ) : state.kind === "submitting" ? (
            <Clock3 size={30} />
          ) : (
            <Info size={30} />
          )}
        </div>
      }
      footer={
        state.kind === "message"
          ? "验证手机号后，继续登录或填写注册资料。"
          : undefined
      }
    >
      {hasCode && (
        <a
          className="primary"
          href={loginPath("/authentication/login", search)}
          onClick={(event) => {
            event.preventDefault();
            location.replace(
              loginPath("/authentication/login", location.search),
            );
          }}
        >
          输入短信验证码
        </a>
      )}
      {continueAllowed && (
        <Button
          className="primary"
          disabled={seconds > 0}
          onClick={() => void recovery.confirm()}
        >
          {seconds > 0 ? `${seconds} 秒后${action}` : action}
          <ArrowRight size={17} aria-hidden="true" />
        </Button>
      )}
      {newAllowed && (
        <Button className="primary" onClick={() => recovery.begin()}>
          {state.kind === "rejected" &&
          ["invalid_input", "permission_denied"].includes(state.reason)
            ? "重新填写并验证"
            : "重新完成安全验证"}
        </Button>
      )}
      {state.kind === "message" && state.message.delivery !== "unknown" && (
        <Button className="text-button" onClick={() => recovery.begin()}>
          重新获取验证码
        </Button>
      )}
      {["reload", "storage", "random"].includes(state.kind) && (
        <Button className="primary" onClick={() => location.reload()}>
          重新加载页面
        </Button>
      )}
      {state.kind !== "submitting" && (
        <a className="quiet-link" href="/">
          返回首页
        </a>
      )}
    </Result>
  );
}

function Codes({ recovery }: { recovery: Recovery }) {
  const state = useSyncExternalStore(
    recovery.subscribe,
    recovery.snapshot,
    recovery.snapshot,
  );
  const [mobile, setMobile] = useState("");
  const [token, setToken] = useState<string>();
  const [site, setSite] = useState<{ operation: string; key: string }>();
  const [settingsFailed, setSettingsFailed] = useState(false);
  const [fieldError, setFieldError] = useState(false);
  const [reset, setReset] = useState(0);
  const [now, setNow] = useState(Date.now);
  const input = useRef<HTMLInputElement>(null);
  const previous = useRef(state.kind);
  const widgetOperation = useRef<string>(undefined);
  if (state.kind === "editing") widgetOperation.current = state.operation;
  const operation = state.kind === "editing" ? state.operation : undefined;
  const siteKey =
    site && site.operation === widgetOperation.current ? site.key : undefined;
  const seconds =
    "retryAt" in state
      ? Math.max(0, Math.ceil((state.retryAt - now) / 1000))
      : 0;

  useEffect(() => {
    if (!operation || siteKey) return;
    setSettingsFailed(false);
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), 10_000);
    let active = true;
    settings(controller.signal)
      .then((key) => {
        if (active) setSite({ operation, key });
      })
      .catch(() => {
        if (active) setSettingsFailed(true);
      });
    return () => {
      active = false;
      clearTimeout(timer);
      controller.abort();
    };
  }, [operation, siteKey]);
  useEffect(() => {
    if (previous.current === "submitting" && state.kind !== "submitting") {
      setToken(undefined);
      setReset((value) => value + 1);
    }
    if (previous.current !== state.kind) {
      if (state.kind === "editing") input.current?.focus();
      else document.querySelector<HTMLElement>("[data-result-title]")?.focus();
    }
    previous.current = state.kind;
  }, [state.kind]);
  useEffect(() => {
    let active = true;
    const timer = setInterval(() => {
      setNow(Date.now());
      recovery.expire();
    }, 1000);
    const hide = () => {
      if (document.hidden) recovery.suspend();
      else {
        recovery.expire();
        currentBuild()
          .then((current) => {
            if (active && !current) requestReload();
          })
          .catch(() => {
            if (active) requestReload();
          });
      }
    };
    const resume = () => recovery.expire();
    const leave = () => recovery.suspend();
    document.addEventListener("visibilitychange", hide);
    window.addEventListener("pagehide", leave);
    window.addEventListener("pageshow", resume);
    window.addEventListener("focus", resume);
    return () => {
      active = false;
      clearInterval(timer);
      document.removeEventListener("visibilitychange", hide);
      window.removeEventListener("pagehide", leave);
      window.removeEventListener("pageshow", resume);
      window.removeEventListener("focus", resume);
    };
  }, [recovery]);

  const editing = state.kind === "editing";
  return (
    <div className={`authentication-layout ${editing ? "" : "compact"}`}>
      {editing && (
        <aside className="service-introduction">
          <p className="eyebrow">日常事务 · 从这里开始</p>
          <h2>
            一份验证码，
            <br />
            安心开始。
          </h2>
          <p>
            填写手机号，完成安全验证。
            <br />
            提交申请后，可在这里查看发送结果。
          </p>
          <img src={botanical} width="400" height="330" alt="" />
          <p className="introduction-footnote">
            断网或刷新后，请在当前标签页查看本次发送结果。
          </p>
        </aside>
      )}
      <section className="authentication-card" aria-label="短信发码">
        <form
          hidden={!editing}
          noValidate
          onSubmit={(event) => {
            event.preventDefault();
            if (!validInput({ mobile, token: "placeholder" })) {
              setFieldError(true);
              input.current?.focus();
              return;
            }
            if (!token) return;
            void recovery.submit({ mobile, token });
          }}
        >
          <p className="eyebrow">手机验证</p>
          <h1>申请短信验证码</h1>
          <p className="card-intro">验证码将发送到你填写的手机号。</p>
          <label htmlFor="mobile">手机号</label>
          <input
            ref={input}
            id="mobile"
            name="mobile"
            type="tel"
            inputMode="numeric"
            autoComplete="off"
            value={mobile}
            aria-invalid={fieldError}
            aria-describedby={fieldError ? "mobile-error" : "mobile-hint"}
            placeholder="请输入 11 位中国大陆手机号"
            onChange={(event) => {
              setMobile(event.target.value);
              setFieldError(false);
            }}
          />
          <p
            id={fieldError ? "mobile-error" : "mobile-hint"}
            className={fieldError ? "field-error" : "field-hint"}
            role={fieldError ? "alert" : undefined}
          >
            {fieldError
              ? "请填写有效的 11 位中国大陆手机号，不含空格或区号。"
              : "仅用于本次验证，请仔细核对。"}
          </p>
          {!siteKey && (
            <div className="notice" role="status">
              <Info size={19} aria-hidden="true" />
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
          {siteKey && widgetOperation.current && (
            <Turnstile
              siteKey={siteKey}
              operation={widgetOperation.current}
              action="send_authentication_code"
              waiting={!editing}
              reset={reset}
              onToken={setToken}
            />
          )}
          <Button
            className="primary send-button"
            type="submit"
            disabled={!token || !siteKey}
          >
            获取短信验证码
            <ArrowRight size={18} aria-hidden="true" />
          </Button>
          <p className="scope-note">
            本页仅办理短信发送。验证码不代表登录成功。
          </p>
        </form>
        {!editing && (
          <ResultView
            state={state}
            recovery={recovery}
            seconds={seconds}
            now={now}
          />
        )}
      </section>
    </div>
  );
}

export default function CodesPage() {
  const recovery = useRecovery();
  return (
    <>
      <Header />
      <main id="main">
        {recovery ? (
          <Codes recovery={recovery} />
        ) : (
          <div className="loading-shell" role="status">
            <p>
              <ShieldCheck aria-hidden="true" />
              正在准备办理页面…
            </p>
            <p className="scope-note">
              若页面长时间未显示，请
              <a href="/authentication/codes">重新加载办理页</a>。
            </p>
          </div>
        )}
      </main>
      <Footer />
    </>
  );
}
