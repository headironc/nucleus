import { createRoot } from "react-dom/client";
import { CSPProvider } from "@base-ui/react/csp-provider";
import { AvatarSelection } from "../../app/avatar/candidate";
import { AvatarSelectionPanel } from "../../app/avatar/panel";
import "../../app/styles.css";

declare global {
  interface Window {
    avatarSelection: AvatarSelection;
    avatarPreview: {
      decoded: number;
      drawn: number;
      closed: number;
      lastBlob: Blob | null;
    };
  }
}

const preview = {
  decoded: 0,
  drawn: 0,
  closed: 0,
  lastBlob: null as Blob | null,
};

const selection = new AvatarSelection({
  async decode(blob) {
    preview.lastBlob = blob;
    const bitmap = await createImageBitmap(blob);
    preview.decoded++;
    return {
      width: bitmap.width,
      height: bitmap.height,
      draw(context) {
        context.drawImage(bitmap, 0, 0);
        preview.drawn++;
      },
      close() {
        bitmap.close();
        preview.closed++;
      },
    };
  },
});

window.avatarSelection = selection;
window.avatarPreview = preview;

createRoot(document.getElementById("fixture")!).render(
  <CSPProvider disableStyleElements>
    <main className="authentication-layout compact">
      <AvatarSelectionPanel
        selection={selection}
        current={
          <img
            src="/current.webp"
            width={96}
            height={96}
            alt="当前头像"
          />
        }
      />
    </main>
  </CSPProvider>,
);
