import { Header, Footer } from "../root";
import botanical from "../../../design/assets/halo-botanical.svg";

export function meta() {
  return [
    { title: "司籍处 · 中医药文化宣传与推广中心" },
    { name: "description", content: "了解司籍处，从这里开始办理。" },
    { name: "robots", content: "noindex" },
  ];
}

export default function Home() {
  return (
    <>
      <Header home />
      <main id="main" className="home">
        <div className="home-copy">
          <p className="eyebrow">中医药文化宣传与推广中心</p>
          <h1>
            承草木之意，
            <br />
            记日常之事。
          </h1>
          <p className="home-description">
            欢迎来到司籍处。
            <br />
            以一份认真，照料每一次办理。
          </p>
          <a className="primary" href="/authentication/codes">
            手机号登录 <span aria-hidden="true">→</span>
          </a>
          <p className="scope-note">
            <a className="quiet-link" href="/account">查看当前账户</a>
          </p>
        </div>
        <img
          className="home-art"
          src={botanical}
          width="520"
          height="520"
          alt=""
        />
      </main>
      <Footer />
    </>
  );
}
