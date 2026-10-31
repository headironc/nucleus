import { useState } from "react";
import { createRoot } from "react-dom/client";
import { CSPProvider } from "@base-ui/react/csp-provider";
import { BadgeImage } from "../../app/workforce/badge-image";
import {
  BadgeActions,
  BadgePreview,
  type BadgeDownload,
  type BadgeMedia,
} from "../../app/workforce/badge-display";
import "../../app/styles.css";

declare global {
  interface Window {
    badgeFixture: {
      ready(): Promise<void>;
      downloadFailed(): void;
      mediaFailed(): void;
      hide(): void;
      downloads: number;
      reads: number;
    };
  }
}

// Isolated display fixture. It has no application API or authorization substitute.
function Fixture() {
  const [media, setMedia] = useState<BadgeMedia>({ kind: "loading" });
  const [download, setDownload] = useState<BadgeDownload>({ kind: "idle" });
  const [visible, setVisible] = useState(true);
  window.badgeFixture = {
    downloads: window.badgeFixture?.downloads ?? 0,
    reads: window.badgeFixture?.reads ?? 0,
    async ready() {
      const canvas = document.createElement("canvas");
      canvas.width = 1004;
      canvas.height = 638;
      const context = canvas.getContext("2d")!;
      context.fillStyle = "#e5eadf";
      context.fillRect(0, 0, canvas.width, canvas.height);
      context.fillStyle = "#30483a";
      context.font = "48px sans-serif";
      context.fillText("Isolated image display fixture", 60, 320);
      const blob = await new Promise<Blob>((resolve) =>
        canvas.toBlob((value) => resolve(value!), "image/webp"),
      );
      const image = await BadgeImage.open(blob, new AbortController().signal);
      if (!image) throw new Error("Expected fixture image");
      if (media.kind === "ready") media.image.close();
      setMedia({ kind: "ready", image });
    },
    downloadFailed() {
      setDownload({
        kind: "failed",
        message: "暂时无法取得下载文件，请重新下载。",
        retryAt: 0,
      });
    },
    mediaFailed() {
      if (media.kind === "ready") media.image.close();
      setMedia({ kind: "failed", retryAt: 0 });
    },
    hide() {
      if (media.kind === "ready") media.image.close();
      setVisible(false);
    },
  };
  if (!visible) return <p>Hidden fixture</p>;
  return (
    <main className="review-layout">
      <h1>成品显示组件</h1>
      <section className="review-panel review-panel-content">
        <h2>工牌图片</h2>
        <p>工牌图片已生成</p>
        {media.kind === "ready" && (
          <BadgeActions
            image={media.image}
            download={download}
            now={Date.now()}
            failed={() => window.badgeFixture.mediaFailed()}
            submit={() => {
              window.badgeFixture.downloads++;
              setDownload({ kind: "pending" });
            }}
          />
        )}
      </section>
      <BadgePreview
        media={media}
        retained={false}
        now={Date.now()}
        failed={() => window.badgeFixture.mediaFailed()}
        retry={() => {
          window.badgeFixture.reads++;
        }}
      />
    </main>
  );
}

createRoot(document.getElementById("fixture")!).render(
  <CSPProvider disableStyleElements>
    <Fixture />
  </CSPProvider>,
);
