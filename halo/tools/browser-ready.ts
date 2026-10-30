import type { ChildProcess } from "node:child_process";
import { get } from "node:https";
import { setTimeout as delay } from "node:timers/promises";

export async function waitForNginx(
  proxy: ChildProcess,
  origin: string,
  certificate: Buffer,
  cancellation: AbortSignal,
) {
  const stopped = new AbortController();
  const signal = AbortSignal.any([
    stopped.signal,
    cancellation,
    AbortSignal.timeout(10_000),
  ]);
  const exited = () =>
    stopped.abort(new Error("Nginx exited before readiness"));
  const failed = (error: Error) => stopped.abort(error);
  proxy.once("exit", exited);
  proxy.once("error", failed);
  try {
    if (proxy.exitCode !== null || proxy.signalCode !== null) exited();
    for (;;) {
      signal.throwIfAborted();
      const ready = await new Promise<boolean>((resolve) => {
        const request = get(
          new URL("/build.json", origin),
          {
            ca: certificate,
            agent: false,
            signal: AbortSignal.any([signal, AbortSignal.timeout(1000)]),
          },
          (response) => {
            response.resume();
            response.once("end", () => resolve(response.statusCode === 200));
            response.once("error", () => resolve(false));
          },
        );
        request.once("error", () => resolve(false));
      });
      signal.throwIfAborted();
      if (ready) return;
      await delay(50, undefined, { signal });
    }
  } finally {
    proxy.off("exit", exited);
    proxy.off("error", failed);
  }
}

export function waitForNodeReady(
  node: ChildProcess,
  marker: string,
  cancellation: AbortSignal,
  timeoutMs = 10_000,
): Promise<void> {
  return new Promise((resolve, reject) => {
    let finished = false;
    const onData = (bytes: Buffer | string) => {
      if (String(bytes).includes(marker)) finish();
    };
    const finish = (error?: Error) => {
      if (finished) return;
      finished = true;
      clearTimeout(timer);
      node.stdout?.off("data", onData);
      node.off("exit", onExit);
      node.off("error", onError);
      cancellation.removeEventListener("abort", onAbort);
      if (error) reject(error);
      else resolve();
    };
    const onExit = () => finish(new Error("Node exited before readiness"));
    const onError = (error: Error) => finish(error);
    const onAbort = () =>
      finish(
        cancellation.reason instanceof Error
          ? cancellation.reason
          : new Error("Acceptance cancelled"),
      );
    const timer = setTimeout(
      () => finish(new Error("Node readiness timeout")),
      timeoutMs,
    );
    if (node.exitCode !== null || node.signalCode !== null) {
      onExit();
      return;
    }
    if (cancellation.aborted) {
      onAbort();
      return;
    }
    node.stdout?.on("data", onData);
    node.once("exit", onExit);
    node.once("error", onError);
    cancellation.addEventListener("abort", onAbort, { once: true });
  });
}
