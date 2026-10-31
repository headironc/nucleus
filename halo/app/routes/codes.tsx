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
    { title: "获取短信验证码 · 司籍处" },
    {
      name: "description",
      content: "获取短信验证码，验证手机号后继续登录或创建账户。",
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
  verification_invalid: ["安全验证未通过", "请重新完成安全验证。"],
  verification_required: [
    "请重新获取验证码",
    "请先查看手机短信，再决定是否重新获取。",
  ],
  authentication_invalid: ["暂时无法查看发送结果", "请先查看手机短信。"],
  request_mismatch: ["暂时无法查看发送结果", "请先查看手机短信。"],
  permission_denied: ["暂时无法获取验证码", "请重新填写手机号并完成安全验证。"],
  invalid_input: ["请检查手机号", "请重新填写手机号并完成安全验证。"],
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
  let title = "暂时无法确认发送结果";
  let copy = "请先查看手机短信，也可稍后查看发送结果。";
  let note = "";
  let successful = false;
  if (state.kind === "submitting") {
    title = "正在获取验证码…";
    copy = "请稍候。";
  } else if (state.kind === "pending" && unsubmitted) {
    title = "尚未发送验证码";
    copy = "点击继续获取验证码。";
  } else if (state.kind === "sending") {
    title = "验证码正在发送";
    copy = "请留意手机短信。";
  } else if (state.kind === "message") {
    const delivery = state.message.delivery;
    successful = delivery === "sent";
    title =
      delivery === "sent"
        ? "验证码已提交发送"
        : delivery === "failed"
          ? "验证码未能发送"
          : "暂时无法确认发送结果";
    copy =
      delivery === "sent"
        ? "请查看手机短信。"
        : delivery === "failed"
          ? "可以重新获取验证码。"
          : "请先查看手机短信。";
    note =
      delivery === "sent" ? "提交发送不代表已送达。请勿向他人透露验证码。" : "";
  } else if (state.kind === "limited") {
    title = "请稍后再试";
    copy = "暂时无法查看发送结果，请稍后再试。";
  } else if (state.kind === "rejected") {
    [title, copy] = labels[state.reason] ?? [
      "暂时无法查看发送结果",
      "请先查看手机短信。",
    ];
    if (
      state.uncertain &&
      ["invalid_input", "permission_denied"].includes(state.reason)
    ) {
      title = "暂时无法查看发送结果";
      copy = "请先查看手机短信。";
    }
  } else if (state.kind === "expired" || state.kind === "missing") {
    title = "暂时无法查看发送结果";
    copy = "请先查看手机短信。";
  } else if (state.kind === "reload") {
    title = "请重新加载";
    copy = unsubmitted
      ? "本次尚未发送验证码，请重新加载后再试。"
      : "请先查看手机短信，或重新加载后再查看发送结果。";
  } else if (state.kind === "storage") {
    title = "暂时无法获取验证码";
    copy = "本次尚未发送验证码，请重新加载后再试。";
  } else if (state.kind === "random") {
    title = "暂时无法获取验证码";
    copy = "本次尚未发送验证码，请重新加载后再试。";
  }
  const continueAllowed =
    state.kind === "pending" ||
    state.kind === "sending" ||
    state.kind === "limited";
  const action = unsubmitted ? "继续获取验证码" : "查看发送结果";
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
            : "重新验证"}
        </Button>
      )}
      {state.kind === "message" && state.message.delivery !== "unknown" && (
        <Button className="text-button" onClick={() => recovery.begin()}>
          重新获取验证码
        </Button>
      )}
      {["reload", "storage", "random"].includes(state.kind) && (
        <Button className="primary" onClick={() => location.reload()}>
          重新加载
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
          <h2>登录个人账户</h2>
          <p>验证手机号后继续登录；首次使用需创建账户。</p>
          <img src={botanical} width="400" height="330" alt="" />
        </aside>
      )}
      <section className="authentication-card" aria-label="获取短信验证码">
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
          <h1>获取短信验证码</h1>
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
            placeholder="请输入手机号"
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
              ? "请填写 11 位中国大陆手机号，不含空格或区号。"
              : "请输入 11 位中国大陆手机号。"}
          </p>
          {!siteKey && (
            <div className="notice" role="status">
              <Info size={19} aria-hidden="true" />
              <p>
                {settingsFailed
                  ? "安全验证暂时无法加载，请重新加载后再试。"
                  : "正在加载安全验证…"}
              </p>
            </div>
          )}
          {settingsFailed && (
            <Button className="text-button" onClick={() => location.reload()}>
              重新加载
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
            获取验证码
            <ArrowRight size={18} aria-hidden="true" />
          </Button>
          <p className="scope-note">请勿向他人透露验证码。</p>
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
              正在打开验证码页面…
            </p>
            <p className="scope-note">
              页面暂未打开，可
              <a href="/authentication/codes">重新加载</a>。
            </p>
          </div>
        )}
      </main>
      <Footer />
    </>
  );
}
