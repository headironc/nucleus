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
  type AffiliationRead,
  type AffiliationSnapshot,
} from "../affiliation/controller";
import { clearLegacyMarker } from "../affiliation/marker";
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
    clearLegacyMarker(
      () => window.sessionStorage,
      () => window.history,
    );
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
  return option?.name ?? "";
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
  title,
  copy,
  person,
  extra,
  actions,
}: {
  busy?: boolean;
  className?: string;
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
  person,
  current,
}: {
  value: AffiliationValue;
  name: string;
  person?: string;
  current?: boolean;
}) {
  return (
    <section
      className={`affiliation-panel ${current ? "current-panel " : ""}record-panel`}
    >
      <h2 className="state-title" data-focus tabIndex={-1}>
        当前政治面貌
      </h2>
      {person && personContext(person)}
      <div className="record-display">
        <strong className="record-name">{name}</strong>
      </div>
      <dl className="record-meta">
        <dt>记录时间</dt>
        <dd>
          <time dateTime={value.createdAt}>{beijing(value.createdAt)}</time>
        </dd>
      </dl>
      <p className="record-note">你不能自行修改、撤回或清空。</p>
    </section>
  );
}

function CurrentReadPanel({
  read,
  now,
  onRetry,
}: {
  read: AffiliationRead;
  now: number;
  onRetry: () => void;
}) {
  if (read.kind === "idle") return null;
  if (read.kind === "loading")
    return (
      <section
        className="affiliation-panel current-panel state-panel"
        aria-busy="true"
      >
        <span className="reading-symbol" aria-hidden="true" />
        <h2 className="state-title" data-focus tabIndex={-1}>
          正在加载当前记录…
        </h2>
        <p className="state-copy">请稍候。</p>
      </section>
    );
  if (read.kind === "record")
    return (
      <RecordPanel
        value={read.value}
        name={read.name}
        person={undefined}
        current
      />
    );
  if (read.kind === "unset")
    return (
      <section className="affiliation-panel current-panel state-panel">
        <h2 className="state-title" data-focus tabIndex={-1}>
          暂时未取得当前记录
        </h2>
      </section>
    );
  const remaining = secondsUntil(read.retryAt, now);
  return (
    <section className="affiliation-panel current-panel state-panel">
      <h2 className="state-title" data-focus tabIndex={-1}>
        {read.kind === "dictionary-error"
          ? "当前记录暂时无法显示"
          : "当前记录暂时无法加载"}
      </h2>
      <p className="state-copy">
        {read.kind === "dictionary-error"
          ? "完整名称暂时无法加载，请重试。"
          : "请重试。"}
      </p>
      <div className="panel-actions">
        <ActionButton onClick={onRetry} disabled={remaining > 0}>
          {remaining > 0 ? `${remaining} 秒后可刷新` : "刷新当前记录"}
        </ActionButton>
      </div>
    </section>
  );
}

type Leave = { href: string; mode: "draft" | "pending" };

function leaveMode(snapshot: AffiliationSnapshot): "" | "draft" | "pending" {
  if (snapshot.kind !== "ready") return "";
  const panel = snapshot.panel;
  if (panel.kind === "submitting") return "pending";
  if (
    ["form", "confirm", "submit-invalid"].includes(panel.kind) &&
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
        title: "正在检查登录状态…",
        copy: "请稍候。",
      };
    case "identity-anonymous":
      return {
        title: "请登录后继续",
        copy: "登录后可查看政治面貌。",
      };
    case "identity-expired":
      return {
        title: "请重新登录",
        copy: "登录后可查看政治面貌。",
      };
    case "identity-denied":
      return {
        title: "目前无法查看政治面貌",
        copy: "可返回个人账户或首页。",
      };
    case "identity-storage":
      return {
        title: "登录信息暂时无法使用",
        copy: "请重新检查，或使用手机号登录。",
      };
    case "identity-reload":
      return {
        title: "请重新加载",
        copy: "重新加载后，请按页面提示继续。",
      };
    default:
      return {
        title: "登录状态暂时无法检查",
        copy: "请稍后重试。",
      };
  }
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
  const readDisabled =
    read.kind === "loading" ||
    ((read.kind === "error" || read.kind === "dictionary-error") &&
      read.retryAt > now);
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
          title="正在加载政治面貌…"
          copy="请稍候。"
          person={person}
        />
      );
    case "read-error": {
      const remaining = secondsUntil(panel.retryAt, now);
      return (
        <StatePanel
          title="政治面貌暂时无法加载"
          copy="请重试。"
          person={person}
          actions={
            <>
              <ActionButton
                onClick={() => affiliation.retryRead()}
                disabled={remaining > 0}
              >
                {remaining > 0 ? `${remaining} 秒后可刷新` : "刷新"}
              </ActionButton>
              {account}
            </>
          }
        />
      );
    }
    case "record":
      return (
        <RecordPanel value={panel.value} name={panel.name} person={person} />
      );
    case "unset":
      return (
        <section className="affiliation-panel state-panel">
          <h2 className="state-title" data-focus tabIndex={-1}>
            尚未填写政治面貌
          </h2>
          {personContext(person)}
          <p className="state-copy">
            自愿填写，不影响员工服务和工牌。
          </p>
          <p className="page-note">
            提交后，你不能自行修改、撤回或清空。
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
          title="填写选项暂时无法加载"
          copy="请重试。"
          person={person}
          actions={
            <>
              <ActionButton
                onClick={() => affiliation.retryRead()}
                disabled={remaining > 0}
              >
                {remaining > 0 ? `${remaining} 秒后可刷新` : "刷新"}
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
          title="当前记录暂时无法显示"
          copy="完整名称暂时无法加载，请重试。"
          person={person}
          actions={
            <>
              <ActionButton
                onClick={() => affiliation.retryRead()}
                disabled={remaining > 0}
              >
                {remaining > 0 ? `${remaining} 秒后可刷新` : "刷新"}
              </ActionButton>
              {account}
            </>
          }
        />
      );
    }
    case "form":
      return (
        <section className="affiliation-panel form-panel">
          <h2 className="state-title" data-focus tabIndex={-1}>
            填写政治面貌
          </h2>
          {personContext(person)}
          <p className="state-copy">
            自愿填写，不影响员工服务和工牌。
          </p>
          <form
            className="affiliation-form"
            autoComplete="off"
            onSubmit={(event) => {
              event.preventDefault();
              affiliation.continueToConfirm();
            }}
          >
            <fieldset>
              <legend>请选择一项</legend>
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
                      <span className="option-name">{option.name}</span>
                    </span>
                  </label>
                ))}
              </div>
            </fieldset>
            <div className="form-actions">
              <button
                type="submit"
                className="button primary"
                disabled={!panel.choice}
              >
                下一步
              </button>
              {account}
            </div>
          </form>
        </section>
      );
    case "confirm":
      return (
        <section className="affiliation-panel confirm-panel">
          <h2 className="state-title" data-focus tabIndex={-1}>
            确认填写政治面貌
          </h2>
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
      return (
        <StatePanel
          busy
          className="submitting-panel"
          title="正在提交…"
          copy="请稍候。"
          person={person}
          extra={pending(panel.choice)}
        />
      );
    case "submit-limited": {
      const remaining = secondsUntil(panel.retryAt, now);
      return (
        <StatePanel
          title="本次填写未受理"
          copy="请稍后刷新查看。"
          person={person}
          actions={
            <>
              <ActionButton
                onClick={() => affiliation.viewCurrent()}
                disabled={remaining > 0}
              >
                {remaining > 0 ? `${remaining} 秒后可刷新` : "刷新"}
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
          title="当前已有政治面貌记录"
          copy="可查看当前记录。"
          person={person}
          actions={
            <>
              <ActionButton
                disabled={readDisabled}
                onClick={() => affiliation.viewCurrent()}
              >
                查看当前记录
              </ActionButton>
              {account}
            </>
          }
        />
      );
    case "submit-invalid":
      return (
        <StatePanel
          title="填写内容未通过检查"
          copy="请返回选择后重新确认。"
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
          title="本次填写未完成"
          copy="请稍后刷新查看。"
          person={person}
          actions={
            <>
              <ActionButton
                onClick={() => affiliation.viewCurrent()}
                disabled={remaining > 0}
              >
                {remaining > 0 ? `${remaining} 秒后可刷新` : "刷新"}
              </ActionButton>
              {account}
            </>
          }
        />
      );
    }
    case "success": {
      const showAction = read.kind === "idle";
      return (
        <>
          <section className="affiliation-panel success-panel">
            <span className="success-mark" aria-hidden="true">
              <Check />
            </span>
            <h2 className="state-title" data-focus tabIndex={-1}>
              政治面貌已填写
            </h2>
            {personContext(person)}
            <div className="panel-actions">
              {showAction && (
                <ActionButton
                  disabled={readDisabled}
                  onClick={() => affiliation.viewCurrent()}
                >
                  查看当前记录
                </ActionButton>
              )}
              {account}
            </div>
          </section>
          <CurrentReadPanel
            read={read}
            now={now}
            onRetry={() => affiliation.retryRead()}
          />
        </>
      );
    }
    case "unknown": {
      const remaining = secondsUntil(panel.retryAt, now);
      return (
        <StatePanel
          title="未能确认填写结果"
          copy="请先查看当前记录，再按页面提示继续。"
          person={person}
          actions={
            <>
              <ActionButton
                onClick={() => affiliation.viewCurrent()}
                disabled={remaining > 0}
              >
                {remaining > 0
                  ? `${remaining} 秒后可查看当前记录`
                  : "查看当前记录"}
              </ActionButton>
              {account}
            </>
          }
        />
      );
    }
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
            <ActionButton onClick={reloadPage}>重新加载</ActionButton>
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
              : "填写结果尚未确认"}
          </Dialog.Title>
          <Dialog.Description>
            {leave?.mode === "draft"
              ? "当前选择尚未提交。离开后需要重新选择。"
              : "离开不会撤回已发出的请求。返回后请查看当前记录。"}
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
              {leave?.mode === "draft" ? "放弃并离开" : "仍然离开"}
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
  useEffect(() => {
    document.title = "政治面貌 · 司籍处";
  }, []);
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

  return (
    <>
      <Header navigation={navigation} onBrandClick={navigate} />
      <main className="affiliation-layout" id="main" tabIndex={-1}>
        <div className="page-heading">
          <div className="page-heading-copy">
            <p className="section-kicker">个人账户</p>
            <h1 id="page-title" tabIndex={-1}>
              政治面貌
            </h1>
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
                正在打开政治面貌页面…
              </h2>
              <p className="state-copy">请稍候。</p>
            </section>
          ) : snapshot.kind === "ready" ? (
            <div className="affiliation-columns single">
              <div className="affiliation-main">
                <PanelView
                  affiliation={controllers.affiliation}
                  snapshot={snapshot}
                  now={now}
                  onNavigate={navigate}
                />
              </div>
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
