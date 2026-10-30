import { createRoot } from "react-dom/client";
import { BrowserRouter, Routes, Route } from "react-router";
import { CSPProvider } from "@base-ui/react/csp-provider";
import MobileChangePage from "../../app/routes/mobile-change";
import AccountPage from "../../app/routes/account";
import { Sessions } from "../../app/authentication/session";

declare global {
  interface Window {
    mobileSessions: Sessions;
  }
}

window.__halo = { reloadRequired: false };
window.mobileSessions = new Sessions();
createRoot(document.getElementById("fixture")!).render(
  <CSPProvider disableStyleElements>
    <BrowserRouter>
      <Routes>
        <Route path="/account/mobile" element={<MobileChangePage />} />
        <Route path="/account" element={<AccountPage />} />
        <Route path="*" element={<p>Public fixture</p>} />
      </Routes>
    </BrowserRouter>
  </CSPProvider>,
);
