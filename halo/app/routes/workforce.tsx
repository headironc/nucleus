import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { flushSync } from "react-dom";
import { Button } from "@base-ui/react/button";
import { Header, Footer } from "../root";
import { Current, type CurrentState } from "../authentication/current";
import { Sessions } from "../authentication/session";
import { current } from "../protocol/session";
import { workforce } from "../protocol/workforce";
import { currentBuild } from "../protocol/build";
import { reloadRequired } from "../integrity";
import { Workforce } from "../workforce/status";
import { Numbers } from "../workforce/numbers";
import { currentNumber } from "../protocol/number";

export function meta() {
  return [
    { title: "员工服务 · 司籍处" },
    { name: "robots", content: "noindex" },
  ];
}

function useWorkforce() {
  const [controllers, setControllers] = useState<{
    current: Current;
    workforce: Workforce;
    numbers: Numbers;
  }>();
  useEffect(() => {
    const sessions = new Sessions();
    const identity = new Current({
      sessions,
      current,
      build: currentBuild,
      now: Date.now,
    });
    const status = new Workforce({
      current: identity,
      workforce,
      build: currentBuild,
      now: Date.now,
    });
    const numbers = new Numbers({
      current: identity,
      workforce: status,
      number: currentNumber,
      build: currentBuild,
      now: Date.now,
    });
    setControllers({ current: identity, workforce: status, numbers });
    const hide = () => flushSync(() => identity.suspend());
    const reload = () => flushSync(() => identity.reload());
    const check = () => {
      hide();
      if (!document.hidden) void identity.verify();
    };
    const unwatch = sessions.watch(check);
    document.addEventListener("visibilitychange", check);
    window.addEventListener("focus", check);
    window.addEventListener("pageshow", check);
    window.addEventListener("pagehide", hide);
    window.addEventListener("halo:reload-required", reload);
    if (reloadRequired()) identity.reload();
    else if (!document.hidden) void identity.verify();
    return () => {
      identity.suspend();
      numbers.close();
      status.close();
      unwatch();
      document.removeEventListener("visibilitychange", check);
      window.removeEventListener("focus", check);
      window.removeEventListener("pageshow", check);
      window.removeEventListener("pagehide", hide);
      window.removeEventListener("halo:reload-required", reload);
      void sessions.close().catch(() => {});
    };
  }, []);
  return controllers;
}

function identityCopy(state: CurrentState): [string, string] {
  switch (state.kind) {
    case "anonymous":
      return ["请使用手机号登录", "登录后可查看本人员工服务状态。"];
    case "expired":
      return ["请重新登录", "当前登录已失效。"];
    case "storage":
      return ["登录信息暂时无法使用", "请重新检查，或使用手机号登录。"];
    case "reload":
      return ["请重新加载", "页面暂时无法继续，请重新加载后再试。"];
    case "denied":
      return ["目前无法查看员工服务", "可以返回个人账户或首页。"];
    case "limited":
      return ["请稍后再检查", "暂时无法检查登录状态，请稍后重试。"];
    case "unavailable":
      return ["暂时无法检查登录状态", "请稍后重试，或先浏览首页。"];
    default:
      return ["正在检查登录状态…", "请稍候。"];
  }
}

function NumberCard({ controller, now }: { controller: Numbers; now: number }) {
  const state = useSyncExternalStore(
    controller.subscribe,
    controller.snapshot,
    controller.snapshot,
  );
  if (state.kind === "hidden") return null;
  const seconds =
    "retryAt" in state
      ? Math.max(0, Math.ceil((state.retryAt - now) / 1000))
      : 0;
  const copy =
    state.kind === "loading"
      ? "正在加载工号…"
      : state.kind === "limited"
        ? "暂时无法加载工号，请稍后再试。"
        : state.kind === "unavailable"
          ? "工号暂时无法加载，请重试。"
          : state.kind === "ready" && state.number === null
            ? "当前没有可用工号"
            : undefined;
  return (
    <section
      className="workforce-number"
      aria-labelledby="number-title"
      aria-busy={state.kind === "loading"}
    >
      <h2 id="number-title">我的工号</h2>
      {state.kind === "ready" && state.number && (
        <>
          <p className="number-subtype">{state.number.subtype}</p>
          <p className="employee-number" role="status">
            {state.number.display}
          </p>
        </>
      )}
      {copy && <p role="status">{copy}</p>}
      {state.kind === "ready" && state.number === null && (
        <a className="primary" href="/workforce/number">
          申请工号
        </a>
      )}
      {state.kind !== "loading" && (
        <Button
          className="number-refresh"
          disabled={seconds > 0}
          onClick={() => void controller.refresh()}
        >
          {seconds > 0 ? `${seconds} 秒后可刷新工号` : "刷新工号"}
        </Button>
      )}
    </section>
  );
}

function Service({
  identity,
  controller,
  numbers,
}: {
  identity: Current;
  controller: Workforce;
  numbers: Numbers;
}) {
  const account = useSyncExternalStore(
    identity.subscribe,
    identity.snapshot,
    identity.snapshot,
  );
  const state = useSyncExternalStore(
    controller.subscribe,
    controller.snapshot,
    controller.snapshot,
  );
  const [now, setNow] = useState(Date.now);
  const heading = useRef<HTMLHeadingElement>(null);
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, []);
  const kind = account.kind === "ready" ? state.kind : account.kind;
  useEffect(() => {
    heading.current?.focus();
  }, [kind]);
  const retryAt =
    account.kind !== "ready" && "retryAt" in account
      ? account.retryAt
      : "retryAt" in state
        ? state.retryAt
        : 0;
  const seconds = Math.max(0, Math.ceil((retryAt - now) / 1000));
  const status = state.kind === "ready" ? state.status : undefined;
  const descriptions = {
    absent: ["尚未加入员工服务", "个人账户可继续使用。"],
    active: ["已加入员工服务", ""],
    suspended: [
      "员工服务已暂停",
      "暂停期间不能申请工号或提交新的工牌申请，个人账户可继续使用。",
    ],
    terminated: ["员工服务已结束", "个人账户可继续使用。"],
  } as const;
  const [title, copy] =
    account.kind !== "ready"
      ? identityCopy(account)
      : status
        ? descriptions[status]
        : state.kind === "limited"
          ? ["暂时无法加载服务状态", "请稍后再试。"]
          : state.kind === "unavailable"
            ? ["员工服务状态暂时无法加载", "请重试。"]
            : ["正在加载员工服务状态…", "请稍候。"];
  return (
    <div className="authentication-layout compact">
      <section
        className="authentication-card"
        aria-busy={kind === "checking" || kind === "loading"}
      >
        <div className="current-content">
          <p className="eyebrow">员工服务</p>
          <h1 ref={heading} tabIndex={-1}>
            {title}
          </h1>
          {account.kind === "ready" && (
            <p className="workforce-name">{account.account.name}</p>
          )}
          {copy && (
            <p className="result-copy" role="status">
              {copy}
            </p>
          )}
          {account.kind === "ready" && status === "active" && (
            <NumberCard controller={numbers} now={now} />
          )}
          <div className="result-actions">
            {account.kind === "ready" && (
              <a className="primary" href="/workforce/applications">
                我的工牌申请
              </a>
            )}
            {account.kind === "ready" && (
              <a href="/workforce/reviews">工牌审核</a>
            )}
            {account.kind === "ready" &&
              (status === "active" || status === "suspended") && (
                <a className="primary" href="/workforce/termination">
                  结束员工服务
                </a>
              )}
            {account.kind === "ready" &&
              state.kind !== "hidden" &&
              state.kind !== "loading" && (
                <Button
                  className="primary"
                  disabled={seconds > 0}
                  onClick={() => void controller.refresh()}
                >
                  {seconds > 0
                    ? `${seconds} 秒后可刷新服务状态`
                    : "刷新服务状态"}
                </Button>
              )}
            {account.kind !== "ready" &&
              ["unavailable", "limited", "storage"].includes(account.kind) && (
                <Button
                  className="primary"
                  disabled={seconds > 0}
                  onClick={() => void identity.verify()}
                >
                  {seconds > 0 ? `${seconds} 秒后可重新检查` : "重新检查"}
                </Button>
              )}
            {account.kind === "reload" && (
              <Button className="primary" onClick={() => location.reload()}>
                重新加载
              </Button>
            )}
            {["anonymous", "expired", "storage"].includes(account.kind) && (
              <>
                <a
                  className="quiet-link"
                  href="/authentication/codes"
                  target="_blank"
                  rel="noopener"
                  aria-describedby="workforce-login-window"
                >
                  手机号登录
                </a>
                <p className="field-hint" id="workforce-login-window">
                  登录页面将在新标签页打开。
                </p>
              </>
            )}
            <a className="quiet-link" href="/account">
              返回个人账户
            </a>
            <a className="quiet-link" href="/">
              返回首页
            </a>
          </div>
        </div>
      </section>
    </div>
  );
}

export default function WorkforcePage() {
  const controllers = useWorkforce();
  return (
    <>
      <Header />
      <main id="main">
        {controllers ? (
          <Service
            identity={controllers.current}
            controller={controllers.workforce}
            numbers={controllers.numbers}
          />
        ) : (
          <div className="loading-shell">
            <p>正在打开员工服务…</p>
          </div>
        )}
      </main>
      <Footer />
    </>
  );
}
