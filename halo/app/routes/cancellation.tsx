import * as page from "../cancellation/page";
import { useCancellation } from "../cancellation/use-cancellation";
import * as root from "../root";

export function meta() {
  return [
    { title: "注销账户 · 司籍处" },
    { name: "robots", content: "noindex" },
  ];
}

export default function CancellationPage() {
  const controller = useCancellation();
  return (
    <>
      <root.Header />
      <main id="main">
        {controller ? (
          <page.Content controller={controller} />
        ) : (
          <div className="loading-shell">
            <p>正在打开注销页面…</p>
          </div>
        )}
      </main>
      <root.Footer />
    </>
  );
}
