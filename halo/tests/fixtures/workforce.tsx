import { createRoot } from "react-dom/client";
import { BrowserRouter, Routes, Route } from "react-router";
import { CSPProvider } from "@base-ui/react/csp-provider";
import WorkforcePage from "../../app/routes/workforce";
import TerminationPage from "../../app/routes/workforce-termination";
import AccountPage from "../../app/routes/account";
import { Sessions } from "../../app/authentication/session";

declare global {
  interface Window {
    workforceSessions: Sessions;
  }
}
window.__halo = { reloadRequired: false };
window.workforceSessions = new Sessions();
createRoot(document.getElementById("fixture")!).render(
  <CSPProvider disableStyleElements>
    <BrowserRouter>
      <Routes>
        <Route path="/workforce" element={<WorkforcePage />} />
        <Route path="/workforce/termination" element={<TerminationPage />} />
        <Route path="/account" element={<AccountPage />} />
        <Route path="*" element={<p>Public fixture</p>} />
      </Routes>
    </BrowserRouter>
  </CSPProvider>,
);
