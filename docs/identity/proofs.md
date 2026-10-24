# Identity 原输入证明目标设计

状态：C1 原 token 的 HMAC 证明及原版本核对已交付。R1 原 secret 的输入解析、SHA-256 证明生成及本地比对已交付；原命令摘要、持久保存及事务核对尚未实施。本文规定 C1 原 token 证明、登录、绑定、解绑、正式注册的原输入摘要，以及三种微信 code 的占用摘要。原操作证明只用于识别同一办理；业务资格、期限和结果由[短信认证](authentication.md)、[正式注册](registration.md)、[小程序手机号认证](mini-mobile.md)及[微信身份](wechat.md)各自判断。密码用途和长度前缀编码由[密钥材料](../keys.md)拥有。

## 输入与编码

下列公式使用[长度前缀编码](../keys.md#51-长度前缀编码)的 `E`。字符串编码为 UTF-8，已校验的 UUID 编码为 16 字节。手机号使用[匿名身份办理 HTTP](../http/identity.md#请求字段) 的规范值，姓名和身份证号使用[正式注册](registration.md#业务边界与不变量)的规范值；短信验证码保留八位 ASCII 数字及前导零。微信平台 code 使用原受控字符串，不修剪、不转换大小写或作 Unicode 规范化。

输入类型先完成解析和规范化，再按固定顺序编码。JSON 字段顺序、缩进及等价转义不改变摘要；缺失、重复或未知字段由原输入契约拒绝。所有摘要均保存完整 32 字节，不截断。原操作重放使用记录上的密钥版本，不能以当前活动键重算旧行；密钥缺失是依赖故障，不是输入不匹配。

## C1 原 token 证明

C1 使用独立 `challenge` 密钥用途。摘要保留完整 32 字节：

```text
hash = HMAC-SHA-256(
    K_challenge[key_id],
    E("nucleus/identity/challenge-token/v1", token)
)
```

`token` 使用受控 `challenge::Token` 的原始 UTF-8 字节，不修剪或规范化。输入不加入 Operation-Id、手机号、发码用途或来源，使同一 token 换用这些输入后仍可查出占用。原操作、用途、规范手机号与来源继续由 Challenge 事实绑定；摘要不替代这些核对。

`domain::identity::keys::challenge::Key` 只由已校验的 Challenge 密钥集合构造，材料来自该用途专用的 `challenge-keys.json`。`keys::challenge` 拥有单个键的版本、材料与指纹，`challenge::proof` 拥有 token 编码和证明值。`Key::prove` 生成 `Proof`，`Key::verify` 按本键核对原 token，返回匹配与否；两者委托 `proof` 内部实现，编码失败保留为技术错误。材料与证明不提供普通文本或调试输出。新证明使用活动键，原请求按记录的 `key_id` 取得原键；缺键不得回退到活动键或解释为证明不匹配。

新 token 占用在同一稳定对象协调内，按全部保留版本计算摘要并查询；没有命中才用活动键形成占用。原 token 不持久保存，因此不能通过换钥重算旧占用。旧材料保留到对应 Challenge 的占用与原证明比较依赖全部结束。供应商 token 过期不终止原结果核对，也不允许用新 token 替代原证明。

文件装载、证明计算和本地核对已有实现，C1 首次持久接纳已接入原证明核对。摘要不能代替数据库唯一约束、事务锁或供应商调用占用；持久事实及恢复见[短信认证](authentication.md#首次接纳边界)。

Platform 的 HMAC-SHA-256 接口使用自己的 `hmac::Key` 和 `hmac::Tag`，ring 类型留在实现内部。算法的消息输入只接收完成长度检查与前缀组合的 `Encoded`，该类型不验证业务字段含义。Challenge 私有的 `Input` 只接收受控 `Token`，实现 `cryptography::Input<2>`，生成与验证共用这一协议输入。`domain::identity::keys` 的指纹输入从受控密钥 ID 与固定长度材料引用构建，实现 `cryptography::Input<4>`；编码时使用 ID 的大端字节。两种实现分别固定协议域、用途和字段顺序。调用处向 `Encoded::new` 传入具名输入，不能直接传字节列表。具名字段与 trait 的字段数量约束输入组合，排列顺序仍由独立固定向量验证。

`Tag` 只由签名计算产生，不提供原始字节构造或 SQL 解码。HMAC 核验直接接收待核验的 32 字节，不要求先把它构造成计算结果。

`Proof` 私有保存 32 字节，提供计算、核验、字节视图及 SQL 编解码。`ToSql` 保存 bytea，`FromSql` 检查完整长度，拒绝 NULL；解码不验证本次请求。`Key::verify` 按原键核对 token，构造摘要不构成验证成功。启动核验检查证明长度和原键存在，原请求回读时才验证匹配。`Debug` 只输出类型占位，不包含证明字节。

## 登录、绑定与解绑

原命令摘要使用 input_proof 用途。R1 的证明与摘要由 Authentication 保存；需要外呼前持久接纳的微信及小程序协议由各自原 Request 保存。Request 仅用于这些有先行协议事实的办理，不是全系统操作账本。下式以 record 表示所属记录，编码域和字段顺序保持一致。固定路由决定 method、path、purpose，path 不含 origin、查询串或 fragment。实际 app_id 由入口身份决定，transport 只取 cookie 或 bearer。

```text
record.hash = HMAC-SHA-256(
    K_input_proof[record.key_id],
    E("nucleus/identity/request/v1",
      method, path, purpose, app_id, operation_id, transport)
    || payload
)

record.proof = SHA-256(decoded_secret)
```

| 原命令             | method | path                       | purpose | payload 的准确字段顺序                 |
| ------------------ | ------ | -------------------------- | ------- | -------------------------------------- |
| 网站短信登录       | POST   | /sessions                  | login   | E("message", message_id, mobile, code) |
| 小程序手机号登录   | POST   | /sessions                  | login   | E("mobile", MobileCode)                |
| 小程序微信身份登录 | POST   | /sessions                  | login   | E("wechat", LoginCode)                 |
| 网站微信登录发起   | POST   | /sessions                  | login   | E("website")                           |
| 小程序绑定         | POST   | /accounts/current/union-id | bind    | E("wechat", LoginCode)                 |
| 网站绑定发起       | POST   | /accounts/current/union-id | bind    | E("website")                           |
| 解绑               | DELETE | /accounts/current/union-id | unbind  | E(binding_revision)                    |

网站短信分支使用固定网站 AppID 和 `cookie`；`message_id` 编码原 UUID 的 16 字节。`binding_revision` 来自原 `Binding-Revision` 头。payload 的 `E` 直接接在公共字段之后，不再整体加一层长度前缀。网站微信发起时尚无供应商 code；回调不能改写原 `record.hash`。

`domain::identity::request::Secret` 已受控解析客户端原 secret：只接受 43 字符的规范无填充 Base64url，并持有可清零的 32 字节值。解析错误保留 `DecodeSliceError` 来源；来源可能含出错字符和位置，HTTP 入口不得将其写入日志或响应。`Secret::proof()` 对解码后的 32 字节计算 SHA-256，并返回 `domain::identity::request::Proof` 值类型；它不哈希 Base64url 文本。`Proof` 的 `PartialEq` 使用 `subtle` 对两个固定长度证明作常数时间比对，并实现 `Eq`。当前只有 `Secret::proof()` 能构造证明；数据库行解码随持久化切片交付。此计算和比对没有可恢复的失败分支，也不生成客户端 secret。证明的持久保存及事务核对仍未实施。secret 文本及其证明均不加入输入摘要。命中原请求时先核对 secret，再核对用途、应用、来源和完整原输入。绑定或解绑记录另存的原账户、会话与期望修订仍按所属用例核对；不把重放时的新会话或当前修订替换进去。独立结果读取只用所属原证明，不要求重新提交 code 或完整命令。

## 正式注册

R2 使用独立输入域和 `input_proof` 用途：

```text
attempts.hash = HMAC-SHA-256(
    K_input_proof[attempts.key_id],
    E("nucleus/identity/registration/v1",
      "POST", "/registrations", operation_id, transport,
      registration_id, name, id_number)
)

attempts.proof = SHA-256(decoded_registration_token)
```

`registration_id` 是 R2 正文中的 ID，`transport` 来自已核验的 `Session-Transport`。注册 token 按规范无填充 Base64url 解码为原 32 字节。手机号与实际 AppID 从可信待注册来源取得，客户端不能另行指定，也不为计算本摘要增加字段。原成功依靠长期 Receipt 的凭据 ID、token 证明及注册时实名版本核对；短期 Attempt 和其输入密钥清理后，不能以当前实名资料重建原摘要。原拒绝则使用该 Attempt 的密钥版本和原输入核对，不依赖已消费的待注册凭据重建编码。

## 微信 code 占用

三种协议分别是小程序手机号、小程序微信身份及网站授权；同协议内的登录和绑定共享 code 占用。协议标签是以下固定 ASCII 字节，不由请求者选择：

| 协议           | `protocol`       |
| -------------- | ---------------- |
| 小程序手机号   | `mini_mobile`    |
| 小程序微信身份 | `mini_wechat`    |
| 网站授权       | `website_wechat` |

摘要使用 `wechat_proof` 用途、实际应用标识和待交换的原 code 字节：

```text
hash = HMAC-SHA-256(
    K_wechat_proof[key_id],
    E("nucleus/wechat-code/v1", protocol, app_id, code)
)
```

输入不加入用途、Operation-Id、请求 ID、secret 或浏览器证明。新占用先按所属协议的全部保留密钥版本计算并查询，未命中才用活动键插入；原 code 没有保存明文，换钥不能重算并替换旧占用。原命令重放仍需核对原证明和输入摘要；正确重复 callback 核对原 state、流程 cookie 与 code 占用，不重新交换。摘要匹配不替代期限、当前账户、原会话或绑定修订检查。秘密和摘要不进入普通日志。

## 验收

用独立固定向量验证域、字段顺序、长度前缀字节边界、全部命令分支和三种微信协议。算法向量与项目编码向量分别验证，不能用被测实现产生期望值。覆盖同键异输入、错误 secret、规范等价的 JSON、code 跨用途占用、跨应用与跨协议隔离、旧密钥版本保留及合法退役。验证原结果读取不要求重新提供一次性 code，也不因摘要匹配绕过当前资格检查。
