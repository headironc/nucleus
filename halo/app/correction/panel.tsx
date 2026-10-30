import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { Button } from "@base-ui/react/button";
import type { History } from "./history";

const dateFormat = new Intl.DateTimeFormat("zh-CN", {
  timeZone: "Asia/Shanghai",
  year: "numeric",
  month: "long",
  day: "numeric",
  hour: "2-digit",
  minute: "2-digit",
  hourCycle: "h23",
});

export function HistoryPanel({ controller }: { controller: History }) {
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
  if (state.kind === "hidden") return null;
  const seconds =
    "retryAt" in state
      ? Math.max(0, Math.ceil((state.retryAt - now) / 1000))
      : 0;
  const refresh = (size?: number) => {
    heading.current?.focus();
    void controller.refresh(size);
  };
  const ready = state.kind === "ready";
  return (
    <section
      className="correction-panel"
      aria-labelledby="history-title"
      aria-busy={state.kind === "loading"}
    >
      <div className="correction-heading">
        <h2 id="history-title" ref={heading} tabIndex={-1}>
          实名更正记录
        </h2>
        <Button
          className="quiet-link"
          disabled={state.kind === "loading" || seconds > 0}
          onClick={() => refresh()}
        >
          刷新记录
        </Button>
      </div>
      <p className="correction-explanation">
        按更正先后从新到旧展示，时间以北京时间显示。证件号仅展示尾四位。
      </p>
      {ready && state.corrections.length > 0 ? (
        <ol className="correction-list">
          {state.corrections.map((record) => (
            <li key={record.id}>
              <article>
                <div className="correction-record-heading">
                  <h3>实名资料更正</h3>
                  <time dateTime={record.correctedAt}>
                    {dateFormat.format(new Date(record.correctedAt))}
                  </time>
                </div>
                <div className="correction-values">
                  <div>
                    <p className="value-label">更正前</p>
                    <p>{record.before.name}</p>
                    <p className="value-suffix">
                      身份证尾号 <span>{record.before.idNumber.suffix}</span>
                    </p>
                  </div>
                  <span className="correction-arrow" aria-hidden="true">
                    →
                  </span>
                  <div>
                    <p className="value-label">更正后</p>
                    <p>{record.after.name}</p>
                    <p className="value-suffix">
                      身份证尾号 <span>{record.after.idNumber.suffix}</span>
                    </p>
                  </div>
                </div>
              </article>
            </li>
          ))}
        </ol>
      ) : (
        <div className="correction-message" role="status">
          {state.kind === "loading" ? (
            <>
              <h3>正在读取更正记录</h3>
              <p>请稍候。</p>
            </>
          ) : ready ? (
            state.count === 0 ? (
              <>
                <h3>暂无实名更正记录</h3>
                <p>当前账户没有可展示的实名更正记录。</p>
              </>
            ) : (
              <>
                <h3>这一页没有记录</h3>
                <p>记录总数仍为 {state.count} 条，请返回第一页查看。</p>
                <Button onClick={() => refresh()}>返回第一页</Button>
              </>
            )
          ) : (
            <>
              <h3>
                {state.kind === "limited"
                  ? "暂时无法读取更正记录"
                  : "暂时无法加载更正记录"}
              </h3>
              <p>
                {state.kind === "limited"
                  ? "暂时无法处理，请稍后再试。"
                  : "请刷新后重试。"}
                当前账户资料仍可查看。
              </p>
              <Button disabled={seconds > 0} onClick={() => refresh()}>
                {seconds > 0 ? `${seconds} 秒后可重新加载` : "重新加载记录"}
              </Button>
            </>
          )}
        </div>
      )}
      {ready && (
        <div className="correction-controls">
          <span>共 {state.count} 条记录</span>
          <label htmlFor="history-size">
            每页{" "}
            <select
              id="history-size"
              value={state.size}
              onChange={(event) => refresh(Number(event.target.value))}
            >
              {[2, 20, 50, 100].map((size) => (
                <option key={size} value={size}>
                  {size} 条
                </option>
              ))}
            </select>
          </label>
          <nav aria-label="实名更正记录分页">
            <Button
              disabled={state.page <= 1}
              onClick={() => {
                heading.current?.focus();
                void controller.previous();
              }}
            >
              上一页
            </Button>
            <span aria-live="polite">第 {state.page} 页</span>
            <Button
              disabled={state.page >= Math.ceil(state.count / state.size)}
              onClick={() => {
                heading.current?.focus();
                void controller.next();
              }}
            >
              下一页
            </Button>
          </nav>
        </div>
      )}
    </section>
  );
}
