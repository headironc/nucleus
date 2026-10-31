import * as page from "../logout/page";
import * as hook from "../logout/use-logout";
import * as root from "../root";

export function meta() {
  return [
    { title: `退出登录 · 司籍处` },
    { name: `robots`, content: `noindex` },
  ];
}

export const Content = page.Content;

export default function LogoutPage() {
  const controller = hook.useLogout(`current`);
  return (
    <>
      <root.Header />
      <main id={`main`}>
        {controller ? (
          <page.Content controller={controller} />
        ) : (
          <div className={`loading-shell`}>
            <p>正在打开退出页面…</p>
          </div>
        )}
      </main>
      <root.Footer />
    </>
  );
}
