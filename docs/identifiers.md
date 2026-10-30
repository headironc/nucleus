# 项目标识

状态：认证、注册及正式身份事实已使用所属 Id 与 UUIDv4 生成。现有文本入口按 UUID 值解析，不限制版本、变体或规范文本。C1/R1/R2 与初始化已接入所属 Id 的 SQL 编解码。HTTP 中间件校验请求标识，响应不回显，见 [HTTP 协议](http/api.md)。本文拥有标识的格式、生成、验证、披露与顺序约束。

项目自生成的实体、业务事实、修订、执行、请求和 UUID 文件名均使用随机 UUIDv4。引用与恢复使用原标识，不重新生成。服务端生成实体及执行身份，客户端生成 `Operation-Id`；二者不能互换。文本输入采用 `uuid` 解析器支持的形式，包括大写、不带连字符、花括号和 `urn:uuid:` 形式。同一 UUID 的不同写法表示同一身份。输出统一为小写、带连字符的文本，数据库使用 `uuid` 类型。输入和存储接受 UUID 类型支持的全部值，不限定版本或变体。零 UUID 和全一 UUID 没有特殊业务含义；生成策略与接纳规则分开。

标识列使用 PostgreSQL 原生 `uuid` 类型，Rust 业务类型封装 `Uuid`。查询、行锁、消费引用及加密绑定保留所属实体的 Id，直到 SQL 或密码字节边界。Account、Authentication、Session 和 Registration 的标识不能互换。解析和数据库解码不附加版本或变体检查。真实唯一约束与外键负责身份及归属。

标识不携带项目解释的时间语义，也不授予读取或执行权。受限人员档案的版本标识只向有权查看者披露。密钥版本号、微信及其他供应商标识、头像公开摘要、自然键、业务编号和 OpenAPI `operationId` 保持各自格式。供应商允许自定义 UUID 时仍须验证其真实接纳，不能凭本地格式推断联调完成。

## 1. 生成与解析入口

服务端自生成的 UUID 标识由各所有者的 `Id` 类型直接调用 uuid crate 的 `Uuid::new_v4()` 产生。它使用系统安全随机源，随机源不可用时 panic。不设独立的生成服务或持久计数器，各业务 `Id` 类型仍然分开。

Challenge、Message、Authentication、Session 和 Registration 的 `Id` 类型各自提供上述生成入口，已有文本解析使用 `Uuid::parse_str`。现有 PNVS 请求使用 Message ID 作为 `OutId`，C1 同时将它用作 Check ID。正式开户使用各自 Person、Revision、Account 和 Avatar 标识。R2 的 Attempt 与成功 Receipt 使用同一办理事实标识，Session 另有独立标识。类型本身不证明供应商接纳或业务事实已保存。

客户端自行生成的原 `Operation-Id` 不经本入口，由各客户端按 HTTP 契约生成、保存并在原重试中复用；服务端只校验它。`domain::identity::operation::Id` 从客户端文本受控构造，供 C1、R1 与 R2 使用；该类型不提供服务端生成入口。数据库编解码保留同一 UUID 值。共同响应不回显请求标识；有权读取的原完成若包含业务标识，仍按统一文本输出，不能与本次查询的请求标识混同。

日志的 `request_id` 是服务端为单次 HTTP 尝试生成的 UUIDv4，由 `tower-http` 的 `MakeRequestUuid` 生成。它不代表业务事实，也不替代客户端的 `Operation-Id`。关联、豁免入口与响应边界见[运行观测](architecture.md#就绪与观测)。

C1 HTTP 入口拒绝请求头缺失、重复或多值，不自动生成替代标识。首次接纳在对象协调后以数据库时间记录开始和截止。服务不从 Operation-Id 提取时间或据此拒绝新操作。当前 UUID 生成依赖使用 `std` 与 `v4` feature。Identity 另启用 `serde`，供供应商 UUID 字段读取及 Operation、Message ID 的 JSON 输出。两个 ID 类型透明委托 UUID 序列化，接口结构保留值类型。Challenge 与 Operation ID 的 SQL 转换委托 UUID 编解码。它们接受原生 uuid 类型，不附加版本或变体限制。解码成功不代表原操作已通过核验。

同文件测试覆盖 v4 生成及同值不同文本的解析，包括其他版本与变体、零 UUID 和全一 UUID。非法文本和非字符串 JSON 输入仍被拒绝。Turnstile 的 `cdata` 和 PNVS 的 `OutId` 按 UUID 值关联原请求，异值仍拒绝匹配。PNVS nonce 接受 UUID 值，不替代调用方的随机生成与防复用责任。固定签名向量与本地 HTTP 替身检查原请求字节。这些检查不构成真实供应商接纳或完整认证验收。

进入 HTTP 输入或输出的 UUID 标识，由所属值类型派生 `ToSchema`。UUID schema 由 utoipa 的 `uuid` 支持生成。消费字段直接保留业务类型，不重复标注字符串和 UUID 格式。现有认证、注册、本人资料及退出响应引用所属具名组件。`Operation-Id` 参数以 `Uuid` 声明。OpenAPI 描述不改变上述 UUID 值解析规则。

## 2. 时间与顺序

接纳、截止和清理使用所属事实中明确保存的数据库时点。标识不代替生成顺序或提交次序；需要顺序时由所属业务保存明确的顺序事实。原操作防重放与最后一份结果证据的清理边界见[写入协调](architecture.md#原操作与恢复)。

## 3. 随机材料边界

客户端原操作的 `Operation-Id` 只作格式校验，服务端不会替客户端生成。短信验证码由独立随机源产生，不使用 UUID 文本。

安全随机材料另有归属，不由标识生成带出来：

- 会话 token
- 注册凭据 token
- recovery secret
- 短信验证码
- AEAD nonce

这些材料使用共享的安全随机源，见[密钥材料 §5.3](keys.md#53-随机源)。AEAD nonce、短信验证码、会话 token、注册凭据 token 和 recovery secret 随各自用例接入。
