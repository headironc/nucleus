# Halo 网站

能力状态见[文档索引](../docs/README.md#能力状态)。网站工程规则见[网站工程](../docs/client/website.md)，交付和安全边界见[网站交付](../docs/http/site.md)。本目录独立管理依赖，不使用根目录的 Node 依赖。

## 构建与本地检查

使用 Node 26 和仓库指定的 Bun 版本。准确运行时版本由根目录 `.tool-versions` 固定，Node 支持范围由本目录 `package.json` 的 `engines` 声明。依赖由本目录的 `bun.lock` 固定。

先在仓库根目录运行 `bun tools/check-inputs.ts --base origin/dev`，报告当前完整工作树是否影响 Halo。基线、输入归属及保守运行规则见[检查归属](../docs/engineering/verification.md#仓库与检查归属)。报告选择检查；本节及后续的实际快照和浏览器命令负责执行检查。

```bash
cd halo
bun install --frozen-lockfile --ignore-scripts
bun run check:generated
bun run typecheck
bun run test
bun run build
```

`build/release.json` 固定构建标识、公开资源路径、媒体类型及 SHA-256。构建器核对文件与清单；Node 启动时再次核对，并验证服务器和浏览器的构建标识相同。生成物不包含 source map。清单核对用于发现发布混版和字节不一致，不是发布者的数字签名。

`app/protocol/build.ts` 负责核对当前页面与 `/build.json` 的构建标识，同时遵守资源失败的重新加载要求。发码、登录、注册和本人检查在业务请求前及页面恢复可见时调用。页面与办理控制器负责处理核对失败，并保留原材料。

协议、恢复和发行校验使用 Vitest。构建、协议生成、资源配置与浏览器夹具采用 TypeScript，由 Node 26 执行，纳入同一类型检查。`server.mjs` 与 `tools/release.mjs` 随生产运行目录交付，不依赖开发工具。浏览器验收另外启动真实后端与 Nginx，命令见下节。格式、类型和单元测试不能替代浏览器或真实环境验收。

完整单元测试包含真实 Nginx 代理回归。默认使用 `/usr/sbin/nginx`，可通过 `HALO_NGINX` 指定可执行文件。

应用类型检查显式执行 `typescript-compiler` 别名中的 TypeScript 7。`openapi-typescript` 仍依赖 TypeScript 5 的 AST API，因此 `typescript` 保留其支持范围内的最新稳定版 5.9.3。根维护工具也使用 TypeScript 7。生成器支持新版 API 后，再移除这项兼容分离；当前不把生成器的 AST 库宣称为全局最新版本。

构建期依赖链 `@react-router/dev@8.5.1 → @babel/core@7.29.7 → gensync@1.0.0-beta.2` 仍要求 `gensync@^1.0.0-beta.2`。现有稳定版 0.1.0 不满足该范围，因此锁定上游要求的 beta.2，保留这一预发布限制。上游提供可合法解析到稳定 gensync 的兼容构建依赖链，并通过相关验收后，再解除限制。

登录短期材料的直接检查可运行 `bun run test -- app/authentication/material.test.ts`。规则见[恢复材料](../docs/client/recovery.md#恢复材料)。检查覆盖保存与删除回读失败、短信截止、秘密到期关闭、旧办理清理及损坏收据隔离。测试使用可控存储验证材料层；页面触发、HTTP 恢复与离页维护由下述集成验收覆盖。

孤立短信交接到期或损坏时，先保存非秘密 C1 占位，再删除交接材料。占位保存失败会保留材料并停止使用，后续检查继续清理。直接回归还核对 C1 重开后保持材料缺失状态，不生成新操作。

## 协议快照

在仓库根目录运行以下命令。隔离 PostgreSQL 的准备方法见[数据库交付验证](../docs/database.md#交付验证)。测试通过实际监听端口读取 `/openapi/v1.json`，同时验证路由及公开设置。

```bash
NUCLEUS_OPENAPI_EXPORT="$PWD/halo/protocol/openapi.json" \
  bun tools/check-halo-test.ts snapshot
cd halo
bun run generate
bun run check:generated
```

`protocol/openapi.json` 是构建输入；`app/protocol/generated/` 全部由工具生成。工具选择 C1、公开设置、创建 Session、正式注册、当前本人账户、本人更正记录、本人员工服务状态与当前工号、本人结束员工服务、本人政治面貌当前值与固定名称、本人注销、两种退出及原结果查询，以及本人换号的原号发码、建立、新号发码、完成与原状态查询，并包含可达 schema。同一路径可选择多个方法，创建 Session 与全部退出分别消费 POST 和 DELETE，本人政治面貌消费 GET 与 PUT，本人完成换号消费 PUT。

Session 协议适配器供短信登录与本人检查使用。注册适配器核对首次 `201` 和原结果重放 `200` 的网站会话形状。退出适配器核对原操作及撤销集合。本次退出只接纳原 Session；全部退出接纳正数会话结果，查询核对完整集合及原发起会话。员工服务适配器只保留当前 Grant 的 ID 与人员定位；本人结束适配器发出无正文 POST，并核对原操作、原 Grant、双方人员及 `self` 模式。

政治面貌适配器分别读取当前值 `{affiliation:{value,createdAt}}`、13 个固定名称，并以 `{affiliation:code}` 首次填写。只有可信 `404 affiliation_unset` 映射为未填，成功回执只接纳 `{code:"ok"}`。本人注销适配器校验账户、人员和实名修订输入，使用原操作与 Session 标识发送请求。成功结果只含服务端注销 ID 和生效时刻，当前登录状态不能证明原结果。

换号适配器分别绑定原号会话、建立原输入、Transfer 与新号输入；完成结果必须回指原 Transfer。恢复材料另保留服务端账户、Grant、人员及 Transfer/Message 标识，生成器把账户、Grant、人员、Session、Transfer 及 Message 标识作为固定校验器导出。Ajv 的独立校验器随 Vite 编译进浏览器包，运行时不编译 schema。类型及校验器的更新必须来自重新导出的快照，不能手改。

保存的 R1 输入由生成校验器核对原 secret 的规范 32 字节编码、既有国内手机号格式和凭据的准确字段。凭据分支以四个必填字段与最多四个属性共同拒绝额外字段；实际 HTTP 反序列化规则保持不变。

本人账户响应的身份证尾号由共享 HTTP schema 约束为四个字符：前三位为 ASCII 数字，末位为 ASCII 数字或大写 `X`。生成校验器拒绝完整号码及其他非法尾号，并保留合法尾号的前导零。

将 `NUCLEUS_OPENAPI_REFERENCE` 指向已保存快照，可检查当前后端的实际导出是否逐字节一致。

## 本人换号页面

本人换号从个人账户的“更换手机号”进入 `/account/mobile`。原号发码、建立、新号发码与最终确认分别使用独立 Operation-Id。两次发码的 Turnstile action 分别为 `send_mobile_change_code` 和 `send_mobile_binding_code`，验证码复用六位受控 CodeInput。协议适配器在 `app/protocol/mobile.ts`，恢复控制器与材料在 `app/mobile-change/`。

材料只存于当前标签页的 `halo.mobile-change`。秘密自证明创建起最多保留 10 分钟；输入及定位保存并回读后，才清理上一阶段秘密。已确认完成按账户保留。材料损坏、到期或时钟回退不自动开始新办理。原写入结果未知时，GET 的 `pending`、`expired` 或查询失败均不授权换键重办。失败退避随原材料保存，页面重进继续遵守。晚到的成功只合并对应完成事实，不覆盖后来办理或恢复旧页面。

原号验证成功后，Transfer 自身的服务端截止约束新号阶段，不再沿用已消费原号验证码的截止。交接移除原号码、验证码和 token；新阶段材料仍有自己的 10 分钟本地上限。明确重取新号验证码可使用 Transfer 的剩余期限，不能延长 Transfer，也不能延长旧验证码或原秘密的保留期。首次发码与原发码核对均先保存材料，再检查当前构建；版本变化或检查失败时保存退避并要求重新加载，不发送请求。

新号短信先到期时，清除其号码、验证码和证明，保留仍有效的 Transfer。只有尚未提交完成的办理才能明确重新取码；完成结果未知时仍只查询原结果。新历史入口恢复已有材料时，先恢复并回读未决标记，再继续使用材料；标记失败不提前删除原证明。

首次发码限流后，只允许等待并明确重试原申请，保留原操作、号码和 token，不重新挂载安全验证。发码的 401 也可能表示原 token 证明不符；此时另由本人读取核验 Session，不能直接清除有效登录。

原号或新号短信仍在发送时，保留原操作、号码和 token，不进入验证码输入。至少等待 5 秒后，由用户明确查看原发送结果；刷新保留退避，不自动重放请求。反复取得发送中结果不会延长原证明或短信的截止，取得最终短信结果后才清理 token 并进入验证码输入。

限流后的原证明重试状态跨构建失败、后台中止和重新加载保留。结束发码或其构建检查的本地等待前，先保存至少 5 秒的恢复退避，并保留更长的原截止。后台、关闭、重新加载与重新核验共用此处理，不能借反复进出页面绕过退避。

四个写入口的首次与恢复请求遇到不可信响应时，保留原材料、未知结果和退避，要求重新加载页面。当前页面不再开放写入；重新加载只恢复显示，之后仍须本人明确核对原操作。

同一账户的新登录只能查询原办理，另一账户不显示原号码、结果或离开提示。成功后原会话已撤销，页面提供固定 `?return=mobile-change` 的手机号登录入口。新号登录后，由用户明确查询原 Transfer；查询不清除后来选中的会话。原定位缺失时，该返回入口仍保持只读。

从个人账户的普通入口返回时，可信当前身份可在已完成面板明确选择“再次更换手机号”。该动作只保存新办理的未发送材料，保留历史完成；填写并确认后才发送新的请求。固定登录返回没有此动作。此前完成不能覆盖新办理的查询或关闭状态，当前完成反馈必须匹配原 Transfer。

`/account/mobile` 与个人账户之间使用原生整页链接；只有实际带 widget 的换号页面在 CSP 中放行 Turnstile。直接检查可运行：

```bash
cd halo
bun run test -- app/mobile-change/material.test.ts app/mobile-change/recovery.test.ts app/protocol/mobile.test.ts
bun run test:browser -- tests/mobile-change.spec.ts
```

单元测试核对五接口、独立操作、保存失败零请求、未知结果、限流、错码、材料期限及恢复绑定。受控浏览器使用真实页面、`Current`、`Sessions` 和 IndexedDB，检查阶段请求、跨账户遮蔽、三类离开提示、键盘焦点和六位验证码，并保留 320、390、768、1440 像素的明暗主题截图。

完整组合验收使用真实 Nginx、Cookie、后端及隔离的 fake Siteverify/PNVS。场景先建立两条原 Session，在真实短信冷却内发码取得 429，等待后以相同操作、证明和输入重试。完成换号后丢失 PUT 回执，再以新号登录并显式 GET 原结果。Rust 在提交前后独立核对手机号归属、原会话撤销、Handoff 删除及永久完成事实；查询前后核对业务事实、后来会话与期限保持不变。具体运行结果归 PR 验收证据，受控响应不替代真实事务验收。

## 头像本地选择与预览

`app/avatar/candidate.ts` 读取本地文件，核对大小、实际格式、动画和正方形尺寸后解码预览。`format.ts` 检查图片容器，并仅为预览去除方向元数据。候选保留原 `File`，不会裁剪、缩放或改写待提交字节。重新选择、取消和关闭时释放旧解码资源；迟到的解码结果不能覆盖后来选择。

`app/avatar/panel.tsx` 展示当前头像与本地候选，提供格式提示、错误说明和取消选择。父消费者拥有确认及提交动作；该组件不读取账户、不上传文件，也不保存恢复材料。图片限制沿用[头像契约](../docs/identity/avatar.md)，浏览器检查不替代服务端验证。

```bash
cd halo
bun run test -- app/avatar/format.test.ts app/avatar/candidate.test.ts
bun run test:browser -- tests/avatar-selection.spec.ts
```

浏览器测试使用真实图片解码、选择组件和 React 面板，检查像素、方向、格式拒绝、资源释放及窄屏布局。测试请求由本地夹具提供，不依赖账户、上传接口或真实供应商；不能据此宣称头像更换流程已完成。

## 浏览器隔离验收

`app/authentication/current.ts` 拥有可信本人读取状态。它只在原 Session ID 与本地选择修订仍匹配时接纳资料。重新检查、离页及响应失信时遮蔽旧资料。401 仅比较清除原选择，403 和网络失败保留选择。直接验证运行 `bun run test -- app/authentication/current.test.ts`。本人页面接入选择通知、可见性与离页检查；页面及真实 Cookie 交接按下述集成验收验证。

会话选择组件按[网站会话规则](../docs/identity/website.md#浏览器选择与并发)验证本地原子操作，可独立运行 Chromium 检查：

```bash
cd halo
bun run test:browser -- tests/session-storage.spec.ts
```

夹具构建真实 `Sessions` 模块，使用浏览器的 IndexedDB 和 BroadcastChannel。检查覆盖并发登录、迟到清除、原期限、非秘密存储，以及打开或写入被拒后的恢复。损坏记录与通知失败也分别验证。通知只触发重读，不提供选择依据。该命令不启动后端或 Nginx，不验证 Cookie 交接或本人账户读取；这些行为由登录集成验收覆盖。

验证码组件可独立运行 Chromium 检查，不需要数据库或 Nginx：

```bash
cd halo
bun run test:browser -- tests/code.spec.ts
```

组件夹具在内存中构建真实组件及网站 CSS。Playwright 拦截提供资源，未知请求直接终止。测试不启动监听服务，也不写入正式构建目录。检查覆盖六格受控值、键盘与焦点、粘贴、组合输入和填充；还检查禁用、错误关联及四种宽度。IME、剪贴板与短信填充通过 DOM 事件模拟，不代表系统输入法或真机验收。截图核对需要可正常渲染数字和中文的浏览器字体。缺字时先准备隔离字体运行环境，不修改产品样式代替字形验收。登录提交、缺位提交焦点及组合期间停止登录请求由下述集成验收覆盖。

准备 Node 26、Bun、OpenSSL、带 HTTP SSL 和 rewrite 模块的 Nginx，以及固定 PostgreSQL 夹具。Nginx 通过 `HALO_NGINX` 指定可执行文件。它不读取系统站点配置，不安装或重启系统服务。

注册页面可独立运行 `bun run test:browser -- tests/registration.spec.ts`。夹具构建真实页面和样式，使用实际 IndexedDB，并拦截 HTTP 响应。检查覆盖四种宽度、输入焦点、草稿不落盘、保存失败、原请求恢复，以及另一标签页更换选择后的迟到成功。该夹具不验证生产 CSP、真实 Cookie 或数据库效果。下述组合验收承担这些边界。

本次退出页面可独立运行 `bun run test:browser -- tests/logout.spec.ts`。夹具在内存中构建实际页面及样式，使用真实 IndexedDB 和 BroadcastChannel。检查覆盖明确确认、首次保存失败、刷新不重提、显式查询、迟到回包、账户遮蔽和离开弹窗，并截图核对四种宽度及明暗主题。夹具通过受控响应提供 API 与资源，不启动后端或 Nginx，也不验证真实 Cookie 和持久事务。

退出恢复由 `app/logout/recovery.ts` 拥有。页面保存原操作、账户和选择，不保存本人资料或认证秘密。返回后先核对当前账户，同原账户才恢复结果；无选择不等于原登录失效。本人读取的失效状态携带实际被清除的选择，原 ID 与修订均匹配时才提示登录后查询。可信退出回包的当次反馈与后续历史恢复分别处理。原规则见[退出](../docs/identity/revocation.md)、[网站会话](../docs/identity/website.md)和[客户端恢复](../docs/client/recovery.md)。

全部退出从账户的独立入口进入 `/sessions/logout-all`，确认弹窗后才发送 DELETE。两种退出分别保存恢复材料与历史标记。全部退出将待取得证明、已匹配及已知不匹配分开保存。分类前中断或结果保存失败后，仍能以原操作查询服务端原范围与发起者；匹配首发材料才确认完成。已知 `request_mismatch` 不升级为本次成功，证明不匹配只展示此前记录。空结果、读取失败、刷新或登录返回本身都不证明完成。固定登录意图为 `all-logout-result`，本次退出仍使用 `logout-result`。已有账户登录后先核对本人，再恢复同账户的查询入口；注册仍进入账户页。原结果查询不重新退出，也不清理后来登录。

本机完整验收在仓库根目录使用本地容器入口。它要求已安装并可用的 Docker、Linux x86_64，以及非 root 的当前用户；不在宿主安装浏览器、Nginx、Rust 或系统包。入口以当前镜像输入摘要命名本地镜像，缺失时调用现有 `tools/halo-ci-image.ts` 构建，之后复用同一摘要的缓存镜像；首次构建需要下载并安装镜像输入，耗时较长。也可用 `--image IMAGE` 复用已安装镜像；入口先解析其不可变镜像 ID，并在安装依赖或执行任何业务命令前要求镜像记录的输入摘要与当前仓库一致，不匹配即失败。入口不拉取私有 CI 镜像，也不登录镜像仓库。

```bash
bun tools/halo-local.ts
```

默认模式在固定环境中依次核对镜像工具、以 `bun install --frozen-lockfile --ignore-scripts` 安装 Halo 依赖并执行浏览器冒烟：分别启动默认 headless shell 与带 `channel: "chromium"`、沿用原生消费者 `ignoreDefaultArgs: ["--disable-back-forward-cache"]` 的完整 Chromium，各自打开页面、导航到无副作用的内联页面、核对内容并关闭。冒烟通过后才运行现有类型检查、正式构建和 `HALO_NGINX=/usr/sbin/nginx bun tools/check-halo-test.ts browser`。`--prepare` 只完成镜像核对、依赖安装和同样的双浏览器冒烟，用于快速验证环境，不宣称完整验收；`--native` 准备后只运行 `tests/identity.spec.ts` 的 `native history and reload` 用例。

容器以当前宿主 UID/GID 运行，使用 `--init --ipc=host`、2 个 CPU，不启用特权或额外能力，不发布端口，也不挂载凭据或 Docker socket。仓库挂载为 `/workspace`；容器目标目录、根与 Halo 依赖、Cargo 与 Bun 缓存位于 `target/halo-local/`。每轮使用独立的 XDG 和截图目录，复用缓存并保留此前结果。入口保留镜像账号记录，并为缺失的宿主 UID/GID 添加本轮映射。映射文件只读挂载到该容器，保证 PostgreSQL 能解析实际运行用户。容器不继承宿主的浏览器路径、库路径、字体或 XDG 配置。信号只转发一次，等待原有清理并回读实际容器状态。客户端提前退出时，入口取消仍在运行的原容器并等待结束；无法核实终止时报告准确容器名并失败。旧 CI 曾直接调用镜像内的验证和浏览器命令；当前自动入口已停用，本地容器入口继续承担本机完整验收。

底层本地验收命令保持可用：在已具备镜像工具、Halo 依赖和构建产物的环境中，可仍在仓库根目录运行 `HALO_NGINX=/usr/sbin/nginx bun tools/check-halo-test.ts browser`。旧 CI 的对应调用已退役。宿主不再提供 `playwright install` 或系统 Nginx 的准备步骤。

直接使用底层命令并需要独立保存浏览器二进制时，安装和测试均设置同一个 `PLAYWRIGHT_BROWSERS_PATH`。测试创建临时数据库、证书、Node 监听、独立 HTTPS 头像入口和 Nginx 配置，结束时清理进程及临时目录。头像入口把浏览器的原生图片 GET 原样转发给真实后端的签名图片路由，只记录路径摘要、请求头存在布尔值、状态、响应头和字节摘要，不记录完整签名链接、Cookie 值或图片内容。截图位于 `halo/test-results/`。

### 失败与中断证据

浏览器验收入口把持久证据写到仓库根目录的 `target/halo-browser-evidence/`，该目录已被忽略且不进入发布物。旧 CI 曾按运行和尝试标识选择子目录；当前入口只保存本地证据，不自动上传。每次运行在 `current.json` 绑定唯一 `runId`，记录位于 `runs/<runId>/`：`reporter.jsonl` 在选择完成时写入完整测试清单，并逐条同步写入开始、结束、状态、时长、期望状态、重试和经脱敏的原始断言消息与调用栈；`runner.jsonl` 记录执行边界、取消原因、Playwright 实际退出码或信号、报告完整性核对和逐项清理结果；`backend.jsonl` 由 Rust 夹具记录它观察到的浏览器子进程退出和后端排空边界；`harness.json` 记录外层 Cargo 退出、准确用例名的完成证明以及可观察到的 runner 与 backend 事实；`summary.json` 是供人工阅读的最终摘要。证据按事件同步写入，因此在后续测试阻塞、超时或进程被强制终止前，先前的失败已经可读。

缺少最终记录表示结果未知，不等于成功。摘要区分实际结束、已开始但未结束、从未开始，以及开始状态未知。报告未完整结束时，缺少开始记录只能记为 `start_unknown`，因为 worker 的开始消息可能还在传输。报告完整结束后，才能把未观察到开始的用例记为 `never_started`。未分配 worker 的占位事件不构成实际开始。未结束的用例不记为通过，也不补写时长或状态。

清理结果按实际观察到的进程退出和资源关闭判断。Cargo、管道关闭、runner 最终记录和实际进程终止分别观察。Cargo 先退出时，入口仍等待已登记的 runner 完成有界清理。返回前还核对 runner 与分离进程组均已终止；无法证明时记为未知并失败。执行预算到期及 `SIGINT`、`SIGTERM` 分别只请求一次取消。清理先向 Playwright 主进程发送 `SIGINT`，让它在原有宽限内排空 worker 事件并结束报告；宽限结束后仍按原边界终止进程组。全部测试及失败后的继续执行不变。证据描述观测结果，失败的业务原因由所属用例进一步核对。

旧工作流的上传范围仅限上述证据目录，不包含 `halo/test-results/` 中的截图、trace、HAR 或附件正文。断言消息与调用栈保留操作符、值和 `文件:行:列`，但会遮蔽 Authorization、Cookie、token、密码、API key、Session 标识、查询密钥（含百分号编码名称）、完整签名头像链接及其查询/片段、私钥和固定虚构手机号；ANSI 颜色被移除，过长内容在脱敏后显式截断。脱敏和截断可能降低诊断细节，但不会把原断言替换成泛化的“失败”。旧 Halo 工作流仅在浏览器验收实际尝试后以 `always()` 上传该目录，使用固定的 `actions/upload-artifact@v7.0.2`，产物名包含运行与尝试标识；浏览器验收已尝试但没有可上传证据时，`if-no-files-found: error` 使任务失败而不是转绿。上述自动上传已停用，本地证据写入失败仍使验收失败。本地独立运行 `bun run test:browser` 也写入同一默认目录，可用 `HALO_TEST_EVIDENCE` 指向其他位置。

Cookie 对象断言可能通过通用 `value` 字段携带秘密，因此该字段也统一遮蔽。标量断言可能只输出 Cookie 值，不带请求头名称；脱敏也按网站 Session Cookie 的名称识别赋值并遮蔽凭据。清理先停止媒体请求的生产者，再取得并排空最新写入队列；清理期间新增的证据写入失败仍使验收失败。

启动 Playwright 前，夹具以临时证书核验 Nginx 的 HTTPS 构建入口已就绪。等待有截止，并监听进程提前退出和验收取消；监听尚未建立或服务不可用时不会直接开始页面导航。

浏览器整组预算为 900 秒，包含真实短信冷却等待、本人结束服务、注销和换号的恢复覆盖，以及有限运行波动。真实登录组合用例预算为 450 秒，包含注销后同号重新注册增加的 61 秒冷却。真实换号用例单独限为 240 秒，包含三段 61 秒的短信间隔。Rust 外层预算为 1020 秒，保留 120 秒供清理和最终断言使用。整组截止仍使验收失败；单请求截止、重试次数及进程清理边界保持原有设置。

浏览器执行正式构建，业务请求经过 Nginx 到达实际发码、登录、注册、本人读取和两种退出路由。Siteverify 与 PNVS 使用受控替身。不同场景使用不同的虚构来源，夹具仅在自己的回环 Nginx 中映射测试来源头，避免所有访客共享回环 IP 的额度；生产配置始终使用 `$remote_addr`。限流仍由真实后端执行。

注册场景丢弃真实首次成功响应，再以原标识、凭据与规范输入恢复。浏览器核对同一 Session、原期限和 Cookie 属性，再读取可信本人资料。后端排空后，Rust 夹具独立核对原注册的 Receipt、Account、Person 和唯一 Session，并确认没有 Workforce Grant。注册恢复沿用已有虚构手机号。

本人记录场景在同一虚构人员上建立三次真实更正，再通过本人 Cookie 读取、翻页和刷新。浏览器核对完整姓名、尾四位、顺序及 Cookie 不变。Rust 在读取窗口前后独立比较 Person、Account、实名版本、更正、Session、Grant、完成与退出事实，并核对永久更正的顺序和时间。受控页面回归另覆盖空态、越界、限流、失败、响应交错、身份遮蔽与窄屏长姓名；这些注入用例不替代真实后端验收。

退出场景复用这两个账户，并丢失真实退出回包。浏览器核对原 Cookie 清理、另一账户查询为空，以及固定的短信登录返回。遵守真实发码间隔后，同账户的新登录显式查询原退出；查询保留新登录及其他账户的 Cookie。全部退出分别覆盖两条会话和单条会话，分别在真实提交后、响应分类前离页，以及分类结果保存失败后刷新。两者重新登录并读取原证明后恢复完成，后来登录仍保持有效。后端排空后，Rust 独立核对三份永久 Logout、四条撤销、对应 Handoff 清理及两个后来登录。前序 current 退出的原集合仍为单条会话。整组使用 19 个虚构手机号完成 27 次明确发码，包含独立工号受益人、同号新注册及换号准备，每次仅调用一次 PNVS。限流的原申请重试沿用原证明，不增加供应商调用。真实发码间隔计入浏览器及外层等待预算。

测试截断真实成功响应，并核对恢复前后输入、标识与证明相同。Rust 夹具持续统计实际 PNVS 调用，直到后端完成排空。错误媒体类型、无效结构和未知码通过修改真实响应注入；这些响应不能结束办理。浏览器覆盖刷新、迟到结果、存储失败、材料过期和缺失、键盘与焦点，以及三种页面宽度。替身的验证按钮只存在于测试拦截的脚本中，不进入发布产物。

受理前拒绝使用真实后端的无效正文和错误 Origin 触发。首次拒绝后可明确重新填写；响应丢失后的恢复拒绝仍保留原办理，不能据此再次发码。路由验收覆盖尾部斜杠和大小写变体下的 CSP 与 widget。构建回归在隔离副本中修改 TypeScript 编译选项，验证实际两次构建具有不同标识。

恢复验收还以错误原证明和异输入触发真实后端拒绝，核对原材料在刷新后仍保留至截止。构建图片加载失败同样关闭发码入口。代理缓存测试用独立受控上游返回私有缓存头，核对实际 `/api/` 分流没有追加网站的 `no-store`；其余业务请求仍到达真实后端。该探针不代表头像业务的完整验收。

页面重进后恢复办理标记。材料丢失或损坏时保留无法确认的状态，占位记录随后丢失也不会在返回或刷新时转成新申请。历史标记失败的回归还核对有效原证明没有被提前清除，损坏材料的占位记录仍能保存。

生命周期回归覆盖整页返回首页后跨过原期限，以及首页和错误页加载时清理过期证明。恢复事件的检查覆盖存储暂时拒绝后恢复。检查核对非秘密占位、其他材料保留和没有新增业务请求。原请求的实际 PNVS 次数仍由同一后端夹具统计。

登录回归使用隔离数据库初始化的虚构账户。截断成功响应后，以原操作和证明恢复，最终核对数据库中只有一个 Session，期限保持不变。本人页在可信读取后展示完整姓名与身份证号尾四位；后台恢复、选择变化、迟到响应和失权时先遮蔽两项资料。独立 IndexedDB 回归覆盖多标签页比较更新、存储拒绝和事务中止。

孤立短信交接材料的页面回归覆盖过期、时钟回退与损坏。关闭占位的写入抛错、静默失败或回读失败时，保留原材料并等待存储恢复；恢复后返回或刷新 C1 均不得创建新操作或发起请求。检查同时保留无关存储项。

本人页的响应交错回归使用真实 IndexedDB 和 BroadcastChannel，配合受控账户响应。有效请求收到不可信响应时，即使选择或修订已变，也必须先重新加载页面，再读取私有资料。通知已使旧请求代次失效时，迟到响应不得影响新读取。检查同时核对页面遮蔽、请求次数和持久选择；这些用例不替代真实 Cookie 验收。

本人账户摘要可独立运行 `bun run test:browser -- tests/identity.spec.ts`。夹具在内存中构建实际页面与样式，使用真实 `Current`、协议解码器、IndexedDB 和 BroadcastChannel，由受控响应提供账户摘要与头像字节。检查覆盖完整姓名、尾号与微信绑定状态同时显示（受控的已绑定与未绑定）、长姓名及 `012X`／`0000` 原样保留、缺失或非法 `binding` 与不可信响应一样整份拒绝而不合并为未绑定、匿名与各类失败的遮蔽、选择通知和迟到响应、生命周期同步遮蔽、原生头像成功与坏图、一次明确重取后的终止失败、头像修订和账户竞争，以及四种宽度和明暗主题下两种绑定状态的资料顺序、可访问角色、键盘入口与截图。夹具终止未知请求，不启动后端或 Nginx，也不验证 HttpOnly Cookie、真实数据库或会话期限效果。真实后端当前只返回显式未绑定（`binding: null`），因此真实链路只证明未绑定显示；已绑定画面来自受控响应，不代表微信登录、绑定或解绑已交付。

真实登录和注册成功后的头像加载，以及媒体入口回传的 200 字节、摘要和隐私缓存头，都是未注入的普通真实流程证据。真实 `Current` 响应同时带有显式 `binding: null`，页面显示未绑定。头像过期验收另行标记为传输延迟故障注入：一次真实 `Current` 成功响应在不超过 35 秒的延迟后原样交付，首个原生图片请求保持在签名链接的后端 `expiresAt` 之后才原样继续。后端因此自行返回 403、`avatar_link_invalid` 与 `no-store`，页面只显示通用失败；移除延迟后，一次明确重取取得新签名并成功加载。该场景不改写响应、不伪造过期投影，也不等同于普通真实流程；同一步骤另核对 Cookie、Session 期限、选择与业务事实，且不发起短信或会话创建。

测试证书只用于该浏览器上下文，不进入主机信任库。这些检查不证明真实 Turnstile、短信送达、生产 TLS、systemd 或生产环境已经验收。

`/account` 的“查看员工服务”进入只读 `/workforce`。页面通过生成契约读取本人最近一份 Grant，区分未开通、已开通、已暂停与已结束；active 态独立展示当前工号与人员细类。只提供读取、刷新、返回账户和首页。登录与注册仍进入账户页。

运行 `bun run test:browser -- tests/workforce.spec.ts` 可核对受控四态、读取失败与限流、401/403、选择变化、生命周期遮蔽和响应式原图。正式组合验收另通过合法管理准入、暂停、恢复、本人退出及重新加入建立真实四态，在每个读取窗口独立比较数据库事实、Cookie 摘要及原期限。受控响应不代替该真实 HTTP、Nginx 和 Cookie 验收。

本人结束员工服务从 `/workforce` 的正常或暂停入口进入 `/workforce/termination`。页面先读取可信本人与服务，确认弹窗初始焦点在“暂不结束”。首次发送前保存原 Operation-Id、账户、人员、Grant 和选择并回读；存储抛错、静默失败或回读失败时不发请求。请求使用空正文 POST，响应必须绑定原操作、原 Grant、双方人员及 `self` 模式。此前结果未知时只由用户明确继续核对，以原操作、原 Grant 和当前同账户会话重发同一空正文；首次可信 429 明确未受理，等待后回到确认，恢复中的 429、拒绝和 401/403 不推断原办理失败。`request_mismatch` 永久停止该原请求；原成功永久保留，当前服务另行读取，重新开通的新 Grant 不自动或沿用旧操作结束。

已完成结果按账户分别保留，切换账户不会展示其他账户的结果，也不阻止当前账户独立结束其服务。另一账户已确定未受理的尝试保留至当前账户明确确认自己的新办理，不阻塞当前账户入口；未决原请求仍受保护。原未决请求的材料丢失后保留未知占位，保存失败仍在当前页面保留未知，不回到新办理入口。不可信的本人或服务读取提供明确的重新加载页面动作，并保留原请求的未决标记。

首次确定冲突后，同账户只有在可信当前读取确认另一个已开通或暂停的 Grant 时，才提供返回确认页面的动作。原拒绝保留至本人明确返回确认，随后仍须单独确认新的结束申请；刷新与重新读取均不提交。

结束材料与状态可运行 `bun run test -- app/workforce/termination.test.ts app/workforce/termination-material.test.ts app/protocol/workforce.test.ts`。受控浏览器运行 `bun run test:browser -- tests/termination.spec.ts`，使用真实页面、IndexedDB 与受控响应核对空正文绑定、保存失败零请求、429、未知重放、mismatch、同账户新会话、跨账户遮蔽、生命周期迟到回包、材料损坏、结果保存失败、当前服务读取失败和新 Grant 保护，并截图四种宽度及明暗主题。回归还覆盖不可信读取后的实际重新加载、两账户分别结束及刷新恢复，以及材料丢失后的核对与刷新均不换键新办。

正式组合验收经真实后端结束当前 Grant 并丢失首次回包，再由有权管理者重新准入本人。刷新和切换另一账户不自动提交；同账户真实新登录后，用户明确核对原操作和原 Grant，新 Grant 保持有效。浏览器比较 Cookie 摘要与持久材料，Rust 在首次提交和重放前后独立观察永久终止、完成记录及账户、会话和 Grant 状态。重放不新增业务事实，也不改变会话期限。受控响应不代替真实事务、Nginx 或 Cookie 验收。

本人注销从 `/account` 的“注销账户”进入 `/account/cancellation`。页面先读取可信当前账户，只展示完整姓名与身份证尾四位，不补造手机号；说明账户与 Access 数据的永久后果，且明确注销不同于退出登录。确认弹窗初始焦点在“暂不注销”，取消、关闭和 Escape 恢复入口焦点。首次发送前保存并回读原操作、账户、人员、实名修订与当前选择；存储抛错、静默失败或回读失败时不发请求，并明确尚未提交。请求绑定原操作、原 Session 和精确确认输入，成功只依据可信 `ok`、注销编号与 `cancelledAt`。

受限、输入拒绝、首次 401/403、`target_changed` 和依赖不可用分别展示，不标为成功。`request_mismatch`、不可信结构、服务端失败和实际回包丢失保持结果未知。未知状态不自动重发，也不提供注销结果查询；“检查登录状态”只读取当前账户，不发送注销请求。随后的登录失效、普通重新登录或同号新注册不能把旧未知升级为成功，也不能承接到新账户。选择变化、另一账户或读取失败先同步遮蔽旧姓名、尾号和原办理提示；已确定拒绝或成功一旦存在后续选择，也不再重新展示。未提交离开与提交后结果未明离开分别提示。

受控浏览器运行 `bun run test:browser -- tests/cancellation.spec.ts`，使用实际页面、IndexedDB、持久材料与受控响应核对四种宽度和明暗主题下的入口、确认、成功、拒绝、未知、保存失败零请求、重复与超时、迟到和乱序回包、选择变化、生命周期遮蔽、焦点及结果页导航。正式组合验收另以两个独立账户经过真实 Nginx、Cookie 和后端完成一次正常注销，并让另一次真实成功回包在事务提交后丢失；Rust 在浏览器返回前独立核对注销与完成记录、Account 墓碑与投影、Person 顺序、Holder 投影、Grant 终止、Session、Handoff、签发、撤销与 Receipt 清理、头像释放及实际 POST 次数。浏览器即使面对未知结果也不重发；检查当前登录、普通重新登录和同号新注册均不重写旧未知。Rust 再核对新账户与人员独立存在且未承接旧注销。受控响应和单元测试不代替该真实事务、Nginx 或 Cookie 验收。

工号协议与状态可运行 `bun run test -- app/protocol/number.test.ts app/workforce/numbers.test.ts`。检查覆盖全部固定细分类、显示格式、历史年份、顺序号边界、不可信结构、空值与失败、限流等待、独立刷新、35 秒超时，以及身份、选择修订和服务重查后的迟到响应。受控浏览器还核对加载与失败文案、键盘动作、同步遮蔽，以及 320、390、768、1440 像素下的明暗主题与长姓名原图。

正式组合验收在独立管理准备窗口建立虚构工号、普通更正及双人恢复。本人读取分别看到空值、原号、退出重加后的原号、更正后的新号及撤销后的空值。每个读取窗口前后，Rust 独立比较工号状态、不可变版本、全部工号历史、Grant、Account、Session 与原期限；浏览器另比较 Cookie 摘要。准备写入不计作读取副作用，受控响应和单元测试也不代替这些真实后端证据。

### 本人政治面貌

`/account` 的“政治面貌”进入 `/account/affiliation`。页面分别确认当前账户、读取当前政治面貌并读取固定名称，不占员工服务资格；已有值只读。只有可信 `404 affiliation_unset` 开放首次填写，身份、当前值或名称读取失败都不转为未填。固定名称不写入客户端常量，`app/protocol/affiliation.ts` 只接受按 `01` 至 `13` 升序、名称唯一且完整的 13 项响应；PUT 正文只为 `{affiliation:code}`，可信成功只接纳 `{code:"ok"}`，不从回执推断当前值或时间。

`app/affiliation/controller.ts` 拥有页面内存中的原操作、当前值读取和失败分类；`app/affiliation/marker.ts` 拥有 `halo:affiliation` 的最小会话标记。标记、结果未知、固定原键重试、当前值独立读取和离开提示的规则见[页面文案](../docs/client/pages.md#网站本人政治面貌)及[客户端恢复](../docs/client/recovery.md)。政治面貌值、选择、名称、摘要与原请求正文不写入存储、URL 或日志。

协议、标记和状态机可运行：

```bash
cd halo
bun run test -- app/protocol/affiliation.test.ts app/affiliation/marker.test.ts app/affiliation/controller.test.ts
```

受控浏览器检查可运行 `bun run test:browser -- tests/affiliation.spec.ts`。夹具构建实际页面与样式，使用真实会话选择、真实 IndexedDB 与 BroadcastChannel，并由受控响应覆盖首次读取、13 个选项、成功与成功后的独立当前读取、各类首次拒绝、401/403、结果未知与同键原重试、标记存在或损坏、迟到响应、账户切换、离页、聚焦、四种宽度及明暗主题。受控响应不代替真实 Cookie、数据库事务或 Nginx 验收。

真实组合验收在现有登录场景内使用独立虚构账户首次填写政治面貌，丢弃真实成功回包后以同一 Operation-Id 和正文重试，再分别读取当前值并以新键触发冲突。浏览器核对页面、Cookie 与固定原键；Rust 在首次提交和重放前后独立比较中心政治面貌、完成、人员档案与非中心账户、人员、会话、授权和终止事实。重放不新增第二条政治面貌事实，当前读取和冲突不改变业务事实。运行方式仍为 `HALO_NGINX=/usr/sbin/nginx bun tools/check-halo-test.ts browser`；受控浏览器检查、类型检查和单元测试都不能替代该验收。

## CI 依赖镜像

[镜像产物工作流](../.github/workflows/halo-image.yml)在面向 `dev/main` 的 PR、`dev` push 和受限手动补建时，按实际环境输入和验收源码判断是否构建。手动补建只接纳 `dev` 历史中的完整提交 SHA，并记录原因；可信工作流在检出后、读取候选版本文件或执行候选脚本前，独立核对实际 HEAD 和 `dev` 祖先关系。PR 绑定 head、base 与实际合成提交；首次加入或修改验收逻辑也会运行。根与 Halo 锁文件、React/Vite 或 Cargo.lock 单独变化不重建环境。

镜像定义在 [Dockerfile](../.github/halo-ci/Dockerfile)。官方 Playwright 基础镜像固定版本与 digest，提供浏览器及系统依赖。派生镜像安装经 SHA-256 核对的准确 Node、Bun 和 rustup，增加 Nginx、Rust 工具链及组件，并编译固定 PostgreSQL。Node 的路径优先于基础镜像自带版本，Linux 运行依赖包含 `libatomic1`。构建上下文仅含镜像定义、忽略文件和 PostgreSQL 构建器及版本文件。

版本来源保持现有归属：Bun 和 Node 读 `.tool-versions`，Rust 读 `rust-toolchain.toml`，PostgreSQL 读 `fixture/postgresql.version` 及其构建脚本，Playwright 读 `halo/package.json`。构建器核对 Halo 的 Bun 声明和 Node 支持范围。构建后端和固定 Buildx 客户端也进入环境摘要；导出与扫描工具的变化强制重新验收。输入摘要、构建 config、daemon 运行引用、OCI manifest、归档 SHA 和 GitHub ZIP digest 分别记录。滚动 APT 来源使相同输入的重建仍可能产生不同字节。

在仓库根目录构建：

```bash
bun tools/halo-ci-image.ts build nucleus-halo-ci:local
```

完整可下载产物可在 Linux 非 root 宿主运行以下命令。使用仓库固定的 Node/Bun、Docker 和 Python 3。输出目录必须不存在；大型临时文件使用磁盘目录，避免占满 `/tmp`。工具为自己的私有目录承担 `finally` 清理，失败证据保留在输出目录。

```bash
bun tools/halo-image-artifact.ts build /absolute/path/to/new-artifact
```

大型归档按消费者寿命释放：OCI 已验证并冻结后释放构建 tar；产物目录的 OCI 字节校验通过后释放工作副本，恢复步骤只读挂载这份成品。load 成功并回读实际镜像引用后释放恢复 tar，再执行完整独立再导出。再导出的标准输出流式压缩为私有 `roundtrip.tar.gz`，Docker 成功退出且压缩文件关闭后，原严格解析器核对 config、全部层与 diff ID。取消或输出失败时终止并等待 Docker 子进程，失败仅移动仍需诊断的本任务原件。日志记录导出、恢复和再导出前的实际剩余磁盘量；工具不清共享缓存或其他任务镜像。

工具把经固定 SHA 校验的 standalone Buildx 放到本次私有目录，使用原 default Docker driver 的内置 BuildKit。它复用原 builder 的四文件 context，执行单平台 load 并保存构建 metadata，随后核 Docker archive 中实际 config 和全部层。它不安装宿主插件、不创建 builder 服务。已有同字节工具可通过 `NUCLEUS_IMAGE_BUILDX_BINARY` 和 `NUCLEUS_IMAGE_GITLEAKS_ARCHIVE` 只读复用，校验规则不变。

构建 metadata 的 config 摘要真正缺省时，工具用 metadata 指定的原始单 manifest 字节闭合 config 和有序层的引用链；非法或冲突的已提供字段仍拒绝。原始 `build-manifest.json` 随该路线的产物保留，下载端重新核其类型、大小、摘要及实际 OCI config。descriptor 真正缺省或经典 Docker archive 没有原 manifest 时，只能使用与独立读取的完整归档字节相符的直接 config 摘要，不声称原 manifest 证据，不重建或猜造 descriptor。提供了 null 或畸形 descriptor 仍拒绝。

固定 Skopeo 用 `copy --format v2s2 --dest-compress-format gzip --dest-force-compress-format` 生成单平台 `linux/amd64` OCI layout 归档，冻结字节后用 `--preserve-digests` 恢复。显式生成 gzip 可避免 Docker archive 传输把未压缩 tar 声明为 gzip。内部严格支持配对的 OCI manifest/config/tar 或 gzip 层，以及 Docker schema 2 manifest/config/diff.tar 或 gzip 层；当前来源使用后者。报告保留真实 media type，不代表任意 OCI 消费者均兼容。Docker 的 `.Id` 可能表示 manifest/index；工具分别保存实际运行引用与 config 摘要。恢复后按准确运行引用再次完整导出，核 config 原字节、平台、RootFS diff IDs 与冻结归档一致，单独核默认用户的实际 UID，再以宿主非 root 身份运行消费者，并核容器实际使用的引用。

源码和已按锁文件、禁脚本准备的 Halo 依赖只读，`target` 和 HOME 私有可写。运行证据写入独立私有目录，容器不能写 host artifact output。确认容器终止后，host 只从已打开的目录 fd 导入固定普通文件，不跟随链接或未知目录，拒绝特殊文件和额外文件，再执行 host 扫描。验收容器禁止外网，执行准确版本、输入摘要、非 root、无 TCP 的私有 PG 查询、双 Chromium 实际页面 smoke 及 `tools/check-inputs.test.ts` 薄消费者。PG fast-stop、进程终态、私有目录删除和容器移除分别核对；清理失败不产生成功结论。首次导出兼容预检使用无网络的 scratch 小镜像；它不替代完整 OCI 的恢复后验收。

产物包含 `image.oci.tar`、来源、config、完整 history、层内容清单、脱敏扫描报告、实际系统包和 copyright 材料。Python `tarfile` 逐层读取，包括后层删除或覆盖的内容；只把普通文件复制为私有编号文件，不跟随链接或创建设备。固定 Gitleaks 扫描全部普通文件、路径及链接元数据，禁用行内放行和整路径豁免。嵌套归档与编码扫描深度为 2；加密、更深层或不受支持格式不展开。二进制字节也交给模式扫描，但任意二进制秘密未必能命中。扫描和许可材料不证明无秘密、无漏洞或法律审批。

扫描处置列表 `.github/halo-ci/scan-exceptions.json` 绑定完整来源字节、准确路径、规则、位置和次数，并绑定检测器及配置摘要；额外或变化的命中仍失败。报告区分检测、已处置和未决。当前列表含上游代码、示例、公钥，以及 Chromium 长期公开分发、用作组件 ID 输入的固定 DER 私钥材料；后者按准确原件接纳，不归类为公钥，也不推广到其他私钥。列表只保值摘要，不含私钥字面量。

只有运行验收、层扫描和清理成功后才上传 `nucleus-ci-<run_id>-<run_attempt>`，保留 7 天。独立下载作业按上传的 artifact ID 下载，摘要不符直接失败，再交叉核源码、repository、run/attempt、内部 tar SHA 与 manifest/config/layers，并核扫描处置列表属于当前候选。artifact ID 和 ZIP digest 在上传后记入输出与 summary，内部来源不虚构未来 ID。本地执行的 CI 身份明确为 null。

工作流只有 `contents: read`，不登录 registry、不传包凭据、不把 Docker socket 挂入候选容器。它不发布 GHCR，也不切换消费者。私有包关联、Actions 访问、发布与冷拉取，以及 Runner 的 VM 接入分别验收；本片产物不能证明这些后续条件。旧发布、Halo 和缓存流程仅保留为[历史夹具](../tools/fixtures/retired-workflows/README.md)。

本地容器验证以可写工作副本挂载仓库，使用镜像的非 root 用户及 `--init --ipc=host`。init 回收测试退出后留下的孤儿进程，使进程组终态可观察。本地完整验收可运行 `bun tools/halo-local.ts`；该入口按本节镜像、输入摘要和非 root 约束准备环境，并在类型与业务执行前核对两种浏览器运行时。底层操作仍先运行 `bun tools/halo-ci-image.ts verify`，再执行本页的协议快照核对、类型与单元测试、正式构建和浏览器验收。将大型 Cargo 产物挂载到磁盘目录，避免占满临时文件系统。容器的源码挂载、缓存及临时数据库均不进入发布镜像。

### 首次私有包发布与冷拉取

[首次发布工作流](../.github/workflows/halo-publish.yml)只接受默认 `dev` 的手动派发，固定消费已经验收的 artifact `12938521777`（producer run `40964056245` / attempt 1）。先核平台仓库、来源、成功作业、artifact 归属、有效期和摘要，再用准确 producer 源码的已审验证器复核完整协议。产物中的脚本不会执行。producer 的构建、原层扫描及成功证据继续复用。

发布目标固定为现有私有包 `ghcr.io/headironc/nucleus-ci`（包 ID `16266672`）。平台操作先保存可见性、仓库关联、继承及 Actions access 前态，保持 private 和已有用户访问；连接准确 `headironc/nucleus`，在 Manage Actions access 给该仓库 Write，不授 Admin 或增加其他仓库，也不自动开启宽泛继承。设置后逐项回读；仓库关联不等于权限已经继承。原 config 中的旧仓库 source 标签作为已验字节保留，平台关联承担当前包的准确绑定。

本次固定发布的每次派发前，操作者用已验证账户只读保存同一包的准确 ID、private、关联仓库 ID `1432619931` 和名称 `headironc/nucleus`；发布和冷消费后再读回同一对象。读取失败、结果未知或身份变化均不能给最终接纳。复用已验 Actions Write 配置，不把账户凭据交给工作流，也不重复修改包权限。

平台和账户收据逐层白名单构造，只保存身份、来源、状态、摘要、有效期及关联视图。原始 API 对象可能含临时凭据，不能整份持久化或打印。账户前后态使用下列只读入口；它要求已验证的 headironc 账户和完整准确关联，只输出白名单字段：

```sh
bun -e 'import { accountAssociation } from "./.github/halo-publish/publication"; console.log(JSON.stringify(accountAssociation(), null, 2))'
```

job token 的包元数据可能不显示关联。工作流记录 repository 视图为 `null`、`missing` 或 `object`；前两者只说明该视图不可用，关联仍依上面的独立回查核验。返回对象时，两个仓库身份字段必须完整匹配；冲突、残缺、数组或其他类型均拒绝。准确包 ID、名称、private、仓库与原产物检查继续执行；可读元数据不证明 registry 写入或冷拉取能力，最终以真实短期令牌操作及消费者清理验收。

只有独立发布作业有 `packages: write`。它仅检出本次可信工作流修订中的 publisher 工具，不检出或执行 producer 候选，也不运行候选镜像。再次核平台身份及冻结归档后，固定 Skopeo 用 `--preserve-digests` 发布不可变标签 `source-55506adfd5c4ba8e34632d2cc2eadcfd189c75e8-run-40964056245-attempt-1`。同标签同 manifest 读回复用，冲突拒绝；不转换、重建或移动 `latest/dev`。认证文件私有且不进入证据，正常、失败和 `always` 收尾均删除并核 absent。

上传进程及其工具容器终态明确后，按固定标签读回原 manifest 字节，并按 digest 读回 config 原字节和有序层描述。上传报错但内容相同则接续验收，不再上传。401/403、限流、网络失败和清理未知均保留未决状态；只有明确的 `MANIFEST_UNKNOWN` 表示不存在。工作流不自动重试。负责人先核原 run/job、上传 journal 与外部状态；原操作终态且确认不存在后，原授权内最多另派一次相同内容的恢复，不能反复 dispatch 或用新构建掩盖未知结果。

冷拉取使用另一台全新 hosted runner。新宿主不保证 Docker 没有预载镜像；本次准备只限准确发布工作流的 `cold` job、GitHub-hosted Linux 和本地 default Unix socket。先记录初始镜像及其引用和空容器清单，只删除该初始枚举中的镜像，不强制删除或执行全系统 prune。删除前核引用仍指向原 ID，删除后回读镜像和容器均为空；有容器、出现新镜像、上下文不符、拒绝或未知即失败并保留前后态，不继续拉取。此准备不用于自托管或共享 daemon。

空缓存成立后，再用仅 `packages: read` 的 Nucleus 短期 job token 按准确 digest 拉取，记录白名单层进度、摘要及原进程结果；发现缓存层复用或摘要冲突则拒绝。权限失败不改用匿名或 PAT。认证文件删除后，核 registry 与本地平台、config、RootFS 及实际运行引用，复用现有隔离运行入口验证非 root、私有 PG、双 Chromium 和薄消费者。源码和依赖只读、容器禁外网且不挂 Docker socket，finally 核容器、PG、目录及本次拉取引用的清理。Docker 运行引用与 config 摘要分别记录。

publisher 的类型、拒绝/恢复及权限回归由[专用检查](../.github/workflows/halo-publish-checks.yml)执行：`./node_modules/.bin/tsc -p .github/halo-publish/tsconfig.json --noEmit` 和 `bun test ./.github/halo-publish/publication.test.ts`。固定 SHA 的 actionlint 1.7.12 另核两份发布工作流的语法与表达式上下文。新增工具独立于 producer 的验收输入集合。单元检查和源码合并不证明包设置、真实发布或冷拉取已完成；这些结果由交付负责人结合实际运行、摘要、权限和清理原件接纳。此入口不自动持续发布、不迁移生产消费者，也不提供 VM 接入证明。

## 启动与发布配置

正式访问入口是同源 HTTPS Nginx。`bun run start` 启动只监听 `127.0.0.1` 的 Node 服务，端口由 `HALO_PORT` 指定，默认 `3080`。Node 交付页面；静态资源由 Nginx 交付，业务请求交给 nucleus。直接打开 Node 端口不构成完整网站预览。

启动时必须同时设置非秘密的 `HALO_AVATAR_ORIGIN`，与后端头像签名设置使用同一个准确 HTTPS origin。它可与网站同源，也可不同，并允许一个根路径斜杠。输入不得含凭据、业务路径、查询、片段、通配符、CSP 分隔符或需解析器修复的字符。缺失或非法时进程在监听前退出，没有默认值。

Node 启动时校验一次并规范化，每个 HTML 响应的 `img-src` 都加入该 origin。登录或注册成功后的页内导航沿用发起文档的 CSP，因此不能只在直接访问 `/account` 的响应中放宽。

发布目录包含 `build/`、`server.mjs`、`tools/release.mjs`、`package.json`、`bun.lock` 及按锁文件安装的运行依赖。先在不可变目录准备并核对这一套材料，再从 Halo 源码目录生成资源配置；参数指向该发布目录：

```bash
node tools/nginx.ts /srv/halo/releases/BUILD
```

资源配置逐项列出清单内文件，不开放整个目录。`deploy/halo.nginx.conf` 是现有 HTTPS `server` 内的分流配置，`deploy/halo.service` 是 Node 服务模板。安装前核对站点 origin、nucleus 端口、Node 路径、服务身份及发布目录。已有发布保留为回退目标；先验证 Nginx 配置，再切换同一套 Node 与资源。真实安装、切换、服务重启和供应商操作另需目标环境授权。

后端必须设置 `NUCLEUS_CHALLENGE__SITE_KEY`，并保持 `NUCLEUS_HTTP__ORIGIN` 与 Nginx 的 HTTPS origin 完全一致。生产主机白名单和真实凭据按所属部署文档准备。浏览器从公开设置读取 site key；构建不写入环境秘密。

当前静态介绍页及办理页均为 `noindex`，`robots.txt` 禁止抓取。公开内容的收录、栏目和文章随其切片交付。已有账户从手机号登录入口申请短信，填写验证码后进入本人检查。首次用户在验证后填写实名资料，明确提交注册，再进入本人检查。账户提供本次退出与全部退出入口，其他本人功能按后续切片交付。
