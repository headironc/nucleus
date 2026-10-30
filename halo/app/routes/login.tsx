import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { useLocation, useNavigate } from "react-router";
import { Button } from "@base-ui/react/button";
import { Clock3, Info, ShieldCheck } from "lucide-react";
import { Header, Footer } from "../root";
import { Login, secret, type LoginState } from "../authentication/recovery";
import { Sessions } from "../authentication/session";
import { CodeInput } from "../authentication/code";
import { currentBuild } from "../protocol/build";
import { login } from "../protocol/session";
import { reloadRequired, requestReload } from "../integrity";
import {
  loginPath,
  logoutReturn,
  allLogoutReturn,
  terminationReturn,
  mobileChangeReturn,
} from "../logout/return";

export function meta() {
  return [
    { title: "验证手机号 · 司籍处" },
    { name: "robots", content: "noindex" },
  ];
}

function useLogin() {
  const [controller, setController] = useState<Login>();
  useEffect(() => {
    const sessions = new Sessions();
    const controller = new Login({
      storage: {
        get length() {
          return sessionStorage.length;
        },
        key: (index) => sessionStorage.key(index),
        getItem: (key) => sessionStorage.getItem(key),
        setItem: (key, value) => sessionStorage.setItem(key, value),
        removeItem: (key) => sessionStorage.removeItem(key),
      },
      sessions,
      now: Date.now,
      uuid: () => crypto.randomUUID(),
      secret,
      current: currentBuild,
      login,
      marked: () => Boolean(history.state?.haloR1),
      mark: () => history.replaceState({ ...history.state, haloR1: true }, ""),
    });
    let active = true;
    setController(controller);
    const reload = () => controller.reload();
    window.addEventListener("halo:reload-required", reload);
    void controller.initialize().then(() => {
      if (reloadRequired()) reload();
    });
    const leave = () => controller.suspend();
    const check = () => {
      if (document.hidden) leave();
      else {
        void controller.selectionChanged();
        currentBuild()
          .then((current) => {
            if (active && !current) requestReload();
          })
          .catch(() => {
            if (active) requestReload();
          });
      }
    };
    const unwatch = sessions.watch(check);
    window.addEventListener("pagehide", leave);
    window.addEventListener("pageshow", check);
    window.addEventListener("focus", check);
    document.addEventListener("visibilitychange", check);
    return () => {
      active = false;
      leave();
      unwatch();
      window.removeEventListener("pagehide", leave);
      window.removeEventListener("pageshow", check);
      window.removeEventListener("focus", check);
      window.removeEventListener("halo:reload-required", reload);
      document.removeEventListener("visibilitychange", check);
      void sessions.close().catch(() => {});
    };
  }, []);
  return controller;
}

function description(state: LoginState): [string, string, string?] {
  switch (state.kind) {
    case "preparing":
      return ["正在打开登录页面", "请稍候。"];
    case "submitting":
      return [
        "正在登录",
        "请稍候，正在验证你填写的验证码。",
        "请勿重复提交或重新获取验证码。",
      ];
    case "pending":
      if (!state.sent)
        return [
          "登录尚未提交",
          "请点击“继续登录”，无需重新填写验证码。",
          "请勿重新获取验证码。",
        ];
      return [
        "登录结果待确认",
        "请点击“查看登录结果”，核对本次登录的处理结果。",
        "请勿重新获取验证码。",
      ];
    case "limited":
      if (!state.sent)
        return [
          "请稍后继续验证",
          "本次登录尚未受理，请稍后继续验证。",
          "无需重新填写或获取验证码。",
        ];
      return [
        "请稍后查看登录结果",
        "暂时无法处理，请稍后查看本次登录结果。",
        "无需重新填写或获取验证码。",
      ];
    case "reload":
      return [
        "请重新加载页面",
        "页面暂时无法继续登录，请重新加载后继续。",
        "请勿重复获取验证码。",
      ];
    case "missing":
    case "expired":
      return [
        "登录状态待确认",
        "请先检查当前登录状态，暂时不用再次获取验证码。",
      ];
    case "changed":
      return ["登录状态有变化", "请检查当前登录状态，再按页面提示继续。"];
    case "storage":
      return [
        "暂时无法继续登录",
        "浏览器无法读取或保存登录信息，请重新加载页面。",
        "请勿重新获取验证码。若仍无法继续，请检查浏览器是否允许此网站保存数据。",
      ];
    case "session-storage":
      return [
        "暂时无法打开个人账户",
        "请点击“重试”继续。",
        "请保留本页，暂时不要刷新或关闭。",
      ];
    case "random":
      return ["暂时无法登录", "请重新加载页面后再试。", "本页尚未提交登录。"];
    case "delivery-failed":
      return [
        "短信发送未成功",
        "本次验证码未能发送，请返回验证码页面重新获取。",
        "本页不会自动重新发送短信。",
      ];
    case "registration":
      return ["手机号已验证", "正在打开注册页面。"];
    case "session":
      return ["正在打开个人账户", "请稍候。"];
    case "rejected":
      if (state.reason === "verification_required")
        return [
          "请重新获取验证码",
          "这个验证码已无法使用。",
          "请重新完成安全验证后再继续。",
        ];
      if (state.reason === "request_mismatch")
        return [
          "登录状态待确认",
          "请先检查当前登录状态，暂时不用再次获取验证码。",
        ];
      if (state.reason === "authentication_invalid")
        return [
          "登录状态待确认",
          "请先检查当前登录状态，暂时不用再次获取验证码。",
        ];
      if (state.uncertain)
        return [
          "登录状态待确认",
          "请先检查当前登录状态，暂时不用再次获取验证码。",
        ];
      return ["暂时无法登录", "本次登录未能完成。", "请联系医院管理员。"];
    default:
      return ["验证手机号", "请输入短信验证码。", ""];
  }
}

function LoginForm({ controller }: { controller: Login }) {
  const { search } = useLocation();
  const state = useSyncExternalStore(
    controller.subscribe,
    controller.snapshot,
    controller.snapshot,
  );
  const [code, setCode] = useState("");
  const [incomplete, setIncomplete] = useState(false);
  const [now, setNow] = useState(Date.now);
  const title = useRef<HTMLHeadingElement>(null);
  const previous = useRef<string | undefined>(undefined);
  const composing = useRef(false);
  const navigate = useNavigate();
  useEffect(() => {
    const timer = setInterval(() => {
      setNow(Date.now());
      controller.expire();
    }, 1000);
    return () => clearInterval(timer);
  }, [controller]);
  useEffect(() => {
    if (state.kind === "registration") {
      void navigate("/authentication/register", { replace: true });
      return;
    }
    if (state.kind === "session") {
      if (terminationReturn(search)) {
        void navigate(`/workforce/termination?return=service-termination`, {
          replace: true,
        });
        return;
      }
      if (allLogoutReturn(search)) {
        void navigate(`/sessions/logout-all?return=all-logout-result`, {
          replace: true,
        });
        return;
      }
      if (mobileChangeReturn(search)) {
        // The change page renders a Turnstile widget, so it needs the policy
        // of its own document rather than this login document's CSP.
        location.replace("/account/mobile?return=mobile-change");
        return;
      }
      void navigate(
        logoutReturn(search)
          ? "/sessions/logout?return=logout-result"
          : "/account",
        { replace: true },
      );
      return;
    }
    if (previous.current !== state.kind || state.kind === "session-storage") {
      if (state.kind === "editing") {
        setCode("");
        setIncomplete(false);
        document.getElementById("login-code")?.focus();
      } else title.current?.focus();
    }
    previous.current = state.kind;
  }, [state, navigate, search]);
  const seconds =
    "retryAt" in state
      ? Math.max(0, Math.ceil((state.retryAt - now) / 1000))
      : 0;
  const editable = state.kind === "editing";
  const [heading, copy, note] = description(state);

  return (
    <div className="authentication-layout compact">
      <section
        className="authentication-card"
        aria-busy={state.kind === "submitting" || state.kind === "preparing"}
      >
        {editable ? (
          <>
            <p className="eyebrow">手机号登录</p>
            <h1 ref={title} tabIndex={-1}>
              请输入验证码
            </h1>
            <p className="card-intro">
              验证手机号 {state.code.mobile.slice(0, 3)} ****{" "}
              {state.code.mobile.slice(-4)}
            </p>
            <form
              autoComplete="off"
              onCompositionStart={() => {
                composing.current = true;
              }}
              onCompositionEnd={() => {
                composing.current = false;
              }}
              onSubmit={(event) => {
                event.preventDefault();
                if (composing.current) return;
                if (/^[0-9]{6}$/.test(code)) void controller.submit(code);
                else {
                  setIncomplete(true);
                  Array.from(
                    event.currentTarget.querySelectorAll<HTMLInputElement>(
                      ".code-slots input",
                    ),
                  )
                    .find((input) => !input.value)
                    ?.focus();
                }
              }}
            >
              <CodeInput
                value={code}
                onChange={(next) => {
                  setCode(next);
                  setIncomplete(false);
                }}
                disabled={false}
                invalid={state.invalid}
                incomplete={incomplete}
              />
              <Button type="submit" className="primary send-button">
                验证并继续
              </Button>
            </form>
            <p className="scope-note">
              请使用本次收到的短信验证码，验证成功后继续。
            </p>
          </>
        ) : (
          <div className="result">
            <div className="result-icon" aria-hidden="true">
              {state.kind === "submitting" ? <Clock3 /> : <Info />}
            </div>
            <h1 ref={title} tabIndex={-1}>
              {heading}
            </h1>
            <p className="result-copy">{copy}</p>
            {note && (
              <div className="notice">
                <ShieldCheck size={20} aria-hidden="true" />
                <p>{note}</p>
              </div>
            )}
            <div className="result-actions">
              {(state.kind === "pending" || state.kind === "limited") && (
                <Button
                  className="primary"
                  disabled={seconds > 0}
                  onClick={() => void controller.confirm()}
                >
                  {seconds > 0
                    ? `${seconds} 秒后可继续`
                    : !state.sent
                      ? state.kind === "limited"
                        ? "继续验证"
                        : "继续登录"
                      : "查看登录结果"}
                </Button>
              )}
              {["storage", "reload", "random"].includes(state.kind) && (
                <Button className="primary" onClick={() => location.reload()}>
                  重新加载页面
                </Button>
              )}
              {state.kind === "session-storage" && (
                <Button
                  className="primary"
                  onClick={() => void controller.initialize()}
                >
                  重试
                </Button>
              )}
              {["changed", "missing", "expired"].includes(state.kind) && (
                <a className="primary" href="/account">
                  检查登录状态
                </a>
              )}
              {state.kind === "rejected" && state.uncertain && (
                <a className="primary" href="/account">
                  检查登录状态
                </a>
              )}
              {(state.kind === "delivery-failed" ||
                (state.kind === "rejected" &&
                  !state.uncertain &&
                  state.reason === "verification_required")) && (
                <a
                  className="primary"
                  href={loginPath("/authentication/codes", search)}
                >
                  {state.kind === "delivery-failed"
                    ? "返回验证码页面"
                    : "重新获取验证码"}
                </a>
              )}
              {state.kind !== "submitting" && state.kind !== "preparing" && (
                <a className="quiet-link" href="/">
                  返回首页
                </a>
              )}
            </div>
          </div>
        )}
      </section>
    </div>
  );
}

export default function LoginPage() {
  const controller = useLogin();
  return (
    <>
      <Header />
      <main id="main">
        {controller ? (
          <LoginForm controller={controller} />
        ) : (
          <div className="loading-shell">
            <p>正在打开登录页面…</p>
          </div>
        )}
      </main>
      <Footer />
    </>
  );
}
