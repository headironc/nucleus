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
import { leaveKind, type State, type UnknownNotice } from "./controller";
import type { Cancellation } from "./controller";

type Screen = Readonly<{
  kicker: string;
  title: string;
  introduction: string;
  documentTitle?: string;
  panel: ReactNode;
  guide?: ReactNode;
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

function noticeCopy(notice: UnknownNotice): [string, string, string?] {
  if (notice === "mismatch")
    return [
      "暂时无法确认注销结果",
      "本次提交与已保存的信息不一致，当前无法确认账户是否已注销。",
      "请勿重复提交",
    ];
  return [
    "暂时无法确认注销结果",
    "注销申请可能已经发出，但现在无法确认账户是否已注销。",
    "登录失效不代表注销成功",
  ];
}

function currentResult(
  screen: CurrentState,
  now: number,
  controller: Cancellation,
): Screen {
  const kicker = "注销结果 · 检查登录";
  const introduction = "检查只读取当前登录，不会重新提交注销。";
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
            <span className="result-status">登录检查</span>
            <h2>正在检查当前登录</h2>
            <p className="stage-description">
              检查只读取当前账户，不会重新提交注销。
            </p>
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
        introduction: "当前登录仍有效，注销结果仍待确认。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <span className="result-status">登录检查</span>
            <h2>当前登录仍有效</h2>
            <p className="stage-description">
              当前登录账户仍可使用。这不能单独确认本次注销结果。
            </p>
            <div className="current-account">
              <span>当前登录</span>
              <strong>{screen.account.name}</strong>
              <span>
                身份证尾号{" "}
                <span className="suffix">{screen.account.idNumber.suffix}</span>
              </span>
            </div>
            <div className="cancellation-note">
              <h3>不能单独确认注销结果</h3>
              <p>
                当前登录可用不能证明本次注销已经完成或没有完成。请勿重复提交注销；本页不提供注销结果查询。
              </p>
            </div>
            <div className="cancellation-actions">
              <Button
                className="primary"
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
        introduction: "当前登录已失效，注销结果仍待确认。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <span className="result-status">登录检查</span>
            <h2>当前登录已无法使用</h2>
            <p className="stage-description">
              登录失效不代表注销成功，也不能确认本次注销结果。
            </p>
            <div className="cancellation-note warning">
              <h3>请勿用登录结果判断注销</h3>
              <p>
                重新登录只检查当前账户，不会继续此前的注销。新注册或另一账户不承接本次注销。
              </p>
            </div>
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
        introduction: "注销结果仍待确认。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <span className="result-status">登录检查</span>
            <h2>需要登录</h2>
            <p className="stage-description">
              当前没有可用的登录。登录后可以查看当前账户，但不能据此确认此前的注销结果。
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
        kicker,
        title: "登录状态",
        introduction: "当前账户资料已隐藏。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <span className="result-status">登录检查</span>
            <h2>暂时无法读取登录信息</h2>
            <p className="stage-description">
              浏览器中的登录信息无法使用。可以重新检查，或选择手机号登录。此前的注销结果仍无法确认。
            </p>
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
        introduction: "当前登录暂时无法检查，注销结果仍待确认。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <span className="result-status">登录检查</span>
            <h2>暂时无法检查当前登录</h2>
            <p className="stage-description">请稍后再试。注销结果仍待确认。</p>
            <div className="cancellation-actions">{home}</div>
          </section>
        ),
      };
    case "limited":
      return {
        kicker,
        title: "登录状态",
        introduction: "当前登录暂时无法检查，注销结果仍待确认。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <span className="result-status">登录检查</span>
            <h2>请稍后再检查登录</h2>
            <p className="stage-description">
              暂时无法处理，注销结果仍待确认。
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
        introduction: "当前登录暂时无法检查，注销结果仍待确认。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <span className="result-status">登录检查</span>
            <h2>请重新加载页面</h2>
            <p className="stage-description">
              页面暂时无法继续，请重新加载后再检查当前登录。
            </p>
            <div className="cancellation-actions">
              <Button className="primary" onClick={() => location.reload()}>
                重新加载页面
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
        introduction: "当前登录暂时无法检查，注销结果仍待确认。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <span className="result-status">登录检查</span>
            <h2>暂时无法检查当前登录</h2>
            <p className="stage-description">
              本次检查没有完成，注销结果仍待确认。
            </p>
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
        title: "注销账户",
        introduction: "正在检查当前账户，检查完成前不展示本人资料。",
        panel: (
          <section
            className="cancellation-panel cancellation-result"
            aria-busy="true"
          >
            <span className="reading-symbol" aria-hidden="true" />
            <h2>正在检查当前账户</h2>
            <p className="stage-description">
              检查完成前不展示本人资料，也不会提交注销。
            </p>
          </section>
        ),
      };
    case "anonymous":
    case "expired":
      return {
        kicker: "账户管理 · 仅限本人",
        title: "请重新登录",
        introduction: "当前登录已失效，本次注销尚未提交。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <span className="result-status">账户检查</span>
            <h2>请重新登录</h2>
            <p className="stage-description">
              当前登录已无法继续使用，本次注销尚未提交。
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
        title: "暂时无法读取登录信息",
        introduction: "当前账户资料已隐藏，本次注销尚未提交。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <span className="result-status">账户检查</span>
            <h2>暂时无法读取登录信息</h2>
            <p className="stage-description">
              浏览器中的登录信息无法使用。可以重新检查，或选择手机号登录。本次注销尚未提交。
            </p>
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
        title: "暂时无法办理注销",
        introduction: "暂时无法读取账户，本次注销尚未提交。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <span className="result-status">账户检查</span>
            <h2>暂时无法办理注销</h2>
            <p className="stage-description">请稍后再试。本次注销尚未提交。</p>
            <div className="cancellation-actions">{home}</div>
          </section>
        ),
      };
    case "limited":
      return {
        kicker: "账户管理 · 仅限本人",
        title: "请稍后再读取账户",
        introduction: "当前账户暂时无法读取，本次注销尚未提交。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <span className="result-status">账户检查</span>
            <h2>请稍后再读取账户</h2>
            <p className="stage-description">
              暂时无法处理，本次注销尚未提交。
            </p>
            <div className="cancellation-actions">
              {retry(screen.retryAt, "重新读取账户")}
              {home}
            </div>
          </section>
        ),
      };
    case "reload":
      return {
        kicker: "账户管理 · 仅限本人",
        title: "请重新加载页面",
        introduction: "当前账户暂时无法读取，本次注销尚未提交。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <h2>请重新加载页面</h2>
            <p className="stage-description">
              页面暂时无法继续，请重新加载后再办理注销。
            </p>
            <div className="cancellation-actions">
              <Button className="primary" onClick={() => location.reload()}>
                重新加载页面
              </Button>
              {home}
            </div>
          </section>
        ),
      };
    default:
      return {
        kicker: "账户管理 · 仅限本人",
        title: "暂时无法读取账户",
        introduction: "当前账户暂时无法读取，本次注销尚未提交。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <span className="result-status">账户检查</span>
            <h2>暂时无法读取账户</h2>
            <p className="stage-description">
              请重新加载后再办理注销。未取得可信资料前，不会提交注销。
            </p>
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
        introduction: "当前登录已失效，本次请求未被受理。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <span className="result-status">未受理</span>
            <h2>当前登录已失效</h2>
            <p className="stage-description">
              本次请求未被受理。请重新登录后再查看当前账户。
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
    case "denied":
      return {
        kicker: "账户管理",
        title: "注销结果",
        introduction: "本次注销未能办理，请求未被受理。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <span className="result-status">未办理</span>
            <h2>本次注销未能办理</h2>
            <p className="stage-description">
              暂时无法办理，请稍后再试。本次请求未被受理。
            </p>
            <div className="cancellation-actions">{home}</div>
          </section>
        ),
      };
    case "limited":
      return {
        kicker: "账户管理",
        title: "注销结果",
        introduction: "本次注销尚未受理。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <span className="result-status">未受理</span>
            <h2>本次注销尚未受理</h2>
            <p className="stage-description">
              本次注销没有开始办理，账户没有因此注销。
            </p>
            <p className="cancellation-guidance">
              等待结束后返回确认页面，核对当前账户并再次确认。
            </p>
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
        introduction: "账户资料已变化，需要重新确认。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <span className="result-status">需要重新确认</span>
            <h2>账户资料已变化</h2>
            <p className="stage-description">
              本次注销没有办理。请重新查看当前账户资料，再确认是否注销。
            </p>
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
        introduction: "本次注销没有完成。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <span className="result-status">未完成</span>
            <h2>本次注销没有完成</h2>
            <p className="stage-description">
              系统暂时无法处理，本次没有产生注销结果。
            </p>
            <p className="cancellation-guidance">
              可以稍后重新查看账户资料，再决定是否重新办理。
            </p>
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
        introduction: "本次注销未能办理。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <span className="result-status">未办理</span>
            <h2>本次注销未能办理</h2>
            <p className="stage-description">
              提交的确认信息未被接受，本次注销没有办理。
            </p>
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
        introduction: "注销前，请核对当前账户并了解后果。",
        entry: true,
        panel: (
          <section
            className="cancellation-panel"
            aria-labelledby="consequences-title"
          >
            {accountIdentity(state.account)}
            <h2 id="consequences-title">注销后会发生什么</h2>
            <ul className="cancellation-consequences">
              <li>账户不能恢复，当前及所有设备的登录都会结束。</li>
              <li>本人服务会结束；尚未办理完的原申请停止办理。</li>
              <li>必要业务记录和已保存的发放仍会保留，原工号不会自动释放。</li>
            </ul>
            <div className="cancellation-note warning">
              <h3>重新注册不会恢复原账户</h3>
              <p>
                同一手机号重新注册会是新账户，不继承原账户的资格、记录或员工服务。
              </p>
            </div>
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
        guide: (
          <aside className="cancellation-guide" aria-labelledby="guide-title">
            <p className="section-kicker">注销前请确认</p>
            <h2 id="guide-title">这不是退出登录</h2>
            <p>
              如果只是暂时离开，可以返回个人账户，选择退出登录。注销会永久结束账户和本人服务，申请发出后不能撤回。
            </p>
            <ul>
              <li>只注销当前登录的本人账户。</li>
              <li>结果未知时，不要重复提交。</li>
            </ul>
          </aside>
        ),
      };
    case "success":
      return {
        kicker: "账户管理",
        title: "账户已注销",
        introduction: "本次注销已完成。",
        panel: (
          <section className="cancellation-panel cancellation-result is-success">
            <span className="result-status success">已完成</span>
            <h2>账户已注销</h2>
            <p className="stage-description">本次注销已经生效。</p>
            <p className="cancellation-time">
              注销时间：
              <time dateTime={state.result.cancelledAt}>
                {stamp(state.result.cancelledAt)}
              </time>
            </p>
            <ul className="result-consequences">
              <li>全部登录和本人服务已结束，账户不能恢复。</li>
              <li>必要业务记录和已保存的发放仍会保留；原工号不会自动释放。</li>
              <li>同一手机号重新注册是新账户，不继承原资格和历史。</li>
            </ul>
            {!state.persisted && (
              <div className="cancellation-note warning">
                <h3>请勿关闭本页</h3>
                <p>
                  浏览器未能保存本次结果。关闭页面后，本页将无法再次显示这次注销结果。
                </p>
              </div>
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
      const [title, description, noteTitle] = noticeCopy(state.notice);
      return {
        kicker: "账户管理",
        title: "注销结果",
        introduction: "本次注销结果尚无法确认。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <span className="result-status">结果待确认</span>
            <h2>{title}</h2>
            <p className="stage-description">{description}</p>
            <p className="cancellation-guidance">
              重新登录或使用同一手机号注册，也不能证明本次注销结果；新账户不承接本次注销。
            </p>
            {!state.persisted && (
              <p className="cancellation-guidance">
                浏览器未能保存本次注销信息。请勿关闭本页，也不要重复提交。
              </p>
            )}
            <div className="cancellation-note warning">
              <h3>{noteTitle}</h3>
              <p>
                页面不会自动重复提交，也不提供注销结果查询。请先检查当前登录状态。
              </p>
            </div>
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
        introduction: "本次注销尚未提交。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <span className="result-status">未提交</span>
            <h2>本次尚未提交注销</h2>
            <p className="stage-description">
              浏览器未能保存本次办理所需的信息，注销申请没有发出。
            </p>
            <p className="cancellation-guidance">
              账户没有因此注销。返回确认页面后，可以重新核对并决定是否再次提交。
            </p>
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
        title: "正在准备注销",
        introduction: "正在核对当前登录，本次注销尚未提交。",
        panel: (
          <section
            className="cancellation-panel cancellation-result"
            aria-busy="true"
          >
            <span className="reading-symbol" aria-hidden="true" />
            <span className="result-status">准备中</span>
            <h2>正在准备注销</h2>
            <p className="stage-description">
              正在核对当前登录，本次注销尚未提交。
            </p>
            <div className="cancellation-actions">
              <Button className="primary" disabled>
                正在准备
              </Button>
              {home}
            </div>
          </section>
        ),
      };
    case "submitting":
      return {
        kicker: "账户管理",
        title: "注销结果",
        introduction: "正在提交注销申请。",
        panel: (
          <section
            className="cancellation-panel cancellation-result"
            aria-busy="true"
          >
            <span className="reading-symbol" aria-hidden="true" />
            <span className="result-status">提交中</span>
            <h2>正在提交注销申请</h2>
            <p className="stage-description">
              请稍候，不要重复提交。离开页面不会取消已经发出的申请。
            </p>
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
            <span className="result-status">
              {state.unsubmitted
                ? "未提交"
                : state.pending
                  ? "结果待确认"
                  : "暂时无法继续"}
            </span>
            <h2>暂时无法继续</h2>
            <p className="stage-description">
              {state.unsubmitted
                ? "浏览器无法保存注销信息，本次注销尚未提交。请重新检查。"
                : state.pending
                  ? "浏览器暂时无法读取本页信息。本次注销结果仍待确认，请重新检查。"
                  : "浏览器暂时无法读取本页信息。请重新检查。"}
            </p>
            {state.pending && (
              <p className="cancellation-guidance">
                离开不会取消已经发出的申请，也不会确认注销结果。
              </p>
            )}
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
        title: "请重新加载页面",
        introduction: state.pending
          ? "本次注销结果仍待确认。"
          : "页面暂时无法继续。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <span className="result-status">
              {state.pending ? "结果待确认" : "需要重新加载"}
            </span>
            <h2>请重新加载页面</h2>
            <p className="stage-description">
              {state.pending
                ? "本次注销结果仍待确认。重新加载不会重新提交注销。"
                : "页面暂时无法继续，请重新加载。"}
            </p>
            <div className="cancellation-actions">
              <Button className="primary" onClick={() => location.reload()}>
                重新加载页面
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
        introduction: "本次注销尚未提交。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <span className="result-status">未提交</span>
            <h2>本次注销尚未提交</h2>
            <p className="stage-description">
              暂时无法发起注销，请重新加载页面后再试。
            </p>
            <div className="cancellation-actions">
              <Button className="primary" onClick={() => location.reload()}>
                重新加载页面
              </Button>
              {home}
            </div>
          </section>
        ),
      };
    case "hidden":
      if (info.checking)
        return {
          kicker: "注销结果 · 检查登录",
          title: "登录状态",
          introduction: "检查只读取当前登录，不会重新提交注销。",
          panel: (
            <section
              className="cancellation-panel cancellation-result"
              aria-busy="true"
            >
              <span className="reading-symbol" aria-hidden="true" />
              <span className="result-status">登录检查</span>
              <h2>正在检查当前登录</h2>
              <p className="stage-description">
                检查只读取当前账户，不会重新提交注销。
              </p>
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
            ? "暂时无法读取当前登录。"
            : "当前登录状态已更新。",
        panel: (
          <section className="cancellation-panel cancellation-result">
            <h2>请检查当前账户</h2>
            <p className="stage-description">
              本页暂时无法显示账户信息。可以重新检查，或打开个人账户查看当前状态。
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
        {screen.entry ? (
          <div className="cancellation-columns">
            {screen.panel}
            {screen.guide}
          </div>
        ) : (
          screen.panel
        )}
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
        <h2 id="confirm-title">确认注销本人账户？</h2>
        <p id="confirm-description">
          注销后，全部登录和本人服务会结束，账户不能恢复。
        </p>
        {account && (
          <p className="confirm-identity">
            {account.name} · 身份证尾号 {account.idNumber.suffix}
          </p>
        )}
        <p className="dialog-note">
          必要业务记录和已保存的发放仍会保留；原工号不会自动释放。
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
            {kind === "unsubmitted" ? "尚未提交注销" : "离开前请留意注销结果"}
          </h2>
          <p id="leave-description">
            {kind === "unsubmitted"
              ? "本次注销尚未提交。离开后，需要重新打开本页并再次确认。"
              : "本次注销结果仍未确定。离开不会取消已经发出的申请。"}
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
