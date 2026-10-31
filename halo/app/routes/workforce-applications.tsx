import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { CancellationDialog } from "../workforce/cancellation-dialog";
import { flushSync } from "react-dom";
import { useParams } from "react-router";
import { Button } from "@base-ui/react/button";
import { Dialog } from "@base-ui/react/dialog";
import { Header, Footer } from "../root";
import { Current, type CurrentState } from "../authentication/current";
import { Sessions } from "../authentication/session";
import { current } from "../protocol/session";
import { workforce } from "../protocol/workforce";
import { currentBuild } from "../protocol/build";
import { reloadRequired } from "../integrity";
import { Workforce } from "../workforce/status";
import { Applications, type ApplicationMode } from "../workforce/applications";
import { ApplicationFields } from "../workforce/application-fields";
import { ApplicationPhotoPanel } from "../workforce/photo-panel";
import {
  APPLICATION_KINDS as KINDS,
  APPLICATION_STATUSES as STATUSES,
  applicationDate as date,
  SnapshotDetails,
  PreviewFigure,
  ReviewFacts,
} from "../workforce/application-display";

export function meta() {
  return [
    { title: "工牌申请 · 司籍处" },
    { name: "robots", content: "noindex" },
  ];
}
type Controllers = {
  current: Current;
  workforce: Workforce;
  applications: Applications;
};
function useApplications(mode: ApplicationMode) {
  const [controllers, setControllers] = useState<{
    key: string;
    value: Controllers;
  }>();
  const key = mode.kind === "detail" ? `detail:${mode.id}` : mode.kind;
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
    const applications = new Applications(mode, {
      current: identity,
      workforce: status,
      build: currentBuild,
      now: Date.now,
    });
    setControllers({
      key,
      value: { current: identity, workforce: status, applications },
    });
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
      applications.close();
      status.close();
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
function IdentityMessage({ state }: { state: CurrentState }) {
  const copy =
    state.kind === "anonymous"
      ? "请使用手机号登录后查看工牌申请。"
      : state.kind === "expired"
        ? "当前登录已失效，请重新登录。"
        : state.kind === "reload"
          ? "请重新加载页面后继续。"
          : state.kind === "denied"
            ? "无法查看这项内容，请返回个人账户。"
            : ["storage", "unavailable", "limited"].includes(state.kind)
              ? "暂时无法检查登录状态，请稍后重试。"
              : "正在检查登录状态，请稍候。";
  return (
    <p role="status" className="application-notice">
      {copy}
    </p>
  );
}

export function ApplicationsPanel({
  controllers,
}: {
  controllers: Controllers;
}) {
  const {
    current: identity,
    workforce: service,
    applications: controller,
  } = controllers;
  const account = useSyncExternalStore(
    identity.subscribe,
    identity.snapshot,
    identity.snapshot,
  );
  const serviceState = useSyncExternalStore(
    service.subscribe,
    service.snapshot,
    service.snapshot,
  );
  const state = useSyncExternalStore(
    controller.subscribe,
    controller.snapshot,
    controller.snapshot,
  );
  const photo = useSyncExternalStore(
    controller.photo.subscribe,
    controller.photo.snapshot,
    controller.photo.snapshot,
  );
  const heading = useRef<HTMLHeadingElement>(null),
    dialogHeading = useRef<HTMLHeadingElement>(null);
  const previewButton = useRef<HTMLButtonElement>(null);
  const cancelTrigger = useRef<HTMLButtonElement>(null);
  const previous = useRef(state.kind);
  const [now, setNow] = useState(Date.now);
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, []);
  useEffect(() => {
    if (state.kind === "edit" && previous.current === "preview")
      previewButton.current?.focus();
    else if (state.kind !== "preview" && state.kind !== "submitting")
      heading.current?.focus();
    previous.current = state.kind;
  }, [state.kind, account.kind]);
  const retryAt = "retryAt" in state ? (state.retryAt ?? 0) : 0;
  const wait = Math.max(0, Math.ceil((retryAt - now) / 1000));
  const title =
    controller.mode.kind === "new"
      ? "申请工牌"
      : controller.mode.kind === "detail"
        ? "工牌申请详情"
        : "我的工牌申请";
  const editing =
    state.kind === "edit" || state.kind === "working"
      ? state.context
      : undefined;
  const modal =
    state.kind === "preview" || state.kind === "submitting" ? state : undefined;
  return (
    <div
      className={
        controller.mode.kind === "detail"
          ? "application-layout review-layout"
          : "application-layout"
      }
    >
      <nav className="application-navigation" aria-label="工牌申请导航">
        <a href="/workforce">员工服务</a>
        <a href="/workforce/applications">我的申请</a>
        <a href="/workforce/reviews">工牌审核</a>
        <a href="/account">个人账户</a>
      </nav>
      <p className="eyebrow">员工服务 / 工牌申请</p>
      <h1 ref={heading} tabIndex={-1}>
        {title}
      </h1>
      {account.kind !== "ready" ? (
        <>
          <IdentityMessage state={account} />
          {["anonymous", "expired"].includes(account.kind) && (
            <a className="primary" href="/authentication/login">
              手机号登录
            </a>
          )}
          {["storage", "unavailable", "limited"].includes(account.kind) && (
            <Button
              className="primary"
              disabled={"retryAt" in account && account.retryAt > now}
              onClick={() => void identity.verify()}
            >
              检查登录状态
            </Button>
          )}
          {account.kind === "reload" && (
            <Button className="primary" onClick={() => location.reload()}>
              重新加载页面
            </Button>
          )}
        </>
      ) : (
        <>
          {(state.kind === "hidden" || state.kind === "loading") && (
            <p role="status">正在读取工牌申请，请稍候。</p>
          )}
          {(state.kind === "error" || state.kind === "blocked") && (
            <div className="application-notice" role="status">
              <p>{state.message}</p>
              <div className="application-actions">
                <Button
                  className="primary"
                  disabled={wait > 0}
                  onClick={() => {
                    if (controller.mode.kind === "new") void service.refresh();
                    else void controller.load();
                  }}
                >
                  {wait
                    ? `${wait} 秒后可刷新`
                    : controller.mode.kind === "new"
                      ? "刷新办理内容"
                      : controller.mode.kind === "list"
                        ? "刷新列表"
                        : "刷新详情"}
                </Button>
                {state.kind === "blocked" &&
                  serviceState.kind === "ready" &&
                  serviceState.status === "active" && (
                    <a href="/workforce/number">申请工号</a>
                  )}
              </div>
            </div>
          )}
          {state.kind === "cancellation-unknown" && (
            <section className="application-notice">
              <h2>暂时无法确认取消结果</h2>
              <p role="status">{state.message}</p>
              <div className="application-actions">
                <Button
                  className="secondary"
                  disabled={wait > 0}
                  onClick={() => void controller.load()}
                >
                  {wait > 0 ? `${wait} 秒后可刷新详情` : "刷新详情"}
                </Button>
                <a href="/workforce/applications">返回申请列表</a>
              </div>
            </section>
          )}
          {state.kind === "cancelled" && (
            <section className="application-notice">
              <h2>申请已取消</h2>
              <p role="status">需要办理时，请重新申请。</p>
              <a href="/workforce/applications">返回申请列表</a>
            </section>
          )}
          {editing && (
            <form
              className="application-form"
              onSubmit={(event) => {
                event.preventDefault();
                void controller.preview();
              }}
            >
              <p className="application-introduction">
                选择本次工牌使用的内容，预览后再提交。
              </p>
              {state.kind === "edit" && state.message && (
                <p className="application-notice" role="alert">
                  {state.message}
                </p>
              )}
              <ApplicationFields
                selection={editing.directories}
                subtype={editing.number.subtype}
                pickups={editing.template.pickups}
                value={editing.choices}
                disabled={state.kind === "working"}
                onChange={(choices) => controller.edit(choices)}
              />
              <ApplicationPhotoPanel
                photo={controller.photo}
                frame={{
                  width: editing.template.template.layout.photo.widthUm,
                  height: editing.template.template.layout.photo.heightUm,
                }}
                backdrop={editing.artwork.backdrop}
                disabled={state.kind === "working"}
              />
              <div className="application-actions">
                <Button
                  ref={previewButton}
                  type="submit"
                  className="primary"
                  disabled={
                    state.kind === "working" ||
                    wait > 0 ||
                    photo.kind !== "editing" ||
                    !photo.confirmed
                  }
                >
                  {state.kind === "working"
                    ? "正在准备完整预览"
                    : wait
                      ? `${wait} 秒后可预览`
                      : "预览工牌"}
                </Button>
                <Button
                  className="secondary"
                  disabled={state.kind === "working" || wait > 0}
                  onClick={() => void controller.load()}
                >
                  刷新办理内容
                </Button>
              </div>
              <p className="field-hint">
                刷新办理内容会清除本页尚未提交的选择和照片，请重新确认。
              </p>
            </form>
          )}
          {state.kind === "created" && (
            <section
              className="application-result"
              aria-labelledby="application-created"
            >
              <h2 id="application-created">工牌申请已提交</h2>
              <p>
                {KINDS[state.application.kind]} ·{" "}
                {STATUSES[state.application.status]}
              </p>
              <p>提交时间：{date(state.application.submittedAt)}</p>
              <a
                className="primary"
                href={`/workforce/applications/${state.application.id}`}
              >
                查看已保存的申请
              </a>
              <a href="/workforce/applications">查看我的申请</a>
            </section>
          )}
          {state.kind === "unknown" && (
            <section
              className="application-result"
              aria-labelledby="application-unknown"
            >
              <h2 id="application-unknown">暂时无法确认申请是否已提交</h2>
              <p>请先查看我的申请，暂时不要重复提交。</p>
              <a className="primary" href="/workforce/applications">
                刷新申请列表
              </a>
            </section>
          )}
          {state.kind === "list" && (
            <>
              {serviceState.kind === "ready" &&
                serviceState.status === "active" && (
                  <a className="primary" href="/workforce/applications/new">
                    申请新工牌
                  </a>
                )}
              {state.result.access === "restricted" && (
                <p className="application-notice">
                  当前仅可查看未发放申请的类型、状态和提交时间。
                </p>
              )}
              {!state.result.applications.length ? (
                <p className="application-empty">
                  {state.result.count === 0
                    ? state.result.access === "restricted"
                      ? "暂无可查看的工牌申请。"
                      : "暂无工牌申请。"
                    : "找不到这一页，请返回第一页。"}
                </p>
              ) : (
                <ul className="application-list">
                  {state.result.applications.map((item) => (
                    <li key={item.id}>
                      <div>
                        <h2>{KINDS[item.kind]}工牌申请</h2>
                        <p>{STATUSES[item.status]}</p>
                        <time dateTime={item.submittedAt}>
                          {date(item.submittedAt)}
                        </time>
                      </div>
                      <a href={`/workforce/applications/${item.id}`}>
                        查看详情
                      </a>
                    </li>
                  ))}
                </ul>
              )}
              <div className="application-pagination">
                <p>
                  共 {state.result.count} 项 · 第 {state.page} 页
                </p>
                <Button
                  disabled={state.page <= 1}
                  onClick={() => void controller.load(state.page - 1)}
                >
                  上一页
                </Button>
                <Button
                  disabled={state.page * 20 >= state.result.count}
                  onClick={() => void controller.load(state.page + 1)}
                >
                  下一页
                </Button>
                <Button onClick={() => void controller.load(1)}>
                  {!state.result.applications.length && state.result.count > 0
                    ? "返回第一页"
                    : "刷新列表"}
                </Button>
              </div>
            </>
          )}
          {state.kind === "detail" && (
            <section className="application-detail own-application-detail">
              {state.notice && (
                <p role="status" className="application-notice">
                  {state.notice}
                </p>
              )}
              <p>
                {KINDS[state.result.application.kind]} ·{" "}
                {STATUSES[state.result.application.status]}
              </p>
              <p>提交时间：{date(state.result.application.submittedAt)}</p>
              {state.result.access === "full" ? (
                <div className="review-columns">
                  <section className="review-panel">
                    <div className="review-panel-heading">
                      <h2>申请内容</h2>
                    </div>
                    <div className="review-panel-content">
                      {state.artwork ? (
                        <PreviewFigure
                          artwork={state.artwork}
                          failed={() => controller.previewFailed()}
                          caption="申请提交时确认的工牌内容。"
                        />
                      ) : (
                        <p className="application-notice" role="status">
                          {state.media === "loading"
                            ? "正在加载工牌预览…"
                            : "工牌预览暂时无法显示，请重新加载。"}
                        </p>
                      )}
                      <SnapshotDetails
                        snapshot={state.result.application.snapshot}
                      />
                    </div>
                  </section>
                  <aside
                    className="review-panel review-panel-content review-decision"
                    aria-label="审核结果"
                  >
                    <h2>
                      {state.result.application.status === "submitted"
                        ? "审核进度"
                        : "审核结果"}
                    </h2>
                    <ReviewFacts application={state.result.application} />
                  </aside>
                </div>
              ) : (
                <p className="application-notice">
                  当前仅可查看申请类型、状态和提交时间。
                </p>
              )}
              <Button
                className="secondary"
                disabled={wait > 0 || state.pending}
                onClick={() => void controller.load()}
              >
                {wait > 0 ? `${wait} 秒后可刷新详情` : "刷新详情"}
              </Button>
              {(state.result.application.status === "submitted" ||
                state.result.application.status === "approved") && (
                <Button
                  ref={cancelTrigger}
                  className="review-danger"
                  disabled={state.pending}
                  onClick={() => controller.confirmCancellation()}
                >
                  取消申请
                </Button>
              )}
              {state.cancellation && (
                <CancellationDialog
                  application={state.result.application}
                  mode="self"
                  pending={!!state.pending}
                  trigger={cancelTrigger}
                  close={() => controller.dismissCancellation()}
                  submit={() => void controller.cancelApplication()}
                />
              )}
            </section>
          )}
          <Dialog.Root
            open={!!modal}
            onOpenChange={(open) => {
              if (!open && state.kind === "preview")
                controller.edit(state.context.choices);
            }}
          >
            <Dialog.Portal>
              <Dialog.Backdrop className="application-backdrop" />
              <Dialog.Popup
                className="application-dialog"
                initialFocus={dialogHeading}
              >
                <Dialog.Title ref={dialogHeading} tabIndex={-1}>
                  确认本次工牌申请
                </Dialog.Title>
                <Dialog.Description>
                  请核对完整工牌和领取地点。提交后保存本次内容，更改内容需新建申请。
                </Dialog.Description>
                <div className="application-dialog-content">
                  {modal && (
                    <>
                      <p className="application-kind-summary">
                        申请类型：{KINDS[modal.preview.kind]}
                      </p>
                      <PreviewFigure
                        artwork={modal.artwork}
                        failed={() => controller.previewFailed()}
                      />
                      <SnapshotDetails snapshot={modal.preview.snapshot} />
                    </>
                  )}
                </div>
                <div className="application-actions">
                  <Button
                    className="secondary"
                    disabled={state.kind === "submitting"}
                    onClick={() => {
                      if (modal) controller.edit(modal.context.choices);
                    }}
                  >
                    返回修改
                  </Button>
                  <Button
                    className="primary"
                    disabled={state.kind !== "preview"}
                    onClick={() => void controller.submit()}
                  >
                    {state.kind === "submitting"
                      ? "正在提交申请"
                      : "确认并提交申请"}
                  </Button>
                </div>
              </Dialog.Popup>
            </Dialog.Portal>
          </Dialog.Root>
        </>
      )}
    </div>
  );
}
export default function WorkforceApplicationsPage() {
  const tail = useParams()["*"] ?? "";
  const mode: ApplicationMode =
    tail === "new"
      ? { kind: "new" }
      : tail
        ? { kind: "detail", id: tail }
        : { kind: "list" };
  const controllers = useApplications(mode);
  return (
    <>
      <Header />
      <main id="main">
        {controllers ? (
          <ApplicationsPanel controllers={controllers} />
        ) : (
          <div className="loading-shell">
            <p>正在打开工牌申请…</p>
          </div>
        )}
      </main>
      <Footer />
    </>
  );
}
