# 数据库

本文规定数据库登录身份、结构交付、连接输入与运行权限的目标边界。业务事实、事务协调和恢复分别见[能力概览](overview.md)、[Identity 写入](architecture.md#原操作与恢复)与[基础设施](architecture.md#设置凭据与部署)。

连接库、nucleus 环境设置及密码与信任证书文件装载已交付，接口及验证见下节。独立[部署工具](engineering/deployment.md)已交付密钥元数据迁移和初始化。C1 接纳、人机校验与短信发送已接入持久结构、受管执行和提交未知核对。C1 HTTP 已接入，完整认证仍未交付。

## 操作安全

`nucleus` 是由 `production` 分支服务的生产数据库，禁止修改或删除。仓库同名不代表数据库目标安全。

本地 `.env` 引用的 `nexus` 是开发数据库，允许删除并重建。任何写入或破坏性命令前必须核实实际连接目标，不能只凭文件名或仓库名判断。`.env` 的维护与加载规则见[部署设置](settings.md#1-加载边界)。

编辑迁移前必须按 [SQL 迁移规则](engineering/sql.md#migrations)核实执行状态。测试与日常检查遵循[验证边界](engineering/verification.md#检查范围)。

## 已交付的连接库

`platform` crate 的 `database::Connections::new` 接收已验证的 `Settings`、`credentials::Password` 和 `rustls::RootCertStore`。构造只装配连接能力，不读取环境或文件，也不建立连接。密码接受条件沿用本文的[密码凭据](#密码凭据)。信任库必须非空，连接构造保留此项检查。TLS 显式选择 ring，仅信任传入的证书，并验证服务端主机名。连接要求 TLS，并设置空 `search_path`。用户名由调用方通过 `Settings` 显式提供，须非空且不含 NUL，保留原文。nucleus 设置转换传入配置的运行用户名；Platform 不选择身份，也不从环境读取用户名。

`database::User` 拥有用户名校验，字段私有，不反序列化部署输入。连接设置构造器与部署设置转换均调用 `User::new`。部署设置 getter、迁移执行和权限核验保留 `User` 类型；SQL 参数通过 `ToSql` 绑定。类型只保证名称格式，角色存在性与权限仍由数据库核验。

`acquire()` 从共用在线池借用会话。池由 `deadpool::managed` 管理，容量由调用方提供，至少为 2，不设固定业务上限。设置校验拒绝超过 Tokio 信号量上限或容器布局可表示范围的容量。布局检查使用包含内部槽位的 Deadpool 公共对象包装，保守防止预分配大小溢出；它不保证内存充足。部署按数据库连接预算确定容量，并为独立恢复连接、维护和其他客户端留出额度。等待、建连和复用检查各有独立预算，默认分别为 3、5、3 秒。显式值须大于零，且可形成当前平台的计时截止点；不设固定的 60 秒上限。建连预算覆盖解析、TCP、TLS 和数据库登录。各预算不等于整笔业务或 SQL 执行预算，也不证明容量满足生产负载。

每个 `Session` 共同持有 Client 与驱动任务。调用方只在事务结局可靠后归还在线会话。复用检查拒绝已结束的驱动或已关闭的 Client。随后分语句回滚残留事务、清除会话状态，并恢复空 `search_path`。任何复用失败或超时均淘汰该会话。复用检查不承担原事务结局核对。

`recover()` 使用相同 TLS 与身份规则另建连接，不占在线池名额。恢复最多一条，等待和建连分别受预算限制。许可由驱动任务持有，随驱动实际结束释放。恢复会话不缓存进在线池。该接口只交付建连能力，尚未执行恢复屏障或业务结局核对。

有疑义的在线会话通过 `Connections::discard` 永久移出池。该方法中止并等待本地驱动，保留非取消类的驱动错误。独立会话使用 `Session::terminate` 消费并收尾。持有会话的组件使用 `Session::stop(&mut self)`，取消等待后仍可继续等待原驱动。完成等待的调用报告驱动错误，后续收尾不重复读取任务结果。直接丢弃 Session 会请求中止驱动，但不提供等待结束的证明。调用方不得据此开放写入。结束本地驱动不证明数据库已经回滚，[写入协调](architecture.md#原操作与恢复)仍要求原操作锁与所属阶段的持久事实核对。

构造返回 `BuildError`，区分空信任库、TLS 配置及池构建失败。`ConnectionError` 只承担建连、建连超时与会话复用失败；池错误包装它，不递归包装外层运行错误。运行 `Error` 区分借池、等待恢复许可、连接和驱动收尾。默认 `Debug` 保留池超时阶段及后端操作，不输出连接目标、密码、SQL 或数据库 detail。原始驱动错误保留在来源链上，供受控检查。密码传入缓冲在释放时清零；驱动 Config 持有的内部密码副本没有清零保证。连接库不记录日志，也不自动重试业务 SQL。

`nucleus::database::load` 从凭据目录读取 `database.json`，再读取 `AUTHORITY` 指定的文件。nucleus 与部署入口共用 `platform::database::assembly::Settings`，它持有已校验的连接参数和证书绝对路径。入口拥有环境键、文件来源与文件读取；设置构建不读取文件。入口调用 `assembly::roots`，将 PEM 字节解析并装入 `RootCertStore`。`Settings::build` 接收已验证密码和信任库，再交给 `Connections::new`。证书解析使用 `rustls-pki-types` 的 PEM 证书迭代器，支持证书集合，忽略非证书段。PEM 格式错误和证书装载错误分别返回 `assembly::Error::Pem` 与 `Certificate`。空信任库由连接构造拒绝。装配不建立连接，不代表启动核验或应用就绪。

## C1 接纳与可信时间

`domain::identity::authentication::Service` 消费连接能力。在线写入使用 `Connections::lease`；只有明确提交或回滚才调用 `Lease::release`。取消、panic 或直接丢弃 Lease 会将会话移出池，不能交给另一请求。未知结局显式丢弃并等待驱动结束，再使用独立恢复连接。C1 依次确认共享屏障 `(1314210636, 1)` 和原操作锁，锁后另发查询核对原事实。首次接纳恢复只读取，不重新插入。原工作仍持有响应时，结论恢复按[所属阶段](identity/authentication.md#c1-阶段与外部调用)核对并保存，不因本地回写失败重复外呼。

`domain::runtime` 拥有可信时间采样。`Context::new` 在 3 秒预算内通过 `Connections::dedicated` 建立专用连接，再构造持有该会话的 `Clock`。建连失败结束已创建的连接资源，不返回上下文。运行期复用这一条连接，采样排队和事务共用 3 秒预算。`security.clocks` 只保留一行最大观察时间，运行身份仅有查询和该时点的列级更新权限。更新条件与触发器拒绝时间下降；提交确认后才交付样本。

采样失败关闭接纳；采样途中取消也保留失败状态，并请求中止原驱动。不重新连接或复用原会话。失败处理与运行上下文收尾等待驱动结束，重复停止可继续未完成的等待。新进程重新核验后才能开放。

`runtime::Error::ClockInvalid` 表示持久时间检查失败：必须恰好更新一行，且新样本不早于已保存时间。记录缺失与时间倒退均违反该条件。`ClockClosed` 表示此前采样已失败、中断或停止，本次不能继续采样。查询故障与超时保留各自错误；这些诊断不回显时间值。

`nucleus::process::Application::start` 创建运行上下文，接收本机锁，返回入口独占的 `Application`。请求通过 `authentication()` 共享 C1 服务。专用连接建立、取得启动屏障与权限核验共用 10 秒预算。`domain::database::Barrier::begin` 取得独占屏障，按实际登录身份核对权限。`Barrier` 将核验结果、屏障会话与同一上下文绑定。C1 服务接收它，在屏障内核对密钥、可信时间与全部保留行，然后确认启动事务。校验或收尾失败均不返回可用服务。

项目权限规则由 `domain::database::permissions` 拥有，部署入口共用同一核验。检查覆盖直接、继承及可通过 SET ROLE 取得的权限。列级写入与 MAINTAIN 也在检查范围内。所有 schema 的 CREATE 权限和所有权均在拒绝范围内。运行身份还不得直接或间接成为 `pg_read_server_files`、`pg_write_server_files`、`pg_execute_server_program` 的成员，包括不继承但可切换的成员关系。这些角色拥有超出业务表授权的服务器能力，见 [PostgreSQL 预定义角色](https://www.postgresql.org/docs/18/predefined-roles.html)。核验只拒绝过量权限，不自动撤销授权；失败时保持接纳关闭。

核验也拒绝 `session_replication_role` 的 SET、ALTER SYSTEM 权限，覆盖直接、PUBLIC、继承与可切换角色。该参数能禁用普通触发器与外键，见 [PostgreSQL 复制角色参数](https://www.postgresql.org/docs/18/runtime-config-client.html#GUC-SESSION-REPLICATION-ROLE)。应用启动还核对当前连接的生效值，拒绝从登录默认值带入的 replica 模式。检查不修改参数或授权。

C1 拥有保留行的业务核对：包含过期行，拒绝未来开始或结论时点，并检查类型、用途、原键、密文绑定和号码摘要。阶段与调用次数、结论时点共同核对；供应商时间沿用其 2 秒容差。Message、Check 与 claim 还核对配对、阶段、原键、工作归属和事件时间。历史按每批 128 行读取和核验，每批最多 10 秒，不限制扫描总时长。核验期间保持独占屏障，全部通过后才开放接纳；失败时结束原连接并保持服务关闭。密钥核验、游标建立和关闭、启动事务确认分别限制为 10 秒，可信时间采样沿用 3 秒预算。Challenge 接纳、阶段更新及恢复事务各使用 10 秒预算，借池和恢复建连另受连接设置约束。

调用方通过 C1 `Settings` 提供正数工作容量和 `message::Limits` 发送限额，通过 `nucleus::process::Settings` 提供停止预算，缺省为 30 秒。`Application::stop(&mut self)` 关闭接纳和新清理选择，等待 HTTP 读取与进程持有的清理任务，再通过 C1 `drain` 等待请求任务，最后停止运行上下文。所有阶段受同一个进程停止截止约束。请求工作登记持有未结束任务和已收集的失败，进程单独持有后台任务及其结束结果。就绪探测或新请求回收异常任务后，接纳保持关闭，原任务失败仍留给排空报告。取消排空等待不会丢失任务或失败；再次排空等待剩余任务结束后，一并返回此前失败。完整排空报告后才取走失败，后续停止不重复报告。`Context::stop` 结束采样，再由 `Connections::close` 关闭池及恢复入口并等待驱动结束。

运行上下文持有时钟连接的停止结果。等待其他连接结束时取消 `Context::stop`，已取得的错误仍保留在上下文中。共享句柄串行执行停止，全部连接结束后才取走结果并报告一次。后续停止不重复报告该错误。

本机锁随 `Application` 存活。停止等待被取消、预算耗尽或正常返回都不释放锁。入口完成停止收尾后释放应用对象，由锁守卫解锁；进程直接退出时，操作系统释放文件锁。直接丢弃应用对象不执行异步排空。停止超时不报告正常排空，调用方必须退出；本机锁不能单独证明旧数据库事务结束。

运行身份具有 Challenge 的必要插入权限，以及 `calls`、`status`、`solved_at`、`finished_at` 的列级更新权限。启动检查拒绝缺失或超出的授权，invoker 触发器保护原事实和合法状态迁移。可信时间表和 Challenge 结构由 `V2__challenge_acceptance` 交付，手机号的嵌套类型与约束由[短信认证](identity/authentication.md#challenge-手机号保护)拥有。`V3__message_sending` 增加 Message、Check 和 claim，只开放必要的查询、插入及 Message 结论列更新。Check、claim 不开放更新，`security.messages` 只读。

`V4__retention` 增加四张 C1 表的受保护 DELETE，以及持久序列的 USAGE。运行身份不能重设序列。期限、配对、隔离级别和共同提交保护由 [C1 保留与清理](identity/authentication.md#c1-保留与清理)拥有。应用启动和部署共用权限核验；HTTP 就绪探测继续复用持久可信时间。进程停止先结束清理选取，分别等待后台任务与请求任务，再关闭共享连接。完整维护及其他能力清理尚未接入。

## 登录身份与部署

结构归部署身份，运行期业务归运行身份。两者均由部署配置指定实际登录用户名，不约定固定名称。不设第三个登录身份，也不以命令专属的 `SECURITY DEFINER` 函数编排业务。运行身份不是对象所有者或部署身份的成员；它不持有结构变更、触发器或截断权限。应用不执行 DDL。

认证先接入、账户写入尚未交付时，运行身份对 Account 保持只读，但 R1 仍须取得与账户修改冲突的共享行锁。目标固定函数 `identity.lock_account(uuid)` 只取得该锁并返回 void，不接受输入 SQL、不承担认证或写入编排。该函数使用 `SECURITY DEFINER` 和空 `search_path`，收回 PUBLIC EXECUTE，只授权运行身份。启动核验同时检查直接、继承及列级权限，拒绝账户表的 INSERT、UPDATE、DELETE、TRUNCATE、REFERENCES、TRIGGER 和 MAINTAIN 权限。这一有限锁定能力尚未交付，不能通过开放账户更新权限替代。

部署先建立两个角色，再由高权限身份执行一次性准备：收回 PUBLIC 在数据库和 `public` 模式上的权限，分别授予连接权限，只向部署身份授予结构创建权限，并预先收回其未来函数的 PUBLIC EXECUTE 与未来类型、domain 的 PUBLIC USAGE。准备步骤不建立业务结构，也不包含密码。

结构由独立[部署工具](engineering/deployment.md)交付，应用启动不运行迁移。密钥元数据只保存[非秘密指纹](keys.md#持久元数据)，运行身份只有查询权限；材料仍留在凭据文件。

运行身份的 `search_path` 为空，运行语句写全模式名。每项运行权限随受保护的用例交付，采用必要的 SELECT、INSERT、列级 UPDATE 和受保护 DELETE。不可变字段优先由不授 UPDATE 表达；跨行与提交完整性使用真实外键、唯一约束、共同事务及必要的 invoker 触发器。触发器只承担声明式约束无法表达的状态迁移、父链、提交完整性和删除资格，不以可伪造的会话变量绕过保护。运行身份不能删除永久历史。

## 连接输入

迁移与应用各读取自己的部署键，互不回退。两组输入均包含 `HOST`、`PORT`、`NAME`、`USER`、`AUTHORITY`：迁移使用 `NUCLEUS_MIGRATION__*` 和 `migration-password`，应用使用 `NUCLEUS_DATABASE__*` 和自己的数据库密码凭据。环境装载归 nucleus 或部署程序，业务模块只接收已校验的连接能力；部署键规则见[设置](settings.md)。

应用主机使用 `rustls::pki_types::ServerName::try_from` 校验，与数据库 TLS 适配器使用相同解析器。接受其支持的 ASCII DNS 名、IPv4 和不带方括号的 IPv6；国际化域名须提供 ASCII IDNA 形式。保留输入原文，不进行 URL 解析、百分号解码或名称修补。URL、附带端口和 Unix socket 路径均拒绝。库名须非空，不含空白或控制字符，最长 63 字节。证书路径须为绝对路径且不含控制字符。迁移输入只接受声明的字段，未知字段拒绝。

连接固定要求 TLS，仅按 `AUTHORITY` 指向的证书核验服务端；系统信任库不参与。在线请求使用多连接池。恢复能力拥有独立建连途径，不占在线池容量；维护程序使用自己的连接能力。各阶段的等待、建连和复用预算由所属设置给出有效值，缺省值在装载时确定，不把缺省语义留作运行期 `Option`。独立恢复途径与未知提交期间的写入准入见 [Identity 写入](architecture.md#原操作与恢复)。

## 密码凭据

应用数据库密码从服务单元凭据目录中的 `database.json` 读取，不从部署键读取。文件为包含整数 `version: 1` 与字符串 `password` 的 JSON 对象。解码后的密码非空，UTF-8 长度不超过 4 KiB，不含控制字符；保留空格，不修剪输入。密码值中的尾部换行拒绝，JSON 对象外的合法空白允许。文件缓冲与解码后的密码使用可清零容器。凭据来源和读取边界见[凭据文件](platform/credentials.md)。本地格式校验不能证明部署主机上的角色密码或证书正确。

`platform::database::credentials::Password` 拥有密码值校验，不实现 `Deserialize`。nucleus 与部署入口共用 `credentials::Input`，将密码反序列化为 `Zeroizing<String>`。`TryFrom<Input>` 显式调用 `Password::new` 构建。错误字段类型在文件解析时拒绝，无效密码由构造器拒绝。错误不回显提交值，`Debug` 隐藏密码。连接构造只接收已验证密码，不重复校验。

## 跨能力保护

Access、Center、Workforce 与文件所有者接入时仍使用两个身份。初始来源只允许迁移身份建立，且源、目标和动作组合唯一。账户注销删除当前账户时，同一事务须有该账户的注销事实。申请首提交与批准首尝试的循环引用，在共同事务内以延迟外键闭合。编号恢复须关联一至两项撤销，原值核对、指针和占用变化与完整事实共同提交。

永久工牌材料从全部业务引用反查保护。已有成品时，资源不得进入 `deleting`，所有者记录也不得删除。照片 ID 在全部状态下保持原定位，合法清理只改变字节状态。技术清理须先证明原任务结束、事务结局已知且期限已到；完整规则见[本地文件](storage/files.md)。

每个触发器只承担具体保护，并取得查询所需权限。不能在读取其他行的 CHECK 函数中实现跨行保护。运行身份须覆盖全部正常与拒绝分支，不能只检查触发器存在。

## 交付验证

每个业务切片交付其实际需要的结构、授权和保护，并在隔离数据库中以真实的运行身份验证正常与拒绝分支。验证包括结构重跑、权限目录、明文连接拒绝、跨主体引用、不可变事实、状态迁移、提交完整性和清理资格。涉及文件的提交与清理按[本地文件](storage/files.md)验证；Access、Center 与 Workforce 的权限及不变量随各自业务切片交付。

共享测试设施由独立的 `fixture` crate 提供，仅通过 `dev-dependencies` 引入。测试 PostgreSQL 固定为 18.4，版本由 [fixture/postgresql.version](../fixture/postgresql.version) 维护。[构建脚本](../fixture/postgresql.ts)从 PostgreSQL 官方下载源码，校验固定的 SHA-256，再使用主机开发库编译。`postgresql_embedded` 只管理这份本地构建的临时实例，不再下载第三方二进制。启动前核对实际二进制版本，缺失、版本不符或动态库不兼容均使测试失败。

隔离服务只监听回环地址并要求 TLS。fixture 拥有临时证书、管理员密码和服务清理，不依赖工作区业务或连接库。Platform、nucleus 和部署测试复用该设施，各自构造连接、准备角色并验证自身行为。

启动失败时，fixture 在清理前报告原始错误。上下文包括固定版本、回环地址和临时集群路径。诊断从 `data/start.log` 末尾最多读取 16 KiB 正文。截断时另读前一字节，判断窗口是否从完整行开始。完整行保留，不完整行舍弃，并隐藏 fixture 密码。日志缺失或无法读取时，同时保留启动错误和读取错误。诊断不读取密码文件或私钥，也不保留临时集群。`cargo test -p fixture --locked` 覆盖真实配置错误、日志不可用、截断与脱敏。测试还检查正常停止及启动失败后的目录清理。

每个数据库测试套件共用一个隔离服务，每项测试拥有独立数据库。使用实际迁移和运行身份验证所需结构与权限。共享服务仅由活动测试持有；全局可保留 Weak 引用，不保留静态强引用。最后持有者释放时停止服务并清理临时数据。证书、密码和角色均为虚构测试材料。所有数据库测试使用 `--locked`；连接库以 `cargo test -p platform --locked` 执行。

已验证两笔独立事务并行、在线池耗尽时独立恢复建连、恢复连接额度、会话复用重置、失效会话替换及显式移除。还验证错误证书、主机名和密码拒绝、明文连接拒绝、TLS 拒绝不降级、握手等待超时，以及丢弃恢复会话后驱动结束。连接测试使用虚构角色 `fixture`，验证在线池和恢复连接均采用调用方指定身份。这些连接测试不构成业务权限与完整性验收；密钥元数据的迁移和权限由[部署测试](engineering/deployment.md#验证边界)覆盖。

在 Ubuntu 主机上先从本机发行版的官方源安装构建依赖，再构建测试数据库：

```bash
sudo apt-get update
sudo apt-get --no-install-recommends --no-remove install build-essential bison flex curl pkg-config libssl-dev libicu-dev libreadline-dev zlib1g-dev libxml2-dev
bun fixture/postgresql.ts
cargo test -p fixture --locked
```

构建保留 ICU、Readline 和 zlib 的默认支持。OpenSSL 与 XML 显式启用。二进制安装在 `target/postgresql/18.4`；临时构建全部成功后才发布该目录，不创建系统数据库服务。重复运行脚本核验并复用已有构建。主机升级导致动态库不兼容时，先确认没有测试运行，再移除这个生成目录并重新构建。源码版本与摘要更新必须一同审阅；不因官方源出现更新版本自动升级。

本地测试使用上述脚本准备 PostgreSQL，每次运行仍校验四个程序的版本和可执行性。测试创建全新的隔离实例。旧 CI 已停用，其安装缓存不构成当前验证证据，见[验证边界](engineering/verification.md#检查范围)。

Ubuntu 24.04 与 26.04 分别链接各自主机的库，无需安装跨发行版的兼容库。隔离验证不证明真实部署凭据、证书、主机环境或执行计划。C1 工作登记、提交未知恢复及库级生命周期已交付；HTTP 和进程信号已接入，具体运行证据与 systemd 限制见 [HTTP 入口](http/api.md#已交付入口)。

nucleus 以 `cargo test -p nucleus --locked` 验证固定文件来源、JSON 与密码边界、PEM 装载和诊断脱敏。装载测试通过虚构角色验证在线及恢复连接的身份与 TLS，以及错误密码和信任证书拒绝。生命周期测试另用实际迁移和运行身份验证 C1 启动、接纳、恢复与停止，覆盖范围见[短信认证](identity/authentication.md#首次接纳边界)。
