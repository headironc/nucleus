import { createRoot } from "react-dom/client";
import { BrowserRouter, Routes, Route } from "react-router";
import { CSPProvider } from "@base-ui/react/csp-provider";
import CancellationPage from "../../app/routes/cancellation";
import { Sessions } from "../../app/authentication/session";

declare global {
  interface Window {
    cancellationSessions: Sessions;
  }
}
window.__halo = { reloadRequired: false };
window.cancellationSessions = new Sessions();
createRoot(document.getElementById("fixture")!).render(
  <CSPProvider disableStyleElements>
    <BrowserRouter>
      <Routes>
        <Route path="/account/cancellation" element={<CancellationPage />} />
        <Route path="*" element={<p>Public fixture</p>} />
      </Routes>
    </BrowserRouter>
  </CSPProvider>,
);
