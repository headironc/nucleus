# Turnstile 人机校验

交付状态见[能力状态](../README.md#能力状态)。本文拥有发码入口的 Turnstile 上下文、Siteverify 协议、响应接纳及外部调用预算。[短信认证](../identity/authentication.md) 拥有 C1 的持久事实、调用占用、事务和原操作恢复；[匿名身份办理 HTTP](../http/identity.md) 拥有匿名发码输入与结果。[HTTP 来源与入口资源预算](../http/admission.md) 拥有可信来源提取。

## 入口与上下文

每笔新发码先取得服务端人机校验。客户端 widget 成功不构成发送许可。登录、注册和已登录的非发码动作不另要求 Turnstile。三个发码用途各使用固定 `action`，服务端由入口确定期望值，不接受客户端自选用途。

`domain::identity::challenge::Purpose` 的 `AsRef<str>` 已实现下表 `action` 映射。当前 C1 库入口固定选择 authentication；其余用途随所属入口接入。供应商响应中的 `action` 反序列化为枚举，`domain::identity::challenge::response::Response::check` 用固定用途核对。未知值视为上下文不匹配。

| 发码入口                                 | Challenge 用途   | widget `action`            |
| ---------------------------------------- | ---------------- | -------------------------- |
| `POST /api/v1/codes`                     | `authentication` | `send_authentication_code` |
| `POST /api/v1/accounts/current/codes`    | `mobile_change`  | `send_mobile_change_code`  |
| `POST /api/v1/mobile-changes/{id}/codes` | `mobile_binding` | `send_mobile_binding_code` |

目标客户端先生成 UUIDv4 `Operation-Id`，再以 Managed widget、对应 `action` 和 `cData` 取得 token。`cData` 是同一操作标识，不是字面字符串 `C1`。发码请求携带该标识、手机号和 token，不另传 `cData`。供应商响应中的字段拼作 `cdata`，Rust 字段 `operation_id` 以 `operation::Id` 反序列化。手机号仅绑定在本地 Challenge，不放入 widget 上下文。服务端用原操作和部署配置确定期望值，不能信任客户端另报的上下文。

## 网站 widget 与 token 生命周期

本节规定网站接入，不规定原生小程序如何承载供应商组件。网站在所属发码入口嵌入 Managed widget；人机校验只增加发码前置条件，不替代该入口的身份、权限、限流和事务判断。浏览器只取得公开 site key，服务端 Secret 按[供应商凭据](credentials.md)装载。生产后端的主机白名单只包含实际生产前端主机，不包含 `localhost` 或 `127.0.0.1`；本地验收使用对应的本地设置。widget 配置允许某个主机，不代替后端白名单核对。

每个发码入口显式渲染并保存自己的 widget ID。实例绑定该入口固定 `action` 和本次 `Operation-Id` 对应的 `cData`，不跨入口共享 token。更换操作标识时重新配置 widget 上下文，不能仅重置仍携带旧 `cData` 的实例。同页多个实例分别重置或移除；离页时移除所属实例。旧实例或旧操作的迟到回调不得覆盖当前输入和证明。

新办理重新读取公开 site key，读取成功后才渲染对应实例，避免继续使用已轮换的旧配置。Turnstile 脚本加载失败显示组件自身的重试入口，保留当前输入；网站构建资源失效才进入整页重新加载流程。

widget 成功回调只表示取得候选 token。首次发码前，客户端保存原操作标识、规范输入和原 token；保存失败则不提交。首次提交前发生过期、组件错误或交互超时时，清除候选 token 并阻止发码，提示用户重新完成挑战。前端计时与回调用于交互，不替代服务端截止判断。

首次发码后，提交中的请求使用已固定的材料。页面仍停留时，在本次 HTTP 等待结束后重置所属 widget，并清除供下一次新发码使用的候选 token；不自动提交新请求。重置、过期或错误回调均不得删除或覆盖已保存的原 C1 恢复材料。断连、取消、`progress` 和未知结局按[客户端原操作恢复](../client/recovery.md#c1-证明与-widget-恢复)继续核对。新 token 只能用于新的 C1，不能替换原 C1 的证明。

widget 生命周期依据 [Cloudflare 组件配置](https://developers.cloudflare.com/turnstile/get-started/client-side-rendering/widget-configurations/)和 [Turnstile Spin 接入流程](https://developers.cloudflare.com/turnstile/spin/prompt.md)。其页面提交后重置建议在本项目中只作用于 widget 候选状态；原请求恢复仍由 C1 契约裁决。

## Siteverify 请求与结果

本节区分现有客户端与目标流程。当前 `operation::Id` 已按[项目标识](../identifiers.md)解析 UUID 值，Challenge ID 已使用 UUIDv4 生成。[C1 库接口](../identity/authentication.md#人机校验)已接入持久调用占用、Siteverify 外呼、结论回写和原结果恢复。短信发送见[短信发送](../identity/authentication.md#短信发送)，HTTP 与网站的交付状态见[能力状态](../README.md#能力状态)。

应用向 `POST https://challenges.cloudflare.com/turnstile/v0/siteverify` 发送 JSON：服务端 `secret`、客户端 token 对应的 `response`，以及固定验证身份 UUID 对应的 `idempotency_key`。可省略 `remoteip`；若发送，只使用[可信来源](../http/admission.md#可信来源)。服务端密钥与短信认证请求的客户端 `secret` 不同。token 是不透明值，最多 2048 字符。`domain::identity::challenge::Token` 只校验非空和字符数，并保留原文供原调用重试。持有的文本在释放时清零，调试输出隐藏内容。[Cloudflare Siteverify 文档](https://developers.cloudflare.com/turnstile/get-started/server-side-validation/)将 token 描述为生成后有效 300 秒且只能验证一次，并提供固定 `idempotency_key` 支持同次验证重试。这是供应商的协议声明，尚无本项目的单次消费保证验证。真实样本的限制见[设计证据](../engineering/evidence.md#turnstile-真实样本边界)。本地 Challenge 占用承担跨 C1 防重复责任；供应商语义不提供数据库或短信调用的原子性，也不承诺无限期返回原结果。

服务端仅在 `success=true`、`hostname` 属于部署允许集合、`action` 等于本入口固定值，且 `cdata` 的 UUID 值等于原 `Operation-Id` 时接纳通过。`challenge_ts` 是供应商挑战完成时间，必须可解析。已交付的 HTTP 客户端把响应反序列化为 `domain::identity::challenge::response::Response`：`success=true` 时要求 `challenge_ts`、`hostname`、`action` 和 `cdata`，允许但不要求 `error-codes`；`success=false` 时要求 `error-codes`，不要求成功字段。必需字段缺失或为 `null`、字段类型错误、时间格式错误、无效 `hostname` 或无法解析为 UUID 的 `cdata` 均为反序列化错误，由 HTTP 客户端处理；提供的 `error-codes` 必须是数组。未知或空 `action` 视为上下文不匹配。`Response::check` 区分供应商失败、上下文不匹配及上下文匹配；失败错误码留给后续调用边界分类。未知供应商字段不影响已知字段的核对。客户端持有 `settings::Settings`，其私有 `hostnames` 字段为已验证且非空的 `AllowedHostnames`。调用方逐次提供原操作标识和用途。允许主机名与供应商 `hostname` 均按 URL 主机规则解析、规范化后比较；设置中无效的主机名被拒绝。上下文匹配仍不是校验通过。`Context::Matched` 持有字段私有的 `Matched`，只有响应核对模块能构造。调用方可匹配分类，不能自行填入时间制造已匹配结果。

`error-codes` 的数组元素必须是字符串，解码后使用 `response::Code` 枚举。已知变体对应 [Cloudflare Siteverify 错误码](https://developers.cloudflare.com/turnstile/get-started/server-side-validation/#error-codes-reference)。未知码以 `Unknown(String)` 保留原文，不修剪或转换大小写，调试输出隐藏原文。C1 按枚举判断重试和明确拒绝；未知码及混合未知码保持无法确定，不授予重试。

校验结论回写取得 Identity 写入协调与事务锁后，以新语句取得数据库时间 `T`，令供应商完成时间为 `C`。`Context::accept(T)` 只在上下文已匹配且 `C <= T + 2 秒`、`T < min(C, T) + 300 秒` 时产生 `Proof`。供应商拒绝、上下文不匹配、未来时间戳、到期及截止计算溢出分别失败。`Proof` 保留 `C`、`T` 和派生截止；调用方不得以本机时间代替 `T`。C1 已使用可信数据库时间保存 `passed`、`solved_at=C` 和 `finished_at=T`。两时点只保存一次；证明截止由 `min(solved_at, finished_at) + 300 秒` 派生，不另存一列。最终发码许可事务用新的数据库时间再次检查截止；等号视为到期。排队、回读和重启不延长证明。两秒正向容差是本项目的接纳规则。[Cloudflare widget 文档](https://developers.cloudflare.com/turnstile/get-started/client-side-rendering/widget-configurations/)定义 `action` 和 `cData` 的客户端字段。

| 结果                                          | 对外映射与发送许可                                                    |
| --------------------------------------------- | --------------------------------------------------------------------- |
| 缺失或格式错误的 token                        | 按目标 JSON 字段协议返回 HTTP 422 `invalid_input`；不创建验证工作。   |
| 供应商明确拒绝 token，或上下文不匹配          | HTTP 403 `verification_invalid`；不发许可，不替换旧短信码。           |
| 密钥配置错误、网络超时、内部错误或无可靠响应  | HTTP 503 `dependency_unavailable`；不宣称通过。                       |
| 成功响应字段缺失或格式错误，或 `C > T + 2 秒` | HTTP 503 `dependency_unavailable`；记录有界诊断，不自动重试外部校验。 |
| 供应商已通过，但本地证明截止已到              | HTTP 410 `verification_required`；以新证明和新 C1 办理。              |
| 本地提交结局不明                              | HTTP 503 `outcome_unknown`；先按原 Challenge 阶段事实核对。           |

时间戳异常不增加短信猜错次数，也不占持久发送许可；实际外部调用仍计入原 Challenge 的调用次数。确认原工作与事务结局后，异常 Challenge 关闭为 `inconclusive`，后续原请求返回 `verification_required`。只有可靠确认仍在执行时才返回原进度。单笔供应商时间戳异常不阻断其他有效验证码、注册凭据或会话；底层时钟可靠性不足时按[单进程启动与就绪](../architecture.md#启动后台与停止)处理。

## 调用预算与恢复

`domain::identity::challenge::client::Client::new(settings, secret)` 接管已验证的设置和 `credentials::Secret`，复用 Reqwest 连接。nucleus 负责装配；客户端不读取文件或环境。密钥随客户端存活，释放时清零，客户端的调试输出隐藏设置和密钥。每次 `verify(token, challenge_id, purpose, operation_id, remaining)` 只借用原 token，并接收固定 Challenge ID、本入口用途、原 Operation ID 和剩余预算。密钥不为每次调用复制为独立凭据值；序列化后的 HTTP 正文包含密钥，不得记录。

nucleus 启动时调用 `Client::new`，将固定端点解析一次并保存为 `Url`。地址解析或 HTTP 客户端初始化失败时，构造返回错误。每次调用复用已解析的地址，不重新解析字符串。

客户端向固定 HTTPS 地址发起单次 JSON 请求，发送 `secret`、`response` 和 `idempotency_key`，省略 `remoteip`。客户端关闭重定向、自动重试和代理，连接超时为 3 秒；单次请求最多使用剩余时间与 10 秒中的较短者。剩余预算为零时不发起请求。

客户端逐块读取成功及错误响应，正文上限为 64 KiB。无声明长度或分块传输也受同一上限约束，超限返回 `client::Error::TooLarge`。非 2xx 响应保留状态码和限长后的响应文本；读取失败保留状态码与原因。默认诊断隐藏正文。请求和正文读取错误保留 Reqwest 原因并移除 URL；JSON 解码错误通过 `client::Error::Decode` 保留 Serde 原因，默认诊断隐藏供应商内容。

HTTP 状态不替代正文的 `success` 判定。[Cloudflare 官方服务端示例](https://github.com/cloudflare/skills/blob/main/skills/turnstile-spin/SKILL.md#L82-L106) 先排除非 2xx，再检查 `success`；该示例不提供错误码与 HTTP 状态的完整对应表。

2xx 响应在客户端内部解码为仅模块内部可见的 `response::Response` 枚举，再按本次用途、操作标识和客户端持有的主机白名单核对，返回 `response::Context`。调用方取得数据库时间后才执行 `Context::accept(T)`，客户端不生成 `Proof`。响应解码、上下文核对与时间接纳集中在 `response` 模块。基础 ID 和用途由 `challenge` 模块拥有；`Token` 的校验、脱敏和测试位于 `challenge::token`，解析错误为 `token::ParseError`，并由 `challenge` 重导出 `Token`。供应商拒绝的错误码保留原值，但 `response::Error` 的默认诊断隐藏这些内容。各模块的测试均与实现同文件。

一次原验证的总外部调用预算是 10 秒。首次接纳保存 `created_at`，外部截止固定为 `expires_at = created_at + 10 秒`。同一工作按可信数据库样本建立单调截止，重试和重启均不重置原截止。C1 服务保存调用占用、重试阶段和数据库结论；单次客户端只负责传输与响应核对，不读取凭据文件。

仅暂时网络或服务错误可触发至多一次额外调用。原验证最多调用两次，每次先持久占用调用次数，再使用同一 token、`idempotency_key`、验证身份和剩余预算。明确拒绝、过期、时间戳异常或预算耗尽均不再调用原 token。重复 HTTP 请求只合并原受监督工作；进程重启不自动恢复旧外呼。原客户端再次提交时先按需核对原阶段，再决定是否仍可调用。供应商重试许可不授权重复发送短信。原 token 不持久保存；配置、token、服务端密钥、原始响应和包含秘密的 URI 查询串不进入日志。

## 验收与证据边界

C1 原工作保留响应供[本地回写恢复](../identity/authentication.md#c1-阶段与外部调用)再次核对。`Context::accept` 借用响应，每次按传入的数据库时间核验，不延长原证明截止。结论回写、重启核对及故障验证见[人机校验交付](../identity/authentication.md#人机校验)。

C1 可重试的网络错误只有连接失败和超时。供应商错误只接受单独的 internal-error。可重试 HTTP 状态只有 500、502、503、504。上述错误状态的正文读取失败保留状态分类。其他 HTTP 状态、明确拒绝、超限或无效响应不因此重试。次数与截止仍由原持久调用占用约束。

网站接入按凭据装载、供应商响应、上下文与时间接纳、完整 C1 发码四层分别验收。文件解码成功不证明密钥有效；无效 token 探针和公开测试密钥不证明真实 widget 接入成功。真实成功样本须核对部署主机、入口用途和原操作标识，再按数据库时间接纳。只有完整业务验收才能证明校验失败不发短信、不替换旧码，以及响应丢失不重复外呼。

前端验收覆盖三个用途、多实例隔离、首次提交前 token 过期、材料保存失败、提交后断连、组件重置、页面重进和旧回调迟到。原 C1 待核对时，即使 widget 已产生新 token，恢复请求仍携带原材料。确定需要新证明后，验证新操作的 `cData` 已更新；仅重置旧上下文不能通过该验收。

nucleus 从导出的环境变量装载主机白名单和公开 site key。白名单的字符、域名和去重规则见[部署设置](../settings.md#31-主机名集合)，直接构造和环境装载共享校验。site key 通过[公开设置](../http/site.md#入口与响应)供浏览器读取，与服务端 Secret 分开。

本地 HTTP 替身已验证同一客户端先后处理不同 token 和 Challenge ID，并按每次用途与 Operation ID 独立核对响应。错配操作、用途或部署主机白名单均不能得到匹配结论。测试还覆盖密钥与客户端脱敏、调用次数、零预算、2xx 供应商拒绝、异常状态、无效 JSON、截断响应及响应丢失。正文上限覆盖声明长度、分块和关闭连接定界的传输。错误来源链与诊断脱敏也有测试覆盖。原有响应和证明时间边界测试保持覆盖。凭据文件装载由 [nucleus](../README.md#能力状态) 交付。C1 已接入可信数据库时间；本片未调用真实供应商。

用可控供应商替身验证三种用途的 `action` 与 `cdata`、允许主机名、挑战时间容差两侧和截止等号。记录实际调用次数，覆盖并发原请求、调用占用后中断、响应丢失、一次允许的重试与次数耗尽。数据库提交未知时，先证明旧事务结束并核对原结果；不得把空查询当作重新验证许可。再证明任何失败都不建立发送许可或替换旧码。本地替身通过不证明真实部署已满足这些条件。有限真实样本也不构成完整 C1 或供应商单次消费保证的验收。
