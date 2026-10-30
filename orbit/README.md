# Orbit 小程序

原生页面入口是公开简介、个人账户与独立登录管理。源码位于 `src/`，包内 JavaScript、WXML、WXSS 与项目配置由构建器生成。能力状态统一见[文档索引](../docs/README.md#能力状态)；手机号、实名注册、本人及退出契约分别由[手机号认证](../docs/identity/mini-mobile.md)、[正式注册](../docs/identity/registration.md)、[当前身份](../docs/identity/current.md)和[会话撤销](../docs/identity/revocation.md)拥有。

## 本地检查

Node 与 Bun 版本沿根目录 `.tool-versions`，依赖由本目录 `bun.lock` 固定。生成器复用 `halo/tools/protocol.ts` 的 OpenAPI 选择与 Ajv standalone 编译，依赖准备包含 Halo 的固定锁文件。TypeScript 7 负责检查；OpenAPI 工具使用 TypeScript 5 的 AST API，与 Halo 保持相同分工。

```bash
cd halo
bun install --frozen-lockfile --ignore-scripts
cd ../orbit
bun install --frozen-lockfile --ignore-scripts
node tools/protocol.ts
bun run typecheck
ORBIT_API_ORIGIN=https://api.example.test bun run build
bun test tests
```

Halo 与 Orbit 直接读取同一份实际后端导出的 `halo/protocol/openapi.json`，不另存小程序副本。生成器在 `.generated/` 生成类型及离线校验，`source.json` 绑定快照路径与 SHA-256。构建只输出正式 `build/`，并在 `build-source.json` 保留同一来源；缺少或无法编译真实快照时失败。构建和受控测试不访问真实 API；上面的 example.test 域名只用于本地隔离运行。

共同契约包含网站短信和小程序手机号分支。Orbit 的输入类型与运行时只接纳 `mobile`，会话类型与登录、R2 响应校验只接纳带 token 的 Bearer 分支。待注册凭据沿同一真实契约，不通过客户端声明来源。网站适配器保留相反的 Message/Cookie 边界。

原生传输固定使用 `wx.request` 的文本响应，按状态、媒体类型、schema 与应用码共同判断。意外的对象或 `ArrayBuffer` 不当作可信 JSON。运行时只使用随包交付的代码，不从项目根之外加载模块，不动态编译 schema。受控包测试在缺少 Node、DOM、fetch、URL 与 structuredClone 的宿主中实际加载生成的 `app.js` 与页面脚本；该测试不替代微信运行验收。

## 项目配置与正式构建

构建要求显式设置 `ORBIT_API_ORIGIN`，没有默认 API 域名。AppID 默认读取公共 `project.config.json` 中已确认的小程序身份，显式 `ORBIT_APP_ID` 可覆盖该选择；私有配置继续忽略入库。API 必须是准确的 HTTPS origin；AppID 保留原文，只拒绝空白与控制字符。字符串通过检查不证明平台资格。构建拒绝 `touristappid`，不以测试项目身份生成正式包。

```bash
ORBIT_API_ORIGIN="$APPROVED_API_ORIGIN" ORBIT_APP_ID="$APPROVED_MINI_APP_ID" \
  bun tools/build.ts
```

正式构建在 `build/` 生成可打开的项目。构建器先解析已有公共与私有项目配置，再合并自己拥有的字段，保留其他字段。私有 AppID 或基础库选择与本次明确输入冲突时停止，不覆盖私有文件。`es6` 与 `enhance` 同时关闭，交付已经打包的 JavaScript；不执行项目钩子。需要指定基础库时使用 `ORBIT_BASE_LIBRARY`，未指定时不另设基础库版本。实际生效仍以开发者工具加载及私有覆盖为准。

安全随机数使用 [wx.getRandomValues](https://developers.weixin.qq.com/miniprogram/dev/api/device/crypto/wx.getRandomValues.html)，其最低基础库为 2.15.0。手机号只读取用户主动触发的 [getPhoneNumber](https://developers.weixin.qq.com/miniprogram/dev/framework/open-ability/getPhoneNumber.html) 事件 `detail.code`，不调用 `wx.login`。所选平台必须支持手机号 code 模式。随机能力缺失或失败时停止办理，不能退回伪随机数。AppSecret、供应商身份与可信来源归服务端；客户端不发送 AppID 来改变来源。

微信平台及真实业务链路验收还需要准确 API 与头像合法域名、开发者登录及手机号授权资格。消费真实导出、项目 JSON 可解析、包内代码可执行及静态截图均不能证明这些前提。构建命令不登录微信、不上传或发布。

## 页面与本地资料

公开首页不读取本人资料。个人账户主动验证手机号；已有账户取得 Bearer 会话，无账户进入只含姓名与居民身份证号的注册表单。登录或注册交付会话后，先成功读取当前本人，才显示姓名、证件尾四位、现有头像及微信关联事实。页面不显示手机号、内部标识或未交付的本人功能入口。

原生私有 storage 只保存会话、短期认证与注册材料，以及按账户隔离的退出定位。首次写入前保存完整材料并回读，失败零请求。每次手机号及 R2 POST 前保存保守未知状态和退避，覆盖传输层的 35 秒本地等待及随后 5 秒。可信响应再按结果调整；取消只结束等待，保留更长退避。已完成请求和 GET 不延长写入退避。未知结果只由用户明确核对原输入与原操作，不自动取得新 code、换键或重做写入。注册填表证明与已发送 R2 的恢复期限独立；原证明先到期不能提前删掉已发送 R2 的原输入。

隐藏、切换及重新检查先遮蔽资料，迟到响应和确认弹窗按原会话与本地代次隔离。返回前台先清理本地过期材料，再由本人页检查会话。停留公开页期间只做本地清理，不读取私有资料或恢复写入；存储暂时失败时每 5 秒重试，成功清理后结束重试。进入后台停止计时，恢复后再清理。30 天会话的计时遵守 JavaScript 的有符号 32 位延迟上限，不把宿主钳制误当到期。

当前与全部退出保存原范围、发起会话和账户。401 不证明原退出结果；同账户重新登录后只读原撤销证明，核对原范围与发起会话，不用新会话重放旧 DELETE。另一账户的未明退出不阻止当前账户正常退出。恢复期限结束只保留不含秘密的未知标记，不声明退出失败。

登录管理消费正式快照中的 `GET /api/v1/sessions` 与 `DELETE /api/v1/sessions/{id}`，来源仍为唯一共同 OpenAPI。列表按服务端 ID 顺序呈现网站或小程序来源、建立时间与本次登录标记，不保存设备、位置或活跃时间。当前项不提供行内退出；确认同时绑定原会话、代次和所选目标。成功只移除该目标，保留发起者。单条结果未知后只由用户明确刷新列表，不自动重发、不查询原结果，不以刷新后的存在或消失推断此前结果。当前及全部退出继续使用原恢复协议。新增页面仍由应用唯一 `Controller` 管理会话与资料寿命。

## 静态视觉材料

`tools/snapshots.ts` 读取实际 WXML 的具名区段与 WXSS，在禁网络的 Chromium 中生成 320、390 像素的登录、注册、账户和结果待确认截图。输出位置由 `ORBIT_VISUAL_OUTPUT` 显式指定；浏览器在 `finally` 关闭。它不实现 WXML 运行框架，也不模拟微信导航、授权窗口或原生输入行为。

`tools/session-snapshots.ts` 读取登录管理 WXML 的具体行、全部退出区域与共享 WXSS，结合实际控制器的受控列表、单条未知、刷新及完成状态生成同样两种宽度的像素。确认窗口只作静态内容示意，实际原生弹窗事件由导出包宿主回归核对。工具禁止网络，并在结束时关闭浏览器和控制器计时；不证明微信原生渲染。

```bash
ORBIT_VISUAL_OUTPUT="$EVIDENCE_DIRECTORY" bun tools/snapshots.ts
```
