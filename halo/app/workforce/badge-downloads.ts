import type { Current } from "../authentication/current";
import { loadBadgeImage, type BadgeImage } from "./badge-image";
import type { BadgeDownload } from "./badge-display";

/** Owns each newly authorized download independently of the visible preview. */
export class BadgeDownloads {
  private state: BadgeDownload = { kind: "idle" };
  private request?: AbortController;
  private timer?: ReturnType<typeof setTimeout>;
  private image?: BadgeImage;
  constructor(
    private readonly dependencies: {
      current: Current;
      build(): Promise<boolean>;
      now(): number;
      read: typeof loadBadgeImage;
      unavailable(): void;
      changed(state: BadgeDownload): void;
    },
  ) {}
  clear() {
    this.request?.abort();
    this.request = undefined;
    clearTimeout(this.timer);
    this.timer = undefined;
    this.image?.close();
    this.image = undefined;
    this.state = { kind: "idle" };
  }
  private show(state: BadgeDownload) {
    this.state = state;
    this.dependencies.changed(state);
  }
  async start(id: string) {
    if (
      this.state.kind === "pending" ||
      (this.state.kind === "failed" &&
        this.state.retryAt > this.dependencies.now())
    )
      return;
    const binding = this.dependencies.current.binding();
    if (!binding) return;
    this.clear();
    const request = new AbortController();
    this.request = request;
    const active = () =>
      this.request === request &&
      !request.signal.aborted &&
      this.dependencies.current.matches(binding);
    const guard = async () => {
      if (!active()) return false;
      if (!(await this.dependencies.build().catch(() => false))) {
        if (active()) this.dependencies.current.reload();
        return false;
      }
      return (
        active() &&
        (await this.dependencies.current.confirm(binding)) &&
        active()
      );
    };
    const failed = (delay = 5000) =>
      this.show({
        kind: "failed",
        message: "暂时无法取得下载文件，请重新下载。",
        retryAt: this.dependencies.now() + delay,
      });
    this.show({ kind: "pending" });
    this.timer = setTimeout(() => {
      if (!active()) return;
      this.clear();
      failed();
    }, 35000);
    try {
      if (!(await guard())) return;
      const result = await this.dependencies.read(
        binding.selection.id,
        id,
        request.signal,
      );
      if (!(await guard())) {
        if (result.kind === "ok") result.value.close();
        return;
      }
      if (result.kind !== "ok") {
        if (result.kind === "failure" && result.status === 404) {
          this.clear();
          this.dependencies.unavailable();
          return;
        }
        if (
          result.kind === "failure" &&
          (result.status === 401 || result.status === 403)
        )
          await this.dependencies.current.reject(binding, result.status);
        else
          failed(
            result.kind === "failure"
              ? Math.max(5000, result.retryAfter)
              : 5000,
          );
        return;
      }
      this.image = result.value;
      const link = document.createElement("a");
      link.href = result.value.url;
      link.download = `badge-${id}.webp`;
      document.body.append(link);
      try {
        link.click();
      } finally {
        link.remove();
      }
      this.show({ kind: "idle" });
    } catch {
      if (active()) failed();
    } finally {
      if (this.request === request) {
        clearTimeout(this.timer);
        this.timer = undefined;
        this.request = undefined;
      }
    }
  }
}
