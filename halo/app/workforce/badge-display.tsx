import { useId, useRef, useState } from "react";
import { Button } from "@base-ui/react/button";
import { Dialog } from "@base-ui/react/dialog";
import type { BadgeImage } from "./badge-image";
import { applicationBadge, type Application } from "../protocol/applications";

const RENDER_ERRORS = {
  missing_glyph: [
    "部分文字无法显示，工牌图片未生成。",
    "请联系管理人员核对。若需调整申请资料或版式，请取消后重新申请。",
  ],
  text_overflow: [
    "文字超出工牌区域，工牌图片未生成。",
    "请联系管理人员核对。原申请无法容纳时，需取消后重新申请。",
  ],
  dependency_unavailable: [
    "所需资源暂时不可用，工牌图片未生成。",
    "请稍后重试；若仍无法生成，请联系维护人员。",
  ],
};

export function BadgeProduction({
  application,
  manager = false,
  eligible = true,
  generating = false,
  retryAt = 0,
  now,
  media,
  download = { kind: "idle" },
  generate,
  submit,
  failed,
}: {
  application: Application;
  manager?: boolean;
  eligible?: boolean;
  generating?: boolean;
  retryAt?: number;
  now: number;
  media?: BadgeMedia;
  download?: BadgeDownload;
  generate?(): void;
  submit(): void;
  failed(): void;
}) {
  const badge = applicationBadge(application);
  if (!badge && application.status !== "approved") return null;
  const retained = application.status === "cancelled";
  const error =
    application.status === "approved" && application.renderError
      ? RENDER_ERRORS[application.renderError]
      : undefined;
  const wait = Math.max(0, Math.ceil((retryAt - now) / 1000));
  return (
    <section
      className="review-panel review-panel-content badge-production"
      aria-busy={generating}
    >
      <h2>工牌图片</h2>
      <h3>
        {badge
          ? retained
            ? "工牌图片已保留"
            : "工牌图片已生成"
          : generating
            ? "正在生成工牌图片"
            : error
              ? "工牌图片未生成"
              : "尚未生成工牌图片"}
      </h3>
      <p>
        {badge
          ? retained
            ? "已保存的图片仍可查看和下载。"
            : "可预览或下载已保存的图片。"
          : generating
            ? "请稍候，正在等待本次生成结果。"
            : manager
              ? eligible
                ? "申请已审核通过，可按提交时资料生成工牌图片。"
                : "申请人的账户或员工服务资格已失效，当前无法生成。"
              : "申请已审核通过，等待管理人员生成工牌图片。"}
      </p>
      {error && (
        <div className="badge-render-error" role="status">
          <p>{error[0]}</p>
          <p>{manager ? error[1] : "请联系管理人员处理。"}</p>
        </div>
      )}
      {!badge && manager && (
        <div className="badge-actions">
          <Button
            className="primary"
            disabled={generating || !eligible || wait > 0}
            onClick={generate}
          >
            {generating
              ? "正在生成…"
              : wait
                ? `${wait} 秒后可生成`
                : error
                  ? "重试生成"
                  : "生成工牌图片"}
          </Button>
        </div>
      )}
      {badge && media?.kind === "ready" && (
        <BadgeActions
          image={media.image}
          download={download}
          now={now}
          submit={submit}
          failed={failed}
        />
      )}
      {badge && media?.kind === "failed" && (
        <p>文件暂时无法读取，请在成品预览处重新读取。</p>
      )}
    </section>
  );
}

export type BadgeMedia =
  | { kind: "loading" }
  | { kind: "failed"; retryAt: number }
  | { kind: "ready"; image: BadgeImage };
export type BadgeDownload =
  | { kind: "idle" | "pending" }
  | { kind: "failed"; message: string; retryAt: number };

export function BadgePreview({
  media,
  retained,
  now,
  retry,
  failed,
}: {
  media: BadgeMedia;
  retained: boolean;
  now: number;
  retry(): void;
  failed(): void;
}) {
  const heading = useId();
  const wait =
    media.kind === "failed"
      ? Math.max(0, Math.ceil((media.retryAt - now) / 1000))
      : 0;
  return (
    <section className="review-panel badge-preview" aria-labelledby={heading}>
      <header className="review-panel-heading badge-preview-heading">
        <h2 id={heading}>成品预览</h2>
        <span>{retained ? "已保留" : "已生成"}</span>
      </header>
      <figure>
        {media.kind === "ready" ? (
          <div className="badge-stage">
            <img
              src={media.image.url}
              width={media.image.width}
              height={media.image.height}
              alt="已保存的工牌图片"
              onError={failed}
            />
          </div>
        ) : (
          <div
            className="badge-media-message"
            aria-busy={media.kind === "loading"}
          >
            <p role="status">
              {media.kind === "loading"
                ? "正在读取工牌图片…"
                : "工牌文件暂时无法读取"}
            </p>
            {media.kind === "failed" && (
              <>
                <p>工牌图片已生成，请重新读取文件。</p>
                <Button className="primary" disabled={wait > 0} onClick={retry}>
                  {wait ? `${wait} 秒后可重新读取` : "重新读取"}
                </Button>
              </>
            )}
          </div>
        )}
        <figcaption>
          {retained
            ? "取消申请后，已生成的图片仍保留。"
            : "按本次申请提交时的资料制作。领取地点不印在工牌上。"}
        </figcaption>
      </figure>
    </section>
  );
}

function DownloadButton({
  download,
  now,
  submit,
}: {
  download: BadgeDownload;
  now: number;
  submit(): void;
}) {
  const wait =
    download.kind === "failed"
      ? Math.max(0, Math.ceil((download.retryAt - now) / 1000))
      : 0;
  return (
    <Button
      className="primary"
      aria-busy={download.kind === "pending"}
      disabled={download.kind === "pending" || wait > 0}
      onClick={submit}
    >
      {download.kind === "pending"
        ? "准备下载…"
        : wait
          ? `${wait} 秒后可重新下载`
          : download.kind === "failed"
            ? "重新下载"
            : "下载图片"}
    </Button>
  );
}

/** Preview reuses the current image; every download is delegated as a new read. */
export function BadgeActions({
  image,
  download,
  now,
  submit,
  failed,
}: {
  image: BadgeImage;
  download: BadgeDownload;
  now: number;
  submit(): void;
  failed(): void;
}) {
  const [expanded, setExpanded] = useState<BadgeImage>();
  const trigger = useRef<HTMLButtonElement>(null);
  const heading = useRef<HTMLHeadingElement>(null);
  return (
    <>
      <div className="badge-actions">
        <Button
          className="secondary"
          ref={trigger}
          onClick={() => setExpanded(image)}
        >
          预览图片
        </Button>
        <DownloadButton download={download} now={now} submit={submit} />
      </div>
      {download.kind === "failed" && (
        <p className="badge-download-error" role="status">
          {download.message}
        </p>
      )}
      <Dialog.Root
        open={expanded === image}
        onOpenChange={(open) => setExpanded(open ? image : undefined)}
      >
        <Dialog.Portal>
          <Dialog.Backdrop className="application-backdrop" />
          <Dialog.Popup
            className="review-dialog badge-lightbox"
            initialFocus={heading}
            finalFocus={trigger}
          >
            <div className="badge-lightbox-heading">
              <Dialog.Title ref={heading} tabIndex={-1}>
                工牌图片
              </Dialog.Title>
              <Button
                className="secondary"
                onClick={() => setExpanded(undefined)}
              >
                关闭
              </Button>
            </div>
            <Dialog.Description className="badge-lightbox-description">
              本次申请已保存的工牌图片。
            </Dialog.Description>
            <div className="badge-stage">
              <img
                src={image.url}
                width={image.width}
                height={image.height}
                alt="已保存的工牌图片"
                onError={failed}
              />
            </div>
            <div className="badge-lightbox-actions">
              {download.kind === "failed" && (
                <p role="status">{download.message}</p>
              )}
              <DownloadButton download={download} now={now} submit={submit} />
            </div>
          </Dialog.Popup>
        </Dialog.Portal>
      </Dialog.Root>
    </>
  );
}
