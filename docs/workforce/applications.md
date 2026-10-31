# 工牌申请

本文拥有提交快照、申请状态、审批、取消与申请表。实际交付范围见[能力状态](../README.md#能力状态)。生成与发放见[成品](fulfillment.md)，固定资源见[媒体](media.md)。共同事务、审计和失败处理按[架构](../architecture.md)。

## 申请与自动分类

新办 issuance、增办 duplication、换证 replacement 只作为申请的 kind 字段，由本人选择。服务端不自动比较牌面，不保存来源成品、来源发放或原 Grant，不追溯最近一次认可展示。三种类型使用同一流程。

本人须有正常账户、有效参与资格和当前合法工号。申请保存稳定 Person ID；授权使用当前资格，不把历史授权版本绑定到申请。允许同一人、同一类型同时有多份进行中申请，不设数量唯一约束。重复提交可能形成多份申请，由管理者按审批或取消处理。

## 固定提交与确认

一次提交创建一条申请，快照与申请、审计共同提交。驳回后再次申请创建新行，不改写旧申请，不建立 Submission 版本、最高序号或当前提交指针。

snapshot 是整体保存的 JSON 对象，至少包含下列实际内容：

| 内容       | 保存方式                                           |
| ---------- | -------------------------------------------------- |
| 姓名       | 提交时由服务端读取的姓名文字                       |
| 工号       | 当时完整打印工号，不依赖旧工号版本读取             |
| 科室       | 实际科室 ID、当时名称及打印科室文字                |
| 职务与职称 | 当时选择和打印文字；未选职称按现有显示规则打印“无” |
| 照片       | 本次已保存的固定文件 ID                            |
| 牌面       | 本次布局参数、物理尺寸、背景和字体固定资源 ID      |
| 领取地点   | 本次选定的非空地点原文                             |

服务端验证快照形状和必要字段，从可信人员、工号及目录读取值，不接受客户端任意冒填姓名或工号。本人选择实际科室、职务、职称、照片和地点。提交使用当次固定内容；关键打印内容与本人刚确认的预览不同时，展示新内容再确认，不静默替换。这里比较实际内容，不要求实名版本、工号版本、模板摘要或全页 Confirmation。

快照不包含手机号、证件号或会话凭据。照片、背景和字体的原字节须保留，不能同路径覆盖。审批、预览和正式生成均使用快照，不因当前资料变化重新拼装牌面。管理页面读取当前资料并提示差异，差异本身不禁止审批。

目录只表示可选打印内容，不证明人员任职。科室范围授权不能从申请人的自选科室推导，见[角色与范围](../access/roles.md#简化目标)。

## 阶段、审批与结束

| 状态      | 含义及后续动作                               |
| --------- | -------------------------------------------- |
| submitted | 待审批；有权者批准、驳回或取消，本人可取消   |
| rejected  | 已驳回，保留理由和审批记录；再次办理新建申请 |
| approved  | 已批准，尚无正式成品；生成、人工重试或取消   |
| ready     | 已保存成品；有权者发放或取消                 |
| issued    | 已发放，终态                                 |
| cancelled | 未发放申请已取消，终态                       |

审批者须具有当前审批权限及对象范围。使用 UPDATE ... WHERE status = 'submitted' 写入批准或驳回。影响行数为零表示状态已改变，不覆盖原决定。审批人与时间、驳回理由、状态及审计同事务保存。驳回理由非空；批准不写驳回理由。

批准不与首次渲染绑定。先提交 approved，再尝试生成；生成未开始、失败或中断均不撤销批准。生成规则见[成品](fulfillment.md#渲染尝试)。

取消只接受 submitted、approved、ready 三种状态；rejected、issued、cancelled 都是终态。本人在账户正常时可取消自己的申请。有当前审批权及对象范围的管理者也可取消，包含对象已失去参与资格的情况；管理取消保留安全说明。取消采用状态条件更新并同事务写审计，不另建 Cancellation 或 Termination 表。取消不删除已有成品、快照或审批结果。

申请人的账户或参与资格失效后，禁止新提交、批准、生成和发放。管理者仍可按当前权限查看、附理由驳回或取消。资料文字变化、目录停用或当前工号暂缺只提示差异，不阻止审批。恢复资格后按当前状态办理，不沿旧 Grant、持有期或版本链判断来源；已取消、驳回或发放的申请不重开。

## 数据结构

申请使用 workforce.applications，审计使用[共同审计表](../architecture.md#审计)。

| 字段             | 类型        | 规则                                     |
| ---------------- | ----------- | ---------------------------------------- |
| id               | uuid        | 主键，服务端生成                         |
| person_id        | uuid        | 非空，引用 identity.persons              |
| kind             | text        | 三种申请类型之一                         |
| status           | text        | 上述六种状态之一                         |
| submitted_at     | timestamptz | 非空，数据库时间                         |
| snapshot         | jsonb       | 非空对象，保存上述固定内容               |
| reviewer_id      | uuid        | 引用审批人的稳定 Person ID，未审批时为空 |
| reviewed_at      | timestamptz | 与 reviewer_id 同时有值或为空            |
| rejection_reason | text        | rejected 时非空，其余状态为空            |
| render_error     | text        | 最近生成失败的安全错误码，可空           |

submitted 没有审批字段；rejected、approved、ready、issued 有审批字段。cancelled 可来自审批前或批准后，保留已有审批字段。审批后状态不得再次改写审批人和时间，快照不得编辑；改内容用新申请。数据库用非空、外键与行内 CHECK 兜底这些字段组合，不建立封印或历史链。

按实际列表需要建立申请人及状态的查询索引，排序用 submitted_at 和 id。不建立 person_id 与 kind 的唯一索引。现有人员等表结构不在本工牌切片里重建。

JSON 中的科室、职务、职称、照片、背景和字体引用使用数据库生成列提取，并由真实外键保护。职务条目必须属于同一实际科室，照片必须属于申请人，背景与字体必须具有对应资源角色；不另存可独立修改的引用副本。已被申请引用的目录项不能删除，启停或改名不改变旧快照。照片元数据和资源字节不可覆盖，资源角色限定为照片、背景和字体。

## 存储与 HTTP

本人只能读取自己的申请，有权管理者按作用范围读取。审批权不自动开放完整证件号。JSON 使用现有封套，失败不伪装成空列表；当前身份、来源防护与请求体校验继续有效。

本人提交前调用 `POST /api/v1/workforce/applications/preview`，正文为 `{kind, selections}`。`selections` 必填实际科室 `departmentId`、科室职务条目 `rosterId`、职称 `titleId`、已准备照片 `photoId` 和领取地点原文 `pickup`。不选职务或职称时显式传 `null`，服务端不补默认科室。全部 16 种分类规则齐备后才开放预览与提交；缺项是 `503 dependency_unavailable`。

预览正文最多 8 KiB。提交包含完整文字行和布局，正文预算独立为 128 KiB，不能因旧输入预算截断已确认内容。两者超限均返回 `413 invalid_input`。

预览响应为 `{code: "ok", kind, snapshot, photo}`。提交正文为 `{kind, selections, expected}`，`expected` 使用本人确认的完整 `snapshot`。成功返回 `201`、`{code: "ok", application}`。服务端在短事务中重读可信内容并比较全部实际字段；差异为 `409 data_changed`。当前无工号为 `409 number_required`；目录或领取地点不可选为 `422 selection_unavailable`；不可引用的照片为 `404 target_unavailable`。缺字、完整文字放不下分别为 `422 missing_glyph`、`422 text_overflow`。

`snapshot` 包含 `name`、完整 `number`、`printedNumber`、`department: {id, name, printed}`、可空的 `position: {rosterId, name}` 与 `title: {id, name}`、`printedTitle`、`photoId`、`template`、`pickup` 和实际文字行 `text`。布局和行的单位及完整文字定位见[媒体规则](media.md#文字排版)。`photo` 是不可变照片元数据，含 `photoId`、`resourceId`、`width`、`height` 和 `mediaType`，供预览和历史详情定位字节及保持原比例；不把照片尺寸另存为可变牌面事实。

本人列表使用[共同分页](../http/lists.md)，按 `submittedAt DESC, id DESC` 排序。正常账户及活跃 Grant 返回 `{code: "ok", access: "full", count, applications}`；每项含 `id`、`kind`、`status`、`submittedAt`、`snapshot` 和 `photo`。当前没有工号仍可读完整历史。Grant 暂停或结束时返回 `access: "restricted"`，只列未发放可取消申请，每项恰为 `{id, kind, status, submittedAt}`，没有快照和照片。详情采用同一 `access` 区分，单项位于 `application`。他人或不可披露目标统一为 `404 target_unavailable`；失效会话或账户为 `401 authentication_invalid`。

完整申请的 `approved` 状态另含稳定 `reviewerId`、当前脱敏姓名 `reviewerName` 和数据库时间 `reviewedAt`；`rejected` 再含非空 `rejectionReason`。姓名从当前受保护人员资料读取，审核人账户失效或注销后仍可读取；姓名更正不改写原审核事实。数据库不另存审核姓名或实名版本。`submitted` 不含审核字段。批准前取消的 `cancelled` 不含审核字段，批准后取消则完整保留这三个审核字段。正常完整历史保留已取消申请、快照与照片；受限列表的条目、详情和 `count` 仅包含 `submitted`、`approved`，不计入或披露已取消、驳回记录及审核字段。

管理入口要求当前正常账户、活跃参与资格和 `workforce.approval`，按全单位范围读取。当前角色的数字等级不参与工牌对象判断，也不从自选科室推导范围。管理列表使用相同分页与排序，`status` 可选 `submitted`、`approved`、`rejected` 或 `cancelled`，省略为 `submitted`。返回 `{code: "ok", count, applications}`，每项为四字段摘要加固定 `name`、`printedNumber`、`department`；不提供手机号或证件号搜索。管理详情保留已取消申请的固定内容与原审批事实。

管理详情返回 `{code: "ok", application, current}`。`application` 为完整固定申请；`current` 含当前 `name`、可空 `number`、`accountActive`、`participationActive`，以及所选 `department`、可空 `position` 和 `title` 的当前 `{name, enabled}`。页面对比当前值与快照提示差异；这些当前值不改写原牌面。申请人无当前工号时 `number` 为 `null`，仍可批准。详情不返回手机号或完整证件号。

审核正文为 `{decision: "approve"}` 或 `{decision: "reject", reason}`，上限 8 KiB。理由去首尾空白后必须非空，不接受 NUL。成功返回 `200` 和 `{code: "ok", application}`。已经决定为 `409 state_changed`；申请人账户或资格失效时批准为 `409 applicant_unavailable`，有权者仍可驳回。未知提交为 `503 outcome_unknown`，只刷新管理详情，不自动重发。批准仅保存 `approved`，不生成成品。

取消正文上限 8 KiB。本人模式为 `{mode: "self", expectedStatus: "submitted" | "approved"}`，只要求当前正常账户与本人归属，不要求活跃 Grant 或当前工号。管理模式为 `{mode: "manager", expectedStatus: "submitted" | "approved", reason}`，要求当前审批权限，理由沿用上述非空校验且仅写审计。管理者取消自己的申请也按显式模式办理；管理权限丢失后不自动降为本人模式。

取消只更新与 `expectedStatus` 完全相同的状态，并在同一事务写入审计。批准先于按 `submitted` 确认的取消保存时，取消返回 `409 state_changed`，不新增取消审计；本人须刷新后重新确认。成功返回 `200`、`{code: "ok", application: {id, status: "cancelled"}}`，不返回快照、审核字段或管理说明。终态不再提供取消动作。

取消结果未知时只刷新列表或详情，不自动重发。`404 target_unavailable` 不证明取消成功；受限查询中的已取消申请可以不可见，不能把不可见直接当成提交成功。安全审计保留操作者、原状态、新状态及显式模式；管理说明不向本人披露。

| 路由                                                   | 结果                   |
| ------------------------------------------------------ | ---------------------- |
| POST /api/v1/workforce/applications                    | 本人提交快照，创建申请 |
| GET /api/v1/workforce/current/applications             | 本人申请列表           |
| GET /api/v1/workforce/current/applications/{id}        | 本人申请详情           |
| GET /api/v1/workforce/applications                     | 管理列表               |
| GET /api/v1/workforce/applications/{id}                | 管理详情及当前资料差异 |
| POST /api/v1/workforce/applications/{id}/reviews       | 批准或附理由驳回       |
| POST /api/v1/workforce/applications/{id}/cancellations | 按当前身份和权限取消   |

不提供重提 Submission、独立 Review 读取或原 Operation-Id 结果恢复。请求关联 ID 不承担去重。未收到响应时刷新列表或详情，不自动重发。创建可能重复，审批与取消由状态条件保护。未收到提交确认时不宣称一定失败。

上述 JSON 和照片准备入口沿用共同 `Operation-Id` 请求头，仅用于诊断。重复该头不合并申请，也不提供原结果。媒体资源 GET/HEAD 按[媒体读取](media.md#存储与原结果)豁免关联头，仍检查当前会话和对象权限。

## 验收

验证本人提交、同人同类型多份申请、三种类型走同一流程和越权拒绝。更改姓名、科室配置、照片或当前模板后，旧快照和生成输入不变。验证固定资源字节保留、预览内容不静默替换、差异提示及敏感信息遮蔽。

验证重复审批只成功一次，审批和审计共同回滚；批准后可以暂时没有成品。验证驳回后新申请保留旧决定，取消与生成接纳、发放通过状态条件互斥，终态不可重开。连接故障后刷新显示真实状态，不要求独立恢复连接或自动恢复。
