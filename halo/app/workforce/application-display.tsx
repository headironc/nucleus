import { useLayoutEffect, useRef } from "react";
import type {
  Application,
  ApplicationSnapshot,
} from "../protocol/applications";
import type { ApplicationArtwork } from "./artwork";

export const APPLICATION_KINDS = {
  issuance: "新办",
  duplication: "增办",
  replacement: "换证",
};
export const APPLICATION_STATUSES = {
  submitted: "待审核",
  approved: "审核通过",
  rejected: "审核未通过",
  cancelled: "已取消",
};
const dateFormat = new Intl.DateTimeFormat("zh-CN", {
  timeZone: "Asia/Shanghai",
  year: "numeric",
  month: "2-digit",
  day: "2-digit",
  hour: "2-digit",
  minute: "2-digit",
});
export function applicationDate(value: string) {
  return dateFormat.format(new Date(value));
}

export function ReviewFacts({ application }: { application: Application }) {
  if (application.status === "submitted")
    return <p>申请已提交，请等待审核。</p>;
  if (!application.reviewedAt) return <p>申请在审核前已取消。</p>;
  return (
    <>
      <dl className="application-snapshot review-facts">
        <div>
          <dt>审核人</dt>
          <dd>{application.reviewerName}</dd>
        </div>
        <div>
          <dt>审核时间</dt>
          <dd>
            <time dateTime={application.reviewedAt}>
              {applicationDate(application.reviewedAt)}
            </time>
          </dd>
        </div>
      </dl>
      {application.status === "rejected" && (
        <div className="review-reason">
          <h3>未通过原因</h3>
          <p>{application.rejectionReason}</p>
        </div>
      )}
    </>
  );
}

export function SnapshotDetails({
  snapshot,
}: {
  snapshot: ApplicationSnapshot;
}) {
  return (
    <dl className="application-snapshot">
      <div>
        <dt>姓名</dt>
        <dd>{snapshot.name}</dd>
      </div>
      <div>
        <dt>工号</dt>
        <dd>{snapshot.printedNumber}</dd>
      </div>
      <div>
        <dt>科室与职务</dt>
        <dd>{snapshot.department.printed}</dd>
      </div>
      <div>
        <dt>职称</dt>
        <dd>{snapshot.printedTitle}</dd>
      </div>
      <div>
        <dt>领取地点</dt>
        <dd>{snapshot.pickup}</dd>
      </div>
    </dl>
  );
}

export function PreviewFigure({
  artwork,
  failed,
  caption = "请核对工牌文字、照片和领取地点。",
}: {
  artwork: ApplicationArtwork;
  failed(): void;
  caption?: string;
}) {
  const canvas = useRef<HTMLCanvasElement>(null);
  useLayoutEffect(() => {
    const node = canvas.current;
    if (!node) return;
    let live = true;
    const paint = () => {
      if (!live) return;
      const width = node.getBoundingClientRect().width;
      if (!width) return;
      try {
        artwork.paint(node, Math.min(1600, width * devicePixelRatio));
      } catch {
        failed();
      }
    };
    const observer = new ResizeObserver(paint);
    observer.observe(node);
    paint();
    return () => {
      live = false;
      observer.disconnect();
      node.width = 0;
      node.height = 0;
    };
  }, [artwork]);
  return (
    <figure className="application-preview">
      <canvas ref={canvas} role="img" aria-label="工牌预览" />
      <figcaption>{caption}</figcaption>
    </figure>
  );
}
