import { createRoot } from "react-dom/client";
import { BrowserRouter, Routes, Route } from "react-router";
import { CSPProvider } from "@base-ui/react/csp-provider";
import AccountPage from "../../app/routes/account";
import AffiliationPage from "../../app/routes/affiliation";
import { Sessions } from "../../app/authentication/session";

declare global {
  interface Window {
    accountSessions: Sessions;
  }
}

window.__halo = { reloadRequired: false };
window.accountSessions = new Sessions();
createRoot(document.getElementById("fixture")!).render(
  <CSPProvider disableStyleElements>
    <BrowserRouter>
      <Routes>
        <Route path="/account" element={<AccountPage />} />
        <Route path="/account/affiliation" element={<AffiliationPage />} />
        <Route path="*" element={<p>Public fixture</p>} />
      </Routes>
    </BrowserRouter>
  </CSPProvider>,
);
