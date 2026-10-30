# 内容 HTTP 目标设计

状态：暂缓实施的后续设计参考，不属于当前内部工牌交付范围。恢复本能力前按[简化架构](../architecture.md)重新核对需求和技术方案；以下旧目标不要求当前功能建立封印、版本链、后台恢复或其他前置设施。

本文拥有网站公开内容的公开读取与管理入口、请求与结果形状、拒绝顺序和专用应用码。业务规则由[共同协议](publication.md)、[栏目](columns.md)、[文章](articles.md)、[首页编排](home.md)和[内容图片](images.md)拥有。请求标识、封套、通用应用码与 OpenAPI 由 [HTTP 入口](../http/api.md)拥有，分页由[列表读取](../http/lists.md)拥有，来源与预算由[入口预算](../http/admission.md#公开内容读取)拥有。

## 公开读取

公开读取匿名，只返回当前上线的内容，供 halo 的 Node 服务在服务端渲染页面时调用。不读取或设置 cookie，不要求 Origin；除图片外均要求有效 `Operation-Id`，由 Node 服务为每次调用生成。全部为 GET 与无正文的 HEAD，拒绝正文和未声明的查询参数。

| 入口                                  | 成功结果                                                                                                  |
| ------------------------------------- | --------------------------------------------------------------------------------------------------------- |
| `GET /api/v1/public/home`             | `home`：解析后的区块数组；首次发布前为 `404 home_unpublished`                                             |
| `GET /api/v1/public/columns`          | `columns`：全部启用栏目的 `key`、名称、说明，按顺序一次返回                                               |
| `GET /api/v1/public/columns/{key}`    | `column`：单个启用栏目；不存在或停用为 `404 resource_not_found`                                           |
| `GET /api/v1/public/articles`         | `count` 与 `articles` 摘要数组；可选 `column`（栏目 `key`）筛选，共同分页；筛选的栏目不存在或停用时为 404 |
| `GET /api/v1/public/articles/{id}`    | `article`：当前上线版本的完整公开字段；从未发布、已撤回或未知为 `404 resource_not_found`                  |
| `GET /api/v1/public/images/{id}.webp` | 图片字节；规则见[内容图片](images.md#读取)                                                                |
| `GET /api/v1/public/sitemap`          | `columns` 与 `articles` 地址条目；供生成 `sitemap.xml`，按下文的游标枚举                                  |

文章摘要含 ID、标题、摘要、署名、封面、栏目名称（栏目启用时另附 `key`）、首次上线时间和当前版本上线时间，不含正文。封面和正文中的图片以公开图片的相对路径返回，不签名。首页结果的形状由[公开解析](home.md#公开解析与预览)规定。每次读取在一个最长 3 秒的只读 `REPEATABLE READ` 事务内完成，读取失败返回 `503 dependency_unavailable`，不返回部分结果。

sitemap 枚举不使用共同的偏移分页，因为偏移在多次请求之间没有快照，文章上线或撤回会让条目跨页移动而重复或遗漏。它按首次上线时间升序、再按 ID 升序，每页结果在还有后续时返回服务端编码的不透明游标 `next`（编码末项的首次上线时间与 ID），最后一页不含 `next`，调用方以它作为 `after` 续读，每页至多 5,000 篇文章，只返回文章 ID 与当前版本上线时间；第一页另附全部启用栏目的 `key`。首次上线时间写入后不变，已枚举的文章不会重复；枚举期间撤回的文章不再出现，期间首次上线的文章排在末尾仍会读到，期间恢复或再次发布的较早文章最迟在下次生成时出现。

公开 JSON 的缓存规则尚待确认；确认前沿用普通 JSON 的 `no-store`。站点服务端自行在内存中短时缓存这些结果，见[网站交付](../http/site.md)。公开图片豁免 `Operation-Id`，豁免清单由 [HTTP 入口](../http/api.md#路由与请求边界)登记。

## 管理入口

管理入口供网站管理页使用，沿[当前身份](../identity/current.md)的网站 Session 选择和同源条件，不设置 cookie，响应为 `no-store`。写入要求准确 Origin，使用已认证写入预算；读取使用元数据预算。写入交给共同 Work，HTTP 最多等待 30 秒，超时返回 `503 outcome_unknown` 而不取消已接纳工作；客户端按[原操作恢复](../client/recovery.md)以原标识和原输入重试。

| 入口                                                  | 权限 | 办理或读取                                                   |
| ----------------------------------------------------- | ---- | ------------------------------------------------------------ |
| `GET /api/v1/columns`                                 | 文章 | 全部栏目（含停用）一次返回                                   |
| `POST /api/v1/columns`                                | 文章 | 创建栏目，`201` 返回 `column` 与 `change`                    |
| `PUT /api/v1/columns`                                 | 文章 | 以 expected 完整顺序整体替换顺序，`200` 返回 `change`        |
| `PUT /api/v1/columns/{id}`                            | 文章 | 修改栏目，`200` 返回 `column` 与 `change`                    |
| `DELETE /api/v1/columns/{id}`                         | 文章 | 删除无引用栏目，`200` 返回 `change`                          |
| `GET /api/v1/columns/changes`                         | 文章 | 栏目变更历史分页，可按 `columnId`、`operationId` 筛选        |
| `GET /api/v1/articles`                                | 文章 | 文章管理列表分页，可按 `columnId`、`state` 筛选              |
| `POST /api/v1/articles`                               | 文章 | 创建文章及首份草稿，`201` 返回 `article`                     |
| `GET /api/v1/articles/{id}`                           | 文章 | 草稿、revision、当前上线摘要与图片管理链接                   |
| `DELETE /api/v1/articles/{id}`                        | 文章 | 以 expected 草稿 revision 删除从未发布的文章，`200`          |
| `PUT /api/v1/articles/{id}/draft`                     | 文章 | 保存草稿，`200` 返回 `draft`                                 |
| `POST /api/v1/articles/{id}/publications`             | 文章 | 发布，`201` 返回 `publication` 与 `version` 摘要             |
| `POST /api/v1/articles/{id}/withdrawals`              | 文章 | 撤回，`201` 返回 `publication`                               |
| `POST /api/v1/articles/{id}/restorations`             | 文章 | 恢复到历史版本，`201` 返回 `publication` 与 `version` 摘要   |
| `GET /api/v1/articles/{id}/versions`                  | 文章 | 版本分页（元数据）                                           |
| `GET /api/v1/articles/{id}/versions/{versionId}`      | 文章 | 单个版本全文                                                 |
| `GET /api/v1/articles/{id}/publications`              | 文章 | 上线事件分页                                                 |
| `GET /api/v1/home`                                    | 首页 | 首页草稿、revision、当前上线摘要与预览所需引用               |
| `PUT /api/v1/home/draft`                              | 首页 | 保存首页草稿，`200` 返回 `draft`                             |
| `POST /api/v1/home/publications`                      | 首页 | 发布首页，`201` 返回 `publication` 与 `version` 摘要         |
| `POST /api/v1/home/restorations`                      | 首页 | 恢复首页历史版本，`201` 返回 `publication` 与 `version` 摘要 |
| `GET /api/v1/home/versions`、`…/versions/{versionId}` | 首页 | 首页版本分页与单个版本全文                                   |
| `GET /api/v1/home/publications`                       | 首页 | 首页上线事件分页                                             |
| `GET /api/v1/home/candidates`                         | 首页 | 可编排的文章候选分页，可按 `columnId`、`state` 筛选          |
| `POST /api/v1/images`                                 | 任一 | 上传原始图片字节，`201` 返回 `image`                         |
| `GET /api/v1/images`                                  | 任一 | ready 图片分页，附短期管理链接                               |
| `DELETE /api/v1/images/{id}`                          | 任一 | 删除无引用图片，`200`                                        |
| `GET /api/v1/images/{id}.webp`                        | 签名 | 凭短期签名读取字节，不要求会话或 `Operation-Id`              |

“文章”“首页”“任一”分别指[文章权限、首页权限或二者之一](../access/capabilities.md#网站公开内容权限)。GET 均支持 HEAD。首页编辑器选择栏目时使用公开读取；选择文章时使用首页候选读取，因为首页草稿可以引用尚未上线的文章，而公开读取不含这些文章。候选只给出编排所需的最小投影，首页维护者因此不需要文章管理读取权。

保存草稿和创建文章的请求正文上限为 320 KiB，是普通 JSON 8 KiB 上限的能力例外；它高于[文章正文](articles.md#正文结构)规范 JSON 的 256 KiB 上限，余量容纳其他字段与请求外层，正文本身是否超限由领域校验以 `422 invalid_input` 拒绝。其他 JSON 写入沿用 8 KiB。图片上传按[内容图片](images.md#上传与处理)的字节与媒体类型限制处理，不套 JSON 上限。部署代理须对这些入口完整缓冲正文。

## 请求确认

创建文章正文为 `{ content }`，`content` 是[文章内容形状](articles.md#身份与内容)的完整值，校验同保存草稿；成功返回 `article`，含服务端生成的文章 ID、首份草稿的 `revision` 和状态 `unpublished`。创建的原完成以规范 `content` 计算输入摘要，目标为新文章 ID，匹配重放返回原文章 ID 与 `replayed: true`，不附 revision，客户端重新读取草稿；文章已删除时按[共同协议](publication.md#原操作与恢复)返回 `deleted: true`。保存草稿正文为 `{ expectedRevision, content }`。发布正文为 `{ expectedRevision, expectedVersionId }`，撤回为 `{ expectedVersionId }`，恢复为 `{ versionId, expectedVersionId }`；`expectedVersionId` 为 `null` 表示确认当前未上线。栏目创建为 `{ key, name, description, enabled }`，`index` 由服务端追加到末尾；修改为 `{ expectedChangeId, name, description, enabled }`，完整替换三个可变字段，不含不可改的 `key` 和只由排序改变的 `index`；删除为 `{ expectedChangeId }`；排序为 `{ expected, replacement }`，两份完整栏目 ID 数组。输入摘要按这些规范对象计算。字段一律 `camelCase`，未知或重复字段拒绝。

`publication` 返回事件 ID、动作、序号、所指版本 ID、前一版本 ID、时间、Operation-Id 和固定脱敏办理人。`version` 摘要返回 ID、`number`、生成时间和固定脱敏办理人。路径 ID 在认证前解析，非法值返回 `400 invalid_input`。

## 拒绝顺序与应用码

请求依次检查：`Operation-Id`、可信来源与入口预算、路径与查询参数、当前会话、写入的 Origin、正文、所需权限、原操作匹配，最后是业务条件。会话无效返回 `401 authentication_invalid`，账户暂停或无权返回 `403 permission_denied`，管理入口的未知文章、版本、栏目或图片返回 `404 target_unavailable`。

| `code`                         | HTTP | 含义                                     |
| ------------------------------ | ---- | ---------------------------------------- |
| `draft_changed`                | 409  | expected 草稿 revision 已不是当前值      |
| `draft_unchanged`              | 409  | 草稿与当前上线版本相同，不生成新版本     |
| `publication_changed`          | 409  | expected 当前上线版本已变化              |
| `article_incomplete`           | 409  | 标题或正文为空，不能发布                 |
| `article_image_limit_exceeded` | 409  | 正文图片超过发布上限，不能发布           |
| `home_incomplete`              | 409  | 首页草稿没有区块，不能发布               |
| `article_published`            | 409  | 文章已有版本，不能删除                   |
| `article_referenced`           | 409  | 文章仍被首页草稿或版本引用，不能删除     |
| `article_unavailable`          | 409  | 首页引用的文章当前未上线                 |
| `column_changed`               | 409  | expected 栏目变更或顺序已变化            |
| `column_unchanged`             | 409  | 栏目修改没有任何变化                     |
| `column_in_use`                | 409  | 栏目仍被引用，不能删除                   |
| `column_key_in_use`            | 409  | 栏目 `key` 已被使用，含已删除栏目        |
| `column_unavailable`           | 409  | 目标栏目不存在或已停用                   |
| `name_in_use`                  | 409  | 栏目名称与现存栏目冲突                   |
| `image_unavailable`            | 409  | 引用的图片不可用                         |
| `image_in_use`                 | 409  | 图片仍被引用，不能删除                   |
| `image_too_large`              | 413  | 图片超过上传上限                         |
| `image_format_unsupported`     | 415  | 图片格式不支持或与声明不符               |
| `image_link_invalid`           | 403  | 管理图片链接签名缺失、错误、过期或跨资源 |
| `home_unpublished`             | 404  | 首页编排尚未首次发布，站点使用内置首页   |

同键异输入返回 `409 request_mismatch`。`name_in_use` 与 `image_link_invalid` 沿用已有能力的同一含义。

## 验收

逐路径用真实 HTTP 核对 Router、OpenAPI、状态、响应头、结果形状和错误，覆盖 HEAD 无正文、匿名读取不设 cookie、公开读取拒绝正文与未知参数、公开图片豁免 `Operation-Id`。管理入口覆盖拒绝顺序、两项权限分别生效、暂停账户拒绝、320 KiB 请求上限与 256 KiB 正文上限分别取边界、HTTP 等待超时后以原标识回读原结果。
