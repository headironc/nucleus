# 凭据文件

本文规定供应商与数据库凭据的文件格式与装载边界。非秘密部署项见[部署设置](../settings.md)，应用自身的密钥材料见[密钥材料](../keys.md)。

## 文件与装载

nucleus 已交付下表文件的本地装载，与部署入口共用 `platform::credentials` 的读取与版本检查。`nucleus::suppliers::Clients::new` 读取人机校验、短信供应商文件；配置小程序设置组时另读取 `mini.json`。`nucleus::database::load` 读取数据库文件。入口均使用服务单元提供的 `CREDENTIALS_DIRECTORY`。目录变量必须存在且为绝对路径；空值和相对路径拒绝。凭据值不从环境变量读取，文件也不从其他路径搜索。

每个解密后的文件须为 `version` 等于整数 `1` 的 JSON 对象。缺字段、未知字段、无效值均使装载失败。字段值不写入错误信息。

| 文件             | 必需字段                  | 用途                                                          |
| ---------------- | ------------------------- | ------------------------------------------------------------- |
| `challenge.json` | `version`、`secret`       | 人机校验服务端密钥                                            |
| `message.json`   | `version`、`id`、`secret` | 阿里云 RAM AccessKey ID 与 AccessKey Secret                   |
| `mini.json`      | `version`、`secret`       | 配置小程序手机号能力时必需的 AppSecret；AppID 来自服务器设置  |
| `database.json`  | `version`、`password`     | 数据库运行身份密码；值规则见[数据库](../database.md#密码凭据) |

`challenge.json` 的 `secret` 和 `message.json` 的 `secret` 都须非空且不含空白或控制字符。`message.json` 的 `id` 当前须为非空 ASCII 字母数字串。文件解析成功只证明本地结构符合要求。它不证明供应商已签发凭据、凭据仍有效，或目标服务已授权。

`mini.json` 的 `secret` 同样拒绝空值、空白和控制字符。该文件仅在小程序设置组存在时读取，缺席组不要求文件。AppSecret 和供应商 access token 不写入设置、数据库、日志或普通响应。文件中的原字符串由可清零缓冲持有；装配只构建固定地址客户端，不调用微信或确认平台资格。

`domain::identity::challenge::credentials::Secret` 已交付人机校验服务端密钥的受控值类型。它拒绝空值、空白和控制字符，持有的文本在释放时清零，调试输出隐藏内容。nucleus 私有输入结构先反序列化原始文本，再显式调用 `Secret::new` 构建。`FromStr` 委托同一个构造器校验。目标类型不实现 `Deserialize`，原始文本使用 `Zeroizing<String>` 持有。

`domain::identity::message::credentials` 已交付 RAM 凭据值类型。`Id::new` 与 `Secret::new` 各自按上述条件校验，并保留原文本。两种类型不可互换；各自持有的文本在释放时清零，调试输出隐藏内容。值校验错误只报告无效字段，不回显值。nucleus 私有输入结构把 `id`、`secret` 反序列化为 `Zeroizing<String>`，再显式调用各自构造器。目标凭据类型不实现 `Deserialize`。nucleus 以 `AccessKey::new(id, secret)` 配对已验证的两种值，不重复字符校验。构造和 PNVS 签名均不读取凭据文件，也不验证真实供应商授权。nucleus 从 `message.json` 装载凭据，并负责文件版本与顶层对象检查。

## 装配与验收边界

各入口拥有环境读取、固定文件名与文件打开。`platform::credentials::read` 只接收调用方提供的 `Read`，不寻找路径或读取环境。它读入完整内容，使用可清零缓冲解析单份 JSON 对象。共享版本检查只接受整数 `1`，字段结构及值构建仍归对应输入与能力。缺失、重复或未知字段，错误字段类型、非法 UTF-8、尾随其他 JSON 和顶层数组均被拒绝。供应商凭据通过后构造对应客户端，两者装配成功后才返回 `Clients`。任一读取、解析、凭据值校验或客户端初始化失败，供应商装配整体返回错误，已构造的客户端随之释放。数据库装配与验证见[数据库](../database.md)。

装配完成后，根进程入口用 `Clients::into_parts` 移出人机校验和短信客户端，按可选设置另构建小程序客户端，再交给 `Application::start_with_mini`。该移交不重新读取或复制凭据。C1 发送流程消费短信客户端，手机号 R1 消费小程序客户端，HTTP 只持有已装配的业务服务。

共享读取错误区分 IO 与 JSON，默认诊断仅显示类别。入口的 `credentials::Error` 补充固定文件名并保留原始原因，再由供应商与数据库装配错误包装。凭据值校验错误由对应的装配错误单独包装，不转换为 JSON 错误。文件错误的默认诊断包含受控类别与固定文件名，不输出目录、文件内容或提交值。IO、JSON 与客户端初始化的原始原因保留在来源链中，供受控检查，不得直接记录来源链。设置加载错误由 nucleus 设置模块负责。客户端持有已验证凭据，装载完成后文件缓冲清零。装载不修改环境或文件，也不调用供应商。

内联测试使用临时目录和虚构凭据，覆盖严格字段、诊断脱敏与两客户端装配。独立测试进程验证导出的目录变量、缺失与空值，以及凭据环境变量和 `.env` 不构成备用来源。尚未读取真实凭据或验证实际服务单元、供应商授权。供应商的非秘密环境设置已接入，范围见[部署设置](../settings.md)。C1 HTTP 服务已装配，运行验证限制见 [HTTP 入口](../http/api.md#nginx-接入与传输目标)。

## AccessKey ID 的字符约束

[阿里云 RAM 的 AccessKey 说明](https://help.aliyun.com/zh/ram/support/faq-about-accesskey-pairs)称 AccessKey ID 为公开的唯一标识符，要求准确填写并区分大小写。该说明没有规定长度或字符集。[阿里云 V3 签名规范](https://help.aliyun.com/zh/sdk/product-overview/v3-request-structure-and-signature)规定 `Authorization` 头使用 `Credential=AccessKeyId,SignedHeaders=...,Signature=...`，也没有声明 ID 只能由 ASCII 字母数字组成。[OSS 的错误说明](https://help.aliyun.com/zh/oss/user-guide/http-403-error-code)列出其 AccessKey ID 可含 `.`、`_`、`=`；这是 OSS 的接受规则，不能直接当作消息服务 V3 接口的规则。以上资料于 2026-09-24 核对。

当前的 ASCII 字母数字检查是本项目为直接拼接 `Credential` 字段所设的本地限制，不能称作阿里云的通用格式规则。它排除逗号、空白与控制字符，避免凭据值改变请求头的字段边界。它也会拒绝含其他字符的凭据，即使该凭据由供应商颁发。部署前须用实际获发的 AccessKey ID 核对这项接受条件。现有资料不足以确定消息服务 V3 接口接受的完整字符集或精确长度。
