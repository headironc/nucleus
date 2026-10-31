import { createRoot } from "react-dom/client";
import { createBrowserRouter, RouterProvider } from "react-router";
import { CSPProvider } from "@base-ui/react/csp-provider";
import Home from "../../app/routes/home";
import CodesPage from "../../app/routes/codes";
import LoginPage from "../../app/routes/login";
import RegisterPage from "../../app/routes/register";
import { ErrorBoundary } from "../../app/root";
import { Sessions } from "../../app/authentication/session";
import { receiveCode } from "../../app/authentication/material";

declare global {
  interface Window {
    seedAuthentication: (stage: "login" | "register") => Promise<void>;
  }
}
window.__halo = { reloadRequired: false };
window.seedAuthentication = async (stage) => {
  const operation = crypto.randomUUID();
  const now = Date.now();
  const expiresAt = new Date(now + 600_000).toISOString();
  if (stage === "login") {
    receiveCode(
      sessionStorage,
      operation,
      "13900000000",
      {
        id: crypto.randomUUID(),
        delivery: "sent",
        expiresAt,
      },
      now,
    );
  } else {
    const sessions = new Sessions();
    const attempt = await sessions.begin(operation);
    sessionStorage.setItem(
      "halo.r1",
      JSON.stringify({
        version: 1,
        kind: "registration",
        attempt,
        registration: {
          id: "019354fd-933c-7e6a-8912-345678901234",
          token: "A".repeat(43),
          expiresAt,
        },
        createdAt: now,
        expiresAt: now + 600_000,
      }),
    );
    await sessions.close();
  }
};
const router = createBrowserRouter([
  { path: "/", element: <Home /> },
  { path: "/authentication/codes", element: <CodesPage /> },
  { path: "/authentication/login", element: <LoginPage /> },
  { path: "/authentication/register", element: <RegisterPage /> },
  {
    path: "/failure",
    loader: () => {
      throw new Error("Fixture failure");
    },
    errorElement: <ErrorBoundary />,
  },
  {
    path: "*",
    loader: () => {
      throw new Response(null, { status: 404 });
    },
    errorElement: <ErrorBoundary />,
  },
]);
createRoot(document.getElementById("fixture")!).render(
  <CSPProvider disableStyleElements>
    <RouterProvider router={router} />
  </CSPProvider>,
);
