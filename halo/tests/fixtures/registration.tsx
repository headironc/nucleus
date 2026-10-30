import { createRoot } from "react-dom/client";
import { BrowserRouter, Routes, Route } from "react-router";
import { CSPProvider } from "@base-ui/react/csp-provider";
import RegisterPage from "../../app/routes/register";
import AccountPage from "../../app/routes/account";
import { Sessions } from "../../app/authentication/session";

declare global {
  interface Window {
    registrationSessions: Sessions;
  }
}
window.__halo = { reloadRequired: false };
window.registrationSessions = new Sessions();
createRoot(document.getElementById("fixture")!).render(
  <CSPProvider disableStyleElements>
    <BrowserRouter>
      <Routes>
        <Route path="/authentication/register" element={<RegisterPage />} />
        <Route path="/account" element={<AccountPage />} />
        <Route path="*" element={<p>Public fixture</p>} />
      </Routes>
    </BrowserRouter>
  </CSPProvider>,
);
