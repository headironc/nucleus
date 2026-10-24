import {
  useEffect,
  useRef,
  useState,
  useSyncExternalStore,
  type MouseEvent,
  type ReactNode,
  type RefObject,
} from "react";
import { flushSync } from "react-dom";
import { Dialog } from "@base-ui/react/dialog";
import { Check, TriangleAlert, X } from "lucide-react";
import { Header } from "../root";
import { Current } from "../authentication/current";
import { Sessions } from "../authentication/session";
import { current } from "../protocol/session";
import {
  createAffiliation,
  affiliationValues,
  currentAffiliation,
  type AffiliationCode,
  type AffiliationValue,
  type NamedAffiliation,
} from "../protocol/affiliation";
import { currentBuild } from "../protocol/build";
import { reloadRequired } from "../integrity";
import {
  Affiliation,
  type AffiliationPanel,
  type AffiliationRead,
  type AffiliationSnapshot,
} from "../affiliation/controller";
import { browserMarker } from "../affiliation/marker";
import "../affiliation/page.css";

const noSubscribe = () => () => {};
const noSnapshot = () => undefined as AffiliationSnapshot | undefined;

export function meta() {
  return [
    { title: "政治面貌 · 司籍处" },
    { name: "robots", content: "noindex" },
  ];
}

const dateFormatter = new Intl.DateTimeFormat("zh-CN", {
  year: "numeric",
  month: "long",
  day: "numeric",
  hour: "2-digit",
  minute: "2-digit",
  hour12: false,
  timeZone: "Asia/Shanghai",
});

function beijing(value: string): string {
  const milliseconds = Date.parse(value);
  return Number.isFinite(milliseconds)
    ? dateFormatter.format(new Date(milliseconds))
    : value;
}

function secondsUntil(at: number, now: number): number {
  return Math.max(0, Math.ceil((at - now) / 1_000));
}

function useController() {
  const [controllers, setControllers] = useState<{
    current: Current;
    affiliation: Affiliation;
  }>();
  useEffect(() => {
    const sessions = new Sessions();
    const identity = new Current({
      sessions,
      current,
      build: currentBuild,
      now: Date.now,
    });
    const affiliation = new Affiliation({
      current: identity,
      read: currentAffiliation,
      values: affiliationValues,
      create: createAffiliation,
      marker: browserMarker(() => window.sessionStorage, () => window.history),
      build: currentBuild,
      now: Date.now,
      operation: () => crypto.randomUUID(),
    });
    setControllers({ current: identity, affiliation });
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
      affiliation.close();
      identity.suspend();
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

function optionLabel(
  options: readonly NamedAffiliation[],
  code: AffiliationCode | undefined,
): string {
  if (!code) return "";
  const option = options.find((entry) => entry.value === code);
  return option ? `${option.value} ${option.name}` : code;
}

function personContext(person: string) {
  return (
    <p className="person-context">
      <span>本人</span>
      <strong>{person}</strong>
    </p>
  );
}

function StatePanel({
  busy,
  className = "",
  kicker,
  title,
  copy,
  person,
  extra,
  actions,
}: {
  busy?: boolean;
  className?: string;
  kicker?: string;
  title: string;
  copy: string;
  person?: string;
  extra?: ReactNode;
  actions?: ReactNode;
}) {
  return (
    <section
      className={`affiliation-panel state-panel ${className}`.trim()}
      aria-busy={busy || undefined}
    >
      {busy && <span className="reading-symbol" aria-hidden="true" />}
      {kicker && <p className="panel-kicker">{kicker}</p>}
      <h2 className="state-title" data-focus tabIndex={-1}>
        {title}
      </h2>
      <p className="state-copy">{copy}</p>
      {person && personContext(person)}
      {extra}
      {actions && <div className="panel-actions">{actions}</div>}
    </section>
  );
}

function ActionButton({
  children,
  onClick,
  secondary,
  disabled,
}: {
  children: ReactNode;
  onClick: () => void;
  secondary?: boolean;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      className={`button ${secondary ? "secondary" : "primary"}`}
      onClick={onClick}
      disabled={disabled}
    >
      {children}
    </button>
  );
}

function LinkButton({
  href,
  children,
  secondary,
  onClick,
  target,
  rel,
}: {
  href: string;
  children: ReactNode;
  secondary?: boolean;
  onClick: (event: MouseEvent<HTMLAnchorElement>) => void;
  target?: string;
  rel?: string;
}) {
  return (
    <a
      className={`button ${secondary ? "secondary" : "primary"}`}
      href={href}
      onClick={onClick}
      target={target}
      rel={rel}
    >
      {children}
    </a>
  );
}

function PendingSummary({
  options,
  code,
}: {
  options: readonly NamedAffiliation[];
  code?: AffiliationCode;
}) {
  if (!code) return null;
  return (
    <div className="pending-summary">
      <span>本次填写</span>
      <strong>{optionLabel(options, code)}</strong>
    </div>
  );
}

function RecordPanel({
  value,
  name,
  kicker = "当前记录",
  title = "当前政治面貌",
  person,
  context,
  current,
}: {
  value: AffiliationValue;
  name: string;
  kicker?: string;
  title?: string;
  person?: string;
  context?: "success" | "unknown";
  current?: boolean;
}) {
  const note =
    context === "success"
      ? "这次读取只补充当前记录，不改变本次已完成的结果。"
      : context === "unknown"
        ? "当前有值只来自这次另行读取，不证明原填写已经完成；原结果仍未确认。"
        : "已有记录后，本人不能自行更正、撤回或清空。";
  return (
    <section
      className={`affiliation-panel ${current ? "current-panel " : ""}record-panel`}
    >
      <div className="record-heading">
        <div>
          <p className="panel-kicker">{kicker}</p>
          <h2 className="state-title" data-focus tabIndex={-1}>
            {title}
          </h2>
        </div>
        <span className="read-only-label">只读</span>
      </div>
      {person && personContext(person)}
      <div className="record-display">
        <span className="record-code" aria-hidden="true">
          {value.value}
        </span>
        <strong className="record-name">{name}</strong>
      </div>
      <dl className="record-meta">
        <dt>生效时间</dt>
        <dd>
          <time dateTime={value.createdAt}>{beijing(value.createdAt)}</time>
        </dd>
      </dl>
      <p className="record-note">{note}</p>
    </section>
  );
}

function CurrentReadPanel({
  read,
  context,
  now,
  onRetry,
}: {
  read: AffiliationRead;
  context: "success" | "unknown";
  now: number;
  onRetry: () => void;
}) {
  const success = context === "success";
  const note = (mode: "loading" | "error" | "unset" | "value") => {
    if (success) {
      if (mode === "loading")
        return "读取完成前不显示记录。已确认的填写结果不受影响。";
      if (mode === "error")
        return "已确认的填写结果仍然有效；读取失败只影响当前值展示。";
      if (mode === "unset")
        return "已确认的填写结果仍然有效；这次读取只补充当前记录。";
      return "这次读取只补充当前记录，不改变本次已完成的结果。";
    }
    if (mode === "loading")
      return "读取完成前不显示记录。原填写结果仍未确认。";
    if (mode === "error")
      return "本次读取失败不说明原填写是否完成，原结果仍未确认。";
    if (mode === "unset")
      return "这次读取没有找到记录，但不能证明原填写未完成，也不会在本页开始新的填写。";
    return "当前有值只来自这次另行读取，不证明原填写已经完成；原结果仍未确认。";
  };

  if (read.kind === "idle") return null;
  if (read.kind === "loading")
    return (
      <section
        className="affiliation-panel current-panel state-panel"
        aria-busy="true"
      >
        <span className="reading-symbol" aria-hidden="true" />
        <p className="panel-kicker">另行读取</p>
        <h2 className="state-title" data-focus tabIndex={-1}>
          正在读取当前政治面貌
        </h2>
        <p className="state-copy">{note("loading")}</p>
      </section>
    );
  if (read.kind === "record")
    return (
      <RecordPanel
        value={read.value}
        name={read.name}
        kicker="另行读取"
        person={undefined}
        context={context}
        current
      />
    );
  if (read.kind === "unset")
    return (
      <section className="affiliation-panel current-panel state-panel">
        <p className="panel-kicker">另行读取</p>
        <h2 className="state-title" data-focus tabIndex={-1}>
          当前尚未填写
        </h2>
        <p className="state-copy">这次读取没有找到政治面貌记录。</p>
        <p className="page-note">{note("unset")}</p>
      </section>
    );
  const remaining = secondsUntil(read.retryAt, now);
  return (
    <section className="affiliation-panel current-panel state-panel">
      <p className="panel-kicker">另行读取</p>
      <h2 className="state-title" data-focus tabIndex={-1}>
        {read.kind === "dictionary-error"
          ? "当前政治面貌暂时无法显示"
          : "暂时无法读取当前政治面貌"}
      </h2>
      <p className="state-copy">
        {read.kind === "dictionary-error"
          ? "当前政治面貌记录已读取，但完整名称暂未取得。请稍后重新读取。"
          : "请稍后重新读取。"}
      </p>
      <p className="page-note">{note("error")}</p>
      <div className="panel-actions">
        <ActionButton onClick={onRetry} disabled={remaining > 0}>
          {remaining > 0
            ? `${remaining} 秒后重新读取`
            : "重新读取当前政治面貌"}
        </ActionButton>
      </div>
    </section>
  );
}

function Aside() {
  return (
    <aside className="affiliation-aside" aria-labelledby="aside-heading">
      <div className="aside-rule" aria-hidden="true" />
      <h2 id="aside-heading">填写前请确认</h2>
      <p>这是可选资料，未填写不影响员工服务和工牌。</p>
      <p>
        未填写不等同于选择“群众”。请按本人情况选择，不确定时可暂不填写。
      </p>
    </aside>
  );
}

type Leave = { href: string; mode: "draft" | "pending" };

function leaveMode(snapshot: AffiliationSnapshot): "" | "draft" | "pending" {
  if (snapshot.kind !== "ready") return "";
  const panel = snapshot.panel;
  if (
    [
      "submitting",
      "retry-submitting",
      "unknown",
      "submit-limited",
      "submit-invalid",
      "submit-conflict",
      "submit-mismatch",
      "submit-failed",
      "retry-wait",
      "retry-mismatch",
      "retry-conflict",
      "retry-stopped",
    ].includes(panel.kind)
  )
    return "pending";
  if (panel.kind === "submit-storage") return panel.retry ? "pending" : "draft";
  if (
    ["form", "confirm"].includes(panel.kind) &&
    "choice" in panel &&
    panel.choice
  )
    return "draft";
  return "";
}

function maskedCopy(snapshot: Exclude<AffiliationSnapshot, { kind: "ready" }>) {
  switch (snapshot.kind) {
    case "identity-loading":
      return {
        title: "正在读取当前账户",
        copy: "确认完成后，再显示本人资料。",
        description: "正在确认当前账户。",
      };
    case "identity-anonymous":
      return {
        title: "请先登录",
        copy: "登录后，再查看个人账户。",
        description: "登录后，再查看个人账户。",
      };
    case "identity-expired":
      return {
        title: "请重新登录",
        copy: "当前登录已失效。",
        description: "当前登录已失效。",
      };
    case "identity-denied":
      return {
        title: "暂时无法继续访问",
        copy: "本次访问暂时未能继续，请从个人账户重新进入。如仍无法打开，请稍后再试。",
        description: "请从个人账户重新进入。",
      };
    case "identity-storage":
      return {
        title: "暂时无法检查登录状态",
        copy: "浏览器无法读取登录信息。请先重试，仍无法确认时可在新标签页重新登录。",
        description: "当前账户暂时无法读取。",
      };
    case "identity-reload":
      return {
        title: "请重新加载页面",
        copy: "暂时无法检查登录状态，请重新加载页面。",
        description: "当前页面需要重新加载。",
      };
    default:
      return {
        title: "暂时无法读取当前账户",
        copy: "请稍后重新检查。暂时无法读取不代表登录已失效。",
        description: "当前账户暂时无法读取。",
      };
  }
}

function description(panel: AffiliationPanel): string {
  switch (panel.kind) {
    case "reading":
      return "正在读取当前政治面貌。";
    case "read-error":
      return "当前政治面貌暂时无法读取。";
    case "record":
      return "查看当前记录。已有记录后，本人不能自行更正或清空。";
    case "unset":
      return "这是一项可选资料。未填写不影响员工服务和工牌。";
    case "dictionary-error":
      return "当前尚未填写，填写选项暂时无法读取。";
    case "value-dictionary-error":
      return "当前政治面貌暂时无法完整显示。";
    case "marker-unavailable":
      return "浏览器存储不可用，本页暂时无法安全开始填写。";
    case "form":
      return "请选择与你本人一致的一项。提交后，本人不能自行修改或清空。";
    case "confirm":
      return "请核对本人和所选内容，再明确提交。";
    case "submitting":
      return "填写正在提交，请稍候。";
    case "submit-limited":
      return "本次填写尚未受理，请稍后再试。";
    case "submit-conflict":
      return "当前已有记录，不能再次填写。";
    case "submit-invalid":
      return "本次填写未提交成功，请重新核对。";
    case "submit-failed":
      return "本次填写未完成，请稍后再试。";
    case "submit-mismatch":
      return "本次填写无法继续。";
    case "submit-storage":
      return panel.uncertain
        ? "本次重试尚未发送，此前填写结果仍未确认。"
        : "本次提交尚未发送，请检查浏览器存储后重试。";
    case "success":
      return "本次填写已确认完成。";
    case "unknown":
      return "本次填写结果尚未确认。";
    case "retry-submitting":
      return "正在重试本次填写。";
    case "retry-wait":
      return "本次重试未被受理，此前填写结果仍未确认。";
    case "retry-mismatch":
      return "本次重试无法继续，此前填写结果仍未确认。";
    case "retry-conflict":
      return "当前已有政治面貌记录，此前填写结果仍未确认。";
    case "retry-stopped":
      return "本次重试未被受理，此前填写结果仍未确认。";
    case "current-unset":
      return "这次读取没有找到政治面貌记录。";
  }
}

function identityHeading(snapshot: AffiliationSnapshot | undefined) {
  if (!snapshot)
    return {
      title: "个人账户",
      kicker: "个人账户",
      documentTitle: "个人账户 · 司籍处",
      description: "正在打开个人账户。",
    };
  if (snapshot.kind !== "ready")
    return {
      title: "个人账户",
      kicker: "个人账户",
      documentTitle: "个人账户 · 司籍处",
      description: maskedCopy(snapshot).description,
    };
  return {
    title: "政治面貌",
    kicker: "个人账户 · 本人资料",
    documentTitle: "政治面貌 · 司籍处",
    description: description(snapshot.panel),
  };
}

function PanelView({
  affiliation,
  snapshot,
  now,
  onNavigate,
}: {
  affiliation: Affiliation;
  snapshot: Extract<AffiliationSnapshot, { kind: "ready" }>;
  now: number;
  onNavigate: (event: MouseEvent<HTMLAnchorElement>) => void;
}) {
  const { panel, person, options, current: read } = snapshot;
  const readDisabled = read.kind === "loading" ||
    ((read.kind === "error" || read.kind === "dictionary-error") && read.retryAt > now);
  const account = (
    <LinkButton href="/account" secondary onClick={onNavigate}>
      返回个人账户
    </LinkButton>
  );
  const home = (
    <LinkButton href="/" secondary onClick={onNavigate}>
      返回首页
    </LinkButton>
  );
  const pending = (code?: AffiliationCode) => (
    <PendingSummary options={options} code={code} />
  );
  const name = (code: AffiliationCode) => optionLabel(options, code);

  switch (panel.kind) {
    case "reading":
      return (
        <StatePanel
          busy
          title="正在读取当前政治面貌"
          copy="读取完成前，不显示旧值，也不判断是否未填写。"
          person={person}
        />
      );
    case "read-error": {
      const remaining = secondsUntil(panel.retryAt, now);
      return (
        <StatePanel
          kicker="当前记录"
          title="暂时无法读取当前政治面貌"
          copy="请稍后重新读取。读取失败不代表未填写。"
          person={person}
          actions={
            <>
              <ActionButton
                onClick={() => affiliation.retryRead()}
                disabled={remaining > 0}
              >
                {remaining > 0
                  ? `${remaining} 秒后重新读取`
                  : "重新读取"}
              </ActionButton>
              {account}
            </>
          }
        />
      );
    }
    case "record":
      return (
        <RecordPanel
          value={panel.value}
          name={panel.name}
          person={person}
        />
      );
    case "unset":
      return (
        <section className="affiliation-panel state-panel">
          <p className="panel-kicker">本人资料</p>
          <h2 className="state-title" data-focus tabIndex={-1}>
            尚未填写政治面貌
          </h2>
          {personContext(person)}
          <p className="state-copy">
            这是可选资料。未填写不影响员工服务和工牌。
          </p>
          <p className="page-note">
            你可以自愿填写。提交后，本人不能自行修改、撤回或清空。
          </p>
          <div className="panel-actions">
            <ActionButton onClick={() => affiliation.startForm()}>
              填写政治面貌
            </ActionButton>
          </div>
        </section>
      );
    case "dictionary-error": {
      const remaining = secondsUntil(panel.retryAt, now);
      return (
        <StatePanel
          kicker="尚未填写"
          title="暂时无法读取填写选项"
          copy="请稍后重新读取。选项尚未取得时不开放填写。"
          person={person}
          actions={
            <>
              <ActionButton
                onClick={() => affiliation.retryRead()}
                disabled={remaining > 0}
              >
                {remaining > 0
                  ? `${remaining} 秒后重新读取`
                  : "重新读取选项"}
              </ActionButton>
              {account}
            </>
          }
        />
      );
    }
    case "value-dictionary-error": {
      const remaining = secondsUntil(panel.retryAt, now);
      return (
        <StatePanel
          kicker="当前记录"
          title="当前政治面貌暂时无法显示"
          copy="当前政治面貌记录已读取，但完整名称暂未取得。请稍后重新读取。"
          person={person}
          actions={
            <>
              <ActionButton
                onClick={() => affiliation.retryRead()}
                disabled={remaining > 0}
              >
                {remaining > 0 ? `${remaining} 秒后重新读取` : "重新读取"}
              </ActionButton>
              {account}
            </>
          }
        />
      );
    }
    case "marker-unavailable":
      return (
        <>
          <StatePanel
            kicker="结果状态"
            title="暂时无法确认本页状态"
            copy="浏览器存储不可用，本页无法安全开始填写。你仍可查看当前记录。"
            person={person}
            actions={
              <>
                <ActionButton disabled={readDisabled} onClick={() => affiliation.viewCurrent()}>
                  查看当前政治面貌
                </ActionButton>
                {account}
              </>
            }
          />
          <CurrentReadPanel
            read={read}
            context="unknown"
            now={now}
            onRetry={() => affiliation.retryRead()}
          />
        </>
      );
    case "form":
      return (
        <section className="affiliation-panel form-panel">
          <p className="panel-kicker">首次填写</p>
          <h2 className="state-title" data-focus tabIndex={-1}>
            填写政治面貌
          </h2>
          {personContext(person)}
          <p className="state-copy">
            请选择与你本人一致的一项。提交后，本人不能自行修改或清空。
          </p>
          <form
            className="affiliation-form"
            autoComplete="off"
            onSubmit={(event) => {
              event.preventDefault();
              affiliation.continueToConfirm();
            }}
          >
            <fieldset aria-describedby="option-hint">
              <legend>选择一项（共 13 项）</legend>
              <p className="option-hint" id="option-hint">
                未选择时不能继续。
              </p>
              <div className="affiliation-options">
                {options.map((option) => (
                  <label className="affiliation-option" key={option.value}>
                    <input
                      id={`affiliation-${option.value}`}
                      type="radio"
                      name="affiliation"
                      value={option.value}
                      checked={panel.choice === option.value}
                      onChange={() => affiliation.choose(option.value)}
                    />
                    <span className="affiliation-option-text">
                      <span className="option-code" aria-hidden="true">
                        {option.value}
                      </span>
                      <span className="option-name">{option.name}</span>
                    </span>
                  </label>
                ))}
              </div>
            </fieldset>
            <p
              className="selection-status"
              id="selection-status"
              aria-live="polite"
            >
              {panel.choice ? (
                <>
                  已选择：<strong>{name(panel.choice)}</strong>
                </>
              ) : (
                "尚未选择"
              )}
            </p>
            <div className="form-actions">
              <button
                type="submit"
                className="button primary"
                disabled={!panel.choice}
              >
                继续
              </button>
              {account}
            </div>
          </form>
        </section>
      );
    case "confirm":
      return (
        <section className="affiliation-panel confirm-panel">
          <p className="panel-kicker">确认填写</p>
          <h2 className="state-title" data-focus tabIndex={-1}>
            确认填写政治面貌
          </h2>
          {personContext(person)}
          <p className="state-copy">请核对本人和所选内容，再明确提交。</p>
          <dl className="confirm-summary">
            <div className="confirm-row">
              <dt>本人</dt>
              <dd>{person}</dd>
            </div>
            <div className="confirm-row">
              <dt>将填写</dt>
              <dd className="confirm-choice">{name(panel.choice)}</dd>
            </div>
          </dl>
          <div className="confirm-warning">
            <TriangleAlert aria-hidden="true" />
            <span>提交后，你不能自行修改、撤回或清空。</span>
          </div>
          <div className="panel-actions">
            <ActionButton onClick={() => void affiliation.submit()}>
              确认填写
            </ActionButton>
            <ActionButton secondary onClick={() => affiliation.backToForm()}>
              返回选择
            </ActionButton>
          </div>
        </section>
      );
    case "submitting":
    case "retry-submitting":
      return (
        <StatePanel
          busy
          className="submitting-panel"
          kicker={panel.kind === "submitting" ? "请稍候" : "结果尚未确认"}
          title={
            panel.kind === "submitting"
              ? "正在提交填写"
              : "正在重试本次填写"
          }
          copy={
            panel.kind === "submitting"
              ? "暂时不用重复操作。"
              : "请稍候，暂时不用重复操作。重试会按原选项再次提交。"
          }
          person={person}
          extra={pending(panel.choice)}
        />
      );
    case "submit-storage":
      return (
        <StatePanel
          kicker={panel.uncertain ? "此前填写结果尚未确认" : "本次提交尚未发送"}
          title="暂时无法准备本次提交"
          copy={panel.uncertain
            ? "浏览器未能保存本次重试所需的标记，重试尚未发出。此前填写可能已经完成；再次重试仍可能完成原填写。"
            : "浏览器未能保存本次提交所需的标记，这次提交尚未发送。请检查浏览器存储设置后重试。"}
          person={person}
          extra={pending(panel.choice)}
          actions={
            <>
              {panel.retry ? (
                <ActionButton onClick={() => void affiliation.retrySubmit()}>
                  重试本次填写
                </ActionButton>
              ) : (
                <ActionButton onClick={() => affiliation.reconfirm()}>
                  返回确认
                </ActionButton>
              )}
              {account}
            </>
          }
        />
      );
    case "submit-limited": {
      const remaining = secondsUntil(panel.retryAt, now);
      return (
        <StatePanel
          kicker="本次填写尚未受理"
          title="请稍后再试"
          copy="本次提交未被受理，尚未收到填写完成的结果。此页会保留你的选择，等待期间不会自动提交。"
          person={person}
          extra={pending(panel.choice)}
          actions={
            <>
              <ActionButton
                onClick={() => void affiliation.retrySubmit()}
                disabled={remaining > 0}
              >
                {remaining > 0
                  ? `${remaining} 秒后可重新提交`
                  : "重试本次填写"}
              </ActionButton>
              {account}
            </>
          }
        />
      );
    }
    case "submit-conflict":
      return (
        <StatePanel
          kicker="不能再次填写"
          title="当前已有政治面貌记录"
          copy="本次填写未提交成功。你可以查看当前记录，不能再次填写。"
          person={person}
          actions={
            <>
              <ActionButton disabled={readDisabled} onClick={() => affiliation.viewCurrent()}>
                查看当前政治面貌
              </ActionButton>
              {account}
            </>
          }
        />
      );
    case "submit-invalid":
      return (
        <StatePanel
          kicker="本次填写未提交成功"
          title="请重新核对所选内容"
          copy="本次提交的内容未能通过检查。请返回选择后重新确认。"
          person={person}
          extra={pending(panel.choice)}
          actions={
            <>
              <ActionButton onClick={() => affiliation.backToForm()}>
                返回选择
              </ActionButton>
              {account}
            </>
          }
        />
      );
    case "submit-failed": {
      const remaining = secondsUntil(panel.retryAt, now);
      return (
        <StatePanel
          kicker="本次填写未完成"
          title="暂时无法完成填写"
          copy="本次填写未完成。服务恢复后，你可以按本次选择重试。"
          person={person}
          extra={pending(panel.choice)}
          actions={
            <>
              <ActionButton
                onClick={() => void affiliation.retrySubmit()}
                disabled={remaining > 0}
              >
                {remaining > 0
                  ? `${remaining} 秒后可重试`
                  : "重试本次填写"}
              </ActionButton>
              {account}
            </>
          }
        />
      );
    }
    case "submit-mismatch":
      return (
        <StatePanel
          kicker="本次填写未提交成功"
          title="本次填写无法继续"
          copy="当前无法把本次提交与原先操作对应，已停止提交。本次填写未完成。"
          person={person}
          extra={pending(panel.choice)}
          actions={
            <>
              <ActionButton disabled={readDisabled} onClick={() => affiliation.viewCurrent()}>
                查看当前政治面貌
              </ActionButton>
              {account}
            </>
          }
        />
      );
    case "success": {
      const showAction = read.kind === "idle";
      return (
        <>
          <section className="affiliation-panel success-panel">
            <span className="success-mark" aria-hidden="true">
              <Check />
            </span>
            <p className="panel-kicker success-kicker">已确认完成</p>
            <h2 className="state-title" data-focus tabIndex={-1}>
              政治面貌已填写
            </h2>
            {personContext(person)}
            <p className="state-copy">
              {showAction
                ? "本次填写已完成。需要查看当前记录时，可以继续读取。"
                : "本次填写已完成。"}
            </p>
            <div className="panel-actions">
              {showAction && (
                <ActionButton disabled={readDisabled} onClick={() => affiliation.viewCurrent()}>
                  查看当前政治面貌
                </ActionButton>
              )}
              {account}
            </div>
          </section>
          <CurrentReadPanel
            read={read}
            context="success"
            now={now}
            onRetry={() => affiliation.retryRead()}
          />
        </>
      );
    }
    case "unknown": {
      const canRetry = panel.canRetry;
      const remaining = panel.canRetry ? secondsUntil(panel.retryAt, now) : 0;
      return (
        <>
          <section className="affiliation-panel unknown-panel">
            <p className="panel-kicker">结果尚未确认</p>
            <h2 className="state-title" data-focus tabIndex={-1}>
              暂时无法确认填写结果
            </h2>
            {personContext(person)}
            <p className="state-copy">
              本次填写可能已经完成。离开本页不会取消已经发出的填写。
            </p>
            {pending(panel.canRetry ? panel.choice : undefined)}
            {canRetry ? (
              <p className="retry-note">
                <strong>重试本次填写</strong>
                会再次提交上面的选项，可能完成本次填写。请不要另选一项。
              </p>
            ) : (
              <p className="retry-note">
                本页已无法重试原填写。你仍可查看当前记录。
              </p>
            )}
            <p className="page-note">
              查看当前政治面貌只读取当前记录，不能证明本次填写是否完成。
            </p>
            <div className="panel-actions">
              {canRetry && (
                <ActionButton disabled={remaining > 0} onClick={() => void affiliation.retrySubmit()}>
                  {remaining > 0 ? `${remaining} 秒后可重试本次填写` : "重试本次填写"}
                </ActionButton>
              )}
              <ActionButton
                onClick={() => affiliation.viewCurrent()}
                disabled={readDisabled}
                secondary={!canRetry}
              >
                查看当前政治面貌
              </ActionButton>
              {account}
            </div>
          </section>
          <CurrentReadPanel
            read={read}
            context="unknown"
            now={now}
            onRetry={() => affiliation.retryRead()}
          />
        </>
      );
    }
    case "retry-wait": {
      const remaining = secondsUntil(panel.retryAt, now);
      return (
        <StatePanel
          kicker="结果尚未确认"
          title="暂时无法重试本次填写"
          copy="本次重试未被受理，请稍后再试。此前填写结果仍未确认。"
          person={person}
          extra={
            <>
              {pending(panel.choice)}
              <p className="retry-note">
                未获受理不等于原填写失败；等待期间不会自动重试。
              </p>
            </>
          }
          actions={
            <>
              <ActionButton
                onClick={() => void affiliation.retrySubmit()}
                disabled={remaining > 0}
              >
                {remaining > 0
                  ? `${remaining} 秒后可重试本次填写`
                  : "重试本次填写"}
              </ActionButton>
              {account}
            </>
          }
        />
      );
    }
    case "retry-mismatch":
    case "retry-conflict":
    case "retry-stopped": {
      const mismatch = panel.kind === "retry-mismatch";
      const conflict = panel.kind === "retry-conflict";
      return (
        <>
          <StatePanel
            kicker="结果尚未确认"
            title={
              mismatch
                ? "本次重试无法继续"
                : conflict
                  ? "当前已有政治面貌记录"
                  : "本次重试未被受理"
            }
            copy={
              mismatch
                ? "当前无法把本次重试与原先填写对应，已停止重试。此前填写结果仍未确认。"
                : conflict
                  ? "本次重试未被受理。此前填写结果仍未确认。"
                  : "本次重试未被受理，已停止重试。此前填写结果仍未确认。"
            }
            person={person}
            extra={
              <>
                {pending(panel.choice)}
                <p className="retry-note">
                  {conflict
                    ? "当前记录需要另行读取，不能用它判断原填写是否完成。"
                    : "不能改选其他选项来代替本次填写。"}
                </p>
              </>
            }
            actions={
              <>
                <ActionButton disabled={readDisabled} onClick={() => affiliation.viewCurrent()}>
                  查看当前政治面貌
                </ActionButton>
                {account}
              </>
            }
          />
          <CurrentReadPanel
            read={read}
            context="unknown"
            now={now}
            onRetry={() => affiliation.retryRead()}
          />
        </>
      );
    }
    case "current-unset":
      return (
        <StatePanel
          title="当前尚未填写"
          copy="这次读取没有找到政治面貌记录。"
          person={person}
          extra={
            <p className="page-note">
              这次读取没有找到记录，但不能证明原填写未完成，也不会在本页开始新的填写。
            </p>
          }
          actions={account}
        />
      );
    default:
      return home;
  }
}

function MaskedPanel({
  snapshot,
  affiliation,
  onNavigate,
  reloadPage,
}: {
  snapshot: Exclude<AffiliationSnapshot, { kind: "ready" }>;
  affiliation: Affiliation;
  onNavigate: (event: MouseEvent<HTMLAnchorElement>) => void;
  reloadPage: () => void;
}) {
  const copy = maskedCopy(snapshot);
  const login = (
    <LinkButton
      href="/authentication/codes"
      target="_blank"
      rel="noopener"
      onClick={() => {}}
    >
      手机号登录
    </LinkButton>
  );
  const home = (
    <LinkButton href="/" secondary onClick={onNavigate}>
      返回首页
    </LinkButton>
  );
  const account = (
    <LinkButton href="/account" onClick={onNavigate}>
      打开个人账户
    </LinkButton>
  );
  const retryAt =
    "retryAt" in snapshot ? secondsUntil(snapshot.retryAt, Date.now()) : 0;
  return (
    <StatePanel
      busy={snapshot.kind === "identity-loading"}
      className="identity-panel"
      title={copy.title}
      copy={copy.copy}
      actions={
        <>
          {snapshot.kind === "identity-reload" && (
            <ActionButton onClick={reloadPage}>重新加载页面</ActionButton>
          )}
          {(snapshot.kind === "identity-storage" ||
            snapshot.kind === "identity-unavailable" ||
            snapshot.kind === "identity-limited") && (
            <ActionButton
              onClick={() => void affiliation.recheck()}
              disabled={retryAt > 0}
            >
              {retryAt > 0 ? `${retryAt} 秒后重新检查` : "重新检查"}
            </ActionButton>
          )}
          {(snapshot.kind === "identity-anonymous" ||
            snapshot.kind === "identity-expired" ||
            snapshot.kind === "identity-storage") &&
            login}
          {snapshot.kind === "identity-denied" && account}
          {home}
        </>
      }
    />
  );
}

function LeaveDialog({
  leave,
  stayRef,
  triggerRef,
  onClose,
  onLeave,
}: {
  leave: Leave | undefined;
  stayRef: RefObject<HTMLButtonElement | null>;
  triggerRef: RefObject<HTMLElement | null>;
  onClose: () => void;
  onLeave: () => void;
}) {
  return (
    <Dialog.Root
      open={leave !== undefined}
      onOpenChange={(open) => {
        if (!open) onClose();
      }}
    >
      <Dialog.Portal>
        <Dialog.Backdrop className="leave-backdrop" />
        <Dialog.Popup
          className="leave-dialog"
          initialFocus={stayRef}
          finalFocus={() => triggerRef.current}
        >
          <div className="dialog-heading">
            <span>离开此页</span>
            <Dialog.Close className="dialog-close" aria-label="关闭离开提示">
              <X aria-hidden="true" />
            </Dialog.Close>
          </div>
          <Dialog.Title>
            {leave?.mode === "draft"
              ? "放弃当前选择？"
              : "离开前，请留意填写结果"}
          </Dialog.Title>
          <Dialog.Description>
            {leave?.mode === "draft"
              ? "当前选择尚未提交。离开后需要重新选择。"
              : "离开不会取消已经发出的填写。返回后无法用本页原输入重试。"}
          </Dialog.Description>
          <div className="dialog-actions">
            <Dialog.Close ref={stayRef} className="button primary">
              留在此页
            </Dialog.Close>
            <button
              type="button"
              className="button secondary"
              onClick={onLeave}
            >
              离开此页
            </button>
          </div>
        </Dialog.Popup>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

export default function AffiliationPage() {
  const controllers = useController();
  const [now, setNow] = useState(() => Date.now());
  const [leave, setLeave] = useState<Leave>();
  const content = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLElement | null>(null);
  const stay = useRef<HTMLButtonElement | null>(null);
  const leaveAllowed = useRef(false);
  const leaveOpen = useRef(false);
  leaveOpen.current = leave !== undefined;
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 1_000);
    return () => clearInterval(timer);
  }, []);
  const snapshot = useSyncExternalStore(
    controllers?.affiliation.subscribe ?? noSubscribe,
    controllers?.affiliation.snapshot ?? noSnapshot,
    controllers?.affiliation.snapshot ?? noSnapshot,
  );
  const departure = snapshot ? leaveMode(snapshot) : "";
  useEffect(() => {
    leaveAllowed.current = false;
    if (!departure) return;
    const before = (event: BeforeUnloadEvent) => {
      if (leaveAllowed.current) return;
      event.preventDefault();
      event.returnValue = "";
    };
    window.addEventListener("beforeunload", before);
    return () => window.removeEventListener("beforeunload", before);
  }, [departure]);
  const heading = identityHeading(snapshot);
  useEffect(() => {
    document.title = heading.documentTitle;
  }, [heading.documentTitle]);
  const identityKey = snapshot
    ? snapshot.kind === "ready"
      ? `ready:${snapshot.person}`
      : snapshot.kind
    : "loading";
  useEffect(() => {
    // An identity change must not leave an old pending dialog over new state.
    setLeave(undefined);
  }, [identityKey]);
  const focusKey = snapshot
    ? snapshot.kind === "ready"
      ? `${snapshot.panel.kind}:${snapshot.current.kind}`
      : snapshot.kind
    : "loading";
  useEffect(() => {
    if (leaveOpen.current) return;
    const frame = requestAnimationFrame(() => {
      content.current?.querySelector<HTMLElement>("[data-focus]")?.focus();
    });
    return () => cancelAnimationFrame(frame);
  }, [focusKey]);

  const navigate = (event: MouseEvent<HTMLAnchorElement>) => {
    if (
      event.defaultPrevented ||
      event.button !== 0 ||
      event.metaKey ||
      event.ctrlKey ||
      event.shiftKey ||
      event.altKey
    )
      return;
    if (!snapshot || !leaveMode(snapshot)) return;
    event.preventDefault();
    trigger.current = event.currentTarget;
    setLeave({
      href: event.currentTarget.getAttribute("href") ?? "/account",
      mode: leaveMode(snapshot) as "draft" | "pending",
    });
  };

  const navigation = (
    <nav className="affiliation-navigation" aria-label="网站导航">
      <a href="/workforce" onClick={navigate}>
        员工服务
      </a>
      <a href="/account" onClick={navigate}>
        个人账户
      </a>
      <a className="public-return" href="/" onClick={navigate}>
        返回首页 ↗
      </a>
    </nav>
  );

  const aside =
    snapshot?.kind === "ready" &&
    ["form", "confirm", "submitting", "submit-limited"].includes(
      snapshot.panel.kind,
    );

  return (
    <>
      <Header navigation={navigation} onBrandClick={navigate} />
      <main className="affiliation-layout" id="main" tabIndex={-1}>
        <div className="page-heading">
          <div className="page-heading-copy">
            <p className="section-kicker">{heading.kicker}</p>
            <h1 id="page-title" tabIndex={-1}>
              {heading.title}
            </h1>
            <p className="page-introduction">{heading.description}</p>
          </div>
          <a className="back-link" href="/account" onClick={navigate}>
            ← 返回个人账户
          </a>
        </div>
        <div id="affiliation-content" aria-live="polite" ref={content}>
          {!controllers || !snapshot ? (
            <section className="affiliation-panel state-panel" aria-busy="true">
              <span className="reading-symbol" aria-hidden="true" />
              <h2 className="state-title" data-focus tabIndex={-1}>
                正在打开页面
              </h2>
              <p className="state-copy">正在确认当前账户。</p>
            </section>
          ) : snapshot.kind === "ready" ? (
            <div
              className={`affiliation-columns${aside ? "" : " single"}`}
            >
              <div className="affiliation-main">
                <PanelView
                  affiliation={controllers.affiliation}
                  snapshot={snapshot}
                  now={now}
                  onNavigate={navigate}
                />
              </div>
              {aside && <Aside />}
            </div>
          ) : (
            <div className="affiliation-columns single">
              <div className="affiliation-main">
                <MaskedPanel
                  snapshot={snapshot}
                  affiliation={controllers.affiliation}
                  onNavigate={navigate}
                  reloadPage={() => location.reload()}
                />
              </div>
            </div>
          )}
        </div>
        <noscript>
          <p className="inline-notice">请开启 JavaScript 查看本人政治面貌。</p>
        </noscript>
      </main>
      <footer className="affiliation-footer">
        <span>司籍处 · 个人账户</span>
      </footer>
      <LeaveDialog
        leave={leave}
        stayRef={stay}
        triggerRef={trigger}
        onClose={() => setLeave(undefined)}
        onLeave={() => {
          const href = leave?.href;
          setLeave(undefined);
          if (href) {
            leaveAllowed.current = true;
            location.assign(href);
          }
        }}
      />
    </>
  );
}
