import { createRoot } from "react-dom/client";
import { CSPProvider } from "@base-ui/react/csp-provider";
import { ApplicationPhoto } from "../../app/workforce/photo";
import { ApplicationPhotoPanel } from "../../app/workforce/photo-panel";
import "../../app/styles.css";

declare global { interface Window { applicationPhoto: ApplicationPhoto; } }
const photo = new ApplicationPhoto();
window.applicationPhoto = photo;
document.addEventListener("visibilitychange", () => { if (document.hidden) photo.clear(); });
window.addEventListener("pagehide", () => photo.clear());
createRoot(document.getElementById("fixture")!).render(<CSPProvider disableStyleElements>
  <main className="application-layout">
    <p className="eyebrow">员工服务 / 工牌申请</p>
    <h1>申请工牌</h1>
    <p>先选择照片并确认裁剪效果。</p>
    <ApplicationPhotoPanel photo={photo} frame={location.pathname === "/micrometers" ? { width: 23589, height: 34123 } : { width: 3, height: 4 }} />
  </main>
</CSPProvider>);
