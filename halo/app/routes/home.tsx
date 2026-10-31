import { Header, Footer } from "../root";
import botanical from "../../../design/assets/halo-botanical.svg";

export function meta() {
  return [
    { title: "司籍处 · 中医药文化宣传与推广中心" },
    {
      name: "description",
      content: "登录个人账户，查看个人资料和员工服务状态。",
    },
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
          <h1>个人账户与员工服务</h1>
          <p className="home-description">
            使用手机号登录后，可查看个人资料和员工服务状态。
          </p>
          <a className="primary" href="/authentication/codes">
            手机号登录 <span aria-hidden="true">→</span>
          </a>
          <p className="scope-note">
            <a className="quiet-link" href="/account">
              打开个人账户
            </a>
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
