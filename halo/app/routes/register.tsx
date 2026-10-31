import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { flushSync } from "react-dom";
import { useNavigate } from "react-router";
import { Button } from "@base-ui/react/button";
import { Check, Clock3, Info } from "lucide-react";
import { Header, Footer } from "../root";
import { Registration, type State } from "../registration/recovery";
import { Sessions } from "../authentication/session";
import { currentBuild } from "../protocol/build";
import { register } from "../protocol/registration";
import { reloadRequired, requestReload } from "../integrity";
import botanical from "../../../design/assets/halo-botanical.svg";

export function meta() {
  return [
    { title: "创建个人账户 · 司籍处" },
    { name: "robots", content: "noindex" },
  ];
}
function useRegistration() {
  const [controller, setController] = useState<Registration>();
  useEffect(() => {
    const sessions = new Sessions();
    const controller = new Registration({
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
      current: currentBuild,
      register,
    });
    let active = true;
    setController(controller);
    const reload = () => flushSync(() => controller.reload());
    const hide = () => flushSync(() => controller.suspend());
    const check = () => {
      hide();
      if (!document.hidden) {
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
    window.addEventListener("pagehide", hide);
    window.addEventListener("pageshow", check);
    window.addEventListener("focus", check);
    window.addEventListener("halo:reload-required", reload);
    document.addEventListener("visibilitychange", check);
    void controller.initialize().then(() => {
      if (reloadRequired()) reload();
    });
    return () => {
      active = false;
      controller.suspend();
      unwatch();
      window.removeEventListener("pagehide", hide);
      window.removeEventListener("pageshow", check);
      window.removeEventListener("focus", check);
      window.removeEventListener("halo:reload-required", reload);
      document.removeEventListener("visibilitychange", check);
      void sessions.close().catch(() => {});
    };
  }, []);
  return controller;
}
function description(state: State): [string, string] {
  switch (state.kind) {
    case "editing":
      return [
        "创建个人账户",
        "手机号已验证。请填写本人居民身份证上的姓名和号码。",
      ];
    case "preparing":
      return ["正在打开注册页面…", "请稍候。"];
    case "submitting":
      return ["正在创建账户…", "请稍候。"];
    case "pending":
      return state.sent
        ? ["暂时无法确认注册结果", "请先查看结果，暂时不要再次提交。"]
        : ["尚未创建账户", "请继续创建账户，无需重新填写资料。"];
    case "limited":
      return state.sent
        ? ["暂时无法查看注册结果", "请稍后再试，暂时不要再次提交。"]
        : ["暂时无法创建账户", "请稍后返回填写资料，再提交。"];
    case "storage-before":
      return ["暂时无法开始注册", "本次尚未提交注册，请重新检查。"];
    case "storage-read":
      return ["注册信息暂时无法使用", "请重新检查，暂时不要再次提交注册。"];
    case "storage-write":
      return ["注册信息暂时无法使用", "请重新检查，暂时不要再次提交注册。"];
    case "created":
      return [
        "账户已创建",
        state.selection ? "正在打开个人账户…" : "请检查登录状态。",
      ];
    case "created-storage":
      return ["账户已创建", "暂时无法保留登录信息，请重新检查。"];
    case "created-cleanup":
      return ["账户已创建", "暂时无法打开个人账户，请重新检查。"];
    case "reload":
      return ["请重新加载", "页面暂时无法继续，请重新加载后查看注册结果。"];
    case "random":
      return ["暂时无法开始注册", "本次尚未提交注册，请重新加载后再试。"];
    case "credential-expired":
      return ["填写已超时", "请重新验证手机号后创建账户。"];
    case "changed":
      return ["登录状态有变化", "请检查当前登录状态，再按页面提示继续。"];
    case "rejected":
      if (state.uncertain)
        return [
          "暂时无法确认注册结果",
          "请先检查当前登录状态，暂时不要再次提交注册。",
        ];
      if (state.reason === "verification_required")
        return ["请重新验证手机号", "请重新验证手机号后创建账户。"];
      if (state.reason === "mobile_taken")
        return [
          "请重新验证手机号",
          "本次注册未完成。请重新验证手机号，再按页面提示继续。",
        ];
      if (state.reason === "id_number_taken")
        return [
          "请检查居民身份证号",
          "本次注册时，该身份证号已被使用。请核对填写的号码。",
        ];
      if (state.reason === "invalid_input")
        return ["请检查填写内容", "请检查姓名和居民身份证号。"];
      return ["目前无法创建账户", "可以返回首页。"];
    default:
      return [
        "暂时无法确认此前的注册结果",
        "请先检查当前登录状态，暂时不要再次提交注册。",
      ];
  }
}
function Form({ controller }: { controller: Registration }) {
  const state = useSyncExternalStore(
    controller.subscribe,
    controller.snapshot,
    controller.snapshot,
  );
  const [name, setName] = useState("");
  const [idNumber, setIdNumber] = useState("");
  const [invalid, setInvalid] = useState<string>();
  const [now, setNow] = useState(Date.now);
  const title = useRef<HTMLHeadingElement>(null);
  const navigate = useNavigate();
  const editing = state.kind === "editing";
  useEffect(() => {
    const timer = setInterval(() => {
      setNow(Date.now());
      controller.expire();
    }, 1000);
    return () => clearInterval(timer);
  }, [controller]);
  useEffect(() => {
    if (state.kind === "created" && state.selection) {
      void navigate("/account", { replace: true });
      return;
    }
    title.current?.focus();
    if (!["editing", "storage-before", "preparing"].includes(state.kind)) {
      setName("");
      setIdNumber("");
      setInvalid(undefined);
    }
  }, [
    state.kind,
    state.kind === "created" ? state.selection : undefined,
    navigate,
  ]);
  useEffect(() => {
    const hide = () => {
      flushSync(() => {
        setName("");
        setIdNumber("");
      });
    };
    const visibility = () => {
      if (document.hidden) hide();
    };
    const before = (event: BeforeUnloadEvent) => {
      if (editing && (name || idNumber)) {
        event.preventDefault();
        event.returnValue = "";
      }
    };
    window.addEventListener("pagehide", hide);
    document.addEventListener("visibilitychange", visibility);
    window.addEventListener("beforeunload", before);
    return () => {
      window.removeEventListener("pagehide", hide);
      document.removeEventListener("visibilitychange", visibility);
      window.removeEventListener("beforeunload", before);
    };
  }, [editing, name, idNumber]);
  const seconds =
    "retryAt" in state
      ? Math.max(0, Math.ceil((state.retryAt - now) / 1000))
      : 0;
  const [heading, copy] = description(state);
  const known = state.kind.startsWith("created");
  return (
    <div className="authentication-layout">
      <aside className="service-introduction">
        <img src={botanical} alt="" />
      </aside>
      <section
        className="authentication-card"
        aria-busy={state.kind === "preparing" || state.kind === "submitting"}
      >
        {editing ? (
          <>
            <h1 ref={title} tabIndex={-1}>
              {heading}
            </h1>
            <p className="card-intro">{copy}</p>
            <p className="field-hint">
              请在 {Math.max(0, Math.ceil((state.expiresAt - now) / 60000))}{" "}
              分钟内提交，超时后需重新验证手机号。
            </p>
            <form
              autoComplete="off"
              noValidate
              onSubmit={(event) => {
                event.preventDefault();
                const canonical = name.trim().normalize("NFC");
                if (
                  !canonical ||
                  [...canonical].length > 64 ||
                  /\p{Cc}/u.test(canonical)
                ) {
                  setInvalid("name");
                  document.getElementById("registration-name")?.focus();
                  return;
                }
                if (!/^[0-9]{17}[0-9Xx]$/.test(idNumber)) {
                  setInvalid("id");
                  document.getElementById("registration-id")?.focus();
                  return;
                }
                setInvalid(undefined);
                void controller.submit(name, idNumber);
              }}
            >
              <div className="registration-field">
                <label htmlFor="registration-name">姓名</label>
                <input
                  id="registration-name"
                  name="name"
                  autoComplete="off"
                  value={name}
                  onChange={(event) => {
                    setName(event.target.value);
                    if (invalid === "name") setInvalid(undefined);
                  }}
                  aria-invalid={invalid === "name"}
                  aria-describedby="registration-name-help"
                />
                <p
                  id="registration-name-help"
                  className={invalid === "name" ? "field-error" : "field-hint"}
                >
                  {invalid === "name"
                    ? "请填写本人完整姓名，最多 64 个字符。"
                    : "请填写本人完整姓名。"}
                </p>
              </div>
              <div className="registration-field">
                <label htmlFor="registration-id">居民身份证号</label>
                <input
                  id="registration-id"
                  name="idNumber"
                  autoComplete="off"
                  spellCheck={false}
                  value={idNumber}
                  onChange={(event) => {
                    setIdNumber(event.target.value);
                    if (invalid === "id") setInvalid(undefined);
                  }}
                  aria-invalid={invalid === "id"}
                  aria-describedby="registration-id-help"
                />
                <p
                  id="registration-id-help"
                  className={invalid === "id" ? "field-error" : "field-hint"}
                >
                  请填写 18 位居民身份证号，末位可为 X。
                </p>
              </div>
              {state.invalid && (
                <p role="alert" className="field-error">
                  请检查姓名和居民身份证号。
                </p>
              )}
              <div className="notice">
                <p>姓名和身份证号用于登记个人账户。员工服务需另行加入。</p>
              </div>
              <Button className="primary send-button" type="submit">
                创建账户
              </Button>
            </form>
          </>
        ) : (
          <div className="result">
            <div
              className={`result-icon ${known ? "success" : ""}`}
              aria-hidden="true"
            >
              {known ? (
                <Check />
              ) : state.kind === "submitting" ? (
                <Clock3 />
              ) : (
                <Info />
              )}
            </div>
            <h1 ref={title} tabIndex={-1}>
              {heading}
            </h1>
            <p className="result-copy">{copy}</p>
            <div className="result-actions">
              {(state.kind === "pending" || state.kind === "limited") && (
                <Button
                  className="primary"
                  disabled={seconds > 0}
                  onClick={() =>
                    void (state.kind === "limited" && !state.sent
                      ? controller.revise()
                      : controller.confirm())
                  }
                >
                  {seconds > 0
                    ? `${seconds} 秒后可继续`
                    : !state.sent
                      ? state.kind === "limited"
                        ? "返回填写资料"
                        : "继续创建账户"
                      : "查看注册结果"}
                </Button>
              )}
              {[
                "storage-before",
                "storage-read",
                "storage-write",
                "created-storage",
                "created-cleanup",
              ].includes(state.kind) && (
                <Button
                  className="primary"
                  onClick={() => void controller.retryLocal()}
                >
                  重新检查
                </Button>
              )}
              {["reload", "random"].includes(state.kind) && (
                <Button className="primary" onClick={() => location.reload()}>
                  重新加载
                </Button>
              )}
              {(["changed", "missing", "expired", "created"].includes(
                state.kind,
              ) ||
                (state.kind === "rejected" && state.uncertain)) && (
                <a className="primary" href="/account">
                  检查登录状态
                </a>
              )}
              {state.kind === "created-storage" && (
                <a
                  className="quiet-link"
                  href="/authentication/codes"
                  target="_blank"
                  rel="noopener noreferrer"
                >
                  手机号登录
                </a>
              )}
              {state.kind === "rejected" &&
                state.editable &&
                !state.uncertain &&
                [
                  "invalid_input",
                  "id_number_taken",
                  "permission_denied",
                ].includes(state.reason) && (
                  <Button
                    className="primary"
                    onClick={() => void controller.revise()}
                  >
                    返回填写资料
                  </Button>
                )}
              {(state.kind === "credential-expired" ||
                (state.kind === "rejected" &&
                  !state.uncertain &&
                  (!state.editable ||
                    ["verification_required", "mobile_taken"].includes(
                      state.reason,
                    )))) && (
                <a
                  className="primary"
                  href="/authentication/codes"
                  target="_blank"
                  rel="noopener noreferrer"
                >
                  重新验证手机号
                </a>
              )}
            </div>
          </div>
        )}
        {!["submitting", "preparing"].includes(state.kind) && (
          <p className="scope-note">
            <a href="/">返回首页</a>
          </p>
        )}
      </section>
    </div>
  );
}
export default function RegisterPage() {
  const controller = useRegistration();
  return (
    <>
      <Header />
      <main id="main">
        {controller ? (
          <Form controller={controller} />
        ) : (
          <div className="loading-shell">
            <p>正在打开注册页面…</p>
          </div>
        )}
      </main>
      <Footer />
    </>
  );
}
