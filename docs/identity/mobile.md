# 手机号更换目标设计

状态：目标设计，尚未实施。本文拥有本人换号、机构协助换号、两次号码验证、原结果和短期事实。发码及供应商监督沿[短信认证与原结果交接](authentication.md)的共同协议；逐会话失效沿[会话撤销与退出](revocation.md)；事务和未知提交核对见 [架构恢复协议](../architecture.md#原操作与恢复)。

## 用例与不变量

手机号更换只在网站办理，各入口只接受当前有效的网站会话。本人须有正常账户，先验证账户当前手机号，再验证拟使用的新手机号。原号无法使用时，机构角色当前含 `institution.accounts` 的管理者可核实本人并建立协助换号，代替原号验证；办理者机构等级须严格高于目标且不能是本人，目标可处于暂停状态。协助不规定固定证明材料清单，也不建立独立审批流程。管理者确认已核实本人必须是受当前权限约束的明确输入，单独的 `verified=true` 不授予任何权力。

成功更换只替换原账户的当前手机号及查找值，保留 Account、Person、实名版本、微信绑定、头像、机构角色、领域授权和业务历史。原手机号释放，新号由该账户占用；暂停账户仍占用其当前号。更换使目标账户当次全部网站与小程序会话失效，但不解除账户或领域暂停，不恢复已结束的授权、角色、申请或会话。原协助者的会话不受目标换号影响。新号与当前号相同返回 409 `state_conflict`；新号已由其他未注销账户占用时，在验证码通过后返回 409 `mobile_taken`，不改变原号。

流程不创建注册凭据或微信绑定，不把匿名登录短信变为换号许可。每次建立的换号绑定建立它的网站会话，固定有效期 10 分钟。同一人员后来建立的新换号使此前尚未完成的换号失效；已经完成的旧结果仍可按原身份解释。换号的 `pending`、`completed`、`expired` 由建立时间、后来建立的换号、原 Completion 和当前时间推导，不另存可漂移的状态。

## 四阶段办理

| 阶段                                                               | 输入与成功结果                                                                                             | 决定性检查                                                                                                                                                                                      |
| ------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 原号发码 `POST /accounts/current/codes`                            | 本人网站会话、当前手机号及人机 token；返回 200 Message。                                                   | 只能发往事务内重读的账户当前号；供应商调用沿原 C1 执行与预算。                                                                                                                                  |
| 建立 `POST /mobile-changes`                                        | 本人网站会话、原号 `messageId` 和验证码、客户端 Operation-Id；返回 201 Transfer ID 与到期时间。            | 原消息与校验属于本人及该当前号，是该人员该号码最新一条；未过期、错误次数小于 5、验证码正确，且消息发送后该人员未建立换号。                                                                      |
| 协助建立 `POST /accounts/{id}/mobile-changes`                      | 管理者网站会话、受控目标确认、`verified=true`、客户端 Operation-Id；返回 201 Transfer ID 与到期时间。      | 当前账户管理权、严格等级、非本人、目标账户与实名版本及查找时手机号仍匹配。                                                                                                                      |
| 新号发码 `POST /mobile-changes/{id}/codes`                         | 建立换号的同一网站会话、新手机号、人机 token、客户端 Operation-Id；返回 200 Message。                      | Transfer 最新、未过期、未完成；目标当前号不同；协助者当前权限和等级仍成立。此时不查询或泄露新号占用。                                                                                           |
| 完成 `PUT /accounts/current/mobile` 或 `PUT /accounts/{id}/mobile` | 同一建立会话、新手机号、新号 `messageId`、验证码、客户端 Operation-Id；返回 200 Completion ID 与生效时刻。 | Transfer 最新、未过期、未完成；目标实名版本仍为原确认；验证码属于同一目标和发码人，发送在 Transfer 建立之后、发往准确新号、为该号最新一条、未过期且错误次数小于 5。协助者当前权限与等级仍成立。 |

本人原号与新号发码分别使用人机用途 `send_mobile_change_code`、`send_mobile_binding_code`，对应持久 `mobile_change`、`mobile_binding` purpose 和独立 PNVS 模板 `MESSAGE__TEMPLATE__MOBILE_CHANGE`、`MESSAGE__TEMPLATE__MOBILE_BINDING`。两个模板代码必须非空且不含空白或控制字符；前者文字写明修改当前手机号，后者写明绑定新手机号，均使用供应商的 `code`、`min` 变量。两者与登录发码共用手机号、来源和部署预算，但验证码校验记录只属于换号；登录 Check 与换号 Confirmation 不能互用。两次供应商调用各有自己的 Operation-Id 和 OutId；恢复原发码只读取原阶段，不自动再次调用供应商。建立及完成不外呼供应商。

有效验证码答错时，在同一事务增加 Confirmation.failures，并保存本动作家族的原操作、规范输入证明和确定拒绝。同一原操作重放只回读，不重复计错；改变验证码必须使用新 Operation-Id。同键异输入返回 request_mismatch。达到 5 次后不再接纳猜测。无效、过期或不适用的验证码不计错，也不创建 Transfer 或 Completion。本人原号验证码最多建立一个 Transfer；成功完成使此前适用的换号验证码失效。

## 事实与约束

`identity.confirmations` 以 Message ID 为主键并真实引用 Message，保存非空 `person_id`、办理者当时实名版本 `operator_id`、验证码 HMAC、`key_id`、`failures` 和固定 `expires_at`。每条换号 Message 恰有一条 Confirmation，登录 Message 恰有 Check，不能两者兼有。发往号码及可信来源沿 Message 取得；最新验证码按同一人员和同一号码的已保存发送事实判断，不设 `latest` 标记。确认码摘要的字段编码和密钥用途沿共同验证码协议，不存明文。

错误猜测保存为独立的短期 `identity.rejections`，按动作家族和原 Operation-Id 唯一。它绑定原 Confirmation、办理者、目标、原会话、用途、输入摘要及密钥版本。原拒绝与计错共同提交；保留到验证码截止之后至少 24 小时，并覆盖原操作恢复及执行。最后结果按[共同保留边界](../architecture.md#原操作与恢复)清理，原验证码失效或消费不因删除而改变。该记录不表示换号成功。

`identity.transfers` 保存独立 UUID ID、目标 `person_id`、原办理者实名版本、目标当时实名版本、建立时网站 `session_id`、`created_at`、唯一非空 `operation_id`，以及原输入摘要和密钥版本。对象实名版本与人员组合真实引用。Transfer 不存 `expires_at` 或状态，有效期从固定建立时间推导。办理者版本所属人员等于目标即本人换号，否则为管理协助；受控权限仍在用例中验证。新号发码与完成都必须使用原 `session_id`，仅持有 Transfer ID 不能继续办理。

`identity.completions.id` 复用成功 Transfer ID，另保存目标人员、原办理者版本、`completed_at`、本次唯一非空 Operation-Id，以及原输入摘要和密钥版本。成功事务核对真实 Transfer、原人员与办理者，保证一个换号至多成功一次；Completion 不保留指向短期 Transfer 的外键。提交检查要求 Account 手机号变化与对应 Completion 共同成立，并核对 Transfer 当时仍是该人员最新、有效的办理。Completion 随账户保留，注销时随该账户结果清理。Confirmation 和 Transfer 按短信技术记录期限清理，默认 24 小时；先清已到期且责任结束的错误尝试，再清该 Transfer 后续 Message 的 Confirmation，再清 Message 与 Challenge，最后清已无短期依赖的 Transfer。原 Completion 保留时，即使 Transfer 已清理，仍可定位已完成状态。

原输入摘要使用 `input_proof` 版本密钥的 HMAC-SHA-256 与[长度前缀编码](../keys.md#51-长度前缀编码)。入口种类固定为 ASCII `self` 或 `assisted`。Transfer 依次编码固定域 `nucleus/identity/mobile-transfer/v1`、入口种类、原 Operation-Id、办理者和目标人员 ID、目标实名版本 ID、网站 Session ID；本人路径再编码原号 Message ID 与八位原码，协助路径再编码 ASCII `verified` 和查找时规范手机号。Completion 依次编码固定域 `nucleus/identity/mobile-completion/v1`、入口种类、原 Operation-Id、Transfer ID、办理者和目标人员 ID、原网站 Session ID、规范新手机号、新号 Message ID 与八位新码。UUID 均编码为 16 字节，手机号使用规范号码，验证码保留 ASCII 前导零；比较使用行内原密钥版本。摘要与原码不进入响应或普通日志；错误尝试按对应建立或完成动作使用同一规范输入编码，不能仅凭 Message ID 重放拒绝。密钥退役扫描覆盖仍保留的错误尝试、Transfer 和 Completion。

运行身份只授予实际读取、插入、Confirmation 失败次数更新、Account `mobile` 整列受控更新、适用技术清理和注销删除所需权限。原人员、实名、绑定、头像和授权不由换号写者修改。密钥维护扫描仍有效的 Confirmation 摘要及其真实密钥引用；清理不能提前释放供应商调用和短信次数预算。

## 原请求、读取与会话

每个写入口使用独立的客户端 Operation-Id。原发码沿 Challenge 与 Message 定位原阶段；原建立沿 Transfer，原完成沿 Completion。以原密钥版本重算规范输入摘要，核对原操作者、目标、会话、手机号、消息及确认内容；同标识异输入返回 409 `request_mismatch`。先定位原成功，再检查当前资格和所属业务期限。原成功重试只回读原事实，不重发短信、不再次建 Transfer 或撤销新会话。

`GET /mobile-changes/{id}` 使用当前有效的网站会话。对象本人或当前有 `institution.accounts` 且机构等级严格高于对象的管理者可以读其最小状态，`completed` 附原生效时刻。不存在与无权统一返回 404。本人换号成功后原会话已撤销，重放完成 PUT 返回 401；本人以新号重新认证后可用 GET 核对原 Completion。协助者会话仍有效，符合当前权限时可重放原 PUT；后来失权不能借原操作取得读取权。仍保留的旧 Transfer 到期或被取代返回 `expired`，已完成优先返回 `completed`；Transfer 已清理且无 Completion 时返回 404。GET 不发码、不改状态，也不把查无事实解释为原事务回滚。

完成事务固定 `transfer` 模式，为目标账户当次所有尚未撤销的会话插入 Revocation，沿本次完成 Operation-Id 与同一服务器时刻成组保存；对应 Handoff 整行删除。手机号替换、Completion、Revocation 集合、Handoff 删除在一笔业务事务共同提交。即使目标此前已有退出或暂停撤销，仍准确清理当次未撤销会话；不为已有撤销的会话插入第二条。登录先提交时新会话在完成的撤销范围内；换号先提交时，旧登录的最终接纳须因号码或账户状态变化拒绝，不能随后建立原号可用会话。用户须用新号重新认证。

## 并发、故障与验收

业务事务按[共同锁顺序](../architecture.md#锁与决定性读取)协调原操作、账户、手机号、Message、Transfer 及目标会话。锁后以新语句重查原结果、当前会话、目标实名版本、管理权限、验证码、时间及号码唯一性。发现新竞争键时回滚并以完整集合有界重试；换号、注册、登录、暂停、注销、实名更正和再次换号按提交先后决定资格。失败仅保留所属协议明确允许的验证码错误计数或发码预算，不留下部分手机号或撤销后果。HTTP 断连不取消已开始的受监督事务。

提交未知按[阶段恢复依据](../architecture.md#原操作与恢复)核对原 Transfer 或 Completion 及所属号码和会话后果。未提交时按原输入和当前条件重新判断。原号不能登录、会话 401、当前手机号变化或原结果合法删除都不能独自证明原办理结局。

验收覆盖原号、新号及协助三条路径的权限、等级、目标更正、暂停及会话绑定；跨登录验证码用途隔离、最新码、过期、五次错误、同一错误原操作只计一次、重复建立、后建 Transfer 取代旧流程、同号及占用号；验证码通过前不泄露占用，手机号唯一冲突整次回滚；网站与小程序全会话失效、已有撤销及新登录交错、协助者会话保持；同标识异手机号或验证码拒绝，短期结果清理后旧验证码不能再次使用、原成功回读、本人旧会话 401、新会话 GET、Transfer 清理后 Completion 仍可读；供应商无响应、数据库晚提交、进程重启与清理交错。真实数据库验证 Message 用途互斥、Transfer 与 Completion 同人同办理者、账户手机号变化与 Completion 配对，以及任何撤销或交接失败都不产生部分成功。
