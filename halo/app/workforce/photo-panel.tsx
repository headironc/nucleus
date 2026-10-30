import { useId, useLayoutEffect, useRef, useSyncExternalStore } from "react";
import { ApplicationPhoto, type PhotoError, type PhotoFrame } from "./photo";

const COPY: Record<PhotoError, string> = {
  size: "照片超过 8 MiB，请选择较小的照片后重新裁剪。",
  format: "请选择静态 JPEG、PNG 或 WebP 照片。",
  animation: "不支持动画图片，请选择静态照片。",
  dimensions: "照片每边最多 4096 像素，总像素最多 1600 万。",
  crop: "裁剪区域太小，无法保持照片比例。请选择更大的照片后重新裁剪。",
  damaged: "无法完整读取这张照片，请检查文件或重新选择。",
  unavailable: "暂时无法准备照片，请重新选择，或换用支持图片处理的浏览器。",
};

export function ApplicationPhotoPanel({
  photo,
  frame,
  disabled = false,
  backdrop,
}: {
  photo: ApplicationPhoto;
  frame: PhotoFrame;
  disabled?: boolean;
  backdrop?: (context: CanvasRenderingContext2D, output: PhotoFrame) => void;
}) {
  const state = useSyncExternalStore(
    photo.subscribe,
    photo.snapshot,
    photo.snapshot,
  );
  const canvas = useRef<HTMLCanvasElement>(null);
  const input = useRef<HTMLInputElement>(null);
  const id = useId();
  const editing = state.kind === "editing" ? state : null;
  const busy = state.kind === "reading" || !!editing?.preparing;
  useLayoutEffect(() => {
    photo.reframe(frame);
  }, [
    photo,
    frame.width,
    frame.height,
    editing?.frame.width,
    editing?.frame.height,
  ]);
  useLayoutEffect(() => {
    const node = canvas.current;
    if (!node || !editing) return;
    const paint = () => {
      const width = node.getBoundingClientRect().width;
      if (!width) return;
      node.width = Math.max(1, Math.round(width * devicePixelRatio));
      node.height = Math.max(
        1,
        Math.round((node.width * editing.frame.height) / editing.frame.width),
      );
      try {
        const context = node.getContext("2d");
        if (!context) throw new Error("photo_context_unavailable");
        context.imageSmoothingQuality = "high";
        photo.draw(context, node);
        if (backdrop) {
          context.save();
          context.globalCompositeOperation = "destination-over";
          backdrop(context, node);
          context.restore();
        }
      } catch {
        photo.previewFailed();
      }
    };
    const observer = new ResizeObserver(paint);
    observer.observe(node);
    paint();
    return () => {
      observer.disconnect();
      node.width = 0;
      node.height = 0;
    };
  }, [photo, editing, backdrop]);

  return (
    <section
      className="application-photo"
      aria-labelledby={`${id}-title`}
      aria-busy={busy}
    >
      <div>
        <h2 id={`${id}-title`}>工牌照片</h2>
        <p>选择本人照片，调整画面后确认裁剪效果。</p>
      </div>
      <label htmlFor={`${id}-file`}>
        {editing ? "重新选择照片" : "选择照片"}
      </label>
      <input
        id={`${id}-file`}
        ref={input}
        type="file"
        accept="image/jpeg,image/png,image/webp"
        disabled={disabled || busy}
        aria-describedby={`${id}-hint${state.kind === "error" ? ` ${id}-error` : ""}`}
        aria-invalid={state.kind === "error" || undefined}
        onChange={(event) => {
          const file = event.currentTarget.files?.[0];
          event.currentTarget.value = "";
          if (file) void photo.choose(file, frame);
        }}
      />
      <p className="field-hint" id={`${id}-hint`}>
        静态 JPEG、PNG 或 WebP；最多 8 MiB，每边最多 4096 像素，总像素最多 1600
        万。工牌照片与账户头像分别使用。
      </p>
      {state.kind === "reading" && <p role="status">正在读取照片，请稍候。</p>}
      {state.kind === "error" && (
        <p id={`${id}-error`} className="field-error" role="alert">
          {COPY[state.error]}
        </p>
      )}
      {editing && (
        <div className="application-photo-editor">
          <figure>
            <canvas ref={canvas} role="img" aria-label="工牌照片裁剪预览" />
            <figcaption>
              画面内的内容会用于工牌。请检查人物完整、清晰，比例自然。
            </figcaption>
          </figure>
          <fieldset disabled={disabled || busy}>
            <legend>调整照片</legend>
            {(
              [
                ["zoom", "放大", 1, 4, 0.01],
                ["horizontal", "左右位置", 0, 1, 0.01],
                ["vertical", "上下位置", 0, 1, 0.01],
              ] as const
            ).map(([key, label, min, max, step]) => (
              <label key={key} htmlFor={`${id}-${key}`}>
                <span>{label}</span>
                <input
                  id={`${id}-${key}`}
                  type="range"
                  min={min}
                  max={max}
                  step={step}
                  value={editing.crop[key]}
                  onChange={(event) =>
                    photo.adjust({
                      ...editing.crop,
                      [key]: Number(event.currentTarget.value),
                    })
                  }
                />
              </label>
            ))}
            <div className="application-photo-actions">
              <button
                type="button"
                className="primary"
                onClick={() => void photo.confirm()}
                disabled={!!editing.confirmed}
              >
                {busy
                  ? "正在准备照片"
                  : editing.confirmed
                    ? "已确认裁剪效果"
                    : "确认裁剪效果"}
              </button>
              <button
                type="button"
                className="secondary"
                onClick={() => {
                  photo.clear();
                  input.current?.focus();
                }}
              >
                移除照片
              </button>
            </div>
          </fieldset>
          <p role="status" className="application-photo-note">
            {editing.confirmed
              ? "裁剪效果已确认。继续填写并预览完整工牌。"
              : "调整照片不会提交申请，请确认当前裁剪效果。"}
          </p>
        </div>
      )}
    </section>
  );
}
