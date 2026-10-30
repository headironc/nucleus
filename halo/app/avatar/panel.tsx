import {
  useLayoutEffect,
  useId,
  useRef,
  useSyncExternalStore,
  type ChangeEvent,
  type ReactNode,
} from "react";
import type { AvatarSelection, AvatarSelectionError } from "./candidate";
import type { ImageFormat } from "./format";

const ERROR_COPY: Record<AvatarSelectionError, string> = {
  size: "图片超过 2 MiB，请选择不超过此大小的原始文件。",
  format: "请选择静态 PNG、JPEG 或 WebP，图片实际格式须与文件类型一致。",
  dimensions:
    "请选择正方形原图，宽高均须为 1–1024 像素。这里不会自动裁剪或缩放。",
  animation: "不支持动画图片，请选择静态 PNG、JPEG 或 WebP。",
  damaged: "无法完整读取这张图片，请检查文件是否损坏，或选择其他图片。",
  "preview-unavailable":
    "暂时无法在本地预览图片，请重新选择或换用支持图片预览的浏览器。",
};

function formatLabel(format: ImageFormat): string {
  switch (format) {
    case "image/png":
      return "PNG";
    case "image/jpeg":
      return "JPEG";
    case "image/webp":
      return "WebP";
  }
}

/**
 * Local candidate surface for the avatar replacement page. The parent owns
 * confirmation and submission; this panel only exposes a verified candidate.
 */
export function AvatarSelectionPanel({
  selection,
  current,
  onBeginSelection,
  onChoose,
  onCancel,
  onDiscard,
  disabled = false,
  visible = true,
  primaryAction,
}: {
  selection: AvatarSelection;
  current: ReactNode;
  onBeginSelection?: () => boolean;
  onChoose?: (file: File) => Promise<void>;
  onCancel?: () => void;
  onDiscard?: () => void;
  disabled?: boolean;
  visible?: boolean;
  primaryAction?: ReactNode;
}) {
  const state = useSyncExternalStore(
    selection.subscribe,
    selection.snapshot,
    selection.snapshot,
  );
  const input = useRef<HTMLInputElement>(null);
  const preview = useRef<HTMLCanvasElement>(null);
  const field = useId();
  const hint = useId();
  const message = useId();
  const heading = useId();
  const candidate = state.kind === "ready" ? state.candidate : undefined;
  const error = state.kind === "error" ? ERROR_COPY[state.error] : undefined;
  const busy = state.kind === "reading";

  useLayoutEffect(() => {
    const node = input.current;
    if (!node || !onCancel) return;
    node.addEventListener("cancel", onCancel);
    return () => node.removeEventListener("cancel", onCancel);
  }, [onCancel]);

  useLayoutEffect(() => {
    const node = preview.current;
    if (!candidate || !node || !visible) return;
    let live = true;
    const paint = () => {
      if (!live) return;
      const bounds = node.getBoundingClientRect();
      if (bounds.width <= 0 || bounds.height <= 0) return;
      node.width = Math.max(1, Math.round(bounds.width * devicePixelRatio));
      node.height = Math.max(1, Math.round(bounds.height * devicePixelRatio));
      try {
        const context = node.getContext("2d");
        if (!context) throw new Error("avatar_preview_unavailable");
        // Sample the display-sized preview here instead of letting CSS shrink
        // a full-resolution canvas with visibly coarse edges. File bytes stay intact.
        context.imageSmoothingQuality = "high";
        context.setTransform(node.width / candidate.width, 0, 0,
          node.height / candidate.height, 0, 0);
        candidate.draw(context);
      } catch {
        selection.previewFailed(candidate);
      }
    };
    const resize = new ResizeObserver(paint);
    resize.observe(node);
    paint();
    return () => {
      live = false;
      resize.disconnect();
      node.width = 0;
      node.height = 0;
    };
  }, [candidate, selection, visible]);

  const choose = (event: ChangeEvent<HTMLInputElement>) => {
    const file = event.currentTarget.files?.[0];
    // Clearing the native value lets the same file be chosen again without
    // keeping any reference to it in the input element.
    event.currentTarget.value = "";
    if (file) void (onChoose ? onChoose(file) : selection.choose(file));
  };

  const discard = () => {
    if (onDiscard) onDiscard();
    else selection.clear();
    input.current?.focus();
  };

  return (
    <section
      className="avatar-replacement"
      aria-labelledby={heading}
      aria-busy={busy}
    >
      <div className="avatar-replacement-heading">
        <h2 id={heading}>
          {candidate ? "预览后，确认更换" : "先选择一张图片"}
        </h2>
        <p>
          {candidate
            ? "更换前，请对比当前头像与新头像。只有确认更换后才会提交。"
            : "选择新头像，预览后确认更换。"}
        </p>
      </div>
      <div className={`avatar-replacement-pair${candidate ? "" : " single"}`}>
        <figure className="avatar-replacement-card">
          <h3>当前头像</h3>
          <div className="avatar-replacement-picture">{current}</div>
        </figure>
        {candidate && (
          <figure className="avatar-replacement-card">
            <h3>新头像</h3>
            <div className="avatar-replacement-picture candidate">
              <canvas
                ref={preview}
                className="avatar-replacement-preview"
                role="img"
                aria-label="新头像预览"
              />
            </div>
            <figcaption>
              {candidate.width} × {candidate.height} 像素 ·{" "}
              {formatLabel(candidate.mediaType)}
            </figcaption>
          </figure>
        )}
      </div>
      {busy && (
        <p className="avatar-replacement-progress" role="status">
          正在读取本地图片，完成后再展示预览。
        </p>
      )}
      {candidate && (
        <p className="visually-hidden" role="status">
          新头像预览已准备好，请对比当前头像。
        </p>
      )}
      {!busy && !candidate && !error && (
        <p className="avatar-replacement-empty">
          还没有选择图片。原头像会保留，选图本身不会更换头像。
        </p>
      )}
      <div className="avatar-replacement-field">
        <label htmlFor={field}>
          {candidate ? "重新选择图片" : "选择本地图片"}
        </label>
        <input
          ref={input}
          id={field}
          type="file"
          accept="image/png,image/jpeg,image/webp"
          onChange={choose}
          onClick={(event) => {
            if (onBeginSelection && !onBeginSelection()) event.preventDefault();
          }}
          aria-describedby={error ? `${hint} ${message}` : hint}
          aria-invalid={error ? true : undefined}
          disabled={busy || disabled}
        />
        <p className="field-hint" id={hint}>
          静态 PNG、JPEG 或 WebP；正方形 1–1024 像素；最多 2 MiB。
        </p>
        {error && (
          <p className="field-error" id={message} role="alert">
            {error}
          </p>
        )}
        {candidate && (
          <p className="avatar-replacement-selection-meta">
            已选择：{candidate.name}
            <br />
            仅在本地预览，尚未提交。
          </p>
        )}
      </div>
      {candidate && (
        <div className="avatar-replacement-actions">
          {primaryAction}
          <button
            type="button"
            className="avatar-replacement-discard"
            onClick={discard}
            disabled={disabled}
          >
            取消选择
          </button>
        </div>
      )}
    </section>
  );
}
