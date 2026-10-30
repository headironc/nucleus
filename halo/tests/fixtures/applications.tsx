import { createRoot } from "react-dom/client";
import { BrowserRouter, Routes, Route } from "react-router";
import { CSPProvider } from "@base-ui/react/csp-provider";
import WorkforceApplicationsPage from "../../app/routes/workforce-applications";
import { Sessions } from "../../app/authentication/session";
import "../../app/styles.css";
declare global {
  interface Window {
    applicationSessions: Sessions;
  }
}
window.__halo = { reloadRequired: false };
window.applicationSessions = new Sessions();
createRoot(document.getElementById("fixture")!).render(
  <CSPProvider disableStyleElements>
    <BrowserRouter>
      <Routes>
        <Route
          path="/workforce/applications/*"
          element={<WorkforceApplicationsPage />}
        />
        <Route path="*" element={<p>Public fixture</p>} />
      </Routes>
    </BrowserRouter>
  </CSPProvider>,
);
