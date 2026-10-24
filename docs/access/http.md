# Access 管理 HTTP 目标设计

本文拥有这些管理入口共同的候选身份、入口选择和拒绝顺序。具体业务资格、结果字段与历史由[角色与指派](roles.md)、[授权生命周期](grants.md)和[邀请码](invitations.md)拥有。普通协议见 [HTTP 入口](../http/api.md)。

[本人机构角色读取](roles.md#原请求与读取)沿当前网站身份协议，不属于管理入口，也不要求管理权限。`GET /api/v1/roles`、`GET /api/v1/roles/changes` 及 HEAD 则要求当前 `institution.roles`，不接受单独指派权替代。`GET /api/v1/assignments/roles` 与 `GET /api/v1/assignments` 及 HEAD 要求 `institution.assignments`，分别返回可指派的完整定义和永久机构指派历史。管理 GET 共用网站身份与元数据预算；总数和本页来自同一只读快照。范围、分页、历史身份及拒绝规则见[角色读取](roles.md#原请求与读取)。

`GET /api/v1/workforce/roles` 及 HEAD 提供完整当前领域定义分页。两类读取资格由[角色读取](roles.md#原请求与读取)规定，不要求请求选择 `entry`。它复用上述网站读取、元数据预算及快照边界，不开放领域历史或业务权限。

`GET /api/v1/workforce/roles/changes` 及 HEAD 提供独立角色历史分页。它要求正常、未暂停的当前成员持有 `workforce.roles`，单独机构资格不适用。历史覆盖全部等级及已删除角色，支持 `roleId` 和 `operationId` 筛选，不接受 `entry`。其余读取、错误与固定脱敏身份协议由[角色历史](roles.md#原请求与读取)拥有。

两个 `/api/v1/workforce/grants` POST 要求明确选择 `entry=domain` 或 `entry=institution`。候选沿已认证写入预算和必填 Origin，在一次只读快照返回本次准入所需确认、可选角色总数和分页定义。领域入口按当前 Workforce 权限和等级判断，机构入口按机构权限和等级判断。它不开放独立角色列表或授权历史；完整输入、成功形状和拒绝码见[授权 HTTP 入口](grants.md#http-入口)。

`POST /api/v1/workforce/assignments/candidates` 和 `POST /api/v1/workforce/assignments` 提供现有成员的角色确认及更换。两者沿相同的明确来源、身份、Origin、预算及正文边界。领域入口要求 `workforce.assignments`，机构入口要求 `institution.workforce`。确认包含原 Grant、当前 Assignment 和所选 Role Change；更换保留授权及暂停状态。完整响应、原证明和恢复归[角色与指派](roles.md#原请求与读取)。

同一路径的 GET/HEAD 提供领域指派历史。正常、未暂停成员的领域指派权是唯一读取资格，机构管理权不适用。它沿网站读取、元数据预算和共同分页，不接受 `entry`。全部等级、首条及已结束 Grant 的可见范围、筛选和固定身份由[指派历史](roles.md#原请求与读取)拥有。

`GET/HEAD /api/v1/workforce/grants` 提供永久准入历史。读取要求正常、未暂停成员的当前领域准入权，机构准入资格不能替代。入口沿相同读取边界，不接受正文或 `entry`。可见范围、筛选及固定脱敏身份归 [Grant 历史](grants.md#历史读取)所有。

`POST /api/v1/workforce/roles` 与 `DELETE /api/v1/workforce/roles/{id}` 沿相同来源类型和已认证写入边界创建或删除自定义角色。领域入口要求 `workforce.roles`，机构入口要求 `institution.workforce`。创建权限只接受 Workforce 目录，删除必填准确的 `changeId`。先完成强类型解码，再核对锁后的业务资格和原操作。来源、等级、权限及永久原结果由[角色办理](roles.md#原请求与读取)拥有。

`POST /api/v1/workforce/grants/{id}/terminations` 以空正文办理正常账户本人退出，以完整目标 JSON 办理管理员结束。`POST /api/v1/workforce/terminations/candidates` 提供管理确认。管理动作固定要求当前 `workforce.termination` 和严格领域等级，不接受机构入口替代。两者不接受查询参数；完整边界由[授权生命周期](grants.md#人工结束与注销)拥有。

`POST /api/v1/workforce/suspensions/candidates` 和 `POST /api/v1/workforce/grants/{id}/suspensions` 提供暂停确认及办理。恢复通过 `POST /api/v1/workforce/resumptions/candidates` 确认准确暂停，由 `POST /api/v1/workforce/suspensions/{id}/resumptions` 解除。入口要求当前 `workforce.suspension` 和严格领域等级，共用成员确认的强类型结构。暂停恢复保留当前成员及身份资料，完整状态、原结果和错误边界见[暂停与恢复](grants.md#暂停与恢复)。

账户状态的 GET/HEAD 历史沿当前账户管理范围，并要求准确的 `institution.accounts`。机构指派历史的全机构披露范围不适用于它。路径、当前目标限制及固定身份投影见[账户状态历史](../identity/suspension.md#历史读取)。

管理入口要求当前会话和合法 Operation-Id；传递 cookie 或 Bearer 的方式不授予管理权。JSON 拒绝未知字段，普通正文最多 8 KiB。角色和指派使用所属资源路径，Workforce 准入以正文 `entry=domain` 或 `entry=institution` 明确选用领域或机构管理资格。服务端只按所选入口校验相应权限和等级，不自动择优。生命周期动作按各自准确权限办理，不增加可替代的机构入口。历史列表读取不带 `entry`，满足该读取列出的任一种当前权限即可。机构入口的 `institution.workforce` 不成为领域暂停、恢复、结束或业务办理权。

`POST /api/v1/roles`、`PUT /api/v1/roles/{id}` 与 `DELETE /api/v1/roles/{id}` 共用网站身份、已认证写入预算、必填准确 Origin 和正文上限。仅有 Sec-Fetch-Site 不够。PUT 和 DELETE 路由在身份中间件之前解析所属角色 ID，非法路径值返回共同 JSON `400 invalid_input`。合法 ID 再进入当前身份与 JSON 边界。正文确认值 `changeId` 是必填强类型，非法或缺失返回 422。DELETE 正文只接受该确认字段。

`POST /api/v1/assignments/candidates` 与 `POST /api/v1/assignments` 使用同样的认证、Origin、预算和正文边界。候选正文只接收完整手机号，是不进入 Work 的纯读取。更换正文接收目标确认和所选角色的 ID、Change ID；全部确认标识直接反序列化为所属类型。两个入口及可选列表只认指派权，不把维护权当作替代。

手机号候选只作完整号码的精确定位，并返回办理者有权确认的对象；身份部分固定账户 ID、人员 ID、当时实名版本 ID、脱敏姓名及 `idNumber: { suffix }`。角色更换另确认目标当前指派和角色版本；领域目标还确认原 Grant。授权建立另确认账户和授权状态及选定角色。目标本人、等级不在可办范围或身份不可披露时，候选与未命中一致返回 `404 target_unavailable`。提交按原固定目标、原版本与当前资格复查；定位号码转属他人、目标实名或指派改变返回 `409 target_changed`，选定角色删除或变更返回 `409 role_changed`。候选不授予完整身份证号或无关历史。

同一写入请求的判定顺序如下；不适用的条件跳过。强类型输入先成立，可靠原成功再先于新办理的等级、权限子集和状态判断回读：

1. 校验请求标识、路径 ID、可信来源与本地预算；当前会话和同源检查在 JSON 解码之前。
2. 解码 JSON 与强类型字段。未知、缺失或非法名称、等级、权限代码、重复及跨组权限为 `422 invalid_input`；正文和媒体类型沿共同协议。
3. 锁后复查当前会话与账户，失败为 401；再校验所选入口的基本权限，失败为 403。
4. 查同家族同键原事实，匹配则回读原结果，异输入为 `409 request_mismatch`；新办理再检查路径对象和披露范围，不可用为 404。
5. 比对目标确认及选定角色变更，分别为 `409 target_changed` 与 `409 role_changed`。
6. 比较本人、双方等级、选定权限集合与系统角色边界，越权为 403。
7. 建立授权时目标账户不适用为 `422 account_not_ready`；已有未结束授权或动作状态不适用为 409。角色名称冲突、仍在使用或定义不变分别使用所属角色的稳定应用码。
8. 必要事实或时钟不可靠为 `503 dependency_unavailable`；提交结局未查明为 `503 outcome_unknown`。

此顺序不把数据库异常伪装成查无目标或业务冲突。角色与授权的结果读取仍按当前权限裁剪；列表按 Operation-Id 过滤时可靠无结果返回总数 0 和空数组，不把它解释成原事务失败。精确路径、成功状态和专用结果形状见各所有者文档。

管理写入验收须逐步注入同时存在的无会话、无权、原成功、异输入、无效字段、隐藏目标、旧确认、等级不足、状态冲突及依赖失败，检查只暴露该顺序允许的结果；原会话认证先于字段，强类型字段先于维护权和原操作判断。机构指派与原结果见[角色验收](roles.md#验收)，Workforce 两种准入来源见[准入验收](grants.md#验收)。
