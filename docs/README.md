# Nucleus 文档导航

## 阅读路径与权威

先读[业务总览](overview.md)，再读[完整架构](architecture.md)和[交付顺序](engineering/delivery.md)。随后按用例进入下列正式业务专题。架构拥有跨能力工程协议，业务专题拥有输入、资格、状态、事实、响应及专属恢复依据。

每项规则只有一个所有者。修改所有者时同步检查消费者、图表、验收要求与引用。新文档接管主题后删除旧文档的重复正文；没有剩余职责的文件删除。既有实现与历史证据不自动成为目标架构要求。

## 实际交付状态

| 范围       | 当前材料与限制                                                                                                                        |
| ---------- | ------------------------------------------------------------------------------------------------------------------------------------- |
| 输入与密码 | 已有手机号、Challenge、Message、八位码、R1 secret、原证明、摘要和封装实现；UUIDv4 生成与按值解析已接入，见[项目标识](identifiers.md)  |
| 供应商     | 已有 Turnstile 上下文及时间检查、PNVS 构造签名与单次客户端、本地 HTTPS 替身；不代表完整认证链已交付                                   |
| 装配       | nucleus 已串联设置、凭据、密钥、供应商、C1 启动和 HTTP；有信号停止与就绪通知，真实 systemd 部署尚未验证                               |
| 数据库     | Platform 已有 TLS 连接、在线池、独立恢复连接及驱动收尾，已有隔离测试材料                                                              |
| 部署       | deployment 已有迁移和首次密钥初始化；完整维护尚未实施                                                                                 |
| C1 发码    | 已有接纳、人机校验、持久额度、唯一发送占用、PNVS 单次发送及到期清理；原结果恢复与清理验证边界见[短信认证](identity/authentication.md) |
| 完整系统   | HTTP 仅交付 C1、健康检查与 OpenAPI；没有 C1→R1、正式注册或工牌业务闭环验收                                                            |
| 验证证据   | 既有测试、真实观察与静态审查均有各自边界，见[设计证据](engineering/evidence.md)；不覆盖目标架构的完整实现                             |

当前实现有 nucleus、domain、platform、deployment、fixture 五个 package。根包 nucleus 负责应用装配与请求适配，domain 内的 Identity 拥有业务事实。根包 `src/` 已按职责拆分，C1 适配位于 `identity/authentication/`；其余能力随真实消费者建立。完整职责及状态共享见[架构](architecture.md#crate-与模块组织)，现有启动停止与诊断限制见 [HTTP 入口](http/api.md#已交付入口)。

共同 HTTP 协议已接入 C1、健康检查和 OpenAPI：标识中间件、统一提取错误、无响应回显及 C1 必填 Origin 见 [HTTP 入口](http/api.md)与[匿名身份入口](http/identity.md)。在线结构化日志已接入请求及 C1 工作关联，范围见[运行观测](architecture.md#就绪与观测)。部署命令日志与指标尚未接入。同路由的 cookie/Bearer 认证与不透明会话 token 仍未交付，规则归[当前身份](identity/current.md)和[会话](identity/session.md)。文档认可不授权一次实现这些独立结果。

## 架构与运行

[Rust 规范](engineering/rust.md)与 [SQL 规范](engineering/sql.md)是仓库维护的语言、依赖、数据与失败处理规则。规则保留上游来源，采用新版本前审阅差异；团队和托管审查不依赖个人 Codex 目录。工具链和测试命令见 [Rust 工作区验证](engineering/rust.md#workspace-validation)。检查范围与故障证据要求见[验证边界](engineering/verification.md)。

| 文档                                    | 独立职责                                                                                |
| --------------------------------------- | --------------------------------------------------------------------------------------- |
| [完整架构](architecture.md)             | 事实所有权、共同事务、并发、原操作、恢复、文件后果、类型、crate、PostgreSQL、启动和停止 |
| [业务交付顺序](engineering/delivery.md) | 结构设计、实施切片、业务阶段、依赖和验收边界                                            |
| [项目标识](identifiers.md)              | UUIDv4 服务端生成、客户端值解析、披露及非时间语义                                       |
| [数据库](database.md)                   | 连接、TLS、登录身份、运行权限和现有连接库                                               |
| [部署设置](settings.md)                 | 环境键、输入规则、默认值与消费所有者                                                    |
| [密钥材料](keys.md)                     | 用途、格式、受控编码、密码信封与持久元数据                                              |
| [凭据文件](platform/credentials.md)     | 入口秘密文件格式、装载和脱敏                                                            |
| [部署工具](engineering/deployment.md)   | 现有迁移与初始化命令及原结果回读                                                        |
| [密钥维护](operations/keys.md)          | 停机换钥及材料切换                                                                      |
| [密钥引用范围](operations/key-scope.md) | 各用途退役前的实际持久引用                                                              |
| [停机维护](operations/maintenance.md)   | 维护目标、身份、计划及业务记录                                                          |
| [Access 初始化](operations/access.md)   | 空库建立首个管理员、系统角色及停机指派                                                  |
| [容量输入](engineering/capacity.md)     | 已有规模估算及证据限制                                                                  |
| [设计证据](engineering/evidence.md)     | 外部来源、真实观察及绑定原材料的审查记录                                                |

## HTTP 与客户端

| 文档                               | 职责                                            |
| ---------------------------------- | ----------------------------------------------- |
| [HTTP 入口](http/api.md)           | 标识中间件、提取与错误、封套、Router 与 OpenAPI |
| [列表读取](http/lists.md)          | 分页、排序和同页快照                            |
| [网站交付](http/site.md)           | 独立托管、后端边界、公开设置与浏览器规则        |
| [入口预算](http/admission.md)      | 可信来源与有界资源                              |
| [匿名身份 HTTP](http/identity.md)  | C1、R1、R2 输入和结果                           |
| [Turnstile](platform/turnstile.md) | 上下文、时间和有限重试                          |
| [PNVS](platform/pnvs.md)           | 原请求字节、签名和单次响应分类                  |
| [客户端协议](client/protocol.md)   | 类型生成、适配与响应校验                        |
| [小程序会话](client/session.md)    | Bearer 与本地资料隔离                           |
| [原操作恢复](client/recovery.md)   | 原材料、待核对和结果交接                        |
| [页面](client/pages.md)            | 网站与小程序的实际办理范围                      |

## Identity 用例

| 文档                                    | 职责                                    |
| --------------------------------------- | --------------------------------------- |
| [短信认证](identity/authentication.md)  | C1 阶段事实、额度、发送占用与 R1 原结果 |
| [原输入证明](identity/proofs.md)        | 固定编码、原材料核验和 code 占用        |
| [正式注册](identity/registration.md)    | R2 资格、共同提交、拒绝与成功保留       |
| [人员与实名](identity/person.md)        | 永久人员、版本和同人引用                |
| [账户](identity/account.md)             | 当前号码占用和独立修订                  |
| [会话](identity/session.md)             | Session、来源和 Handoff                 |
| [号码摘要](identity/lookup.md)          | 手机号与身份证号查找用途                |
| [当前身份](identity/current.md)         | Actor、当前资格和本人投影               |
| [网站会话](identity/website.md)         | Cookie、Session 选择和多标签页          |
| [退出](identity/revocation.md)          | 撤销集合及原结果                        |
| [头像](identity/avatar.md)              | 更换、完成定位和资源生命周期            |
| [暂停恢复](identity/suspension.md)      | 管理资格与会话后果                      |
| [注销](identity/cancellation.md)        | 当前关系移除、晚到结果与永久历史        |
| [换号](identity/mobile.md)              | 原号或协助、新号验证与会话撤销          |
| [实名更正](identity/correction.md)      | 永久更正与当前占用                      |
| [微信身份](identity/wechat.md)          | 登录、绑定、解绑和一次交换              |
| [微信事实](identity/wechat-data.md)     | 协议记录与引用约束                      |
| [小程序手机号](identity/mini-mobile.md) | 可信手机号与登录/注册分流               |

## Access

| 文档                               | 职责                         |
| ---------------------------------- | ---------------------------- |
| [权限目录](access/capabilities.md) | 两组权限及系统角色全集       |
| [管理 HTTP](access/http.md)        | 候选、入口及错误顺序         |
| [角色与指派](access/roles.md)      | 当前角色、永久定义与管理范围 |
| [授权生命周期](access/grants.md)   | Grant 建立、暂停、恢复和结束 |
| [邀请](access/invitations.md)      | 原码、容量、有效性和接纳     |

## Center

| 文档                               | 职责                     |
| ---------------------------------- | ------------------------ |
| [政治面貌固定值](center/values.md) | 固定代码与未填写语义     |
| [政治面貌](center/affiliation.md)  | 首次填写、授权读取与更正 |

## Workforce

| 文档                                   | 职责                             |
| -------------------------------------- | -------------------------------- |
| [人员编号](workforce/numbers.md)       | 号码池、发号、更正、撤销和恢复   |
| [目录](workforce/directories.md)       | 科室、职务、职称及分类规则       |
| [配置完成](workforce/completions.md)   | 可变配置的原操作结果             |
| [模板与照片](workforce/media.md)       | 模板内容、字体、照片和排版       |
| [申请](workforce/applications.md)      | 固定提交、分类、审批和终止       |
| [成品与发放](workforce/fulfillment.md) | Render、Badge、Delivery 及原结果 |

## 存储

[本地文件](storage/files.md)拥有可信路径、密码封装、资源保护和签名格式。共同发布、接纳、删除和换钥协议由[架构](architecture.md#文件与数据库后果)拥有。

## 文档维护

正式设计以 UTF-8 Markdown 保存。本文指定规则的所有者与阅读路径。每条规则只有一个维护位置；正文按业务需要组织，不要求统一章节模板。所属文档描述实际交付的行为、边界和不变量，区分目标设计、当前代码与验证证据。

主题迁移时保留仍有效的决定和证据，并同步更新消费者、图示、验收要求及链接。旧的活动文档删除重复正文，只保留必要导航；没有剩余职责的文档删除。活动文档只保留当前规则、契约、证据限制和未决问题。移除已解决问题清单、过时方案和编辑流水，不把助手判断或授权对话写成业务要求。

使用 Bun 1.4.2 和仓库的 `bun.lock`。需要安装依赖时执行 `bun install --frozen-lockfile --ignore-scripts`。默认只格式化改动文档：`./node_modules/.bin/prettier --write PATH...`。`bun run format:docs` 会重写整个文档集及 `AGENTS.md`，只有该完整范围已选定时才执行。

必须运行 `bun run check:docs` 检查格式与 Markdown 结构。安装 lychee 0.24.2 后，运行 `bun run check:docs:links` 检查本地链接及标题锚点。两项检查覆盖 `AGENTS.md` 和 `docs/**/*.md`，[文档工作流](../.github/workflows/docs.yml)规定 CI 调用。离线检查不验证外部来源的真实性或当前可用性，外部证据按对应主张核验。

本地 Codex 会话还须对改动文档运行 `node "${CODEX_HOME:-$HOME/.codex}/tools/check-prose.mjs" PATH...`，对编号表运行同目录的 `check-ids.mjs`。它们是 CI 之外的补充工具；不可用时报告未运行，仓库检查仍须执行。

格式、结构和链接检查不能证明业务规则正确或实现已交付。检查范围见[验证边界](engineering/verification.md#检查范围)，来源及审查覆盖见[设计证据](engineering/evidence.md)。
