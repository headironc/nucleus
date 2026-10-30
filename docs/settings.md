# 部署设置

本文规定非秘密设置的加载边界、键名规则、接受条件、所有者分工与失败定位。C1、网站短信与小程序手机号 R1、正式注册 R2、本人资料读取和退出已接入环境装载。当前设置包含供应商、数据库、HTTP、共享工作容量、额度、网站与小程序身份、凭据期限、头像链接和停止预算。迁移工具的独立装载也已交付，其余业务设置仍为目标设计；完整阅读路径见[文档导航](README.md)。

## 迁移工具

独立 `deployment` crate 使用以下环境键，除单独说明外均必填。装载沿用 `NUCLEUS` 前缀和嵌套输入结构，`migration` 组只声明工具所需字段。没有缺省值；未知字段和显式空值拒绝。部署进程只提供自己的配置，应用配置不能补足迁移输入。执行与文件边界见[部署工具](engineering/deployment.md)。

| 键                                | 接受条件                               |
| --------------------------------- | -------------------------------------- |
| `NUCLEUS_MIGRATION__USER`         | 非空且不含 NUL 的部署登录用户名        |
| `NUCLEUS_MIGRATION__RUNTIME_USER` | 必填；非空且不含 NUL 的运行登录用户名  |
| `NUCLEUS_MIGRATION__HOST`         | 与 Platform 数据库 TLS 主机校验一致    |
| `NUCLEUS_MIGRATION__PORT`         | 1–65535 的十进制端口                   |
| `NUCLEUS_MIGRATION__NAME`         | 非空、至多 63 字节，不含空白或控制字符 |
| `NUCLEUS_MIGRATION__AUTHORITY`    | 无控制字符的证书文件绝对路径           |

迁移用户名由 `USER` 提供，`RUNTIME_USER` 指定业务授权接收者。`migrate` 和 `keys init` 共用该装载结构，均要求提供这两个字段。数据库密码不经环境键；这些命令从凭据目录读取 `migration-password`。数据库阶段的 300 秒执行预算由部署工具拥有。连接预算沿用 Platform 默认值，与在线业务预算分开。

`keys init` 与管理命令（含停机更正）均读取唯一的根密钥文件。文件、派生用途及算法由[密钥材料](keys.md#2-根版本与派生用途)规定。首次密钥登记要求元数据表全空，或全部与原登记一致；不自动补齐部分登记。

## 管理命令

`deployment admin initialize`、`deployment admin appoint`、`deployment admin correct` 和 `deployment admin correct-affiliation` 读取 `NUCLEUS_DATABASE__*`，字段与下文数据库连接设置相同。它们还要求提供真实的 `NUCLEUS_WECHAT__WEBSITE__APP_ID`，用于核对保留的网站认证与会话事实。存在小程序保留事实时，还须提供原 `NUCLEUS_WECHAT__MINI_PROGRAM__APP_ID`；缺少或改变该身份会拒绝完整性扫描。管理命令使用运行数据库用户名和 `CREDENTIALS_DIRECTORY/database.json`，不回退到迁移配置或 `migration-password`。

管理入口只选择数据库环境前缀和准确的网站、小程序 AppId 键。数据库组拒绝未知字段、显式空值及无效数字；预算缺省值与应用相同。供应商、HTTP 和其他应用环境组不参与管理设置装载。入口不读取 `.env`，也不构造供应商客户端或要求头像 URL。输入文件、原操作标识及实例互斥见[停机维护](operations/maintenance.md#目标命令与互斥)。

## 1. 加载边界

nucleus 设置装载只读取已导出的进程环境变量，不自动读取 `.env`，不修改环境，也不启动服务。`.env` 只在本地维护，不纳入 Git 跟踪。

示例环境文件只可作为部署说明，不能成为自动加载源。

加载关闭类型推断并保留空串，输入反序列化严格解析数字。因此布尔词、浮点、带空白的数字和全角数字一律拒绝。加载不做宽松转换。装配全部成功后才返回完整的私有字段设置值。

各消费模块拥有自己的设置、缺省值、值校验与错误。供应商业务设置类型只保留构造校验，不实现 `Deserialize`。模块不读取环境变量或文件，由 nucleus 装配后注入。

`nucleus::settings::Settings::load()` 使用 `config::Environment` 读取环境并映射嵌套键。私有输入结构通过 `serde_with` 解析格式：`DisplayFromStr` 读取数字和监听地址，`JsonString` 读取数组。`TryFrom` 接收解析后的值，调用消费能力的值类型和设置构造方法。主机白名单、消息签名、三种模板、供应商超时及网站认证与注册设置已接入。根可执行文件将结果交给供应商装配、共同启动与 HTTP 入口。各能力只接收自己消费的设置。

C1 设置接收已校验的 `domain::identity::message::Limits`。该类型拥有发码期限、部署总量、手机号和来源限额的默认值与范围校验。`limits::Mobile` 和 `limits::Source` 拥有各自组合规则。入口反序列化缺省字段时读取这些所有者的默认值，显式输入继续通过相同构造校验。

R1/R2 共用来源、Session 和恢复设置，默认值与校验归 `domain::identity::session::settings`。R1 待注册期限仍归 `domain::identity::authentication::session::settings`。可选的 `wechat.mini_program` 整组缺席表示手机号能力未配置，`mobile` 请求返回 503，不占用 code 或外呼。提供该组时 AppID 必填，AppSecret 从受保护的 `mini.json` 装载；缺失或无效即启动失败。保留小程序事实时，在线及 Administration 入口必须提供原小程序身份，不能当作网站或跳过核验。

`domain::startup::Settings` 拥有共享正数工作容量。`NUCLEUS_IDENTITY__AUTHENTICATION__CAPACITY` 同时约束 C1、两类 R1、R2、退出及现有其他写用例。候选及原退出结果 GET 不占写入名额，HTTP 预算另按[入口预算](http/admission.md)分组。设置加载不读取凭据文件、不修改环境，也不调用供应商；其他微信身份设置仍未接入。

数据库连接库已有独立的 `platform::database::Settings` 构造校验。它接收主机、端口、库名、用户名、在线池容量和阶段预算；具体范围见[数据库连接库](database.md#已交付的连接库)。应用与管理入口复用数据库输入结构及受控转换，环境来源仍由各入口选择。主机、端口、库名和在线池容量必填；容量由部署连接预算确定，不设默认值。各阶段预算缺省时取模块定义的默认值，显式空值或无效值拒绝。用户名由必填 `USER` 输入提供。共享的 `platform::database::assembly::Settings` 组合连接设置与必填证书路径。入口负责读取密码和证书文件；装配边界见[数据库连接库](database.md#已交付的连接库)。

## 2. 键名规则

部署键是 `NUCLEUS_` 前缀加上嵌套路径，层级之间用双下划线分隔。例如 `NUCLEUS_IDENTITY__MESSAGE__MOBILE__HOURLY` 对应 `identity.message.mobile.hourly`。段内的词用单个下划线分隔，每个词非空且只含 ASCII 大写字母或数字。

全大写名称、单一声明和不同时赋值父子路径是部署约定。加载器直接使用 `config::Environment`，不另行检查归一化前的名称或路径冲突，也不保证诊断这些约定的违反。未知字段和未知节由输入结构拒绝，装配层保留加载库的原始错误。前缀范围外的变量由环境源忽略。

## 3. 必填项

下表包含已交付和目标字段，实际接入范围见 §1。目标 nucleus 与 deployment 各自拥有输入键、来源和读取，值与设置校验仍归消费能力。已承诺能力的必填项缺失即启动失败；显式空串拒绝，不当作缺省。

| 部署键                                       | 接受条件                                                                         | 所有者                                            |
| -------------------------------------------- | -------------------------------------------------------------------------------- | ------------------------------------------------- |
| `NUCLEUS_WECHAT__MINI_PROGRAM__APP_ID`       | 配置小程序组时必填；非空，无 Unicode 空白或控制字符，保留原文，与网站 AppID 互异 | `domain::identity::mini::AppId`、Session 设置     |
| `NUCLEUS_WECHAT__WEBSITE__APP_ID`            | 非空，无 Unicode 空白或控制字符；保留原文，不要求固定前缀或长度                  | `domain::identity::website::AppId`                |
| `NUCLEUS_CHALLENGE__SITE_KEY`                | 非空，无空白或控制字符                                                           | `nucleus::identity::challenge::Settings`          |
| `NUCLEUS_CHALLENGE__HOSTNAMES`               | 非空 JSON 字符串数组，逐项按 §3.1 校验                                           | `domain::identity::challenge::settings::Settings` |
| `NUCLEUS_MESSAGE__SIGNATURE`                 | 非空，无控制字符；允许空格                                                       | `domain::identity::message::settings::Settings`   |
| `NUCLEUS_MESSAGE__TEMPLATE__AUTHENTICATION`  | 模板码字符串，非空且无空白或控制字符；登录与注册发码使用                         | `domain::identity::message::settings::Settings`   |
| `NUCLEUS_MESSAGE__TEMPLATE__MOBILE_CHANGE`   | 同上；换号第一次发码，发往账户当前号码                                           | `domain::identity::message::settings::Settings`   |
| `NUCLEUS_MESSAGE__TEMPLATE__MOBILE_BINDING`  | 同上；换号第二次发码，发往新号                                                   | `domain::identity::message::settings::Settings`   |
| `NUCLEUS_IDENTITY__AVATAR__ORIGIN`           | 绝对 HTTPS origin，按 §3.2 校验                                                  | `domain::identity::avatar::link::Link`            |
| `NUCLEUS_STORAGE__ROOT`                      | 私人文件根目录的绝对路径，严格位于指定挂载点内；目录须已存在                     | `platform::files::Settings`                       |
| `NUCLEUS_STORAGE__MOUNT`                     | 已挂载目录的绝对路径；逐层拒绝符号链接                                           | `platform::files::Settings`                       |
| `NUCLEUS_STORAGE__DEVICE`                    | 预期挂载设备的十进制 `st_dev`，启动时核对实际句柄                                | `platform::files::Settings`                       |
| `NUCLEUS_DATABASE__HOST`                     | 按 Rustls ServerName 校验，接受范围见[数据库连接输入](database.md#连接输入)      | `platform::database::Settings`                    |
| `NUCLEUS_DATABASE__PORT`                     | 必填整数，1 至 65535，无默认值                                                   | `platform::database::Settings`                    |
| `NUCLEUS_DATABASE__USER`                     | 非空且不含 NUL，保留原文                                                         | `platform::database::Settings`                    |
| `NUCLEUS_DATABASE__NAME`                     | 1 至 63 字节，无空白或控制字符                                                   | `platform::database::Settings`                    |
| `NUCLEUS_DATABASE__AUTHORITY`                | 绝对路径，无控制字符；指向信任证书 PEM 文件                                      | `platform::database::assembly::Settings`          |
| `NUCLEUS_HTTP__LISTEN`                       | 明文 HTTP 的 IP 与端口，按 `SocketAddr` 解析                                     | `nucleus::listener`                               |
| `NUCLEUS_HTTP__ORIGIN`                       | 准确的 HTTPS origin，按 §9 校验                                                  | `nucleus::identity::Settings`                     |
| `NUCLEUS_HTTP__TRUSTED_PROXIES`              | 显式非空 JSON CIDR 数组，只登记直接代理，拒绝全网信任                            | `nucleus::source::Settings`                       |
| `NUCLEUS_IDENTITY__AUTHENTICATION__CAPACITY` | C1/R1/R2 与两种网站退出共享的正数工作容量；无生产默认值                          | `domain::startup::Settings`                       |

三项模板的文字各自写明用途，但共享同一个签名，并属于同一份供应商赠送额度。本地校验只证明形状，不证明供应商已开通、模板已审核或凭据有效。

### 3.1 主机名集合

每项先拒绝百分号、空白和控制字符，再用 `url::Host` 解析：接受域名与 IPv4，拒绝 IPv6。域名继续经 UTS-46 的 STD3 字符集、首尾连字符检查和 DNS 长度检查，允许末尾根域点。重复名称按规范化结果合并，错误原因标明原输入数组下标。`domain::identity::challenge::settings::AllowedHostnames` 使用 `NonEmpty<Host>` 保存结果，直接构造与环境装载共用校验。

不接受 URL、端口和通配符，也不从请求的 Host 头推导。

### 3.2 头像 origin

只接受绝对 HTTPS URL 的 scheme、host 和可选端口，允许单个根斜杠。拒绝凭据、业务路径、query、fragment、空白与控制字符。依赖 URL 解析器自动修补才能成立的输入同样拒绝。

## 4. 可调项

未提供时在反序列化阶段取所属模块的默认值，不保留可选字段。显式提供的越界值照常拒绝。时间以秒为单位，供应商超时以毫秒为单位。

已接入的次数与容量按 `usize` 解析，再由消费能力校验范围；零和负数均无效。时间值先按 `u64` 解析，再由消费能力校验范围或计时截止点。输入层保留完整的 `u64` 范围，不使用按 `i64` 解析文本的时长适配器。

端口按 `u16` 解析。大于 65535 的值在输入反序列化时失败；端口为零由连接设置构造器拒绝。

| 部署键                                                | 默认值  | 允许范围（含两端）            | 所有者                                                          |
| ----------------------------------------------------- | ------- | ----------------------------- | --------------------------------------------------------------- |
| `NUCLEUS_IDENTITY__MESSAGE__LIFETIME_SECONDS`         | 300     | 120 – 600                     | `domain::identity::message::Limits`                             |
| `NUCLEUS_IDENTITY__MESSAGE__DAILY`                    | 100     | 1 – 100                       | `domain::identity::message::Limits`                             |
| `NUCLEUS_IDENTITY__MESSAGE__MOBILE__INTERVAL_SECONDS` | 60      | 60 – 600                      | `domain::identity::message::Limits`                             |
| `NUCLEUS_IDENTITY__MESSAGE__MOBILE__HOURLY`           | 5       | 1 – 5                         | `domain::identity::message::Limits`                             |
| `NUCLEUS_IDENTITY__MESSAGE__MOBILE__DAILY`            | 10      | 1 – 10                        | `domain::identity::message::Limits`                             |
| `NUCLEUS_IDENTITY__MESSAGE__SOURCE__MINUTE`           | 3       | 1 – 3                         | `domain::identity::message::Limits`                             |
| `NUCLEUS_IDENTITY__MESSAGE__SOURCE__HOURLY`           | 10      | 1 – 10                        | `domain::identity::message::Limits`                             |
| `NUCLEUS_IDENTITY__SESSION__MESSAGE_SECONDS`          | 86400   | 3600 – 86400                  | `domain::identity::session::settings::Settings`                 |
| `NUCLEUS_WECHAT__MINI_PROGRAM__SESSION_SECONDS`       | 2592000 | 3600 – 2592000                | `domain::identity::session::settings::Settings`                 |
| `NUCLEUS_IDENTITY__REGISTRATION__LIFETIME_SECONDS`    | 600     | 60 – 600                      | `domain::identity::authentication::session::settings::Settings` |
| `NUCLEUS_IDENTITY__REGISTRATION__HOURLY`              | 5       | 1 – 5                         | `domain::registration::Limits`                                  |
| `NUCLEUS_IDENTITY__AUTHENTICATION_RECOVERY_SECONDS`   | 600     | 60 – 600                      | `domain::identity::session::settings::Settings`                 |
| `NUCLEUS_IDENTITY__AVATAR__LIFETIME_SECONDS`          | 86400   | 60 – 86400                    | `domain::identity::avatar::link::Link`                          |
| `NUCLEUS_IDENTITY__AVATAR__RETENTION_SECONDS`         | 86400   | 正整数且时钟可表示            | `domain::identity::avatar::retention::Settings`                 |
| `NUCLEUS_IDENTITY__AVATAR__HOURLY`                    | 10      | 正整数且可表示为 SQL `bigint` | `domain::identity::avatar::UploadLimits`                        |
| `NUCLEUS_MESSAGE__TIMEOUT_MILLISECONDS`               | 10000   | 1000 – 10000                  | `domain::identity::message::settings::Settings`                 |
| `NUCLEUS_PROCESS__STOP_SECONDS`                       | 30      | 正数且能形成单调截止点        | `nucleus::process::Settings`                                    |

数据库在线池容量通过必填键 `NUCLEUS_DATABASE__CAPACITY` 提供，接受可表示为 `usize` 且至少为 2 的整数，不设固定业务上限；还须通过[连接库的技术边界校验](database.md#已交付的连接库)。阶段预算通过 `NUCLEUS_DATABASE__BUDGETS__WAIT_SECONDS`、`NUCLEUS_DATABASE__BUDGETS__CONNECT_SECONDS` 和 `NUCLEUS_DATABASE__BUDGETS__RECYCLE_SECONDS` 提供，默认分别为 3、5、3 秒。环境输入接受非零整数秒，并校验能否形成计时截止点；各项独立应用缺省值。容量和预算由 `platform::database::settings` 校验。

R2 每手机号滚动小时额度由 `domain::registration::Limits` 校验。业务事务在手机号协调下应用配置值，数据库保护不可放宽的 5 次上限。计数依据与不计数分支见[注册办理顺序](identity/registration.md#单事务办理顺序)，不另设 HTTP 注册限流键。

## 5. 组合规则

- 手机号小时额度不得大于其日额度，由 `domain::identity::message::Limits` 校验。
- 恢复期限不得超过任一种已配置会话期限，由 R1/R2 共用的 Session 设置校验。
- 同时配置两端身份时，Session 设置拒绝相同 AppID。

恢复期上限 600 秒与两种会话期限下限 3600 秒已经蕴含第二条。显式检查仍然保留，并按公开输入可达的最紧边界测试。

来源额度是业务发码额度，不替代 HTTP 请求限流。

## 6. 所有者与校验分工

| 模块与类型                                                      | 约束责任                                                                  |
| --------------------------------------------------------------- | ------------------------------------------------------------------------- |
| `domain::identity::message::Limits`                             | 发码有效期、日额度，以及嵌套的手机号额度与来源额度                        |
| `domain::identity::session::settings::Settings`                 | 网站短信、小程序手机号 Session 与恢复期限，固定身份及互异校验，R1/R2 共用 |
| `domain::identity::authentication::session::settings::Settings` | R1 待注册凭据期限的默认值及校验，并组合 Session 设置                      |
| `domain::registration::Limits`                                  | R2 每手机号小时额度的默认值及范围校验，已接入环境装载                     |
| `domain::identity::avatar::link::Link`                          | 头像 origin 与新签链接有效期，已接入环境装载                              |
| `domain::identity::avatar::retention::Settings`                 | 更换或注销后头像的保留宽限、默认值与校验                                  |
| `domain::identity::avatar::UploadLimits`                        | 每账户滚动一小时内实际更换次数；无需变更不计数                            |
| `platform::files::Settings`                                     | 挂载身份和私人根目录；头像是当前文件消费者                                |
| `domain::identity::website::AppId`                              | 已交付网站身份的受控构造及 SQL 解码；无 SDK 或网络依赖                    |
| `nucleus::identity::challenge::Settings`                        | 公开 site key 的受控构造与公开投影                                        |
| `domain::identity::challenge::settings::Settings`               | 主机名集合，已交付                                                        |
| `domain::startup::Settings`                                     | C1/R1/R2、网站退出与机构角色创建和修改共享工作容量及用例设置              |
| `domain::identity::authentication::Settings`                    | C1 发码限额与 R1 凭据设置的组合                                           |
| `nucleus::process::Settings`                                    | 应用停止预算，已接入环境装载                                              |
| `nucleus::identity::Settings`                                   | 匿名身份办理与本人 cookie 请求共用的准确 Origin 设置                      |
| `nucleus::source::Settings`                                     | 可信代理规范化及来源解析规则，已接入环境装载                              |
| `domain::identity::message::settings::Settings`                 | 签名、三项模板与供应商超时，已交付                                        |
| `platform::database::Settings`                                  | 主机、端口、库名、用户名、在线池容量与阶段预算；已交付                    |

## 7. 失败定位与脱敏

`nucleus` 装配层通过 `serde_path_to_error` 读取输入结构，保留失败的字段路径。路径采用加载器归一化后的字段名，例如 `database.port`，不转换为完整部署键名。嵌入 JSON 的格式错误定位到所属设置字段；主机名的业务校验继续保留数组下标。

`nucleus::settings::Error` 区分来源装载、输入反序列化和业务校验。`Config` 变体保留来源装载的 `ConfigError`。`Input` 变体保留字段路径，并将底层 `ConfigError` 放入来源链。缺失、未知字段和格式错误沿用同一反序列化错误边界。不再分别保留 `ParseIntError`、JSON 或地址解析错误类型。

反序列化通过后，转换继续返回消费能力的具体校验错误。预算错误标明 wait、connect 或 recycle 阶段。主机名、签名、模板、超时、来源和容量等业务校验保留各自原因。

| 失败                     | 保留的原因                          |
| ------------------------ | ----------------------------------- |
| 环境来源装载             | `ConfigError`                       |
| 缺失、未知字段、格式错误 | 字段路径与底层 `ConfigError`        |
| 业务值与组合校验         | 消费能力的具体错误类型              |
| SDK 校验，尚未接入       | 由实际消费者保留 SDK 的具体错误类型 |

未交付设置由实际消费者实现受控构造，不预设新的校验框架。

常规诊断只输出失败类别、字段路径、业务规则说明和主机数组下标，不回显提交值。加载器和依赖错误可能含有输入内容，保留的原因只供受控核对，不进入默认 `Debug` 或 `Display`。

## 8. 尚未交付

事务与时间采样的固定预算见[数据库](database.md#c1-接纳与可信时间)。设置及构造校验归[消费能力](architecture.md#进程内工作与容量)，环境键与读取归入口。测试容量不作为生产默认值。

以下能力尚未完整交付，不能据本文认为相关启动行为已经可用：

- 微信身份登录及绑定设置尚未交付；小程序手机号身份和期限、头像链接的 origin 和有效期已接入。
- 消息 RAM 凭据、人机校验密钥和小程序手机号 AppSecret 走[供应商凭据文件](platform/credentials.md)。[应用密钥材料](keys.md)使用根文件及 `encryption`、`lookup`、`proof`、`seal` 与 `link` 用途派生。这些秘密不经部署键；网站微信及其他身份流程仍未接入。
- 数据库连接的密码不是部署键，走服务单元提供的凭据目录，见[数据库](database.md)。
- 后续能力的启动核验和依赖检查；当前已核验 C1、网站短信 R1、网站 R2、正式身份、Access 保留事实及默认头像读取所需材料。

设置校验只证明形状与本地配对一致，不证明供应商已开通、凭据有效、真实受理或送达。

## 9. HTTP 设置

入口按[根包职责](architecture.md#根包入口目标)拆分消费设置。监听地址以 `SocketAddr` 交给 `listener`；匿名身份入口的 Origin 由 `identity::Settings` 校验，可信代理由 `source::Settings` 校验。根 `settings` 继续读取 `NUCLEUS_HTTP__*` 部署键并解析输入。内部模块重组保留现有键与解析契约，多监听设置尚未交付。

`NUCLEUS_HTTP__LISTEN` 明确指定监听 IP 与端口。生产的代理与可达范围见 [Nginx 接入目标](http/api.md#nginx-接入与传输目标)。`NUCLEUS_HTTP__ORIGIN` 是准确的 HTTPS origin，包含 scheme 与主机，可有非默认端口。不含凭据、路径、query 或 fragment。输入必须等于 URL 库的 origin 规范文本，包括不带尾部斜杠。校验通过后，身份入口设置以 `url::Origin` 保存该值。请求是否要求 Origin 由[所属 HTTP 入口](http/identity.md#路由与装配)决定，不由环境装载器推导。当前 C1、网站短信 R1 和 R2 要求该请求头必填、单值且与配置逐字节相等。本人 cookie 读取另接受[当前身份协议](identity/current.md#凭据选择与来源)规定的同源 Fetch Metadata。比较前不归一化请求头。

网站共用该 origin，由部署入口分别路由到网站托管与 nucleus。公开 site key 是必填非秘密设置，只投影到[公开设置接口](http/site.md#入口与响应)。部署本片后端前配置该键；网站构建不固化某个环境的值。微信接入时由 nucleus 构造固定回调与完成页 URI；客户端不得指定跳转目标。回调由后端处理，完成页按[网站交付边界](http/site.md)独立托管，应用身份及交接目标见[微信身份登录与共享绑定](identity/wechat.md)。

`NUCLEUS_HTTP__TRUSTED_PROXIES` 是明确给出的非空 CIDR 数组，只登记直接接入的 Nginx 对端。目标设计中，网站服务读取公开内容时直接接入，其准确地址同样登记在此，见[网站服务的后端读取](http/admission.md#网站服务的后端读取)。不接受全网信任，也拒绝多个网段合并后的默认路由。IPv4-mapped IPv6 网段按 IPv4 归一化。单值来源头的接纳与拒绝规则见 [HTTP 来源与入口资源预算](http/admission.md#可信来源)。nucleus 在启动时校验并将固定结果交给 HTTP 入口，不由 Identity 模块自行读取环境变量。
