import { createRoot } from "react-dom/client";
import { BrowserRouter, Routes, Route } from "react-router";
import { CSPProvider } from "@base-ui/react/csp-provider";
import NumberPage from "../../app/routes/workforce-number";
import { Sessions } from "../../app/authentication/session";

window.__halo = { reloadRequired: false };
window.accountSessions = new Sessions();
createRoot(document.getElementById("fixture")!).render(
  <CSPProvider disableStyleElements>
    <BrowserRouter>
      <Routes>
        <Route path="/workforce/number" element={<NumberPage />} />
        <Route path="*" element={<p>Public fixture</p>} />
      </Routes>
    </BrowserRouter>
  </CSPProvider>,
);
