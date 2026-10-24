import { CSPProvider } from "@base-ui/react/csp-provider";
import { useEffect } from "react";
import {
  Links,
  Meta,
  Outlet,
  Scripts,
  isRouteErrorResponse,
  useRouteError,
} from "react-router";
import logo from "../../design/assets/logo.png";
import bootstrap from "./bootstrap.js?url";
import { expireMaterial } from "./codes/material";
import { maintainMaterial } from "./authentication/material";
import { maintain as maintainRegistration } from "./registration/material";
import { maintain as maintainMobileChange } from "./mobile-change/material";
import "./styles.css";

export function Layout({ children }: { children: React.ReactNode }) {
  useEffect(() => {
    const check = () => {
      // Accessing sessionStorage itself can throw when browser storage is denied.
      try {
        expireMaterial(sessionStorage, Date.now());
        maintainMaterial(sessionStorage, Date.now());
        maintainRegistration(sessionStorage, Date.now());
        maintainMobileChange(sessionStorage, Date.now());
      } catch {
        // The next lifecycle event retries without interrupting the public page.
      }
    };
    const resume = () => {
      if (!document.hidden) check();
    };
    check();
    const timer = setInterval(check, 1000);
    window.addEventListener("pageshow", check);
    window.addEventListener("focus", check);
    document.addEventListener("visibilitychange", resume);
    return () => {
      clearInterval(timer);
      window.removeEventListener("pageshow", check);
      window.removeEventListener("focus", check);
      document.removeEventListener("visibilitychange", resume);
    };
  }, []);
  return (
    <html lang="zh-CN">
      <head>
        <meta charSet="utf-8" />
        <meta name="viewport" content="width=device-width, initial-scale=1" />
        <script src={bootstrap} />
        <link rel="icon" href={logo} />
        <Meta />
        <Links />
      </head>
      <body>
        <CSPProvider disableStyleElements>{children}</CSPProvider>
        <Scripts />
      </body>
    </html>
  );
}

export function Header({
  home = false,
  navigation,
  onBrandClick,
}: {
  home?: boolean;
  navigation?: React.ReactNode;
  onBrandClick?: (event: React.MouseEvent<HTMLAnchorElement>) => void;
}) {
  return (
    <>
      <a className="skip-link" href="#main">
        跳到主要内容
      </a>
      <header className="site-header">
        <div className="header-inner">
          <a
            className="brand"
            href="/"
            aria-label="司籍处首页"
            onClick={onBrandClick}
          >
            <img src={logo} width="52" height="52" alt="" />
            <span>
              <strong>司籍处</strong>
              <small>中医药文化宣传与推广中心</small>
            </span>
          </a>
          {navigation}
          {!home && !navigation && (
            <a className="quiet-link" href="/">
              ← 返回首页
            </a>
          )}
        </div>
      </header>
    </>
  );
}

export function Footer() {
  return (
    <footer className="site-footer">
      <span>司籍处</span>
      <span>一事一记 · 办理有据</span>
    </footer>
  );
}

export default function App() {
  return <Outlet />;
}

export function ErrorBoundary() {
  const error = useRouteError();
  const missing = isRouteErrorResponse(error) && error.status === 404;
  return (
    <>
      <title>{`${missing ? "页面未找到" : "暂时无法加载"} · 司籍处`}</title>
      <meta name="robots" content="noindex" />
      <Header />
      <main id="main" className="error-page">
        <p className="eyebrow">司籍处 · 网站</p>
        <h1>{missing ? "这里还没有页面" : "页面暂时无法加载"}</h1>
        <p>
          {missing
            ? "请检查链接，或返回首页。"
            : "请稍后重新加载。已保存的办理信息仍留在当前标签页。"}{" "}
        </p>
        <a className="primary" href="/">
          返回首页
        </a>
      </main>
      <Footer />
    </>
  );
}
