import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { flushSync } from "react-dom";
import { Button } from "@base-ui/react/button";
import { Dialog } from "@base-ui/react/dialog";
import { Header, Footer } from "../root";
import { Sessions } from "../authentication/session";
import { current } from "../protocol/session";
import { workforce } from "../protocol/workforce";
import { allocateNumber, currentNumber } from "../protocol/number";
import { currentBuild } from "../protocol/build";
import { reloadRequired } from "../integrity";
import { numberSubtypes, summarize } from "../workforce/number";
import { AllocationController, type State } from "../workforce/allocation";

export function meta() {
  return [
    { title: "领取工号 · 司籍处" },
    { name: "robots", content: "noindex" },
  ];
}

function useAllocation() {
  const [controller, setController] = useState<AllocationController>();
  useEffect(() => {
    const sessions = new Sessions();
    const allocation = new AllocationController({
      sessions,
      current,
      workforce,
      number: currentNumber,
      allocate: allocateNumber,
      build: currentBuild,
      now: Date.now,
      uuid: () => crypto.randomUUID(),
      storage: {
        getItem: (key) => sessionStorage.getItem(key),
        setItem: (key, value) => sessionStorage.setItem(key, value),
        removeItem: (key) => sessionStorage.removeItem(key),
      },
      marker: () => history.state?.numberAllocation ?? null,
      mark: (operation) =>
        history.replaceState(
          { ...history.state, numberAllocation: operation },
          "",
        ),
    });
    setController(allocation);
    const hide = () => flushSync(() => allocation.suspend());
    const reload = () => flushSync(() => allocation.reload());
    const check = () => {
      hide();
      if (!document.hidden) void allocation.verify();
    };
    const unwatch = sessions.watch(check);
    document.addEventListener("visibilitychange", check);
    window.addEventListener("focus", check);
    window.addEventListener("pageshow", check);
    window.addEventListener("pagehide", hide);
    window.addEventListener("halo:reload-required", reload);
    if (reloadRequired()) allocation.reload();
    else if (!document.hidden) void allocation.verify();
    return () => {
      allocation.close();
      unwatch();
      document.removeEventListener("visibilitychange", check);
      window.removeEventListener("focus", check);
      window.removeEventListener("pageshow", check);
      window.removeEventListener("pagehide", hide);
      window.removeEventListener("halo:reload-required", reload);
      void sessions.close().catch(() => {});
    };
  }, []);
  return controller;
}

function copy(state: State): [string, string] {
  switch (state.kind) {
    case "entry":
      return [
        "领取我的工号",
        "请选择本人的人员类别。系统将按本年度分配工号，无需审批。",
      ];
    case "reading":
      return ["正在读取员工服务", "请稍候。"];
    case "busy":
      return ["正在确认领取结果", "请稍候。暂时离开不会取消已经提交的申请。"];
    case "ineligible":
      return [
        "暂时不能领取工号",
        "员工服务开通且未暂停时，才可首次领取工号。请返回员工服务查看当前状态。",
      ];
    case "present":
      return ["你已有工号", "请返回员工服务查看。工号无需重复领取。"];
    case "unavailable":
      return ["暂时无法读取工号信息", "请稍后重试，或返回员工服务。"];
    case "other":
      return [
        "当前账户已变化",
        "本标签页保留了另一账户的领取申请。请切换回原账户后查看，当前账户不会继续提交。",
      ];
    case "missing":
      return [
        "无法继续查看本次申请",
        "本标签页的申请信息已缺失或损坏。请返回员工服务查看当前工号，仍有疑问时联系医院管理员。",
      ];
    case "storage":
      return [
        "暂时无法保存申请信息",
        "请检查浏览器是否允许本网站保存信息，然后重新检查。已有申请的结果仍待确认，请勿重复领取。",
      ];
    case "reload":
      return [
        "请重新加载页面",
        "本页暂时无法继续处理。申请信息会保留，重新加载后请查看本次领取结果。",
      ];
    case "current": {
      switch (state.current.kind) {
        case "anonymous":
          return [
            "请先登录",
            "登录后可领取本人首个工号，或查看此前的领取结果。",
          ];
        case "expired":
          return [
            "请重新登录",
            "当前登录已失效。请登录原账户，再返回本页查看申请。",
          ];
        case "denied":
          return ["暂时无法读取本人信息", "请返回个人账户或首页。"];
        case "reload":
          return ["请重新加载页面", "重新加载后再查看本次领取结果。"];
        case "storage":
          return [
            "暂时无法检查登录状态",
            "请检查浏览器是否允许本网站保存信息，然后重试。",
          ];
        case "limited":
          return ["请稍后再检查", "暂时无法检查登录状态，请等待后重试。"];
        case "unavailable":
          return ["暂时无法检查登录状态", "请稍后重试。"];
        default:
          return ["正在检查登录状态", "请稍候。"];
      }
    }
    case "result": {
      const result = state.allocation;
      if (result.outcome === "complete")
        return ["工号已领取", "本次领取已完成。请保存或查看下方工号。"];
      if (result.outcome === "mismatch")
        return [
          "无法继续提交本次申请",
          "本次申请信息与此前记录不一致。请返回员工服务查看当前工号，仍有疑问时联系医院管理员。",
        ];
      if (result.code === "number_present")
        return [
          "无法重复领取工号",
          "你已经领取过工号。请返回员工服务查看；当前没有工号时，请联系医院管理员。",
        ];
      if (result.code === "number_pool_exhausted")
        return [
          "本年度该类别的工号已用完",
          "请联系医院管理员。只有人员类别选错时，才重新选择。",
        ];
      if (result.outcome === "refused")
        return [
          "本次领取未完成",
          "申请未被接纳。请返回员工服务查看，仍有疑问时联系医院管理员。",
        ];
      if (result.code === "permission_denied")
        return [
          "暂时无法确认领取结果",
          "当前无法办理本次领取。请查看员工服务状态；恢复办理条件后，可继续查看本次申请。",
        ];
      return [
        "领取结果待确认",
        "暂时无法确认本次领取是否完成。请稍后点击“查看本次领取结果”，不要重复申请。",
      ];
    }
  }
}

function AllocationPanel({ controller }: { controller: AllocationController }) {
  const state = useSyncExternalStore(
    controller.subscribe,
    controller.snapshot,
    controller.snapshot,
  );
  const [choice, setChoice] = useState("");
  const [confirm, setConfirm] = useState(false);
  const [now, setNow] = useState(Date.now);
  const heading = useRef<HTMLHeadingElement>(null);
  const cancel = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, []);
  useEffect(() => {
    setChoice("");
    setConfirm(false);
    heading.current?.focus();
  }, [state.kind, "account" in state ? state.account.id : null]);
  const pending =
    state.kind === "busy" ||
    (state.kind === "result" && state.allocation.outcome === "pending");
  useEffect(() => {
    if (!pending && !choice) return;
    const warn = (event: BeforeUnloadEvent) => {
      event.preventDefault();
      event.returnValue = "";
    };
    window.addEventListener("beforeunload", warn);
    return () => window.removeEventListener("beforeunload", warn);
  }, [pending, choice]);
  const [title, description] = copy(state);
  const retryAt =
    state.kind === "result"
      ? state.allocation.retryAt
      : state.kind === "unavailable"
        ? state.retryAt
        : state.kind === "current" && "retryAt" in state.current
          ? state.current.retryAt
          : 0;
  const seconds = Math.max(0, Math.ceil((retryAt - now) / 1000));
  const fixed =
    state.kind === "result" && state.allocation.number
      ? summarize(state.allocation.number)
      : null;
  return (
    <div className="authentication-layout compact">
      <section
        className="authentication-card"
        aria-busy={state.kind === "busy" || state.kind === "reading"}
      >
        <div className="current-content">
          <p className="eyebrow">员工服务 / 工号</p>
          <h1 ref={heading} tabIndex={-1}>
            {title}
          </h1>
          {"account" in state && (
            <p className="workforce-name">{state.account.name}</p>
          )}
          <p className="result-copy" role="status">
            {description}
          </p>
          {fixed && (
            <section className="workforce-number" aria-label="本次领取的工号">
              <p className="number-subtype">{fixed.subtype}</p>
              <p className="employee-number">{fixed.display}</p>
            </section>
          )}
          {state.kind === "result" &&
            state.allocation.outcome === "complete" &&
            !state.persisted && (
              <p role="alert">
                领取结果已确认，但浏览器未能保存。请记下工号，或返回员工服务查看。
              </p>
            )}
          {state.kind === "entry" && (
            <form
              onSubmit={(event) => {
                event.preventDefault();
                if (choice) setConfirm(true);
              }}
            >
              <label className="field-label" htmlFor="number-subtype">
                人员类别
              </label>
              <select
                id="number-subtype"
                className="number-subtype-select"
                value={choice}
                required
                onChange={(event) => setChoice(event.target.value)}
              >
                <option value="" disabled>
                  请选择本人的人员类别
                </option>
                {numberSubtypes.map((subtype) => (
                  <option key={subtype.value} value={subtype.value}>
                    {subtype.name}
                  </option>
                ))}
              </select>
              <p className="field-hint">
                类别用于分配工号，不表示正式任职关系。不确定时请先联系医院管理员。
              </p>
              <Button type="submit" className="primary" disabled={!choice}>
                领取工号
              </Button>
            </form>
          )}
          <div className="result-actions">
            {state.kind === "result" &&
              state.allocation.outcome === "pending" && (
                <Button
                  className="primary"
                  disabled={seconds > 0}
                  onClick={() => void controller.query()}
                >
                  {seconds ? `${seconds} 秒后可查看结果` : "查看本次领取结果"}
                </Button>
              )}
            {state.kind === "result" &&
              state.allocation.code === "number_pool_exhausted" && (
                <Button
                  className="primary"
                  onClick={() => void controller.chooseAgain()}
                >
                  重新选择类别
                </Button>
              )}
            {state.kind === "unavailable" && (
              <Button
                className="primary"
                disabled={seconds > 0}
                onClick={() => void controller.refresh()}
              >
                {seconds ? `${seconds} 秒后可重试` : "重新读取"}
              </Button>
            )}
            {(state.kind === "storage" ||
              (state.kind === "current" &&
                ["storage", "unavailable", "limited"].includes(
                  state.current.kind,
                ))) && (
              <Button
                className="primary"
                disabled={seconds > 0}
                onClick={() => void controller.verify()}
              >
                重新检查
              </Button>
            )}
            {(state.kind === "reload" ||
              (state.kind === "current" &&
                state.current.kind === "reload")) && (
              <Button className="primary" onClick={() => location.reload()}>
                重新加载页面
              </Button>
            )}
            {state.kind === "current" &&
              ["anonymous", "expired"].includes(state.current.kind) && (
                <>
                  <a
                    className="primary"
                    href="/authentication/codes"
                    target="_blank"
                    rel="noopener"
                  >
                    手机号登录
                  </a>
                  <p className="field-hint">
                    登录页面将在新标签页打开。登录原账户后，请返回本页。
                  </p>
                </>
              )}
            <a className="quiet-link" href="/workforce">
              返回员工服务
            </a>
            <a className="quiet-link" href="/account">
              返回个人账户
            </a>
          </div>
        </div>
        <Dialog.Root
          open={confirm && state.kind === "entry"}
          onOpenChange={setConfirm}
        >
          <Dialog.Portal>
            <Dialog.Backdrop className="number-backdrop" />
            <Dialog.Popup className="number-dialog" initialFocus={cancel}>
              <Dialog.Title>确认领取工号？</Dialog.Title>
              <Dialog.Description>
                人员类别：
                {
                  numberSubtypes.find(
                    (subtype) => String(subtype.value) === choice,
                  )?.name
                }
                。系统将分配本年度工号；领取后不能自行重新选择。
              </Dialog.Description>
              <div className="dialog-actions">
                <Dialog.Close ref={cancel} className="button primary">
                  暂不领取
                </Dialog.Close>
                <Button
                  className="button"
                  onClick={() => {
                    setConfirm(false);
                    void controller.submit(Number(choice));
                  }}
                >
                  确认领取
                </Button>
              </div>
            </Dialog.Popup>
          </Dialog.Portal>
        </Dialog.Root>
      </section>
    </div>
  );
}

export default function WorkforceNumberPage() {
  const controller = useAllocation();
  return (
    <>
      <Header />
      <main id="main">
        {controller ? (
          <AllocationPanel controller={controller} />
        ) : (
          <div className="loading-shell">
            <p>正在打开工号领取…</p>
          </div>
        )}
      </main>
      <Footer />
    </>
  );
}
