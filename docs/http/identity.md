# 匿名身份办理 HTTP 目标设计

状态：C1 的 `POST /codes` 已交付。共同提取、错误响应及必填 Origin 已接入，边界见 [HTTP 交付状态](api.md#已交付入口)。R1、R2 仍为目标设计。C1 与短信 R1 共用的手机号解析由 `domain::identity::mobile::Mobile` 实现。客户端原操作标识由 `domain::identity::operation::Id` 解析。C1 原 Turnstile token 由 `domain::identity::challenge::Token` 解析，R1 原 secret 由 `domain::identity::request::Secret` 解析。`Session-Transport` 的精确取值由 `domain::identity::request::Transport` 解析。这些类型已有单元测试。本文拥有 `POST /codes`、`POST /sessions` 和 `POST /registrations` 的 HTTP 输入、结果形状与入口装配范围。[HTTP 入口与响应](api.md)拥有共同请求头、封装和应用码规则；[短信认证](../identity/authentication.md)、[小程序手机号认证](../identity/mini-mobile.md)、[微信身份登录](../identity/wechat.md)和[正式注册](../identity/registration.md)分别拥有业务资格、原办理事实与恢复。网站 cookie 交接见[网站持久会话](../identity/website.md)。

## 路由与装配

Identity 请求适配位于根包 `src/identity/`。当前 C1 已归其 `authentication/`，调用 `domain::identity::authentication` 的业务用例。注册仍为目标设计，保留独立入口与跨能力用例，共同规则见[根包入口目标](../architecture.md#根包入口目标)。

C1 的调用链依次经过共同操作标识中间件、可信来源提取、入口预算、Origin 检查和 JSON 提取，再调用业务服务。操作标识和可信来源均从已建立的 extensions 读取；`Input` 只描述 `{ mobile, token }` 正文。处理函数把独立取得的标识、来源和值组成领域输入。可信来源及预算归[入口资源预算](admission.md)，JSON 和 Extension 的自定义 rejection 归[共同提取边界](api.md#提取与错误归属)。

`authentication::input` 拥有正文结构及向领域输入的显式转换。共享 `Json<Input>` 提取器委托 Axum 读取和反序列化正文，处理函数取得解析后的输入。`authentication::request::admit` 中间件仅装配到 `POST /codes`，在正文前提取来源、执行入口预算及 Origin 检查。通过后将 `Source` 放入 extensions，供处理函数读取。来源不重复解析，入口额度不重复扣减。其他方法的 405 处理不经过这项中间件。领域库不依赖 Axum，手机号和 token 的值校验由各自类型拥有。

`POST /codes` 面向网站短信办理，必须携带唯一且逐字节匹配配置 HTTPS origin 的 `Origin`。缺失、空值、`null`、重复、多值或不匹配均返回 403 `permission_denied`，不创建 Challenge 或调用供应商。它是匿名入口自身的浏览器来源限制，不依赖当前会话；附带 Authorization 不能绕过。该要求不证明客户端身份，非浏览器客户端可以自行构造 Origin。小程序手机号认证使用所属授权入口，不据此扩展 C1 的来源范围。

C1 的 HTTP 适配区分业务拒绝、额度限制、结果未知和依赖不可用，并保留原始结构化原因。`authentication::error::Code` 拥有 `verification_invalid` 和 `verification_required`，并复用[通用错误码](api.md#json-结果与应用码)。处理函数在用例失败后，显式等待 `identity::authentication::error::prepare` 准备响应数据。该步骤直接使用领域分类，仅额度拒绝通过用例取得可信等待秒数。HTTP 状态和错误码在同一处同步映射，字符串编码不重新判断业务错误。`From` 和 `IntoResponse` 不执行异步采样。采样失败保留原额度原因和采样错误，记录一次 `quota_response` 诊断，返回依赖不可用且不附 `Retry-After`。等待秒数不得用系统时间或任意值代替。普通业务失败沿用工作完成边界的诊断，不在响应准备时重复记录。原操作结果未知时，收尾失败不能把它改成已知拒绝；其余收尾失败作为依赖不可用。

正文接纳形式和 8 KiB 上限沿用[共同 JSON 协议](api.md#路由与请求边界)。正文读取没有独立超时，传输仍受[连接限制](api.md#已交付入口)约束。业务调用最多等待 30 秒，已接纳结果仍未知时返回 `outcome_unknown`，不取消受监督工作或释放其占用。数据库、供应商和后台工作保留原预算；最终失败诊断归[实际完成边界](../architecture.md#就绪与观测)。

三个入口均为匿名业务入口，要求合法 `Operation-Id`。`POST /sessions` 和 `POST /registrations` 还要求 `Session-Transport`，准确取值为 `cookie` 或 `bearer`。`domain::identity::request::Transport` 只解析单个取值；头缺失、重复或多值的拒绝留给 HTTP 入口。服务端按已验证来源决定实际应用身份和会话传递方式；请求头只声明期望交接，不能更改来源。三个入口均不根据请求正文、原请求重试或附带的 cookie、Authorization 临时要求当前会话。新业务调用使用新操作标识；重试保留原标识、证明和业务输入。

| 入口                  | 请求                                                              | 可交付的具名成功结果                                                                                                             |
| --------------------- | ----------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------- |
| `POST /codes`         | `{ mobile, token }`；`token` 是 Turnstile 证明。                  | HTTP 200 的 `message` 或 `progress`。                                                                                            |
| `POST /sessions`      | `{ secret, credential }`；`credential` 使用本节封闭联合。         | HTTP 200 的 `session`、`registration` 或 `progress`；网站微信首次授权为 HTTP 201 的 `authorization`，原等待授权回读为 HTTP 200。 |
| `POST /registrations` | `{ id, token, name, idNumber }`；`id` 和 `token` 来自待注册凭据。 | 首次成功为 HTTP 201 的 `session`，原成功交接为 HTTP 200 的同一 `session`；可靠在途为 HTTP 200 的 `progress`。                    |

入口只在对应能力及真实依赖交付时装配。首个手机号注册单元接纳 `message`、`mobile` 两种登录凭据；`wechat`、`website` 分支随微信身份及网站授权能力交付，未接入时作为无效输入拒绝，不发起供应商调用。Router 和 OpenAPI 只声明实际装配的分支。小程序手机号、微信身份和网站授权各自的来源与业务结果见所属文档。

## 请求字段

`POST /codes` 和短信登录凭据的 `mobile` 使用同一解析规则。只接受 11 位 ASCII 数字，并完整匹配以下项目号段。拒绝 `+86` 等前缀、首尾或内部空白以及标点，不修剪或转换输入。集合外的 JSON 字段值按目标协议返回 HTTP 422、`invalid_input`，不进入业务写入。号段不证明号码已分配或调用者控制它。

```regex
^(?:13[0-9]|14[01456879]|15[0-35-9]|16[2567]|17[0-8]|18[0-9]|19[0-35-9])[0-9]{8}$
```

`POST /codes` 的 `token` 是非空、最多 2048 字符的供应商不透明证明，不按本系统的 Base64url 凭据解析。HTTP 输入通过 `domain::identity::challenge::Token` 执行该校验。它只用于本次人机校验及原 C1 证明，不是短信 code。原请求重试仍带原手机号和原 token；已保存发送许可时不再次消费 token 或发送短信。Siteverify 协议与接纳见 [Turnstile 人机校验](../platform/turnstile.md)，原调用占用见[短信认证](../identity/authentication.md#c1-与-r1-的目标事务契约)。登录与注册入口不额外要求 Turnstile。

`POST /sessions` 的 `secret` 是客户端为本次 R1 独立生成的 32 字节安全随机值，传输为规范无填充 Base64url。`domain::identity::request::Secret` 已交付格式解析；HTTP 字段绑定尚未实施。客户端在首次发送前保存它、操作标识和完整凭据；原请求重试保留三者。`credential` 以 `type` 为内部标签，只接受下列准确字段。缺失、`null`、多余字段或不在当前已装配联合内的分支返回 HTTP 422、`invalid_input`。

| `type`    | 其余必填字段           | 来源与用途                                                       |
| --------- | ---------------------- | ---------------------------------------------------------------- |
| `message` | `id`、`mobile`、`code` | 短信消息 ID、规范手机号和固定八位 ASCII 数字验证码；保留前导零。 |
| `mobile`  | `code`                 | 小程序手机号授权的 `MobileCode`。                                |
| `wechat`  | `code`                 | 小程序微信身份登录的 `LoginCode`。                               |
| `website` | 无                     | 网站微信授权发起；OAuth code 由回调提供，本请求不接收 `code`。   |

短信验证码必须作为 JSON 字符串传入，不能使用数字、空白或其他位数。`domain::identity::message::Code` 已实现八位 ASCII 数字解析，`domain::identity::message::Id` 已实现短信消息 ID 解析；HTTP 字段绑定尚未实施。小程序手机号、微信登录和网站 OAuth 的 code 属于不同协议，不能交换用途。`website` 仅是 HTTP 分支名；已建立的微信身份认证及会话方法仍为 `wechat`。绑定入口的合法凭据范围由[微信身份登录](../identity/wechat.md)拥有，不能把本表四种登录凭据一并接纳。

`POST /registrations` 使用独立于原登录的操作标识。`id`、`token` 来自原 `registration`；原 R2 重试保留二者及原姓名、身份证号和可信传递方式。姓名、身份证号的规范化与办理日边界由[正式注册](../identity/registration.md#业务边界与不变量)拥有。HTTP 不接受已验证手机号、账户 ID 或由客户端选择的认证来源。注册成功不提供历史完成查询，也不增加凭 Bearer 读取旧完成的分支。

JSON 请求遵守共同的 8 KiB 上限，拒绝未知、重复、缺失、`null` 或类型错误的必需字段。除所属协议明确允许的缺席值外，反序列化结构自身声明必填字段与封闭联合；传输解析不把供应商 code、注册 token 和会话 token 混成一种可互换证明。实体 ID 与 `Operation-Id` 各自按[项目标识](../identifiers.md)验证。

## 结果与恢复

成功 JSON 使用共同的 `code: "ok"`，并且仅包含一项适用的具名结果，不回显请求标识。`message` 为 `{ id, delivery, expiresAt }`；`delivery` 取 `sending`、`sent`、`failed` 或 `unknown`。它表示本地消息与供应商受理进度，`sent` 不证明送达，`unknown` 不等于数据库提交的 `outcome_unknown`。可靠在途使用 `progress: { status: "pending", retryAfterSeconds }`；暂时查到未完成行不足以证明仍在执行。

无账户的手机号认证返回 `registration: { id, token, expiresAt }`，不建立正式账户或会话。`id` 定位待注册凭据，`token` 是注册证明；两者不能代替新 R2 的操作标识。有账户的认证和正式注册成功返回原会话。小程序 Bearer 结果为 `session: { token, expiresAt }`；网站结果为 `session: { id, expiresAt }` 并按原会话设置 cookie，JSON 不含会话 token。token 使用规范无填充 Base64url，解码后为 32 字节。时间使用 UTC RFC 3339。

网站微信授权返回 `authorization: { id, href, expiresAt }`，仅表示可继续浏览器授权，不表示登录成功。授权开始后的其他分支及回调结果由[微信身份登录](../identity/wechat.md#入口与结果)拥有。任何 `session` 响应都不附带完整本人资料；客户端另用 `GET /accounts/current` 验证当前身份。`message`、`registration`、`session`、`authorization` 与 `progress` 不在同一响应中并列。

原请求先匹配证明、规范输入及所属用例要求的应用来源，才能交付原结果。R1 的错误 secret、R2 的错误注册 token、C1 的错误原 Turnstile token 均返回 `authentication_invalid`；证明成立但原规范输入或要求绑定的应用来源改变返回 `request_mismatch`。C1 重试允许网络来源变化，统计归属仍使用首次可信来源，见[首次接纳](../identity/authentication.md#首次接纳边界)。原 R2 成功优先于新办理凭据检查；恢复期内只交付仍有效的原会话，不新建或续期。原会话、账户或交接不再有效时返回 `verification_required` 并要求重新认证。HTTP 断连、响应超时和 `progress` 都不证明原业务回滚；提交结局不明返回 `outcome_unknown`，客户端保留原请求继续核对。

已接纳的 R1、R2 确定拒绝由所属用例持久保存，并按原应用码返回。手机号或身份证号占用分别为 `mobile_taken`、`id_number_taken`。新 C1 的人机证明被供应商明确拒绝，或已被另一 C1 占用时，返回 HTTP 403、`verification_invalid`，不披露占用者。原人机证明已无继续资格，或供应商已通过但本地截止到达时，返回 HTTP 410、`verification_required`。R1 的消息或验证码已失效、消费、到期或达到猜错上限时，也返回 `verification_required`，不创建认证结论。

供应商超时、服务错误、密钥配置错误、缺失或异常的成功时间戳返回 `dependency_unavailable`，不算用户猜错。错误证明、输入冲突和格式错误只拒绝本次调用，不消费原证明、不增加注册次数，也不返回原输入或他人的办理定位。数据库故障同样不伪造确定拒绝。原 C1 的发送进度、R1 结论和 R2 原完成的细节由各自业务文档裁决。

## 验收

C1 原结果仅在记录保留期间可回读。清理后的请求按新申请处理，不承诺固定返回 404；原标识不免除人机校验与额度检查。集成测试由进程入口触发清理，再提交旧证明；供应商替身拒绝后返回 `403 verification_invalid`，没有新 Message。保留条件见[短信认证](../identity/authentication.md#c1-保留与清理)。

以实际 HTTP 请求验证三个匿名入口的请求头、来源、精确字段、成功分支和错误封装，并逐分支核对 Router 与 OpenAPI。覆盖短信 code 的前导零、JSON 数字和非法长度，四种凭据的互斥、未装配分支无外呼、R1 与 R2 不同操作标识、网站与小程序两种会话形状。注入原响应丢失、同键异输入、错误证明、可靠在途和提交未知，验证原请求不换键、不重复发送或建会话，且网站 cookie 只在适用的成功交接时设置。

实际 HTTP 测试覆盖 C1 的 Origin 缺失、空值、`null`、重复和不匹配，携 Authorization 也不能绕过。另验证 JSON 后缀媒体类型、Serde 对象与序列输入、422 字段错误，以及成功和失败均无请求标识。提取器单元测试覆盖缺失 extension 的安全 500；错误映射测试覆盖额度采样错误不附 `Retry-After`。独立测试进程验证等待者取消及 HTTP 客户端断开后，worker 仍记录一次脱敏技术失败，并保留原请求与操作关联。在线日志的范围和限制见[运行观测](../architecture.md#就绪与观测)。

当前 C1 测试覆盖真实 HTTP 的输入拒绝、供应商拒绝、错误原证明、输入冲突、响应断连、停止与独立进程重启。重启后返回同一 Message，供应商替身只接到一次发送。新 C1 仍受原持久额度限制，返回 429 与 `Retry-After`；原结果回读不重新扣额。HTTP 等待取消不释放原工作或本机锁。现有库级故障测试另覆盖提交响应丢失和延迟提交；不能据此声称所有 HTTP 故障分支均已注入。当前只交付 `message` 成功分支，R1、R2、`progress` 和真实部署尚未验收。
