import * as page from "../logout/page";
import * as hook from "../logout/use-logout";
import * as root from "../root";

export function meta() {
  return [
    { title: `账户管理 · 司籍处` },
    { name: `robots`, content: `noindex` },
  ];
}

export default function LogoutAllPage() {
  const controller = hook.useLogout(`all`);
  return (
    <>
      <root.Header />
      <main id={`main`}>
        {controller ? (
          <page.Content controller={controller} scope={`all`} />
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
