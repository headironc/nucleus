import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "../../halo/node_modules/@playwright/test";

// Static source excerpts for visual review, not a WXML runtime or a platform test.
const root = fileURLToPath(new URL("../", import.meta.url));
const output = process.env.ORBIT_VISUAL_OUTPUT;
if (!output) throw new Error("An explicit persistent visual evidence directory is required");
await mkdir(output, { recursive: true });
const source = await readFile(join(root, "src/pages/account/index.wxml"), "utf8");
const css = (await readFile(join(root, "src/app.wxss"), "utf8"))
  .replace(/^page\s*\{/gm, "body {")
  .replace(/(-?[\d.]+)rpx/g, (_, value) => `calc(${Number(value)} * 100vw / 750)`);
function panel(name: string) {
  const start = `<!-- snapshot:${name}:start -->`;
  const end = `<!-- snapshot:${name}:end -->`;
  if (source.split(start).length !== 2 || source.split(end).length !== 2) throw new Error("Missing static snapshot source boundary");
  return source.split(start)[1].split(end)[0];
}
const variants = {
  login: { title: "个人账户", message: "使用微信绑定的手机号登录。首次使用可在验证后注册。", content: panel("login") },
  registration: { title: "创建个人账户", message: "手机号已验证。请填写本人姓名与居民身份证号，提交后创建账户。", content: panel("registration") },
  account: { title: "个人账户", message: "", content: panel("account").replace(/<image[^>]+\/>/, "").replace(/<view class="small-copy" wx:if="{{avatarFailed}}">[^<]+<\/view>/, "") },
  pending: { title: "个人账户", message: "暂时无法确认注册结果。请继续检查本次注册，暂时不要再次提交。",
    content: source.match(/<button class="secondary" wx:if="{{canRecover}}"[^>]*>[\s\S]*?<\/button>/)![0]
      .replace("{{logoutPending ? '查看退出结果' : '检查本次办理结果'}}", "检查本次办理结果") },
};
const browser = await chromium.launch();
try {
  for (const width of [320, 390]) {
    const page = await browser.newPage({ viewport: { width, height: 844 }, deviceScaleFactor: 2 });
    await page.route("**/*", route => route.abort());
    for (const [name, value] of Object.entries(variants)) {
      const content = value.content.replace(/\s(?:wx:if|wx:else|disabled)=(?:"[^"]*"|'[^']*')/g, "")
        .replace(/\s(?:wx:else)\b/g, "")
        .replaceAll("{{inputName}}", "").replaceAll("{{inputIdNumber}}", "")
        .replaceAll("{{name}}", "张宁").replaceAll("{{suffix}}", "002X")
        .replaceAll("{{bound ? '已关联' : '未关联'}}", "未关联");
      const html = `<!doctype html><html lang="zh-CN"><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>Orbit source snapshot: ${name}</title><style>button{border:0}${css}\nbody{margin:0} view,navigator{display:block} button,input{font-family:inherit;width:100%} input{height:auto} .home-link{text-decoration:none}</style><body><main class="page account"><div class="eyebrow">司籍处 · 本人账户</div><div class="page-title">${value.title}</div>${value.message ? `<div class="notice">${value.message}</div>` : ""}${content}<div class="home-link">返回公开首页</div></main></body></html>`;
      if (html.includes("{{")) throw new Error("Unresolved static display value");
      await writeFile(join(output, `${name}-${width}.html`), html);
      await page.setContent(html);
      await page.screenshot({ path: join(output, `${name}-${width}.png`), fullPage: true });
    }
    await page.close();
  }
} finally { await browser.close(); }
console.log(`Static source screenshots: ${output}; browser closed. No native platform acceptance.`);
