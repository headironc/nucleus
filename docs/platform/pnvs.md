# PNVS 短信发送协议

交付状态见[能力状态](../README.md#能力状态)。本文拥有阿里云 PNVS `SendSmsVerifyCode` 的请求字节、签名、供应商响应及直接发送边界。[短信认证](../identity/authentication.md)拥有 C1 的发送许可、调用占用、本地验证码效力和未知结局恢复。[供应商凭据](credentials.md)拥有 RAM 凭据接受条件与文件；[部署设置](../settings.md)规定非秘密配置装载。

## 请求与验证码归属

请求省略可选的 `SchemeName`，由供应商使用默认方案；部署设置不提供方案名。该参数的可选性与缺省行为见下方官方接口文档。

使用 `Dypnsapi` 的 `2017-05-25` 版 `SendSmsVerifyCode`，目标为 `POST https://dypnsapi.aliyuncs.com/`。业务参数全部置于 query，请求体为空，以 `Accept: application/json` 请求 JSON 结果。不添加该操作未声明的 `RegionId`。只采用本地生成的六位数字验证码，将其作为模板变量传入；不使用 `##code##` 让供应商生成验证码。本地 Check 负责有效期、错误次数和单次消费。自带验证码不能通过 PNVS 的 `CheckSmsVerifyCode` 校验。接口及参数依据见[阿里云 SendSmsVerifyCode 文档](https://help.aliyun.com/zh/pnvs/developer-reference/api-dypnsapi-2017-05-25-sendsmsverifycode)。

| 参数                                        | 固定输入或来源                                                                                              |
| ------------------------------------------- | ----------------------------------------------------------------------------------------------------------- |
| `CountryCode`、`PhoneNumber`                | 固定 `86`；从规范的 11 位大陆手机号取得号码。                                                               |
| `SignName`、`TemplateCode`                  | 部署提供配套的 PNVS 赠送签名和模板；按认证、换当前号、绑定新号选择三个模板。每个模板含 `code`、`min` 变量。 |
| `TemplateParam`                             | 含本地六位 `code` 和发送时剩余分钟向上取整后的字符串 `min` 的 JSON 文本；作为单个 query 值编码。            |
| `ValidTime`                                 | 发送前按原 Check 的 `expires_at` 计算剩余整秒；不改变本地截止。                                             |
| `OutId`                                     | 原 `identity.messages.id` 的规范小写 UUID 文本；原消息始终复用同一关联值。                                  |
| `Interval`、`AutoRetry`、`ReturnVerifyCode` | 分别固定 `60`、`0`、`false`；本地持久频控仍独立执行，不要求返回明文码。                                     |
| `CodeLength`、`CodeType`、`DuplicatePolicy` | 分别固定 `6`、`1`、`1`；这些参数不代替本地生成、旧码失效或消费判断。                                        |
| `SmsUpExtendCode`                           | 不发送。                                                                                                    |

PNVS 的 `OutId` 是透传外部流水号，没有已核验的发送去重保证。响应中的 `BizId` 是供应商业务标识，`RequestId` 是 API 请求标识，三者不能混用，也不证明用户控制手机号。无需在本地 Message 再存一份 `OutId`。当前不接入回执 HTTP 入口或自动状态查询；缺少回执不能证明短信没有发出。此前带方案名的认证短信已获供应商受理，并由接收者确认到达；省略方案名后的成功发送尚未实测。

## 签名与发送字节

`domain::identity::message::client::Client::new(settings, access_key)` 接收并持有已验证的部署设置和凭据，建立可复用的 HTTP 客户端。nucleus 负责构造客户端，凭据随客户端存活，不为每个请求复制。`Client::request(input, context)` 构造并签名一次请求，绑定该客户端的连接。`Input` 引用规范 `Mobile` 和六位 `Code`，携带原 Message ID、剩余整秒与固定 `Purpose`。内部始终使用同一 11 位手机号；出站添加 `CountryCode=86` 不改变身份。构造过程不读环境、文件或时钟，也不连接供应商。nucleus 从专用 `message.json` 装载凭据；目标 RAM 权限仅授 `dypns:SendSmsVerifyCode`，资源为 `*`。

`message::settings::Settings` 通过构造方法接收已验证值，保存私有的 `signature` 和 `template: Templates`。嵌套模板配置的字段为 `authentication`、`mobile_change`、`mobile_binding`。`signature` 和各用途的模板代码分别使用 `Signature`、`TemplateCode` 值类型。值类型仅通过受控构造按[部署设置](../settings.md#3-必填项)的字符条件校验并保留原文本，设置类型不实现 `Deserialize`。nucleus 单独定义嵌套输入结构，反序列化时拒绝缺字段和未知字段，随后调用业务构造方法校验值。请求按已有 `challenge::Purpose` 选择本次 `TemplateCode`。`TemplateParam` 的 `code` 仍表示六位验证码，和模板码是不同的值。nucleus 仍拥有用途的可信来源和设置装载。供应商超时的默认值与接受条件见下节。环境键解析已由 nucleus 设置加载器交付，尚未验证真实供应商模板配对。

调用方在发送前按原 Check 截止计算剩余整秒，并提供当前 UTC 时间和独立 nonce。当前 `Context` 接收 UUID 值，构造不附加版本或变体限制。C1 调用方按[项目标识](../identifiers.md)生成独立 UUIDv4 nonce，并复查未复用、日期足够新及消息可发送。当前构造不生成 nonce，也不证明这些资格。构造已拒绝剩余不足 60 秒、超出供应商 `int64` 的秒数，以及不能表示为四位正年份的日期。日期输出到整秒，模板 `min` 向上取整；本地截止不变。

query 键按名称排序，键和值按 UTF-8 与 RFC 3986 编码；空格使用 `%20`，不用 `+`。先将 `TemplateParam` 序列化为 JSON 文本，再仅进行一次 query 编码。公共头包括 `host`、`x-acs-action=SendSmsVerifyCode`、`x-acs-version=2017-05-25`、当前 UTC 的 `x-acs-date`、每次调用独立生成的 UUID `x-acs-signature-nonce`，以及空正文的 `x-acs-content-sha256`。签名 nonce 与 AEAD nonce 没有共用格式或用途。

按实际头名的小写字典序规范化 `host`、`x-acs-*` 和存在的 `content-type`，去掉头值两端空格，并保留每行结尾换行。`SignedHeaders` 按同一顺序列名。以方法、路径 `/`、规范 query、规范头、`SignedHeaders`、空正文 SHA-256 组成规范请求；其 SHA-256 参与待签名串。以 AccessKey Secret 做 HMAC-SHA-256，十六进制签名填入 `Authorization`。发送阶段复用已签名的 URI、头和空正文。[阿里云 ACS3 原生 HTTP 规范](https://help.aliyun.com/zh/sdk/product-overview/v3-request-structure-and-signature)给出字段、规范化顺序和固定测试向量。

`Request` 仅提供消费自身的 `execute`，不对外暴露方法、URI、头或正文。URI 先由编码后的 query 装配，并核对解析器保留原字节，再参与签名。底层 Reqwest 请求保持私有，由 `Request::execute` 原样发送。请求同时保留原 Message ID，供内部核对响应关联。不要再次调用 query 编码、替换 URI、增加参与签名的头或更改正文。当前不添加 `content-type`；`accept` 不参与签名。HMAC 使用现有依赖图中的 `ring`，query 编码使用 `percent-encoding`。摘要与签名的十六进制输出使用 `hex`，时间使用 Chrono 的整秒 RFC 3339 接口并输出 UTC `Z`。

构造失败返回本地错误，不返回部分请求。错误只报告字段或失败类别。凭据和请求的 `Debug` 隐藏内容，`Authorization` 标记为敏感头。临时 JSON、query、规范请求和鉴权文本使用可清零缓冲。最终 URI 仍含手机号与验证码。调用方不得记录 URI 或转换后的原始请求。底层 URI 存储没有清零保证。

## 单次 HTTP 调用

`message::settings::Settings` 在构造时校验 `timeout_milliseconds` 的范围。缺省为 10000，由根输入在反序列化时应用；提供时只接受 1000 至 10000 的整数毫秒，保存为私有 `Duration`。无效值和显式空值不使用缺省值。`Client::new` 使用设置中的预算，不重复校验。根输入转换负责严格解析 `NUCLEUS_MESSAGE__TIMEOUT_MILLISECONDS` 的文本，再构造消息设置，规则见[部署设置](../settings.md#4-可调项)。连接上限为 3 秒与总预算的较小者。总预算从执行请求开始，覆盖连接、TLS 握手和响应体读取。底层使用 [Reqwest 的 timeout 与 connect_timeout](https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html#method.timeout)。

`Request::execute(self)` 消费已签名的请求，通过构造时借用的客户端连接执行原请求并核对响应。请求不复制凭据或设置，也不需要调用方再次传入客户端。生产目标固定为上述 HTTPS 主机，并校验证书和主机名。客户端禁用自动重定向、自动重试和代理。生产接口不接受端点、解析器或信任证书覆盖。隔离测试通过模块内部装配注入本地 HTTPS 替身，仍保留原主机名、URI、头和空正文。

```rust
let client = Client::new(settings, access_key)?;
let request = client.request(input, context)?;
let result = request.execute().await;
```

`Request::execute` 返回 `Result<Attempt, request::Error>`。收到响应头且所需的正文读取、JSON 解码均未失败时返回 `request::Attempt`，通过只读方法 `outcome()`、`status()` 和 `elapsed()` 提供结果分类、HTTP 状态与单调时钟耗时。`Attempt` 字段私有，只由请求执行产生。`Outcome::Accepted` 持有私有构造的 `Accepted`，通过 `biz_id()` 和 `request_id()` 读取已核对的供应商标识。分类枚举仍可匹配，调用方不能自行构造或修改受理值。

请求发送失败返回 `Error::Reqwest`。收到响应头后的正文读取失败返回 `Error::Read`，保留 HTTP 状态。两者的传输原因均经 `without_url` 移除请求地址。2xx 与 4xx 正文按块读取，最多 64 KiB；不依赖 Content-Length。超限返回带状态的 `Error::TooLarge`。JSON 解码失败返回 `Error::Decode`，仅保留状态、类别、行与列。解码错误不保留原始原因或供应商原文。这些错误均表示发送结局未知，不证明供应商未发送。调用边界用传输原因的 `is_timeout` 判断超时，并自行测量失败耗时。执行方法不保存 Message 状态，也不提供重发循环。同一 Message 的跨调用唯一性由 C1 的持久发送占用保护。

C1 调用方在发送前再次核对 Check 截止；剩余不足 60 秒时，本地关闭为 `failed`，不调用供应商。`ValidTime` 用剩余整秒，模板 `min` 用该秒数除以 60 后向上取整。客户端不延长验证码截止，也不证明请求仍有发送资格。发送间隔、占用提交与原进程资格见[短信认证](../identity/authentication.md#c1-与-r1-的目标事务契约)。C1 库接口已接入这些保护，HTTP 通过该接口发送或核对原结果。

## 直接响应与故障

响应限长与错误类型见[单次 HTTP 调用](#单次-http-调用)。原可信结果仅重试本地持久化，次数及恢复连接规则由[短信认证](../identity/authentication.md#c1-阶段与外部调用)拥有。

| 直接响应                                                              | 本地结果                                            |
| --------------------------------------------------------------------- | --------------------------------------------------- |
| HTTP 2xx，`Code=OK`、`Success=true` 且关联本次调用的 `Model` 可解码   | `Accepted`，Message 进入 `sent`；只证明 PNVS 受理。 |
| HTTP 2xx 或 400，`Success=false` 或缺失，且 `Code` 属于明确拒绝集合   | `Rejected`，Message 进入 `failed`；不自动重试。     |
| HTTP 5xx、3xx、超时、断连、响应无法可靠解析、未知错误码或自相矛盾字段 | `Unknown`，Message 进入 `unknown`；不推定未发送。   |

明确拒绝集合包含实测码与文档码。实测的 `ILLEGAL_MOBILE`、`biz.FREQUENCY`、`isv.INVALID_PARAMETERS` 分别对应 `IllegalMobile`、`Frequency`、`InvalidParameters`。文档中的 `FUNCTION_NOT_OPENED`、`isv.BUSINESS_LIMIT_CONTROL` 分别对应 `FunctionNotOpened`、`BusinessLimitControl`，表示未开通融合认证功能和号码流控；这两项尚未实测。每个变体仅匹配一个准确错误码，不提供别名。`InternalError` 和公共鉴权错误不纳入明确拒绝集合，保留原码并返回未知结局。供应商频控或配置错误不是用户验证码猜错。成功 `Model` 须有 `OutId`、`BizId` 和 `RequestId`；`OutId` 与原 Message ID 不一致时不能宣布受理。`ReturnVerifyCode=false` 时不要求返回明文 `VerifyCode`。响应原文不进入日志。诊断只保留受控错误类别、HTTP 状态、耗时及无秘密的关联标识。鉴权头、完整 URI/query、手机号、验证码和模板变量均不得写入日志。一次 PNVS 调用结果未知时，不对同一 Message 再次发送；`AutoRetry=0` 也不构成恰好一次送达保证。

`domain::identity::message::response::Response` 是解码 HTTP 2xx 的 `untagged` 枚举，包含 `Success(Success)` 和 `Failure(Failure)` 两种互斥结构。`Success` 强制要求 `Success=true`、`Code=OK` 和完整 `Model`；`Failure` 接受 `Success=false` 或字段缺失，要求非 `OK` 的 `Code`。4xx 直接解码为 `Failure`。这些结构及其 `check` 仅供 `message` 内部使用。成功 `Model.OutId` 反序列化为 `message::Id`，`Model.RequestId` 按供应商 UUID 解析；供应商 UUID 不受项目生成策略限制。反序列化拒绝缺失或格式错误的字段，也拒绝 `Success=true` 配非 `OK` 的 `Code`，以及 `Success=false` 配 `Code=OK`。已知拒绝码使用 `FailureCode`；未知码的原值随 `Unknown` 结果保留。已解码响应中的空业务标识、关联不匹配或未知错误码由 `check` 判为 `Unknown`。失败结构的 `Success` 使用 `Flag<false>`：缺失时取默认值，显式提供时只接受 `false`；`true`、`null` 和非布尔值均拒绝。成功结构不提供默认值，仍要求 `Success=true` 和完整 `Model`。失败结构读取非 `OK` 的 `Code`，不解析 `Model`。[阿里云接口文档](https://help.aliyun.com/zh/pnvs/developer-reference/api-dypnsapi-2017-05-25-sendsmsverifycode)列出 `Model` 并仅展示成功响应，没有规定失败时是否出现；已观察的失败响应没有该字段。[OpenAPI 请求标识说明](https://help.aliyun.com/en/openapi/support/how-to-get-requestid)将 `RequestId` 描述为 UUID 格式。

客户端按 HTTP 状态选择正文结构。2xx 使用 `Response`，以请求内的 Message ID 和实际状态调用 `check`。只有完整成功内容且关联一致才成为 `Accepted`。所有 4xx 均读取正文并解码为 `Failure`。HTTP 2xx 或 400 携带已核验的拒绝码时返回 `Rejected`。其他 4xx 携带这些码时，保留该码并判为 `Unknown::UnexpectedStatus`。未知码保留原值并判为 `Unknown::UnrecognizedCode`。以下情况返回 `Error::Decode`：4xx 携带成功内容、成功与失败字段自相矛盾，或成功 `Model` 缺失或无法解码。2xx 和 4xx 之外的 HTTP 状态归为 `Unknown::HttpStatus`。截断响应和无效 JSON 均不能形成确定结论。

`Outcome` 的调试输出隐藏任意 `BizId` 文本，`Unknown` 的调试输出隐藏未知错误码原值。业务读取这些原值时仍不得写入日志。执行方法与客户端自身不记录请求、响应或错误日志。

C1 保留未知 `Attempt` 的状态、耗时和受控原因。执行失败时，C1 单独测量 `execute` 的单调时钟耗时，不计入排队或结论回写。`Read` 的安全调试输出包含 HTTP 状态与 `is_timeout` 判断，区分正文超时和截断。最终事件中的供应商状态不表示本地 HTTP 响应状态。最终日志及回写失败的组合原因见[短信认证](../identity/authentication.md#首次接纳边界)。

## 验收与限制

官方[返回码总表](https://help.aliyun.com/zh/pnvs/developer-reference/api-return-code)列出 `biz.FREQUENCY` 和 `isv.INVALID_PARAMETERS`，与实测一致。总表的手机号错误为 `isv.MOBILE_NUMBER_ILLEGAL`，但当前发送接口对非数字文本和 10 位数字串均返回 `ILLEGAL_MOBILE`。手机号错误采用实测准确值。另按总表的融合认证章节纳入未开通功能和号码流控码；本地替身覆盖其解析与状态判定，不代表真实接口已返回这些码。尚未纳入的码保留原文并返回未知结局。

2026-10-01 的真实错误请求得到以下结果。低频补测每次间隔超过 65 秒，未自动重试。表中频控来自此前连续错误请求；其他参数错误采用避开频控后的有效样本。

| 构造条件             | HTTP | `Code`                        | `Success` |
| -------------------- | ---- | ----------------------------- | --------- |
| 错误签名密钥         | 400  | `SignatureDoesNotMatch`       | 缺失      |
| 缺少手机号           | 400  | `MissingPhoneNumber`          | 缺失      |
| 签名时间过期         | 400  | `InvalidTimeStamp.Expired`    | 缺失      |
| `ValidTime` 为非数字 | 400  | `InvalidValidTime`            | 缺失      |
| 虚构 AccessKey ID    | 404  | `InvalidAccessKeyId.NotFound` | 缺失      |
| 非法手机号文本       | 200  | `ILLEGAL_MOBILE`              | `false`   |
| 不存在的模板码       | 200  | `isv.INVALID_PARAMETERS`      | `false`   |
| 短信签名与模板不匹配 | 200  | `isv.INVALID_PARAMETERS`      | `false`   |
| 模板变量缺少 `code`  | 200  | `isv.INVALID_PARAMETERS`      | `false`   |
| 模板参数为非法 JSON  | 200  | `InternalError`               | `false`   |
| 连续错误请求触发频控 | 200  | `biz.FREQUENCY`               | `false`   |

已测 HTTP 400/404 正文含 `RequestId`、`Message`、`Recommend`、`HostId`、`Code`，没有 `Success`。已测 HTTP 200 失败正文含 `Message`、`Code` 和 `Success=false`。这些样本不证明所有错误的结构相同。欠费、日额度耗尽、未开通服务和权限拒绝尚未通过真实账户状态验证。

保存的真实错误响应已通过本地 HTTPS 替身回放。2xx 业务拒绝按准确实测码映射到 `Rejected`；`InternalError` 和公共错误原码由 `Unknown::UnrecognizedCode` 保留，均不自动重发。原始响应仅保存在权限为 `0600` 的本地验收文件，不进入普通日志或 Git。普通单元测试只使用脱敏样本，不读取真实凭据或发起供应商调用。

2026-09-30 回读了 [SendSmsVerifyCode 官方 API 元数据](https://api.aliyun.com/meta/v1/products/Dypnsapi/versions/2017-05-25/apis/SendSmsVerifyCode/api.json)与[端点元数据](https://api.aliyun.com/meta/v1/products/Dypnsapi/endpoints.json)。同日复核 ACS3 正文。API 支持 POST 与 GET，全部业务参数位于 query，没有 `RegionId`。本项目固定使用 POST 和 HTTPS。端点元数据列出的服务地址均为 `dypnsapi.aliyuncs.com`。

请求构造的单元测试使用 ACS3 官方固定向量。测试逐项核对 query、规范请求、空正文摘要、待签名串和最终签名。官方后续 HTTP 展示的日期及 nonce 与固定向量不同。测试采用固定参数表和签名流程中的值。独立 PNVS 样例用 Python 的 `json`、`urllib.parse`、`hashlib` 和 `hmac` 计算预期值。样例含中文、空格、加号、百分号、斜杠及 query 分隔符。另核对 JSON 引号、反斜线和换行转义。测试验证转换为 Reqwest 请求后，URI/query、鉴权头和空正文保持一致。测试也覆盖分钟取整、输入边界和脱敏。

本地 HTTPS 替身使用公开的测试证书与测试凭据。测试专用 DNS 把固定主机解析到回环监听端口，客户端仍执行证书和主机名校验。测试核对收到的原始 query、签名头和空正文，并逐次检查只有一个连接和请求。成功、五种明确拒绝、异常 HTTP 状态、关联错配、矛盾字段、截断或无效 JSON、响应丢失和超时均有覆盖。重定向目标同样指向替身，以观察错误的后续调用。停顿分别注入 TLS 握手、响应头和正文读取；测试核对连接上限、总预算和无自动重发。供应商回显秘密样例用于检查结果调试输出脱敏。TLS 替身使用仅供测试的 `tokio-rustls`；依赖图保持 ring-only。

复用测试在同一 TLS 连接上先后发送两个不同 Message，核对各自的用途模板、验证码和 `OutId`，并分别确认响应关联。客户端的调试输出隐藏设置和凭据。

此前一次真实认证短信已验证当前凭据、赠送签名模板和自带八位码的受理，接收者确认到达。当前六位码尚未进行真实供应商验收。两次先后发码的 `OutId`、其他用途模板和完整 C1 业务验收尚未覆盖。C1 隔离测试已覆盖旧工作结束后的孤立占用回读，未知结局只回写 `unknown`。有限真实请求与本地替身均不构成完整业务流程的交付证据。
