import {
  useCallback,
  useEffect,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";
import { flushSync } from "react-dom";
import { Button } from "@base-ui/react/button";
import { Info, UserRound } from "lucide-react";
import { Header, Footer } from "../root";
import { Current, type CurrentState } from "../authentication/current";
import { Sessions } from "../authentication/session";
import { Avatar } from "../avatar/image";
import { current } from "../protocol/session";
import { currentBuild } from "../protocol/build";
import { reloadRequired } from "../integrity";
import { History } from "../correction/history";
import { HistoryPanel } from "../correction/panel";
import { history } from "../protocol/correction";

export function meta() {
  return [
    { title: "个人账户 · 司籍处" },
    { name: "robots", content: "noindex" },
  ];
}

function useCurrent() {
  const [controllers, setControllers] = useState<{
    current: Current;
    history: History;
    avatar: Avatar;
  }>();
  useEffect(() => {
    const sessions = new Sessions();
    const controller = new Current({
      sessions,
      current,
      build: currentBuild,
      now: Date.now,
    });
    const records = new History({
      current: controller,
      history,
      build: currentBuild,
      now: Date.now,
    });
    const avatar = new Avatar({ current: controller });
    setControllers({ current: controller, history: records, avatar });
    const hide = () => flushSync(() => controller.suspend());
    const reload = () => flushSync(() => controller.reload());
    const check = () => {
      hide();
      if (!document.hidden) void controller.verify();
    };
    const unwatch = sessions.watch(check);
    document.addEventListener("visibilitychange", check);
    window.addEventListener("focus", check);
    window.addEventListener("pageshow", check);
    window.addEventListener("pagehide", hide);
    window.addEventListener("halo:reload-required", reload);
    if (reloadRequired()) controller.reload();
    else if (!document.hidden) void controller.verify();
    return () => {
      controller.suspend();
      records.close();
      avatar.close();
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

function status(state: CurrentState): [string, string] {
  switch (state.kind) {
    case "checking":
      return ["正在检查登录状态", "请稍候。"];
    case "anonymous":
      return ["请使用手机号登录", "请验证手机号后继续。"];
    case "expired":
      return ["请重新登录", "当前登录已失效。"];
    case "changed":
      return ["正在检查登录状态", "请稍候。"];
    case "storage":
      return [
        "暂时无法检查登录状态",
        "浏览器无法读取登录信息。请先重试，仍无法确认时可在新标签页重新登录。",
      ];
    case "reload":
      return ["请重新加载页面", "暂时无法检查登录状态，请重新加载页面。"];
    case "denied":
      return ["暂时无法完成检查", "本次检查未能完成，你可以返回首页。"];
    case "limited":
      return ["请稍后再检查", "暂时无法检查登录状态，请稍后重试。"];
    default:
      return ["暂时无法检查登录状态", "请稍后重试，或先浏览首页。"];
  }
}

function AvatarView({
  controller,
  name,
}: {
  controller: Avatar;
  name: string;
}) {
  const state = useSyncExternalStore(
    controller.subscribe,
    controller.snapshot,
    controller.snapshot,
  );
  const image = useRef<HTMLImageElement>(null);
  const attempt =
    state.kind === "loading" || state.kind === "ready"
      ? state.attempt
      : undefined;
  // Keep the ref stable through loading -> ready. Detaching an attempt drops
  // its URL and listeners, without claiming cancellation of browser work.
  const attach = useCallback(
    (node: HTMLImageElement | null) => {
      image.current = node;
      if (!node || attempt === undefined) return;
      const loaded = () => controller.loaded(attempt);
      const failed = () => controller.error(attempt);
      node.addEventListener("load", loaded);
      node.addEventListener("error", failed);
      return () => {
        if (image.current === node) image.current = null;
        node.removeEventListener("load", loaded);
        node.removeEventListener("error", failed);
        node.removeAttribute("src");
      };
    },
    [attempt, controller],
  );
  useEffect(() => {
    if (attempt === undefined) return;
    const node = image.current;
    if (!node) return;
    // Cached bytes may complete before listeners were attached.
    if (node.complete) {
      if (node.naturalWidth > 0) controller.loaded(attempt);
      else controller.error(attempt);
    }
  }, [attempt, controller]);
  if (state.kind === "hidden") return null;
  const showing = state.kind === "ready";
  return (
    <>
      <div
        className="avatar-slot"
        data-state={state.kind}
        {...(state.kind === "failed"
          ? { role: "img", "aria-label": "头像暂时无法加载" }
          : {})}
      >
        {!showing && <UserRound aria-hidden="true" />}
        {state.kind !== "failed" && (
          <img
            key={attempt}
            ref={attach}
            className="avatar-image"
            src={state.href}
            width={72}
            height={72}
            alt={`${name}的头像`}
            referrerPolicy="no-referrer"
            decoding="async"
            hidden={!showing}
          />
        )}
      </div>
      {state.kind === "loading" && (
        <span className="visually-hidden" role="status">
          正在加载头像
        </span>
      )}
      {state.kind === "failed" && (
        <p className="avatar-message" role="status">
          {state.retry ? (
            <>
              图片暂时无法加载。
              <button
                type="button"
                className="text-action"
                onClick={() => void controller.retry()}
              >
                重新加载头像
              </button>
            </>
          ) : (
            "头像仍暂时无法加载，其他资料可继续查看。"
          )}
        </p>
      )}
    </>
  );
}

function Account({
  controller,
  history,
  avatar,
}: {
  controller: Current;
  history: History;
  avatar: Avatar;
}) {
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
  useEffect(() => {
    heading.current?.focus();
  }, [state.kind]);
  const account = state.kind === "ready" ? state.account : undefined;
  const seconds =
    "retryAt" in state
      ? Math.max(0, Math.ceil((state.retryAt - now) / 1000))
      : 0;
  const [title, copy] = status(state);

  return (
    <div className="authentication-layout compact">
      <section
        className="authentication-card"
        aria-busy={state.kind === "checking"}
      >
        {account ? (
          <div className="current-content">
            <AvatarView controller={avatar} name={account.name} />
            <p className="eyebrow">个人账户</p>
            <h1 ref={heading} tabIndex={-1}>
              当前已登录
            </h1>
            <p className="card-intro">以下是当前登录的账户。</p>
            <dl className="current-summary">
              <div>
                <dt>姓名</dt>
                <dd>{account.name}</dd>
              </div>
              <div>
                <dt>身份证号</dt>
                <dd>
                  尾号 <span className="suffix">{account.idNumber.suffix}</span>
                </dd>
              </div>
              <div>
                <dt>微信</dt>
                <dd>
                  <span
                    className={
                      account.binding
                        ? "binding-state"
                        : "binding-state unbound"
                    }
                  >
                    {account.binding ? "已绑定" : "未绑定"}
                  </span>
                </dd>
              </div>
            </dl>
            <p className="current-help">你可以继续浏览公开内容。</p>
            <div className="account-affiliation">
              <a className="quiet-link" href="/account/affiliation">
                政治面貌
              </a>
              <p className="field-hint">
                进入本人政治面貌页面。未填写时可自愿首次填写；已有值只读。
              </p>
            </div>
            <div className="account-actions">
              <a className="quiet-link" href="/workforce">
                查看员工服务
              </a>
              <a className="quiet-link" href="/account/mobile">
                更换手机号
              </a>
              <a className="quiet-link" href="/sessions/logout-all">
                退出全部设备
              </a>
              <a className="quiet-link" href="/sessions/logout">
                退出本次登录
              </a>
              <a className="quiet-link danger-link" href="/account/cancellation">
                注销账户
              </a>
            </div>
            <a className="primary send-button" href="/">
              返回首页
            </a>
          </div>
        ) : (
          <div className="result">
            <div className="result-icon" aria-hidden="true">
              <Info />
            </div>
            <h1 ref={heading} tabIndex={-1}>
              {title}
            </h1>
            <p className="result-copy">{copy}</p>
            <div className="result-actions">
              {state.kind === "reload" && (
                <Button className="primary" onClick={() => location.reload()}>
                  重新加载页面
                </Button>
              )}
              {["unavailable", "limited", "storage"].includes(state.kind) && (
                <Button
                  className="primary"
                  disabled={seconds > 0}
                  onClick={() => void controller.verify()}
                >
                  {seconds > 0 ? `${seconds} 秒后可重新检查` : "重新检查"}
                </Button>
              )}
              {["anonymous", "expired", "storage"].includes(state.kind) && (
                <>
                  <a
                    className={
                      state.kind === "storage" ? "quiet-link" : "primary"
                    }
                    href="/authentication/codes"
                    target="_blank"
                    rel="noopener"
                    aria-describedby="account-login-window"
                  >
                    手机号登录
                  </a>
                  <p className="field-hint" id="account-login-window">
                    登录页面将在新标签页打开。
                  </p>
                </>
              )}
              <a className="quiet-link" href="/">
                返回首页
              </a>
            </div>
          </div>
        )}
      </section>
      {account && <HistoryPanel controller={history} />}
    </div>
  );
}

export default function AccountPage() {
  const controllers = useCurrent();
  return (
    <>
      <Header />
      <main id="main">
        {controllers ? (
          <Account
            controller={controllers.current}
            history={controllers.history}
            avatar={controllers.avatar}
          />
        ) : (
          <div className="loading-shell">
            <p>正在打开个人账户…</p>
          </div>
        )}
      </main>
      <Footer />
    </>
  );
}
