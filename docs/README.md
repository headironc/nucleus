# Nucleus 文档导航

## 阅读路径与权威

当前设计按约 1500 人的内部小系统简化，先读[业务总览](overview.md)和[架构](architecture.md)。新增工牌使用固定快照、状态条件、人工重试与审计；同人同类型申请不限制数量。已有复杂实现按消费者迁移，目标不冒充已交付。公开内容管理、小程序暂缓，真实数据上线前完成[备份与恢复](operations/backup.md)。

先读[业务总览](overview.md)，再读架构的[应用内核](architecture.md#应用内核)与[威胁模型](architecture.md#威胁模型)，随后按用例进入下列正式业务专题。实施切片、开发循环与授权规则见[交付顺序](engineering/delivery.md)，模型选择、委派和接续见[模型政策](engineering/models.md)。架构拥有跨能力工程协议，业务专题拥有输入、资格、状态、事实、响应及专属恢复依据。

每项规则只有一个所有者。修改所有者时同步检查消费者、图表、验收要求与引用。新文档接管主题后删除旧文档的重复正文；没有剩余职责的文件删除。既有实现与历史证据不自动成为目标架构要求。

## 能力状态

本表是交付状态的唯一台账：每个能力一行，“已交付”表示当前代码提供该能力，“目标”表示只有目标设计；标为暂缓的能力不属于当前交付范围。已交付能力的具体路由、方法与字段以实际 [OpenAPI](http/api.md#openapi-与实际入口) 为准，所属文档只保存业务规则与公开契约，不另记交付状态。交付范围变化时，由对应 PR 更新本表。

| 能力                                           | 状态   | 所属文档                                                                                                                                             |
| ---------------------------------------------- | ------ | ---------------------------------------------------------------------------------------------------------------------------------------------------- |
| 网站短信发码 C1 与登录 R1                      | 已交付 | [短信认证](identity/authentication.md)、[匿名身份 HTTP](http/identity.md)                                                                            |
| 网站正式注册 R2                                | 已交付 | [正式注册](identity/registration.md)                                                                                                                 |
| 空库初始化与停机指派                           | 已交付 | [Access 初始化与指派](operations/access.md)                                                                                                          |
| 个人 main 固定发布候选与可信晋级工具           | 已退役 | [历史晋级契约](engineering/review-gate.md#个人-main-固定发布候选)；自动程序和专属回归已移除；历史源码与外部原操作材料保留                            |
| 个人 dev 独立 PR 准入与可信 squash             | 已退役 | [历史 dev 契约](engineering/review-gate.md#个人-dev-严格同步-pr)；自动程序和专属回归已移除；历史源码与外部原操作材料保留                             |
| 网站当前身份、本人资料与账户会话封印           | 已交付 | [当前身份](identity/current.md)、[账户](identity/account.md)、[会话事实](identity/session.md)                                                        |
| Halo 网站本人姓名与身份证尾号摘要              | 已交付 | [当前身份](identity/current.md#本人账户投影)、[页面](client/pages.md#网站本人账户设计稿)                                                             |
| Halo 网站本人微信绑定事实摘要                  | 已交付 | [当前身份](identity/current.md#本人账户投影)、[页面](client/pages.md#网站本人账户设计稿)                                                             |
| Halo 网站本人当前头像读取                      | 已交付 | [头像](identity/avatar.md)、[网站交付](http/site.md)、[网站验收](../halo/README.md#浏览器隔离验收)                                                   |
| 默认头像与已知账户头像读取                     | 已交付 | [头像](identity/avatar.md)                                                                                                                           |
| 注销后默认头像元数据清理                       | 已交付 | [头像](identity/avatar.md#工作事务与故障恢复)                                                                                                        |
| 网站本人头像上传、更换及原结果后端             | 已交付 | [头像](identity/avatar.md)                                                                                                                           |
| 头像更换页面与小程序接入                       | 目标   | [头像](identity/avatar.md)                                                                                                                           |
| 私人文件持久发布、读取句柄与幂等删除组件       | 已交付 | [本地文件](storage/files.md)                                                                                                                         |
| 网站当前及全部退出                             | 已交付 | [退出](identity/revocation.md)                                                                                                                       |
| 账户暂停恢复及状态历史                         | 已交付 | [暂停恢复](identity/suspension.md)                                                                                                                   |
| 实名更正及系统管理员停机更正                   | 已交付 | [实名更正](identity/correction.md)                                                                                                                   |
| 人员状态、永久实名版本封印与统一鉴别           | 已交付 | [人员与实名版本](identity/person.md)                                                                                                                 |
| 本人及管理注销                                 | 已交付 | [注销](identity/cancellation.md)                                                                                                                     |
| Halo 网站本人注销                              | 已交付 | [注销](identity/cancellation.md)、[页面](client/pages.md#网站本人注销设计稿)                                                                         |
| 注销历史读取                                   | 目标   | [注销](identity/cancellation.md)                                                                                                                     |
| 网站本人及协助换号                             | 已交付 | [换号](identity/mobile.md)                                                                                                                           |
| 小程序手机号认证与小程序会话（暂缓）           | 目标   | [小程序手机号](identity/mini-mobile.md)、[小程序会话](client/session.md)                                                                             |
| 微信身份登录、绑定与解绑（暂缓）               | 目标   | [微信身份](identity/wechat.md)、[微信事实](identity/wechat-data.md)                                                                                  |
| 权限目录与系统角色                             | 已交付 | [权限目录](access/capabilities.md)                                                                                                                   |
| 机构与 Workforce 角色、机构 Holder 及指派      | 已交付 | [角色与指派](access/roles.md)、[状态历史与完整性](architecture.md#状态历史与完整性)                                                                  |
| Workforce 授权生命周期与 Grant 封印            | 已交付 | [授权生命周期](access/grants.md)                                                                                                                     |
| Halo 网站机构入口开通员工服务                  | 已交付 | [授权生命周期](access/grants.md)、[页面](client/pages.md#网站机构入口开通员工服务)、[网站检查](../halo/README.md#机构入口开通员工服务)               |
| Workforce 邀请码与封印状态                     | 已交付 | [邀请](access/invitations.md)                                                                                                                        |
| 政治面貌与封印状态                             | 已交付 | [政治面貌](center/affiliation.md)、[状态历史与完整性](architecture.md#状态历史与完整性)                                                              |
| 政治面貌显示名称读取接口                       | 已交付 | [政治面貌固定值](center/values.md)                                                                                                                   |
| 政治面貌停机更正                               | 已交付 | [政治面貌](center/affiliation.md#存储密码与维护)                                                                                                     |
| Halo 网站本人政治面貌首次填写与当前值读取      | 已交付 | [政治面貌](center/affiliation.md)、[页面](client/pages.md#网站本人政治面貌)、[网站检查](../halo/README.md#本人政治面貌)                              |
| 工号生命周期与封印状态                         | 已交付 | [工号](workforce/numbers.md)                                                                                                                         |
| Workforce 目录、分类规则与配置完成             | 已交付 | [目录](workforce/directories.md)、[配置完成](workforce/completions.md)                                                                               |
| 工牌固定模板、照片、申请、成品与发放           | 目标   | [模板与照片](workforce/media.md)、[申请](workforce/applications.md)、[成品与发放](workforce/fulfillment.md)                                          |
| 栏目创建领域组件                               | 已交付 | [栏目](content/columns.md)；含永久原结果、导航顺序与封印核验；尚未装配栏目 HTTP 入口                                                                 |
| 栏目 HTTP 接入、管理列表与历史查询（暂缓）     | 目标   | [栏目](content/columns.md)、[内容 HTTP](content/http.md)                                                                                             |
| 栏目修改、排序、删除与其余网站公开内容（暂缓） | 目标   | [栏目](content/columns.md)、[草稿、版本与上线](content/publication.md)                                                                               |
| 健康检查与 OpenAPI                             | 已交付 | [HTTP 入口](http/api.md)                                                                                                                             |
| 事务可信时间与后台高水位                       | 已交付 | [时间与个人顺序](architecture.md#时间与个人顺序)、[数据库](database.md#c1-接纳与可信时间)                                                            |
| 换钥与退役                                     | 目标   | [密钥维护](operations/keys.md)                                                                                                                       |
| Halo 网站 C1 页面与原操作恢复                  | 已交付 | [页面](client/pages.md)、[网站工程](client/website.md)、[客户端恢复](client/recovery.md)                                                             |
| Halo 网站短信登录、原结果恢复与可信本人        | 已交付 | [客户端恢复](client/recovery.md)、[网站会话](identity/website.md)、[当前本人](identity/current.md)                                                   |
| Halo 网站首次注册与原结果恢复                  | 已交付 | [正式注册](identity/registration.md)、[客户端恢复](client/recovery.md)、[网站会话](identity/website.md)                                              |
| Halo 网站本次登录退出与原结果恢复              | 已交付 | [退出](identity/revocation.md)、[客户端恢复](client/recovery.md)、[网站会话](identity/website.md)                                                    |
| Halo 网站全部退出与原结果恢复                  | 已交付 | [退出](identity/revocation.md)、[客户端恢复](client/recovery.md)、[网站会话](identity/website.md)                                                    |
| Halo 网站本人换号与原结果查询                  | 已交付 | [换号](identity/mobile.md)、[页面](client/pages.md#网站本人换号设计稿)、[客户端恢复](client/recovery.md)、[网站验收](../halo/README.md#本人换号页面) |
| Halo 网站本人实名更正记录                      | 已交付 | [实名更正](identity/correction.md#历史读取)、[客户端协议](client/protocol.md)、[网站验收](../halo/README.md#浏览器隔离验收)                          |
| Halo 网站本人员工服务状态                      | 已交付 | [授权生命周期](access/grants.md)、[客户端协议](client/protocol.md)、[网站验收](../halo/README.md#浏览器隔离验收)                                     |
| Halo 网站本人结束员工服务                      | 已交付 | [授权生命周期](access/grants.md)、[客户端协议](client/protocol.md)、[网站验收](../halo/README.md#浏览器隔离验收)                                     |
| Halo 网站本人当前工号只读摘要                  | 已交付 | [工号](workforce/numbers.md)、[客户端协议](client/protocol.md)、[网站验收](../halo/README.md#浏览器隔离验收)                                         |
| Halo 网站本人首次领取工号                      | 目标   | [页面](client/pages.md#网站本人首次领取工号)、[网站检查](../halo/README.md#本人首次领取工号)；客户端候选已实现，真实组合验收待完成                   |
| 公开设置与网站交付配置                         | 已交付 | [网站交付边界](http/site.md)、[部署设置](settings.md)                                                                                                |
| 网站其余页面与小程序客户端                     | 目标   | [页面](client/pages.md)、[网站工程](client/website.md)                                                                                               |
| 真实部署、Nginx 与 systemd 验收                | 目标   | [业务交付顺序](engineering/delivery.md#里程碑与生产准备)                                                                                             |
| 统一审计表                                     | 目标   | [审计](architecture.md#审计)                                                                                                                         |
| 每日备份与恢复演练                             | 目标   | [备份与恢复](operations/backup.md)                                                                                                                   |

现有业务历史不表示已有统一审计表。暂停公开内容后，已交付的栏目创建组件继续保留，后续接入不在当前范围。

后续范围与依赖见[剩余交付清单](engineering/delivery.md#剩余交付清单)。检查时机与连续交付见[开发协作与交接](engineering/delivery.md#开发协作与交接)，委派和上下文恢复见[模型政策](engineering/models.md)。[里程碑与生产准备](engineering/delivery.md#里程碑与生产准备)区分功能完成、隔离验收和真实部署证据。尚需业务决定的事项集中在[待确认事项](engineering/delivery.md#待确认事项)。

## 架构与运行

[Rust 规范](engineering/rust.md)与 [SQL 规范](engineering/sql.md)是仓库维护的语言、依赖、数据与失败处理规则。[模型政策](engineering/models.md)维护模型选择与协作要求。这些规则保留上游来源，采用新版本前审阅差异；团队和托管审查不依赖个人 Codex 目录。工具链和测试命令见 [Rust 工作区验证](engineering/rust.md#workspace-validation)。工具选型、源码检查方法、自动化范围与证据要求见[验证边界](engineering/verification.md)。

| 文档                                           | 独立职责                                                                                |
| ---------------------------------------------- | --------------------------------------------------------------------------------------- |
| [完整架构](architecture.md)                    | 事实所有权、共同事务、并发、原操作、恢复、文件后果、类型、crate、PostgreSQL、启动和停止 |
| [业务交付顺序](engineering/delivery.md)        | 实施切片、开发循环、持续授权、分支提升、剩余范围与依赖、里程碑验收和生产准备            |
| [审查关卡契约](engineering/review-gate.md)     | 退役部署的身份、候选及基线绑定，以及保留的原操作历史与恢复解释                          |
| [模型选择与协作](engineering/models.md)        | 模型默认值与升级、运行限制、委派接纳和上下文接续                                        |
| [模型行为场景](engineering/model-scenarios.md) | 模型政策的验收输入、可观察预期与失败条件                                                |
| [Claude Code 调用](engineering/claude-code.md) | 经官方 DeepSeek 或 OpenCode 的只读、有界执行调用、工具审批、输出与失败恢复边界          |
| [项目标识](identifiers.md)                     | UUIDv4 服务端生成、客户端值解析、披露及非时间语义                                       |
| [数据库](database.md)                          | 连接、TLS、登录身份、运行权限和现有连接库                                               |
| [部署设置](settings.md)                        | 环境键、输入规则、默认值与消费所有者                                                    |
| [密钥材料](keys.md)                            | 用途、格式、受控编码、密码信封与持久元数据                                              |
| [凭据文件](platform/credentials.md)            | 入口秘密文件格式、装载和脱敏                                                            |
| [部署工具](engineering/deployment.md)          | 现有迁移与初始化命令及原结果回读                                                        |
| [密钥维护](operations/keys.md)                 | 停机换钥及材料切换                                                                      |
| [密钥引用范围](operations/key-scope.md)        | 各用途退役前的实际持久引用                                                              |
| [停机维护](operations/maintenance.md)          | 实例互斥、运行身份、原输入重放与未知结局恢复                                            |
| [Access 初始化与指派](operations/access.md)    | 空库建立首个管理员、系统角色及停机指派                                                  |
| [容量输入](engineering/capacity.md)            | 已有规模估算及证据限制                                                                  |
| [设计证据](engineering/evidence.md)            | 外部来源及其核验边界                                                                    |

## HTTP 与客户端

| 文档                               | 职责                                               |
| ---------------------------------- | -------------------------------------------------- |
| [HTTP 入口](http/api.md)           | 标识中间件、提取与错误、封套、Router 与 OpenAPI    |
| [列表读取](http/lists.md)          | 分页、排序和同页快照                               |
| [网站交付](http/site.md)           | 托管与路由、渲染分工、公开设置与浏览器规则         |
| [入口预算](http/admission.md)      | 可信来源与有界资源                                 |
| [匿名身份 HTTP](http/identity.md)  | C1、R1、R2 输入和结果                              |
| [Turnstile](platform/turnstile.md) | 上下文、时间和有限重试                             |
| [PNVS](platform/pnvs.md)           | 原请求字节、签名和单次响应分类                     |
| [网站工程](client/website.md)      | 网站技术栈、样式、组件与测试工具                   |
| [客户端协议](client/protocol.md)   | 类型生成、适配与响应校验                           |
| [小程序会话](client/session.md)    | Bearer 与本地资料隔离                              |
| [原操作恢复](client/recovery.md)   | 原材料、待核对和结果交接                           |
| [页面](client/pages.md)            | 两端办理范围、用户文案、网站 UI 设计与小程序稿边界 |

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
| [注销](identity/cancellation.md)        | 注销投影、晚到结果与永久历史            |
| [换号](identity/mobile.md)              | 原号或协助、新号验证与会话撤销          |
| [实名更正](identity/correction.md)      | 永久更正与当前占用                      |
| [微信身份](identity/wechat.md)          | 登录、绑定、解绑和一次交换              |
| [微信事实](identity/wechat-data.md)     | 协议记录与引用约束                      |
| [小程序手机号](identity/mini-mobile.md) | 可信手机号与登录/注册分流               |

## Access

| 文档                               | 职责                                   |
| ---------------------------------- | -------------------------------------- |
| [权限目录](access/capabilities.md) | 两组权限及系统角色全集                 |
| [管理 HTTP](access/http.md)        | 候选、入口及错误顺序                   |
| [角色与指派](access/roles.md)      | 角色与 Holder 状态、固定指派与管理范围 |
| [授权生命周期](access/grants.md)   | Grant 建立、暂停、恢复和结束           |
| [邀请](access/invitations.md)      | 原码、容量与期限调整、有效性和接纳     |

## Center

| 文档                               | 职责                     |
| ---------------------------------- | ------------------------ |
| [政治面貌固定值](center/values.md) | 固定代码与未填写语义     |
| [政治面貌](center/affiliation.md)  | 首次填写、授权读取与更正 |

## Workforce

| 文档                                   | 职责                             |
| -------------------------------------- | -------------------------------- |
| [工号](workforce/numbers.md)           | 号码池、分配、更正、撤销和恢复   |
| [目录](workforce/directories.md)       | 科室、职务、职称及分类规则       |
| [配置完成](workforce/completions.md)   | 可变配置的原操作结果             |
| [模板与照片](workforce/media.md)       | 模板内容、字体、照片和排版       |
| [申请](workforce/applications.md)      | 固定提交、分类、审批和终止       |
| [成品与发放](workforce/fulfillment.md) | Render、Badge、Delivery 及原结果 |

## Content

| 文档                                       | 职责                                       |
| ------------------------------------------ | ------------------------------------------ |
| [草稿、版本与上线](content/publication.md) | 共用草稿、永久版本、上线事件、并发与原操作 |
| [栏目](content/columns.md)                 | 栏目事实、顺序、启停、删除与变更历史       |
| [文章](content/articles.md)                | 文章内容、正文约束、发布校验与公开可见性   |
| [首页编排](content/home.md)                | 区块类型、链接目标、整体发布与公开解析     |
| [内容图片](content/images.md)              | 上传处理、引用保护、匿名与管理读取及清理   |
| [内容 HTTP](content/http.md)               | 公开读取与管理入口、请求形状和专用应用码   |

## 存储

[本地文件](storage/files.md)拥有私有路径、授权读取、固定资源保留与人工清理。[架构](architecture.md#文件与数据库后果)规定文件先完成再保存引用。[备份与恢复](operations/backup.md)拥有数据库、文件、配置与密钥的共同备份、异机保存和恢复演练。

## 文档维护

正式设计以 UTF-8 Markdown 保存。本文指定规则的所有者与阅读路径。每条规则只有一个维护位置；正文按业务需要组织，不要求统一章节模板。所属文档只保存业务规则（参与者、权限、状态与迁移、披露、保留）与公开契约（路由、状态码与应用码、字段形状）；必要的业务数据结构在所属专题定义；完整 SQL、触发器、锁、字节编码、模块结构、查询计划和验收证据归代码、测试与 PR，不在文档中复制实现。能力的交付状态只记在[能力状态](#能力状态)，其他文档不另写已交付范围；架构、密钥与平台文档描述共享机制时，以“当前”“目标”区分已实现与未实现的部分。

主题迁移时保留仍有效的决定和证据，并同步更新消费者、图示、验收要求及链接。旧的活动文档删除重复正文，只保留必要导航；没有剩余职责的文档删除。活动文档只保留当前规则、契约、证据限制和未决问题。移除已解决问题清单、过时方案和编辑流水，不把助手判断或授权对话写成业务要求。

从复盘或外部经验提炼规则时，先定位具体触发条件、错误后果及现有所有者。区分规则缺失、冲突、入口难找与执行或证据不足。已有规则覆盖时，修正执行或补齐必要入口，不换一种措辞重复添加。确需新增约束时，写清适用场景、动作、可核对结果及必要例外，并清理被替代的条文。可由现有工具可靠判定的条件优先纳入对应检查，不另建重复框架。模型选择与协作规则变化时，同步维护[行为场景](engineering/model-scenarios.md)。静态场景不作为实际执行证明。

采用外部经验时按[证据要求](engineering/evidence.md#1-范围与使用)核对来源、适用条件与限制。模型、任务、团队或环境不同的结果不直接变成项目能力保证。也不据此自动增加审批、审查轮次或流程角色。提案在对应 PR 中说明解决的实际问题、已有规则为何不足和预期效果。长期文档保留可执行的规则与必要来源，不积累研究摘要或重复复盘台账。

Bun 版本以根目录 [`.tool-versions`](../.tool-versions) 为准，本地检查从该文件核对版本。本机安装 [mise](https://mise.jdx.dev/) 后，在仓库内执行 `mise install` 即安装并启用该版本；未使用 mise 时，按 `check:bun` 的提示用 Bun 官方安装脚本安装同一版本。根 `package.json` 的 `packageManager` 与 `@types/bun` 须与它一致。`bun run check:bun` 核对这三处及本机版本，`check:docs` 与 `check:tools` 也先执行它。升级 Bun 时同时修改这三处并更新 `bun.lock`；Dependabot 只提升 `@types/bun`，合并前须补齐 `.tool-versions` 与 `packageManager`。依赖以仓库的 `bun.lock` 为准。需要安装依赖时执行 `bun install --frozen-lockfile --ignore-scripts`。默认只格式化改动文档：`./node_modules/.bin/prettier --write PATH...`。`bun run format:docs` 会重写根 README、AGENTS.md、`docs/` 和 `design/` 下的 Markdown。只有该完整范围已选定时才执行。

必须运行 `bun run check:docs` 检查格式与 Markdown 结构。范围包含根 README、AGENTS.md、`halo/README.md`、`docs/**/*.md` 和 `design/**/*.md`。安装 lychee 0.24.2 后，运行 `bun run check:docs:links` 检查上述文档及 `design/**/*.html` 的本地链接、图片引用和标题锚点。运行 `bun test tools/preview-design.test.ts` 验证实际设计文件的预览传输及服务启动、失败和停止行为。[原生 Docs 工作流](../.github/workflows/docs.yml)在面向 `dev` 或 `main` 的每个 PR 上直接执行这些检查，事件与结果边界见[原生 Docs](engineering/verification.md#原生-docs)。旧[文档工作流定义](../tools/fixtures/retired-workflows/docs.yml)仅作为离线夹具保留。检查归属见[仓库与检查归属](engineering/verification.md#仓库与检查归属)。离线链接检查不验证外部来源、CSS 中的资源、脚本中的动态地址或浏览器效果。外部证据及设计验收按对应主张核验。

本地 Codex 会话还须对改动文档运行 `node "${CODEX_HOME:-$HOME/.codex}/tools/check-prose.mjs" PATH...`，对编号表运行同目录的 `check-ids.mjs`。它们是仓库检查之外的补充工具；不可用时报告未运行，仓库检查仍须执行。

格式、结构和链接检查不能证明业务规则正确或实现已交付。检查范围见[验证边界](engineering/verification.md#检查范围)，外部来源见[设计证据](engineering/evidence.md)。
