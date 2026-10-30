import { createRoot } from "react-dom/client";
import { BrowserRouter, Routes, Route } from "react-router";
import { CSPProvider } from "@base-ui/react/csp-provider";
import AdmissionPage from "../../app/routes/workforce-admission";
import AccountPage from "../../app/routes/account";
import WorkforcePage from "../../app/routes/workforce";
import { Sessions } from "../../app/authentication/session";
declare global {
  interface Window {
    admissionSessions: Sessions;
  }
}
window.__halo = { reloadRequired: false };
window.admissionSessions = new Sessions();
createRoot(document.getElementById("fixture")!).render(
  <CSPProvider disableStyleElements>
    <BrowserRouter>
      <Routes>
        <Route path="/workforce/grants/new" element={<AdmissionPage />} />
        <Route path="/account" element={<AccountPage />} />
        <Route path="/workforce" element={<WorkforcePage />} />
        <Route path="*" element={<p>公开页面</p>} />
      </Routes>
    </BrowserRouter>
  </CSPProvider>,
);
