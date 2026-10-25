import { useEffect, useRef, useState } from "react";
import { Button } from "@base-ui/react/button";
import { ShieldCheck, RefreshCw } from "lucide-react";

export type Action =
  | "send_authentication_code"
  | "send_mobile_change_code"
  | "send_mobile_binding_code";

type Options = {
  sitekey: string;
  action: Action;
  cData: string;
  "response-field": false;
  theme: "auto";
  size: "flexible";
  callback: (token: string) => void;
  "expired-callback": () => void;
  "error-callback": () => void;
  "timeout-callback": () => void;
};
type Widget = {
  render: (element: HTMLElement, options: Options) => string;
  reset: (id: string) => void;
  remove: (id: string) => void;
};
declare global {
  interface Window {
    turnstile?: Widget;
  }
}
let loading: Promise<Widget> | undefined;

function load(): Promise<Widget> {
  if (window.turnstile) return Promise.resolve(window.turnstile);
  if (loading) return loading;
  loading = new Promise((resolve, reject) => {
    const script = document.createElement("script");
    script.src =
      "https://challenges.cloudflare.com/turnstile/v0/api.js?render=explicit";
    script.async = true;
    const timer = setTimeout(() => {
      script.remove();
      loading = undefined;
      reject(new Error("widget_unavailable"));
    }, 15_000);
    script.onload = () => {
      clearTimeout(timer);
      if (window.turnstile) resolve(window.turnstile);
      else {
        loading = undefined;
        reject(new Error("widget_unavailable"));
      }
    };
    script.onerror = () => {
      clearTimeout(timer);
      script.remove();
      loading = undefined;
      reject(new Error("widget_unavailable"));
    };
    document.head.append(script);
  });
  return loading;
}

export function Turnstile({
  siteKey,
  operation,
  action,
  waiting,
  reset,
  onToken,
}: {
  siteKey: string;
  operation: string;
  action: Action;
  waiting: boolean;
  reset: number;
  onToken: (token: string | undefined) => void;
}) {
  const element = useRef<HTMLDivElement>(null);
  const instance = useRef<{ widget: Widget; id: string } | undefined>(
    undefined,
  );
  const generation = useRef(0);
  const callback = useRef(onToken);
  callback.current = onToken;
  const [state, setState] = useState<
    "loading" | "ready" | "verified" | "expired" | "failed"
  >("loading");
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    const current = ++generation.current;
    let active = true;
    setState("loading");
    callback.current(undefined);
    load()
      .then((widget) => {
        if (!active || current !== generation.current || !element.current)
          return;
        const change = (next: typeof state, token?: string) => {
          if (!active || current !== generation.current) return;
          callback.current(token);
          setState(next);
        };
        const id = widget.render(element.current, {
          sitekey: siteKey,
          action,
          cData: operation,
          "response-field": false,
          theme: "auto",
          size: "flexible",
          callback: (token) => change("verified", token),
          "expired-callback": () => change("expired"),
          "error-callback": () => change("failed"),
          "timeout-callback": () => change("expired"),
        });
        instance.current = { widget, id };
        setState((previous) => (previous === "loading" ? "ready" : previous));
      })
      .catch(() => {
        if (active && current === generation.current) setState("failed");
      });
    return () => {
      active = false;
      ++generation.current;
      callback.current(undefined);
      if (instance.current) instance.current.widget.remove(instance.current.id);
      instance.current = undefined;
    };
  }, [siteKey, operation, action, attempt]);
  useEffect(() => {
    if (reset && instance.current) {
      callback.current(undefined);
      instance.current.widget.reset(instance.current.id);
      setState("ready");
    }
  }, [reset]);
  return (
    <section className="verification" aria-label="安全验证" hidden={waiting}>
      <div className="verification-title">
        <ShieldCheck size={18} aria-hidden="true" />
        安全验证
      </div>
      <p className="field-hint" role="status">
        {state === "loading"
          ? "正在加载安全验证…"
          : state === "verified"
            ? "安全验证已完成"
            : state === "expired"
              ? "安全验证已过期，请重新完成。"
              : state === "failed"
                ? "安全验证暂时无法加载，请检查网络后重试。"
                : "请完成下方安全验证。"}
      </p>
      <div ref={element} className="widget" />
      {(state === "expired" || state === "failed") && (
        <Button
          className="text-button"
          type="button"
          onClick={() => setAttempt((value) => value + 1)}
        >
          <RefreshCw size={15} aria-hidden="true" />
          重新加载安全验证
        </Button>
      )}
    </section>
  );
}
