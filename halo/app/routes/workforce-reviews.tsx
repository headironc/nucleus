import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { flushSync } from "react-dom";
import { useParams } from "react-router";
import { Button } from "@base-ui/react/button";
import { Header, Footer } from "../root";
import { Current, type CurrentState } from "../authentication/current";
import { Sessions } from "../authentication/session";
import { current } from "../protocol/session";
import { currentBuild } from "../protocol/build";
import { reloadRequired } from "../integrity";
import {
  Reviews,
  type ReviewMode,
  type ReviewDetail,
} from "../workforce/reviews";
import { ReviewDialog } from "../workforce/review-dialog";
import { CancellationDialog } from "../workforce/cancellation-dialog";
import { BadgePreview, BadgeProduction } from "../workforce/badge-display";
import {
  APPLICATION_KINDS as KINDS,
  APPLICATION_STATUSES as STATUSES,
  applicationDate as date,
  PreviewFigure,
  SnapshotDetails,
  ReviewFacts,
} from "../workforce/application-display";
import type {
  ApplicationStatus,
  ManagedApplicationDetail,
} from "../protocol/applications";
import { summarize } from "../workforce/number";
import { sameApplicationContent } from "../protocol/applications";

export function meta() {
  return [
    { title: "工牌审核 · 司籍处" },
    { name: "robots", content: "noindex" },
  ];
}
type Controllers = { current: Current; reviews: Reviews };
function useReviews(mode: ReviewMode) {
  const [controllers, setControllers] = useState<{
    key: string;
    value: Controllers;
  }>();
  const key = mode.kind === "detail" ? mode.id : "list";
  useEffect(() => {
    const sessions = new Sessions();
    const identity = new Current({
      sessions,
      current,
      build: currentBuild,
      now: Date.now,
    });
    const reviews = new Reviews(mode, {
      current: identity,
      build: currentBuild,
      now: Date.now,
    });
    setControllers({ key, value: { current: identity, reviews } });
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
      reviews.close();
      unwatch();
      document.removeEventListener("visibilitychange", check);
      window.removeEventListener("focus", check);
      window.removeEventListener("pageshow", check);
      window.removeEventListener("pagehide", hide);
      window.removeEventListener("halo:reload-required", reload);
      void sessions.close().catch(() => {});
    };
  }, [key]);
  return controllers?.key === key ? controllers.value : undefined;
}
function identityMessage(state: CurrentState) {
  switch (state.kind) {
    case "anonymous":
      return "请使用手机号登录后查看工牌审核。";
    case "expired":
      return "请重新登录后继续。";
    case "denied":
      return "目前无法查看工牌审核。";
    case "reload":
      return "请重新加载页面后继续。";
    case "limited":
      return "请稍后重新检查登录状态。";
    case "unavailable":
    case "storage":
      return "暂时无法打开工牌审核，请重新检查。";
    default:
      return "正在检查登录状态，请稍候。";
  }
}
function CurrentChanges({ result }: { result: ManagedApplicationDetail }) {
  const fixed = result.application.snapshot,
    value = result.current;
  const changes: {
    label: string;
    before: string;
    after: string;
    disabled?: boolean;
  }[] = [];
  if (fixed.name !== value.name)
    changes.push({ label: "姓名", before: fixed.name, after: value.name });
  if (value.number && !sameApplicationContent(fixed.number, value.number))
    changes.push({
      label: "工号",
      before: `${fixed.printedNumber}（${fixed.number.year} 年）`,
      after: `${summarize(value.number)?.display ?? "暂时无法显示"}（${value.number.year} 年）`,
    });
  for (const [label, before, after] of [
    ["科室", fixed.department, value.department],
    ["职务", fixed.position, value.position],
    ["职称", fixed.title, value.title],
  ] as const) {
    if (before?.name !== after?.name || after?.enabled === false)
      changes.push({
        label,
        before: before?.name ?? "无",
        after: after?.name ?? "无",
        disabled: after?.enabled === false,
      });
  }
  return (
    <section className="review-panel review-current review-panel-content">
      <h2>提交后的资料变化</h2>
      {!value.number && <p>当前没有工号，本申请仍使用提交时的工号。</p>}
      {!changes.length && value.number && <p>当前资料与申请内容一致。</p>}
      {!!changes.length && (
        <>
          <dl className="review-changes">
            {changes.map((item) => (
              <div key={item.label}>
                <dt>{item.label}</dt>
                <dd>
                  提交时：{item.before}
                  <br />
                  当前：{item.after}
                  {item.disabled && "（已停用）"}
                </dd>
              </div>
            ))}
          </dl>
          <p>本次审核以申请内容为准。</p>
        </>
      )}
    </section>
  );
}
function Tag({ status }: { status: ApplicationStatus }) {
  return <span className={`review-tag ${status}`}>{STATUSES[status]}</span>;
}

function Detail({
  state,
  controller,
  now,
}: {
  state: ReviewDetail;
  controller: Reviews;
  now: number;
}) {
  const approveTrigger = useRef<HTMLButtonElement>(null),
    rejectTrigger = useRef<HTMLButtonElement>(null),
    cancelTrigger = useRef<HTMLButtonElement>(null);
  const item = state.result.application;
  const eligible =
    state.result.current.accountActive &&
    state.result.current.participationActive;
  const mediaWait =
    state.media.kind === "failed"
      ? Math.max(0, Math.ceil((state.media.retryAt - now) / 1000))
      : 0;
  return (
    <>
      <div className="review-meta">
        <Tag status={item.status} />
        <span>{KINDS[item.kind]}</span>
        <time dateTime={item.submittedAt}>提交于 {date(item.submittedAt)}</time>
      </div>
      {state.notice && (
        <p className="application-notice" role="status">
          {state.notice}
        </p>
      )}
      <div className="review-columns">
        <div className="badge-content-stack">
          <section className="review-panel">
            <div className="review-panel-heading">
              <h2>申请内容</h2>
            </div>
            <div className="review-panel-content">
              {state.media.kind === "ready" ? (
                <PreviewFigure
                  artwork={state.media.artwork}
                  failed={() => controller.mediaFailed()}
                  caption="申请提交时确认的工牌内容。"
                />
              ) : (
                <div
                  className="review-media"
                  role="status"
                  aria-busy={state.media.kind === "loading"}
                >
                  <p>
                    {state.media.kind === "loading"
                      ? "正在加载工牌预览…"
                      : "工牌预览暂时无法显示，请重新加载。"}
                  </p>
                  {state.media.kind === "failed" && (
                    <Button
                      className="secondary"
                      disabled={state.media.retryAt > now}
                      onClick={() => void controller.load()}
                    >
                      {state.media.retryAt > now
                        ? `${Math.ceil((state.media.retryAt - now) / 1000)} 秒后可重新加载`
                        : "重新加载预览"}
                    </Button>
                  )}
                </div>
              )}
              <SnapshotDetails snapshot={item.snapshot} />
            </div>
          </section>
          {state.badge && (
            <BadgePreview
              media={state.badge}
              retained={item.status === "cancelled"}
              now={now}
              retry={() => void controller.load()}
              failed={() => controller.badgeFailed()}
            />
          )}
        </div>
        <aside className="review-aside" aria-label="当前变化与审核">
          <CurrentChanges result={state.result} />
          <section className="review-panel review-panel-content review-decision">
            <h2>{item.status === "submitted" ? "审核决定" : "审核结果"}</h2>
            {item.status === "submitted" ? (
              <>
                <p id="review-eligibility">
                  {!eligible
                    ? "申请人目前不能继续办理工牌，无法批准。可填写理由驳回申请。"
                    : state.media.kind !== "ready"
                      ? "请先加载完整工牌预览，再批准申请。"
                      : "请核对工牌内容和照片。"}
                </p>
                <div className="review-actions">
                  <Button
                    ref={approveTrigger}
                    className="primary"
                    disabled={
                      !eligible || state.media.kind !== "ready" || state.pending
                    }
                    aria-describedby="review-eligibility"
                    onClick={() => controller.confirm("approve")}
                  >
                    批准申请
                  </Button>
                  <Button
                    ref={rejectTrigger}
                    className="review-danger"
                    disabled={state.pending}
                    onClick={() => controller.confirm("reject")}
                  >
                    驳回申请
                  </Button>
                </div>
              </>
            ) : (
              <>
                <p>
                  {item.status === "approved"
                    ? "申请已审核通过。"
                    : item.status === "ready"
                      ? "工牌图片已生成。"
                      : item.status === "cancelled"
                        ? "申请已取消。申请人如需办理，须重新申请。"
                        : "审核未通过，申请人可修改资料后重新申请。"}
                </p>
                <ReviewFacts application={item} />
              </>
            )}
          </section>
          <BadgeProduction
            application={item}
            manager
            eligible={eligible}
            generating={state.pending && !state.confirmation}
            retryAt={state.renderRetryAt}
            now={now}
            media={state.badge}
            download={state.download}
            generate={() => void controller.render()}
            submit={() => controller.downloadBadge()}
            failed={() => controller.badgeFailed()}
          />
        </aside>
      </div>
      <div className="review-actions">
        {(item.status === "submitted" ||
          item.status === "approved" ||
          item.status === "ready") && (
          <Button
            ref={cancelTrigger}
            className="review-danger"
            disabled={state.pending}
            onClick={() => controller.confirm("cancel")}
          >
            取消申请
          </Button>
        )}
        <Button
          className="secondary"
          disabled={state.pending || mediaWait > 0}
          onClick={() => void controller.load()}
        >
          {mediaWait > 0 ? `${mediaWait} 秒后可刷新详情` : "刷新详情"}
        </Button>
        <a href="/workforce/reviews">返回审核列表</a>
      </div>
      {state.confirmation === "cancel" && (
        <CancellationDialog
          application={item}
          mode="manager"
          pending={state.pending}
          trigger={cancelTrigger}
          close={() => controller.dismiss()}
          submit={(reason) => void controller.submit(reason)}
        />
      )}
      {state.confirmation && state.confirmation !== "cancel" && (
        <ReviewDialog
          application={item}
          decision={state.confirmation}
          pending={state.pending}
          trigger={
            state.confirmation === "approve" ? approveTrigger : rejectTrigger
          }
          close={() => controller.dismiss()}
          submit={(reason) => void controller.submit(reason)}
        />
      )}
    </>
  );
}

export function ReviewsPanel({ controllers }: { controllers: Controllers }) {
  const { current: identity, reviews: controller } = controllers;
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
  const heading = useRef<HTMLHeadingElement>(null);
  const [now, setNow] = useState(Date.now);
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, []);
  useEffect(() => {
    heading.current?.focus();
  }, [state.kind, account.kind]);
  const retryAt =
    "retryAt" in state
      ? state.retryAt
      : "retryAt" in account
        ? account.retryAt
        : 0;
  const wait = Math.max(0, Math.ceil((retryAt - now) / 1000));
  const full =
    account.kind === "ready" &&
    (state.kind === "list" || state.kind === "detail");
  return (
    <div className="review-layout">
      <nav className="review-navigation" aria-label="工牌导航">
        <a href="/workforce/reviews" aria-current="page">
          工牌审核
        </a>
        <a href="/workforce/applications">我的申请</a>
        <a href="/account">个人账户</a>
      </nav>
      <div className="review-heading">
        <div>
          <h1 ref={heading} tabIndex={-1}>
            {controller.mode.kind === "list" ? "工牌审核" : "工牌申请详情"}
          </h1>
          {controller.mode.kind === "list" ? (
            <p>按审核状态查看申请。</p>
          ) : state.kind === "detail" &&
            state.result.application.status === "submitted" ? (
            <p>核对申请内容后，批准或驳回申请。</p>
          ) : null}
        </div>
        {full && <span className="review-scope">审核范围：全单位</span>}
      </div>
      {account.kind !== "ready" ? (
        <section className="review-panel review-message">
          <p role="status">{identityMessage(account)}</p>
          <div className="review-actions">
            {(account.kind === "anonymous" || account.kind === "expired") && (
              <a className="primary" href="/authentication/login">
                使用手机号登录
              </a>
            )}
            {account.kind === "reload" && (
              <Button className="primary" onClick={() => location.reload()}>
                重新加载页面
              </Button>
            )}
            {["storage", "unavailable", "limited", "denied"].includes(
              account.kind,
            ) && (
              <Button
                className="secondary"
                disabled={wait > 0}
                onClick={() => void identity.verify()}
              >
                {wait ? `${wait} 秒后可重新检查` : "重新检查"}
              </Button>
            )}
            <a href="/account">返回个人账户</a>
          </div>
        </section>
      ) : (
        <>
          {(state.kind === "hidden" || state.kind === "loading") && (
            <section className="review-panel review-message" aria-busy="true">
              <p role="status">正在加载工牌申请…</p>
            </section>
          )}
          {(state.kind === "error" ||
            state.kind === "missing" ||
            state.kind === "unknown") && (
            <section className="review-panel review-message">
              <h2>
                {state.kind === "unknown"
                  ? state.operation === "cancellation"
                    ? "暂时无法确认取消结果"
                    : state.operation === "generation"
                      ? "暂时无法确认生成结果"
                      : "暂时无法确认审核结果"
                  : state.kind === "missing"
                    ? "目前无法查看这份申请"
                    : "工牌申请暂时无法加载"}
              </h2>
              <p role="status">{state.message}</p>
              <div className="review-actions">
                <Button
                  className="primary"
                  disabled={wait > 0}
                  onClick={() => void controller.load()}
                >
                  {wait
                    ? `${wait} 秒后可刷新`
                    : controller.mode.kind === "detail"
                      ? "刷新详情"
                      : "重新加载"}
                </Button>
                <a href="/workforce/reviews">返回审核列表</a>
              </div>
            </section>
          )}
          {state.kind === "cancelled" && (
            <section className="review-panel review-message">
              <h2>申请已取消</h2>
              <p role="status">取消已确认，原申请内容和已有审核记录仍保留。</p>
              <div className="review-actions">
                <Button onClick={() => void controller.load()}>
                  查看申请记录
                </Button>
                <a href="/workforce/reviews">返回审核列表</a>
              </div>
            </section>
          )}
          {state.kind === "list" && (
            <section className="review-panel" aria-label="工牌申请列表">
              <div className="review-toolbar">
                <nav className="review-tabs" aria-label="按审核状态查看">
                  {(Object.keys(STATUSES) as ApplicationStatus[]).map(
                    (status) => (
                      <Button
                        key={status}
                        aria-pressed={state.status === status}
                        onClick={() => void controller.load(1, status)}
                      >
                        {STATUSES[status]}
                      </Button>
                    ),
                  )}
                </nav>
                <p>共 {state.result.count} 份申请</p>
              </div>
              {!state.result.applications.length ? (
                <div className="review-message">
                  <h2>
                    {state.result.count
                      ? "找不到这一页。"
                      : `暂无${STATUSES[state.status]}的申请。`}
                  </h2>
                  <p>
                    {state.result.count
                      ? "请返回第一页继续查看。"
                      : "可切换审核状态查看其他申请。"}
                  </p>
                  {state.result.count > 0 && (
                    <Button
                      className="secondary"
                      onClick={() => void controller.load(1)}
                    >
                      返回第一页
                    </Button>
                  )}
                </div>
              ) : (
                <table className="review-table">
                  <thead>
                    <tr>
                      {[
                        "姓名",
                        "工号",
                        "提交科室",
                        "类型",
                        "状态",
                        "提交时间",
                        "查看申请",
                      ].map((label) => (
                        <th key={label} scope="col">
                          {label}
                        </th>
                      ))}
                    </tr>
                  </thead>
                  <tbody>
                    {state.result.applications.map((item) => (
                      <tr key={item.id}>
                        <td>{item.name}</td>
                        <td data-label="工号">{item.printedNumber}</td>
                        <td data-label="提交科室">{item.department.printed}</td>
                        <td data-label="类型">{KINDS[item.kind]}</td>
                        <td>
                          <Tag status={item.status} />
                        </td>
                        <td data-label="提交时间">
                          <time dateTime={item.submittedAt}>
                            {date(item.submittedAt)}
                          </time>
                        </td>
                        <td>
                          <a
                            href={`/workforce/reviews/${item.id}`}
                            aria-label={`查看${item.name}的申请`}
                          >
                            查看 →
                          </a>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              )}
              <div className="review-pagination">
                <span>共 {state.result.count} 份申请</span>
                <div>
                  <Button
                    disabled={state.page <= 1}
                    onClick={() => void controller.load(state.page - 1)}
                  >
                    上一页
                  </Button>
                  <span>第 {state.page} 页</span>
                  <Button
                    disabled={state.page * 20 >= state.result.count}
                    onClick={() => void controller.load(state.page + 1)}
                  >
                    下一页
                  </Button>
                  <Button onClick={() => void controller.load(1)}>
                    刷新列表
                  </Button>
                </div>
              </div>
            </section>
          )}
          {state.kind === "detail" && (
            <Detail state={state} controller={controller} now={now} />
          )}
        </>
      )}
    </div>
  );
}
export default function WorkforceReviewsPage() {
  const tail = useParams()["*"] ?? "";
  const controllers = useReviews(
    tail ? { kind: "detail", id: tail } : { kind: "list" },
  );
  return (
    <>
      <Header />
      <main id="main">
        {controllers ? (
          <ReviewsPanel controllers={controllers} />
        ) : (
          <div className="loading-shell">
            <p>正在打开工牌审核…</p>
          </div>
        )}
      </main>
      <Footer />
    </>
  );
}
