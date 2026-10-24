import { createRoot } from "react-dom/client";
import { BrowserRouter, Routes, Route } from "react-router";
import { CSPProvider } from "@base-ui/react/csp-provider";
import LogoutPage from "../../app/routes/logout";
import LogoutAllPage from "../../app/routes/logout-all";
import { Sessions } from "../../app/authentication/session";

declare global {
  interface Window {
    logoutSessions: Sessions;
  }
}
window.__halo = { reloadRequired: false };
window.logoutSessions = new Sessions();
createRoot(document.getElementById("fixture")!).render(
  <CSPProvider disableStyleElements>
    <BrowserRouter>
      <Routes>
        <Route path="/sessions/logout" element={<LogoutPage />} />
        <Route path="/sessions/logout-all" element={<LogoutAllPage />} />
        <Route path="*" element={<p>Public fixture</p>} />
      </Routes>
    </BrowserRouter>
  </CSPProvider>,
);
