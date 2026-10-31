import {
  useEffect,
  useRef,
  useState,
  useSyncExternalStore,
  type ReactNode,
} from "react";
import { Button } from "@base-ui/react/button";
import type { CurrentState } from "../authentication/current";
import type { Account } from "../protocol/session";
import { leaveKind, type State } from "./controller";
import type { Cancellation } from "./controller";

type Screen = Readonly<{
  kicker: string;
  title: string;
  introduction: string;
  documentTitle?: string;
  panel: ReactNode;
  entry?: boolean;
}>;

function stamp(value: string) {
  const date = new Date(value);
  return Number.isFinite(date.valueOf())
    ? new Intl.DateTimeFormat("zh-CN", {
        dateStyle: "long",
        timeStyle: "short",
        timeZone: "Asia/Shanghai",
      }).format(date)
    : value;
}

function remaining(at: number, now: number) {
  return Math.max(0, Math.ceil((at - now) / 1000));
}

function currentResult(
  screen: CurrentState,
  now: number,
  controller: Cancellation,
): Screen {
  const kicker = "当前账户";
  const introduction = "此前的注销结果仍未确认。";
  const home = (
    <a className="quiet-link" href="/">
      返回首页
    </a>
  );
  const retry = (label: string, at: number) => {
    const seconds = remaining(at, now);
    return (
      <Button
        className="primary"
        disabled={seconds > 0}
        onClick={() => void controller.check()}
      >
        {seconds ? `${seconds} 秒后可${label}` : label}
      </Button>
    );
  };
  switch (screen.kind) {
    case "checking":
    case "changed":
      return {
        kicker,
        title: "登录状态",
        introduction,
        panel: (
          <section
            className="cancellation-panel cancellation-result"
            aria-busy="true"
          >
            <span className="reading-symbol" aria-hidden="true" />
            <h2>正在检查当前登录</h2>
            <p className="stage-description">请稍候。</p>
            <div className="cancellation-actions">
              <Button className="primary" disabled>
                正在检查
              </Button>
              {home}
            </div>
          </section>
        ),
      };
    case "ready":
      return {
        kicker,
        title: "登录状态",
        introduction,
        panel: (
          <section className="cancellation-panel cancellation-result">
            <h2>当前登录仍有效</h2>
            <p className="stage-description">可打开个人账户查看当前资料。</p>
            <div className="current-account">
              <span>当前登录</span>
              <strong>{screen.account.name}</strong>
              <span>
                身份证尾号{" "}
                <span className="suffix">{screen.account.idNumber.suffix}</span>
              </span>
            </div>
            <div className="cancellation-actions">
              <a className="primary" href="/account">
                打开个人账户
              </a>
              <Button
                className="quiet-link"
                onClick={() => void controller.check()}
              >
                重新检查登录状态
              </Button>
              {home}
            </div>
          </section>
        ),
      };
    case "expired":
      return {
        kicker,
        title: "登录状态",
        introduction,
        panel: (
          <section className="cancellation-panel cancellation-result">
            <h2>请重新登录</h2>
            <p className="stage-description">
              请使用手机号登录，查看当前账户状态。
            </p>
            <div className="cancellation-actions">
              <a className="primary" href="/authentication/codes">
                手机号登录
              </a>
              {home}
            </div>
          </section>
        ),
      };
    case "anonymous":
      return {
        kicker,
        title: "登录状态",
        introduction,
        panel: (
          <section className="cancellation-panel cancellation-result">
            <h2>请使用手机号登录</h2>
            <p className="stage-description">登录后可查看当前账户状态。</p>
            <div className="cancellation-actions">
              <a className="primary" href="/authentication/codes">
                手机号登录
              </a>
              {home}
            </div>
          </section>
        ),
      };
    case "storage":
      return {
        kicker,
        title: "登录状态",
        introduction,
        panel: (
          <section className="cancellation-panel cancellation-result">
            <h2>登录信息暂时无法使用</h2>
            <p className="stage-description">请重新检查，或使用手机号登录。</p>
            <div className="cancellation-actions">
              <Button
                className="primary"
                onClick={() => void controller.check()}
              >
                重新检查登录状态
              </Button>
              <a className="quiet-link" href="/authentication/codes">
                手机号登录
              </a>
              {home}
            </div>
          </section>
        ),
      };
    case "denied":
      return {
        kicker,
        title: "登录状态",
        introduction,
        panel: (
          <section className="cancellation-panel cancellation-result">
            <h2>目前无法查看个人账户</h2>
            <p className="stage-description">可以返回首页。</p>
            <div className="cancellation-actions">{home}</div>
          </section>
        ),
      };
    case "limited":
      return {
        kicker,
        title: "登录状态",
        introduction,
        panel: (
          <section className="cancellation-panel cancellation-result">
            <h2>请稍后重试</h2>
            <p className="stage-description">
              暂时无法检查登录状态，请稍后再试。
            </p>
            <div className="cancellation-actions">
              {retry("检查登录状态", screen.retryAt)}
              {home}
            </div>
          </section>
        ),
      };
    case "reload":
      return {
        kicker,
        title: "登录状态",
        introduction,
        panel: (
          <section className="cancellation-panel cancellation-result">
            <h2>请重新加载</h2>
            <p className="stage-description">请重新加载后再检查登录状态。</p>
            <div className="cancellation-actions">
              <Button className="primary" onClick={() => location.reload()}>
                重新加载
              </Button>
              {home}
            </div>
          </section>
        ),
      };
    default:
      return {
        kicker,
        title: "登录状态",
        introduction,
        panel: (
          <section className="cancellation-panel cancellation-result">
            <h2>暂时无法检查登录状态</h2>
            <p className="stage-description">请稍后重新检查。</p>
            <div className="cancellation-actions">
              {retry("检查登录状态", screen.retryAt)}
              {home}
            </div>
          </section>
        ),
      };
  }
}

function readFailure(
  screen: Exclude<CurrentState, { kind: "ready" }>,
  now: number,
  controller: Cancellation,
): Screen {
  const home = (
    <a className="quiet-link" href="/">
      返回首页
    </a>
  );
  const retry = (at: number, label = "重新检查") => {
    const seconds = remaining(at, now);
    return (
      <Button
        className="primary"
        disabled={seconds > 0}
        onClick={() => void controller.verify()}
      >
        {seconds ? `${seconds} 秒后可${label}` : label}
      </Button>
    );
  };
  switch (screen.kind) {
    case "checking":
    case "changed":
      return {
        kicker: "账户管理 · 仅限本人",
        title: "正在检查账户…",
        introduction: "请稍候。",
        panel: (
          <section
            className="cancellation-panel cancellation-result"
            aria-busy="true"
          >
            <span className="reading-symbol" aria-hidden="true" />
          </section>
        ),
      };
    case "anonymous":
    case "expired":
      return {
        kicker: "账户管理 · 仅限本人",
        title: screen.kind === "anonymous" ? "请使用手机号登录" : "请重新登录",
        introduction: "请验证手机号后继续。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <p className="stage-description">
              手机号验证后可核对当前账户资料。
            </p>
            <div className="cancellation-actions">
              <a className="primary" href="/authentication/codes">
                手机号登录
              </a>
              {home}
            </div>
          </section>
        ),
      };
    case "storage":
      return {
        kicker: "账户管理 · 仅限本人",
        title: "登录信息暂时无法使用",
        introduction: "请重新检查，或使用手机号登录。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <div className="cancellation-actions">
              {retry(0)}
              <a className="quiet-link" href="/authentication/codes">
                手机号登录
              </a>
              {home}
            </div>
          </section>
        ),
      };
    case "denied":
      return {
        kicker: "账户管理 · 仅限本人",
        title: "目前无法注销账户",
        introduction: "可以返回首页。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <div className="cancellation-actions">{home}</div>
          </section>
        ),
      };
    case "limited":
      return {
        kicker: "账户管理 · 仅限本人",
        title: "暂时无法检查账户",
        introduction: "请稍后重新检查。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <div className="cancellation-actions">
              {retry(screen.retryAt)}
              {home}
            </div>
          </section>
        ),
      };
    case "reload":
      return {
        kicker: "账户管理 · 仅限本人",
        title: "请重新加载",
        introduction: "页面暂时无法继续，请重新加载后再试。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <div className="cancellation-actions">
              <Button className="primary" onClick={() => location.reload()}>
                重新加载
              </Button>
              {home}
            </div>
          </section>
        ),
      };
    default:
      return {
        kicker: "账户管理 · 仅限本人",
        title: "暂时无法检查账户",
        introduction: "请稍后重新检查。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <div className="cancellation-actions">
              {retry(screen.retryAt)}
              {home}
            </div>
          </section>
        ),
      };
  }
}

function accountIdentity(account: Account) {
  return (
    <div className="identity-block">
      <p className="section-kicker">将要注销的账户</p>
      <h2 className="identity-name">{account.name}</h2>
      <p className="identity-meta">
        本人账户 · 身份证尾号{" "}
        <span className="suffix">{account.idNumber.suffix}</span>
      </p>
    </div>
  );
}

function refusedScreen(
  state: Extract<State, { kind: "refused" }>,
  now: number,
  controller: Cancellation,
): Screen {
  const seconds = remaining(state.retryAt, now);
  const home = (
    <a className="quiet-link" href="/">
      返回首页
    </a>
  );
  const restart = (label: string) => (
    <Button
      className="primary"
      disabled={seconds > 0}
      onClick={() => void controller.start()}
    >
      {seconds ? `${seconds} 秒后可${label}` : label}
    </Button>
  );
  switch (state.notice) {
    case "authentication":
      return {
        kicker: "账户管理",
        title: "注销结果",
        introduction: "本次尚未注销。请使用手机号登录，查看当前账户。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <h2>请重新登录</h2>
            <div className="cancellation-actions">
              <a className="primary" href="/authentication/codes">
                手机号登录
              </a>
              {home}
            </div>
          </section>
        ),
      };
    case "denied":
      return {
        kicker: "账户管理",
        title: "注销结果",
        introduction: "可以返回首页。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <h2>本次尚未注销</h2>
            <div className="cancellation-actions">{home}</div>
          </section>
        ),
      };
    case "limited":
      return {
        kicker: "账户管理",
        title: "注销结果",
        introduction: "请稍后返回确认页面再试。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <h2>本次尚未注销</h2>
            <div className="cancellation-actions">
              {restart("返回确认页面")}
              {home}
            </div>
          </section>
        ),
      };
    case "target":
      return {
        kicker: "账户管理",
        title: "注销结果",
        introduction: "请重新查看账户资料后确认。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <h2>账户资料已变化</h2>
            <div className="cancellation-actions">
              {restart("重新查看账户资料")}
              {home}
            </div>
          </section>
        ),
      };
    case "dependency":
      return {
        kicker: "账户管理",
        title: "注销结果",
        introduction: "请重新查看账户资料后再试。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <h2>本次注销未完成</h2>
            <div className="cancellation-actions">
              {restart("重新查看账户资料")}
              {home}
            </div>
          </section>
        ),
      };
    default:
      return {
        kicker: "账户管理",
        title: "注销结果",
        introduction: "请重新查看账户资料后再试。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <h2>本次注销未完成</h2>
            <div className="cancellation-actions">
              {restart("重新查看账户资料")}
              {home}
            </div>
          </section>
        ),
      };
  }
}

function screenFor(
  state: State,
  info: ReturnType<Cancellation["checkInfo"]>,
  now: number,
  controller: Cancellation,
  openConfirm: () => void,
): Screen {
  const home = (
    <a className="quiet-link" href="/">
      返回首页
    </a>
  );
  if (state.kind === "unknown" && info.visible)
    return currentResult(info.current, now, controller);
  switch (state.kind) {
    case "entry":
      return {
        kicker: "账户管理 · 仅限本人",
        title: "注销账户",
        introduction: "注销前，请核对账户并了解以下后果。",
        entry: true,
        panel: (
          <section
            className="cancellation-panel"
            aria-labelledby="consequences-title"
          >
            {accountIdentity(state.account)}
            <h2 id="consequences-title">注销后会发生什么</h2>
            <ul className="cancellation-consequences">
              <li>账户无法恢复，网站和小程序的全部登录与员工服务将结束。</li>
              <li>
                尚未完成的原申请将停止办理，必要业务记录和已保存的发放记录保留。
              </li>
              <li>
                原工号不会自动释放。重新注册将创建新账户，不继承原账户的资格和历史。
              </li>
            </ul>
            <p className="cancellation-guidance">
              暂时离开可返回个人账户，选择退出登录。
            </p>
            <div className="cancellation-actions">
              <Button className="primary" onClick={openConfirm}>
                注销账户
              </Button>
              <a className="quiet-link" href="/account">
                返回个人账户
              </a>
            </div>
          </section>
        ),
      };
    case "success":
      return {
        kicker: "账户管理",
        title: "账户已注销",
        introduction: "全部登录和员工服务已结束，原账户无法恢复。",
        panel: (
          <section className="cancellation-panel cancellation-result is-success">
            <p className="cancellation-time">
              注销时间（北京时间）：
              <time dateTime={state.result.cancelledAt}>
                {stamp(state.result.cancelledAt)}
              </time>
            </p>
            <ul className="result-consequences">
              <li>必要业务记录和已保存的发放记录保留，原工号不会自动释放。</li>
              <li>重新注册将创建新账户，不继承原账户的资格和历史。</li>
            </ul>
            {!state.persisted && (
              <p className="cancellation-guidance">
                本页暂时无法保存结果，关闭后可能无法再次显示本次结果。
              </p>
            )}
            <div className="cancellation-actions">
              <a className="primary" href="/">
                返回首页
              </a>
            </div>
          </section>
        ),
      };
    case "unknown": {
      return {
        kicker: "账户管理",
        title: "注销结果尚不确定",
        introduction: "暂时不要重复提交，可先检查登录状态。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            {!state.persisted && (
              <p className="cancellation-guidance">
                本页暂时无法保存注销信息，关闭后可能无法再次显示本次状态。
              </p>
            )}
            <div className="cancellation-actions">
              <Button
                className="primary"
                onClick={() => void controller.check()}
              >
                检查登录状态
              </Button>
              {home}
            </div>
          </section>
        ),
      };
    }
    case "refused":
      return refusedScreen(state, now, controller);
    case "unsent":
      return {
        kicker: "账户管理",
        title: "本次尚未提交注销",
        introduction: "请返回确认页面，重新核对账户。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <div className="cancellation-actions">
              <Button
                className="primary"
                onClick={() => void controller.start()}
              >
                返回确认页面
              </Button>
              {home}
            </div>
          </section>
        ),
      };
    case "preparing":
      return {
        kicker: "账户管理",
        title: "正在核对账户…",
        introduction: "请稍候。",
        panel: (
          <section
            className="cancellation-panel cancellation-result"
            aria-busy="true"
          >
            <span className="reading-symbol" aria-hidden="true" />
            <div className="cancellation-actions">
              <Button className="primary" disabled>
                请稍候
              </Button>
              {home}
            </div>
          </section>
        ),
      };
    case "submitting":
      return {
        kicker: "账户管理",
        title: "正在注销账户…",
        introduction: "请稍候。",
        panel: (
          <section
            className="cancellation-panel cancellation-result"
            aria-busy="true"
          >
            <span className="reading-symbol" aria-hidden="true" />
            <p className="stage-description">离开不会撤回已发出的注销请求。</p>
            <div className="cancellation-actions">
              <Button className="primary" disabled>
                正在注销
              </Button>
              {home}
            </div>
          </section>
        ),
      };
    case "storage":
      return {
        kicker: "账户管理",
        title: "暂时无法继续",
        introduction: state.unsubmitted
          ? "本次注销尚未提交。"
          : state.pending
            ? "本次注销结果仍待确认。"
            : "页面暂时无法继续。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <p className="stage-description">
              {state.unsubmitted
                ? "本次尚未提交注销，请重新检查。"
                : state.pending
                  ? "暂时无法确认注销结果，请重新检查。"
                  : "请重新检查后再试。"}
            </p>
            <div className="cancellation-actions">
              <Button
                className="primary"
                onClick={() => void controller.verify()}
              >
                重新检查
              </Button>
              <a className="quiet-link" href="/authentication/codes">
                手机号登录
              </a>
              {home}
            </div>
          </section>
        ),
      };
    case "reload":
      return {
        kicker: "账户管理",
        title: "请重新加载",
        introduction: state.pending
          ? "本次注销结果仍待确认。"
          : "页面暂时无法继续。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <p className="stage-description">请重新加载后按页面提示继续。</p>
            <div className="cancellation-actions">
              <Button className="primary" onClick={() => location.reload()}>
                重新加载
              </Button>
              {home}
            </div>
          </section>
        ),
      };
    case "random":
      return {
        kicker: "账户管理",
        title: "本次注销尚未提交",
        introduction: "请重新加载后再试。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <div className="cancellation-actions">
              <Button className="primary" onClick={() => location.reload()}>
                重新加载
              </Button>
              {home}
            </div>
          </section>
        ),
      };
    case "hidden":
      if (info.checking)
        return {
          kicker: "当前账户",
          title: "登录状态",
          introduction: "正在检查登录状态…",
          panel: (
            <section
              className="cancellation-panel cancellation-result"
              aria-busy="true"
            >
              <span className="reading-symbol" aria-hidden="true" />
              <p className="stage-description">请稍候。</p>
              <div className="cancellation-actions">
                <Button className="primary" disabled>
                  正在检查
                </Button>
                {home}
              </div>
            </section>
          ),
        };
      return {
        kicker: "当前账户",
        title: "账户状态",
        introduction:
          state.current.kind === "storage"
            ? "登录信息暂时无法使用。"
            : "请重新检查当前登录。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <p className="stage-description">
              可打开个人账户，查看当前资料。
            </p>
            <div className="cancellation-actions">
              <Button
                className="primary"
                onClick={() => void controller.verify()}
              >
                重新检查当前登录
              </Button>
              <a className="quiet-link" href="/account">
                打开个人账户
              </a>
              <a className="quiet-link" href="/authentication/codes">
                手机号登录
              </a>
              {home}
            </div>
          </section>
        ),
      };
    case "current":
      return readFailure(state.current, now, controller);
  }
}

export function Content({ controller }: { controller: Cancellation }) {
  const state = useSyncExternalStore(
    controller.subscribe,
    controller.snapshot,
    controller.snapshot,
  );
  const info = controller.checkInfo();
  const [now, setNow] = useState(Date.now);
  const [confirming, setConfirming] = useState(false);
  const [account, setAccount] = useState<Account>();
  const [leaving, setLeaving] = useState<string | null>(null);
  const confirmDialog = useRef<HTMLDialogElement>(null);
  const confirmCancel = useRef<HTMLButtonElement>(null);
  const confirmTrigger = useRef<HTMLElement | null>(null);
  const leaveDialog = useRef<HTMLDialogElement>(null);
  const leaveStay = useRef<HTMLButtonElement>(null);
  const leaveRestore = useRef<HTMLElement | null>(null);
  const leaveAllowed = useRef(false);
  const heading = useRef<HTMLHeadingElement>(null);
  const kind = leaveKind(state);
  const screen = screenFor(state, info, now, controller, openConfirm);

  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, []);
  useEffect(() => {
    document.title = `${screen.documentTitle ?? screen.title} · 司籍处`;
  }, [screen.documentTitle, screen.title]);
  useEffect(() => {
    heading.current?.focus();
  }, [screen.title]);
  useEffect(() => {
    if (state.kind !== "entry") {
      setConfirming(false);
      setAccount(undefined);
    }
  }, [state.kind]);
  useEffect(() => {
    const element = confirmDialog.current;
    if (!element) return;
    if (confirming && !element.open) {
      element.showModal();
      confirmCancel.current?.focus();
    } else if (!confirming && element.open) element.close();
  }, [confirming]);

  function openConfirm() {
    if (state.kind !== "entry") return;
    confirmTrigger.current =
      document.activeElement instanceof HTMLElement
        ? document.activeElement
        : null;
    setAccount(state.account);
    setConfirming(true);
  }
  const closeConfirm = () => {
    confirmDialog.current?.close();
    setConfirming(false);
    setAccount(undefined);
    confirmTrigger.current?.focus();
  };

  useEffect(() => {
    const off = controller.onHide(() => {
      // Native close is synchronous; the controller has already concealed
      // its private state before this callback runs.
      if (confirmDialog.current?.open) confirmDialog.current.close();
      if (leaveDialog.current?.open) leaveDialog.current.close();
      setConfirming(false);
      setAccount(undefined);
      setLeaving(null);
    });
    return () => {
      off();
    };
  }, [controller]);

  useEffect(() => {
    if (!kind) {
      setLeaving(null);
      return;
    }
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
      leaveRestore.current =
        document.activeElement instanceof HTMLElement
          ? document.activeElement
          : null;
      setLeaving(target.href);
    };
    window.addEventListener("beforeunload", before);
    document.addEventListener("click", follow);
    return () => {
      window.removeEventListener("beforeunload", before);
      document.removeEventListener("click", follow);
    };
  }, [kind]);

  useEffect(() => {
    const element = leaveDialog.current;
    if (!element) return;
    if (leaving && kind && !element.open) {
      element.showModal();
      leaveStay.current?.focus();
    } else if ((!leaving || !kind) && element.open) element.close();
  }, [leaving, kind]);

  const stay = () => {
    leaveDialog.current?.close();
    setLeaving(null);
    leaveRestore.current?.focus();
  };

  return (
    <div className="cancellation-layout">
      <div className="cancellation-heading">
        <div>
          <p className="eyebrow">{screen.kicker}</p>
          <h1 ref={heading} tabIndex={-1}>
            {screen.title}
          </h1>
          <p className="page-introduction">{screen.introduction}</p>
        </div>
        {screen.entry && (
          <a className="back-to-services" href="/account">
            ← 返回个人账户
          </a>
        )}
      </div>
      <div className="cancellation-content" aria-live="polite">
        {screen.panel}
      </div>

      <dialog
        ref={confirmDialog}
        className="cancellation-dialog"
        aria-labelledby="confirm-title"
        aria-describedby="confirm-description"
        onCancel={(event) => {
          event.preventDefault();
          closeConfirm();
        }}
      >
        <div className="dialog-heading">
          <span>账户不可恢复</span>
          <button
            type="button"
            className="dialog-close"
            aria-label="关闭"
            onClick={closeConfirm}
          >
            ×
          </button>
        </div>
        <h2 id="confirm-title">确认注销此账户？</h2>
        <p id="confirm-description">账户无法恢复，全部登录和员工服务将结束。</p>
        {account && (
          <p className="confirm-identity">
            {account.name} · 身份证尾号 {account.idNumber.suffix}
          </p>
        )}
        <p className="dialog-note">
          必要业务记录和已保存的发放记录保留，原工号不会自动释放。
        </p>
        <div className="dialog-actions">
          <Button
            ref={confirmCancel}
            className="quiet-link"
            onClick={closeConfirm}
          >
            暂不注销
          </Button>
          <Button
            className="primary"
            onClick={() => {
              confirmDialog.current?.close();
              setConfirming(false);
              void controller.submit();
            }}
          >
            确认注销
          </Button>
        </div>
      </dialog>

      {leaving && kind && (
        <dialog
          ref={leaveDialog}
          className="cancellation-dialog"
          aria-labelledby="leave-title"
          aria-describedby="leave-description"
          onCancel={(event) => {
            event.preventDefault();
            stay();
          }}
        >
          <div className="dialog-heading">
            <span>尚未离开</span>
            <button
              type="button"
              className="dialog-close"
              aria-label="关闭"
              onClick={stay}
            >
              ×
            </button>
          </div>
          <h2 id="leave-title">
            {kind === "unsubmitted" ? "尚未提交注销" : "注销结果尚未确认"}
          </h2>
          <p id="leave-description">
            {kind === "unsubmitted"
              ? "离开后需要重新打开本页并确认。"
              : "离开不会撤回已经发出的注销请求。"}
          </p>
          <div className="dialog-actions">
            <Button ref={leaveStay} className="primary" onClick={stay}>
              留在此页
            </Button>
            <Button
              className="quiet-link"
              onClick={() => {
                const target = leaving;
                leaveAllowed.current = true;
                leaveDialog.current?.close();
                setLeaving(null);
                controller.suspend();
                location.assign(target);
              }}
            >
              离开页面
            </Button>
          </div>
        </dialog>
      )}
    </div>
  );
}
