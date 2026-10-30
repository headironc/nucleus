# Cladiron 批准机器合同

本文拥有普通开发 PR 的机器证据、批准原操作及其恢复合同。提供方选择、额度兜底、审查范围和发现处置由[交付规则](delivery.md#审查关卡与证据复用)拥有。本文不扩大到 `dev`→`main` 提升，也不恢复退役的准入控制器。

## 入口与信任

[批准入口](../../.github/workflows/review-approval.yml)只接受仓库所有者在 `dev` 上的手动运行。首次执行者和重运行者都须为数字身份 `19648989`。全仓库共用一个串行组，不取消在途写入。凭据作业只检出 `github.workflow_sha`，不检出或执行 PR 源码；批准时要求该修订仍是当前 `dev` 基线。

读取使用当前仓库的 `GITHUB_TOKEN`，权限为 Actions、checks、contents 和 pull requests 的只读。写入使用 Cladiron App `3104501` 的 installation `175475891`；申请令牌时仅选择仓库 `1432619931` 和 `pull_requests: write`。App、所有者、installation、返回的仓库与权限逐项核对，结束时撤销令牌。使用现有 `dev-admission` 环境的 `NUCLEUS_DEV_PRIVATE_KEY` 和 `CLADIRON_CLIENT_ID` 元数据；源码不保存凭据。

部署所有者在启用时核实 `dev` 禁止 force push 且无 bypass、环境只允许 `dev`、App 安装范围和真实凭据可用性。基线保护变化需重新接纳。这些是部署信任前提，程序的只读权限不证明它们一直成立。目前保护要求的批准数为零；App 批准不是平台强制 gate，不阻止管理员改保护，也不授权合并。

## 候选与请求

唯一消费者为开放、非草稿、同仓库、目标为 `dev` 的普通 PR。拒绝 fork、stack、queue、提升及合并状态未知的候选。完整头提交 `H` 必须包含当前基线 `B`；当前合成提交 `M` 必须有且仅有有序父提交 `[B, H]`。

`prepare` 模式输出请求正文，并按下节捕获已观察到的启动原件；它不发送评论。正文保留原供应商命令，并追加：

```text
<!-- cladiron-request:v1 head=<full H> base=<full B> target=dev -->
```

仓库和 PR 由 API 目标绑定。只接受数字身份 `19648989` 发布、`created_at == updated_at`、命令与 marker 完全匹配的请求。再通过同一数字 ID 的 GraphQL comment 核对 `lastEditedAt=null`，避免同秒编辑被时间精度隐藏。分别保留请求 ID 和创建时间；旧基线、编辑过的请求、伪用户名及旧轮结果都拒绝。安全请求已联动时使用代码请求，缺失时才由负责人补发安全命令，程序本身不触发审查。

供应商未公开独立的 review-baseline SHA。这里依赖可信请求、供应商以当前 PR 输入执行的合同、请求后的唯一完整 `H` 结果，以及未变化且受保护的 `B`；不把历史 PR 的当前 base 当作当时输入。缺少这些条件即拒绝，不能宣称取得了供应商未暴露的证明。

## 持久摘要的启动观察

Codex 可能持续更新同一条 summary。容器创建时间早于本轮请求时，终态里的 completed 时间不能证明启动顺序。此时在审查仍为 running 时运行只读 `prepare`，保存完整原始响应。没有 running lane 时只输出准备信息，明确不产生捕获原件。已完成状态不能补造启动事实。

捕获沿用所有者手动运行的受信 `dev` 入口。程序先核执行输入冻结与当前 `H/B`，再读取有效请求和可信 App/bot 的 summary。逐 lane 核对完整 `H`、Manual request、`Running since` 时间严格晚于有效请求。安全 lane 使用独立安全请求，缺失时使用联动代码请求。捕获前后重新核对候选及请求的 ID、未编辑正文和创建时间；变化即拒绝。原件保存第一次完整响应，不拼接后续更新。

固定原件 `review-start.json` 绑定仓库、PR、`H/B` 和请求。它保留 summary 数字 ID、node ID、App/bot、完整正文及时间，也绑定 producer run/attempt/revision。只有实际捕获成功才上传 `cladiron-review-start-<run>-<attempt>`。缺失文件或上传失败均不构成成功证据。prepare 不接收 App 私钥，也不申请 installation token。程序不触发或轮询审查。

批准时通过 `code_start_artifact`、`security_start_artifact` 显式选择最多两个 artifact ID；两者允许相同。不扫描历史 artifact 寻找可通过的结果。消费者核对仓库、固定名称、digest、未过期状态和唯一上传。producer 核验覆盖 owner/triggering actor、workflow 路径和 dispatch 事件，也覆盖 `dev`、当前 `B` 和准确 run/attempt。捕获和上传步骤、作业、Actions App/suite 均须成功。失败、取消、旧 attempt、缺少 lane 或原件均拒绝，不回退旧证据。

消费者通过已有有界 ZIP 读取器读取唯一 JSON 文件，再重新解析原始 running 正文。终态须来自同一 summary ID/node ID 和可信 App/bot，绑定同一完整 `H`。完成时间晚于该 lane 的启动且不早于捕获。请求、`H/B`、身份、summary 或时间发生矛盾均拒绝。安全标志为 null 时，独立安全回执还须不早于捕获。最终发现处置、未知正文、线程和 CI 继续单独核验。snapshot digest 与 operation intent 保存 artifact 身份、digest 和完整原件。后续读取保持同一显式选择。

该路径证明“请求后观察到同 PR、完整 head 的最新摘要正在运行，随后同一摘要报告完成”。供应商未暴露独立运行 ID 或 `B`；本合同不把摘要关联称为独立运行证明。首次新建且创建时间晚于请求的 summary 继续使用原路径。新的安全请求仍会使旧安全启动证据失效。聊天中的历史投影只用于设计，不能代替受信 producer 捕获的机器原件。

## 供应商解析

[适配器](../../.github/review-approval/providers.ts)只解析已识别的供应商结构，不做通用自然语言裁判。Cubic 使用 App `1082092`、bot `191113872` 的真实 check 和 review。核对请求后的开始和完成时间、完整 `H`、唯一 review 及 delivery marker。单独的 `SUCCESS` 无效；merge-only、quota、`NEUTRAL`、未知完成形状和缺失报告都不能证明 Cubic 审查通过。Quota 只按下节的已验证输入选择适用提供方，审计状态保留 `quota-unavailable`。

Codex 使用 App `1144995`、bot `199175422` 的完整状态 marker，分别核对代码和安全审查的完整 `H` 及 completed 状态。代码发现须有明确的 `has_unresolved_findings=false`。安全标志为 `null` 时，只接受独立、身份可信且属于对应请求和摘要的明确无安全问题回执；空内容或 `null` 本身无效。不要求不存在公开入口的私有完整报告，也不把 P0 报告阈值当作其余发现均已处理。

保留旧轮报告。已有发现须明确处置：Cubic 的旧发现需要明确的 addressed 结论；Codex 通过明确的未解决发现状态核对处置。新一轮 no-findings 不能抹掉旧的未知发现。未知供应商评论、threadless blocker 或无法关联的报告保持阻塞，待适配器有真实样本和接纳后扩展；不凭合成测试推定新供应商格式真实存在。

PR reviews、issue comments、inline comments、review threads 和嵌套 comments 都完整分页。GraphQL comment 使用 `fullDatabaseId`，拒绝无法精确表示的数字 ID。每个 inline comment 必须归属已完整读取的 thread，每个 thread 须为 `isResolved=true`。`isOutdated=true` 不能替代解决；已处理且 resolved 的线程可以接受。部分 GraphQL 数据、重复页面、读取失败与缺失游标均失败。完整 receipt 与 summary 只接受固定的说明、表格和链接区块，追加或嵌入的未知正文均拒绝。仓库夹具使用去标识数据，原始私有报告链接保留在仓库外。

REST PR reviews 中尚无已接纳的 Codex 正文合同，因此该 bot 在此表面的报告保守阻塞，等待真实样本及可核处置合同。现有 issue summary 的无未解决发现状态不能代替未知 formal-review 正文的解析，也不能消除更晚的 threadless 报告。

## Cubic 额度选择输入

本节规定程序怎样消费[交付规则](delivery.md#审查关卡与证据复用)中的提供方选择。兜底仍验证完整 Codex Code Review、Security Review、发现处置、线程和 CI；不会把 Cubic 的额度拒绝改成审查成功。

`prepare` 的可选 `cubic_quota_check` 指向已有的同仓库 check ID。程序读取真实 Cubic App 的 terminal/neutral 原件，解析明确的 monthly review limit、用量和限额，再生成最小版本化选择 marker。Marker 保存来源仓库、check ID、完整原 head、源摘要、适用个人 pool、周期依据和负责人已核可用恢复表面的声明。它和既有请求 marker 一起绑定当前 PR、`H/B`。

额度原件接受已识别的最小通知，或附有完整固定 flex-capacity 与 usage tips 尾段的通知。尾段的展示计数不证明额度或恢复。固定链接变化、缺段、额外正文或 finding 均拒绝；摘要仍覆盖完整输出，不剥除尾段。

代码请求尚未存在时，准备入口把选择 marker 附在 Code 请求后。已有未编辑的有效请求时，准备入口输出引用该请求 ID 的独立选择声明，保留原运行和覆盖。输出只是待发正文，程序不发布声明，也不触发审查。负责人代理沿既有交付授权作出选择，不增加逐 PR 的 Human 审批。

App 验证数字身份 `19648989`、未编辑状态和源原件。声明时间晚于额度拒绝，也不能早于所引用的 Code 请求。当前输入发生变化时，旧声明无效。源内容、pool 或周期变更、源被删除、选择当时已有真实执行的 Cubic 审查反证，均停止兜底。程序核对 quota 来源 head 和当前 head 的完整 checks，以及同 PR 全部已读 reviews；旧 head 报告按其自身完整 commit ID 校验。额度拒绝之后、选择时刻之前或同时的真实完成可构成恢复反证；merge-only 等未执行审查的 SUCCESS 无效。负责人另行选择新的 Cubic 轮次时，程序按正常路线复用仍有效的 Codex 覆盖。

只有明确的年度日期才用于跨 PR 的周期核验。日期没有时区时，程序保守停在其前一个 UTC 零点；到点只表示旧原件不能选择新工作，不表示额度已恢复。无法解析周期的原件只允许真实的 current-head quota 路径，不猜年份或附加任意 TTL。

选择及有效覆盖仅限当时绑定的 `H/B` 和轮次。后来的周期结束或恢复不强制重做这一轮已有效的 Codex 审查。新输入或新选择继续核实来源、周期和已知反证。该声明不是 App 对当前账单余额的证明；负责人承担跨 pool 的已知恢复判断，不宣称排除了不可见付款，也不扫描整个账户。

`prepare` 同时输出绑定 `H/B` 的 `cladiron-cubic-quota-withdraw:v1` 撤回正文。负责人发布未编辑的撤回 marker 后，旧选择不能继续用于该轮批准。Timeout、429、普通失败、未知 neutral、无来源声明和未解析报告均不能取得 quota 资格。审计输出和 operation intent 保存 `quota-unavailable` 及来源、声明和请求 ID。

## CI receipt

适用 CI 的归属继续从[现有工作流及选检](verification.md#ci-触发与选检)读取。四项基础工作流始终必需；其他 `pull_request` 工作流按受信基线中的 branches、paths 和完整 PR 文件列表选择。`on` 的字符串、数组和映射写法都先识别是否声明 PR 事件。新增或不支持的 PR selector 明确拒绝，不能默认为无需检查。

四项基础 CI 及 Halo publication checks 调用 [receipt producer](../../.github/review-approval/ci.ts)，上传唯一的 `cladiron-ci-<run>-<attempt>`。receipt 保存版本、仓库、PR、目标、`H`、实际检出的 `M`、有序父提交、tree、workflow 路径和 run/attempt。depth-one 检出读取原始 commit 头，不用受 shallow 边界影响的历史遍历代替父链。receipt 可以先于检查生成，只有整次工作流及适用作业成功后才有可消费的成功证据。

消费者核对最新有效 run/attempt、实际作业、Actions App `15368`、suite、稳定作业名及 artifact 归属，随后核对 receipt 的 `[B,H]`、`M` 和 tree。旧失败被新成功替代；新失败、取消、缺失、旧 attempt、相同 `H` 下的新 `M`、后页失败和目标推进均拒绝。Actions API 的 `head_sha=H` 本身不能证明运行过当前 `M`。

批准执行输入及适用 workflow 必须和受信 `B` 一致。程序在 intent 核验中比较完整能力目录的 Git tree 身份，以及 dispatch workflow、`check-bun.ts`、工具版本、根清单与锁文件。根目录、`.github` 和 `tools` 中影响实际执行的 Bun、模块、TypeScript 和自动环境配置也绑定成员、类型、模式及内容身份；能力目录内配置由完整 tree 覆盖。缺失状态同样绑定，新增配置或能力目录中的任何成员变化均拒绝。这里只读取树元数据，不读取秘密配置值。修改协议的候选不能用自己修改后的协议证明通过。首次部署和协议更新须由部署所有者另行决定 bootstrap 接纳与启用顺序；人工批准、源码合并或旧 gate 结果均不能作为该机器合同通过或真实 App 验收的证据。源码交付与真实批准的阶段边界由[交付规则](delivery.md#app-核验与批准目标)拥有。

OCI 工作流保持原状。本合同未把 Docker 链路迁移到宿主机，也未给 OCI 生产者添加 receipt；当 OCI 专用 CI 适用而缺少本消费者可核实的精确候选原件时，程序明确拒绝批准。该缺口由 OCI 协议与验收负责人接续，不能将基础四项绿灯扩大为全部 CI 已通过。

## 批准、未知结果与撤回

写入前持久上传最小 operation intent，绑定 run/attempt、PR、`H/B/M` 和证据摘要。缺失 intent 不写入。连续两次完整读取同一证据快照后，复用同 App、同绑定的既有批准，或只发送一次 `APPROVE`，显式指定 `commit_id=H`。API 的[创建 review 合同](https://docs.github.com/en/rest/pulls/reviews#create-a-review-for-a-pull-request)规定 event 和 commit_id；传输成功还须回读实际 review。

正常响应和超时都核对新 review ID、Cladiron 数字 bot 身份、App 身份（API 返回时）、完整 `H`、body marker 与 `APPROVED` 状态。每次写入的 body 另绑定唯一 run/attempt 操作身份。超时只查找相对写前列表唯一新增的精确 review；零个或多个匹配保持未知，不重复 POST。多个精确自有匹配逐项撤回并读回，不能任意选一个当成功。

写入程序另保留绑定同一 intent 的 outcome，区分未尝试 POST、已确认原操作和仍未知的写入。只读 prepare 和批准使用不同步骤；prepare 失败不构成未知 POST。后续 dispatch 及 rerun 逐个核对每个已启动写入的 attempt，旧 attempt 的安全原件不能掩盖新 attempt 丢失的原件。没有可信终态时，原目标存在唯一、绑定原操作身份的 `APPROVED` 或 `DISMISSED` review 才能解除该项未知。同绑定的更早已撤回 review 不能充当后来写入的回执。intent 过期或写入步骤已开始但 intent 丢失时，停止并报告原 run/attempt，交所有者调查。

写后再次读取完整快照。`H/B/M`、报告、线程或 CI 漂移即撤回自己准确的批准并读回 `DISMISSED`。执行输入信任检查通过后，其他前置证据核验失败才通过专用失败标志尝试撤回该 App 的旧批准。执行输入未知或变化时不申请 App 令牌。只读 prepare 或无关准备失败不撤回有效批准。写入阶段自行处理精确 review 的撤回，不通过通用失败步骤再次发送未知撤回。不撤回人工或其他 App 的 review。撤回失败保留 review ID 并失败，不宣称批准已失效。令牌申请、批准和撤回均不进行传输重试。

手动入口没有持续监听，写后复核与异步撤回不能消除 TOCTOU。作业硬终止可能阻止撤回和令牌主动撤销；原 token 到期、后续 intent 调查和平台人工处理仍是实际恢复边界。上线前须在真实权限下核实批准、撤回、取消与未知回执；本地模拟不建立这些平台结论。

## 本地验证

固定 Bun 和 Node 后，在根依赖已按锁文件准备的检出执行：

```sh
./node_modules/.bin/tsc -p .github/review-approval/tsconfig.json --noEmit
bun test ./.github/review-approval
```

模拟使用隔离 Git 仓库、内存 API 与供应商完整正文的去标识布局。历史样本来自不同 PR，组合请求和一致 head 是合成的；它们证明适配器的接受和拒绝边界，不证明曾有完整真实批准运行。测试覆盖分页、身份、旧请求、供应商拒绝、额度选择、发现处置、CI 漂移、重复事件、未知 POST 以及撤回失败。工作流语法、现有 CI 回归、文档检查与真实平台验收分别保留各自结果。
