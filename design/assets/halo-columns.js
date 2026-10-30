const content = document.getElementById("column-content");
const picker = document.getElementById("scenario-picker");
const dialog = document.getElementById("column-dialog");
const previewToggle = document.querySelector(".preview-toggle");
const scenarios = {
  list: [
    "栏目列表",
    "list",
    "有 institution.articles 的正常网站账户。全部栏目含停用，一次读取，按管理顺序展示，不分页；不以员工服务资格作为前提。",
  ],
  loading: [
    "列表读取中",
    "message",
    "未取得可信列表前不显示此前栏目或把它解释为空。",
  ],
  empty: [
    "成功读取 · 暂无栏目",
    "list",
    "只有可信成功且列表为空才显示空态，可明确创建第一个栏目。",
  ],
  "at-limit": [
    "已有 100 个栏目 · 达到上限",
    "list",
    "可信完整列表已有 100 个栏目，达到现有上限。创建入口不可用，已有栏目仍可修改；不为此虚构 HTTP 应用码。",
  ],
  "read-error": [
    "列表读取失败",
    "message",
    "读取失败隐藏整份列表，不冒充零个栏目，不展示部分结果。",
  ],
  limited: [
    "列表读取受限",
    "message",
    "受限按 Retry-After 等待，不指责用户操作频繁。预览工具可演示等待结束。",
  ],
  "session-invalid": [
    "当前登录无效",
    "message",
    "401 authentication_invalid：遮蔽管理列表、输入与办理内容，停止旧身份的管理读取。",
  ],
  denied: [
    "当前管理读取被拒绝",
    "message",
    "403 permission_denied 可来自账户暂停或无权，不推断具体原因，也不把它一律说成登录失效。",
  ],
  "homepage-only": [
    "仅有首页编排权限",
    "message",
    "可信的当前权限只有 institution.homepage，不包含栏目和文章维护。权限不互相推导。",
  ],
  "permission-changed": [
    "管理权限已变化",
    "message",
    "权限变化立即遮蔽列表、表单和办理摘要。重新核对当前会话、正常账户与准确权限，不沿用旧授权。",
  ],
  "account-changed": [
    "账户选择已变化",
    "message",
    "切换账户立即隐藏旧管理内容与输入；旧响应不能覆盖新选择。新账户重新读取，不能沿用另一账户的办理材料。",
  ],
  "other-account": [
    "切换后重新检查完成",
    "list",
    "示例为另一个正常账户，经重新读取确认具有 institution.articles。没有沿用旧输入、旧办理定位或员工服务身份。",
  ],
  create: [
    "创建栏目",
    "form",
    "完整提交 key、名称、说明、启用状态；创建后 key 永久占用，顺序由服务端追加到末尾。不预选创建的启用状态。",
  ],
  edit: [
    "修改栏目",
    "form",
    "读取可信当前值与 expectedChangeId。仅完整替换名称、说明、启用状态；key 不可修改，index 不由本表单维护。",
  ],
  "recovery-save-failed": [
    "首次发送前材料保存失败",
    "form",
    "客户端原操作恢复要求首次发送前先确认最小材料已保存。此示例尚未发送，保留可编辑输入，不进入提交中或待确认。重试仍须核对并明确确认。",
  ],
  "submit-limited": [
    "首次保存受限 · 明确未受理",
    "form",
    "可信 rate_limited 在进入工作前拒绝本次保存，不是结果待确认。保留输入并按 Retry-After 等待；等待结束后仍须重新核对和明确确认，不自动发送。",
  ],
  "field-errors": [
    "字段校验失败",
    "form",
    "名称 1–20、说明 0–100 个 Unicode 标量，NFC 并去首尾空白，不含控制字符。key 2–32 个小写 ASCII 字母、数字或连字符，以字母开头。",
  ],
  "key-in-use": [
    "公开地址已被永久占用",
    "form",
    "409 column_key_in_use：已删除栏目曾使用的 key 也永久保留；当前列表没有它并不表示可以复用。",
  ],
  "name-in-use": [
    "栏目名称已被使用",
    "form",
    "409 name_in_use：现存栏目名称唯一，包括停用栏目。保留输入，修改冲突字段后重新核对。",
  ],
  unchanged: [
    "没有发生修改",
    "message",
    "409 column_unchanged：名称、说明和启用状态都相同，不伪造成功变更记录。",
  ],
  "column-changed": [
    "栏目已被其他修改推进",
    "message",
    "409 column_changed：expected 失效，没有覆盖当前栏目。先重读当前内容，保留此前输入供对照，再由人重新决定。",
  ],
  "refresh-loading": [
    "冲突后正在重读",
    "message",
    "重新读取可信当前栏目和变更前提，期间不开编辑与提交，不把旧内容标为当前。",
  ],
  "refresh-error": [
    "冲突后重读失败",
    "message",
    "重读失败不证明此前输入已保存。暂不开放再次保存，只允许重新读取。",
  ],
  refreshed: [
    "已重读 · 需要重新确认",
    "form",
    "使用刚读取的当前内容及新 expectedChangeId，旧输入只供对照。不会静默合并、覆盖或自动提交。",
  ],
  submitting: [
    "正在保存",
    "message",
    "明确确认后固定本次办理的标识、目标、expected 和完整输入。HTTP 等待结束不取消已接纳工作。",
  ],
  pending: [
    "保存结果尚未查明",
    "message",
    "断连、超时、outcome_unknown 或不可信写响应均不能证明失败。只沿用原标识与同一输入核对，不建立新办理。",
  ],
  "create-pending": [
    "创建结果尚未查明",
    "message",
    "创建响应未知时，新栏目可能已经建立。保留原标识与完整栏目字段，不能因列表暂时没有新栏目就重复创建。",
  ],
  checking: [
    "正在查询保存结果",
    "message",
    "使用原标识、目标和完整输入核对所属栏目变更，不凭当前列表、名称相同或启停状态推断原完成。",
  ],
  "check-error": [
    "保存结果查询失败",
    "message",
    "普通读取失败或 503 不等于确定未保存。保留原定位，仍待确认，不换标识再提交。",
  ],
  "check-limited": [
    "保存结果查询受限",
    "message",
    "按 Retry-After 等待后继续核对同一办理，不开放直接重传。",
  ],
  "result-unavailable": [
    "暂时还没有查询到结果",
    "message",
    "暂未读到原完成不证明原事务回滚；不把当前列表作为完成证据，不自动换标识重办。",
  ],
  "request-mismatch": [
    "查询信息与原提交不一致",
    "message",
    "409 request_mismatch 不改变保留的原输入。继续核对时仍使用原标识和原输入，不能覆盖原定位。",
  ],
  "recovery-missing": [
    "恢复材料缺失",
    "message",
    "缺少本次办理的最小材料时禁止猜测性重复提交。当前栏目值不能重建原完成证据，只提供当前有权的只读入口。",
  ],
  "current-loading": [
    "只读当前栏目 · 读取中",
    "message",
    "只读当前列表并不确认之前那次保存。读取完成前不展示旧列表，也不开放新办理。",
  ],
  "current-only": [
    "只读当前栏目 · 保存结果尚未查明",
    "list",
    "此前办理仍待确认，当前列表不能证明它完成。没有恢复材料时不开放创建或修改。",
  ],
  complete: [
    "本次保存已完成",
    "result",
    "原标识匹配返回本次原变更。当前栏目另行读取，不能拿当前值替代原完成。",
  ],
  "completed-current-loading": [
    "原完成已知 · 当前栏目读取中",
    "result",
    "本次原变更已经确认，原完成继续展示。当前栏目是另一次鉴权读取，正在读取时隐藏旧当前值，不用原保存内容冒充当前。",
  ],
  "completed-current-error": [
    "原完成已知 · 当前栏目读取失败",
    "result",
    "普通 503 或断连不影响已经确认的原完成，也不证明当前栏目已删除。保留完成与保存内容，只重读当前栏目，不重新保存。",
  ],
  "later-changed": [
    "本次保存已完成 · 当前后来变化",
    "result",
    "即使后来有人修改栏目，匹配重放仍返回原变更。分别显示本次保存内容与后来读取的当前内容。",
  ],
  "later-unavailable": [
    "本次保存已完成 · 当前已无栏目",
    "result",
    "栏目后来被删除也不否认原完成。本稿不提供删除动作；key 仍永久保留，不用新建去猜测原办理失败。",
  ],
  "target-unavailable": [
    "目标栏目不可用",
    "message",
    "404 target_unavailable：不展示旧目标，不猜测它被删除的原因。返回当前有权读取的列表。",
  ],
};
for (const [id, [label]] of Object.entries(scenarios))
  picker.add(new Option(label, id));
const createFormStates = new Set([
  "create",
  "field-errors",
  "key-in-use",
  "name-in-use",
  "recovery-save-failed",
  "submit-limited",
]);
const waitingStates = new Set(["limited", "check-limited", "submit-limited"]);
const initialColumns = [
  {
    id: "culture",
    key: "culture",
    name: "四时拾记",
    description: "循着四季，记录草木与生活的细微变化。",
    enabled: true,
    change: "change-culture-1",
  },
  {
    id: "life",
    key: "life",
    name: "器物与日常",
    description: "从身边器物出发，认识生活中的文化。",
    enabled: true,
    change: "change-life-1",
  },
  {
    id: "archive",
    key: "archive",
    name: "往日印记",
    description: "留存旧时的生活片段。",
    enabled: false,
    change: "change-archive-1",
  },
];
let columns = structuredClone(initialColumns);
const permanentlyUsedKeys = new Set([
  "culture",
  "life",
  "archive",
  "past-seasons",
]);
let current = "list",
  selected = "culture",
  draft,
  initialDraft,
  previousInput,
  operation,
  receipt;
let errors = {},
  timer,
  dialogAction,
  returnFocus,
  waitEnded = false,
  unresolved = false;
const esc = (value) =>
  String(value).replace(
    /[&<>"']/g,
    (c) =>
      ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[
        c
      ],
  );
const button = (action, label, primary = false, disabled = false) =>
  `<button type="button" class="button${primary ? " primary" : ""}" data-action="${action}"${disabled ? " disabled" : ""}>${label}</button>`;
const back = () =>
  '<button type="button" class="text-action" data-action="list">返回栏目列表</button>';
const editableFields = (value) => ({
  key: value.key,
  name: value.name,
  description: value.description,
  enabled: value.enabled,
});
function values(value) {
  return `<dl class="column-values"><dt>栏目名称</dt><dd>${esc(value.name || "未填写")}</dd><dt>公开地址</dt><dd class="column-key">/columns/${esc(value.key || "未填写")}</dd><dt>栏目说明</dt><dd>${esc(value.description || "未填写")}</dd><dt>启用状态</dt><dd>${value.enabled === true ? "启用" : value.enabled === false ? "停用" : "尚未选择"}</dd></dl>`;
}
function guide() {
  return '<aside class="columns-guide" aria-label="栏目维护说明"><div><h2>改动直接生效</h2><p>栏目只有一层。保存后，名称、说明和启用状态直接生效，不需要另行发布。</p><p>公开页面可能在缓存更新后展示变化。</p></div><div><h3>停用不会撤回文章</h3><p>停用后，栏目不再出现在公开导航，栏目页不可访问；其中已经上线的文章仍可从文章地址、全部文章与首页引用访问。</p><h3>地址创建后不再改变</h3><p>公开地址一经使用便永久保留。修改名称不会改地址，删除过的栏目地址也不能重新分配。</p></div></aside>';
}
function listView() {
  const readonly = unresolved || current === "current-only";
  const data = current === "empty" ? [] : columns;
  const atLimit = data.length >= 100;
  const createEntry = readonly
    ? ""
    : atLimit
      ? '<button class="button primary" type="button" disabled aria-describedby="column-limit">已达栏目上限</button>'
      : '<a class="button primary" href="halo-columns.html?state=create" data-local="create">新增栏目</a>';
  return `<div class="columns-grid"><section class="columns-panel" aria-labelledby="list-title"><p class="permission-note">${current === "other-account" ? "已重新检查当前账户 · " : ""}当前可维护栏目与文章</p>${readonly ? '<div class="column-notice"><strong>此前的保存结果尚未查明</strong><p>这里只显示当前栏目，此前的保存结果仍未确定。暂不新增或修改栏目。</p></div>' : ""}${atLimit ? '<div class="column-notice" id="column-limit">已有 100 个栏目，达到 100 个的上限，暂不能新增。已有栏目仍可修改。</div>' : ""}<div class="columns-toolbar"><div><h2 id="list-title">全部栏目</h2><p>包括已停用栏目</p></div>${createEntry}</div>${data.length ? `<div class="column-list">${data.map((column) => `<article class="column-row"><div><div class="column-heading"><h3>${esc(column.name)}</h3><span class="column-tag${column.enabled ? "" : " disabled"}">${column.enabled ? "启用" : "停用"}</span></div><p class="column-key">/columns/${esc(column.key)}</p><p>${esc(column.description || "暂无说明")}</p></div>${readonly ? "" : `<a class="text-action" href="halo-columns.html?state=edit&amp;id=${column.id}" data-edit="${column.id}" aria-label="修改${esc(column.name)}">修改栏目 <span aria-hidden="true">→</span></a>`}</article>`).join("")}</div><p class="column-list-footer">共 ${data.length} 个栏目 · 按管理顺序展示</p>` : '<div class="column-state"><h3>还没有栏目</h3><p>创建一个栏目，开始整理公开导航与文章分组。</p></div>'}</section>${guide()}</div>`;
}
function prepareForm(create = false, source) {
  draft = source
    ? editableFields(source)
    : create
      ? { key: "", name: "", description: "", enabled: null }
      : editableFields(columns.find((c) => c.id === selected) || columns[0]);
  initialDraft = JSON.stringify(draft);
  errors = {};
}
function field(name, label, hint, multiline = false, readonly = false) {
  const counter =
    name === "name" || name === "description"
      ? `<span class="column-counter" id="${name}-count">${Array.from(draft[name]).length} / ${name === "name" ? 20 : 100}</span>`
      : "";
  const attrs = `id="column-${name}" name="${name}" class="column-input" aria-describedby="${name}-hint${errors[name] ? " " + name + "-error" : ""}"${errors[name] ? ' aria-invalid="true"' : ""}${readonly ? " readonly" : ""}`;
  return `<div class="column-field"><label class="column-label" for="column-${name}">${label}${counter}</label>${multiline ? `<textarea ${attrs} rows="3">${esc(draft[name])}</textarea>` : `<input ${attrs} value="${esc(draft[name])}" autocomplete="off"${name === "key" ? ' spellcheck="false" autocapitalize="off"' : ""} />`}<p class="field-hint" id="${name}-hint">${hint}</p>${errors[name] ? `<p class="field-error" id="${name}-error">${esc(errors[name])}</p>` : ""}</div>`;
}
function formView() {
  const create = createFormStates.has(current);
  if (!draft) prepareForm(create);
  const waiting = current === "submit-limited" && !waitEnded;
  const beforeSubmitNotice =
    current === "recovery-save-failed"
      ? '<div class="column-notice error" role="alert"><strong>尚未提交</strong><p>暂时无法保存查询结果所需的信息，因此尚未发送保存请求。当前输入仍保留，可继续编辑后确认保存。</p></div>'
      : current === "submit-limited"
        ? `<div class="column-notice"><strong>本次未保存</strong><p>${waiting ? "本次保存请求未被受理，请等待后再试。输入仍保留，可以继续编辑；等待结束后请重新检查内容，再确认保存。" : "现在可以重新检查内容，再确认保存。不会自动再次提交。"}</p></div>`
        : "";
  return `<div class="columns-grid"><section class="columns-panel column-editor" aria-labelledby="editor-title"><div class="column-back">${back()}</div><h2 id="editor-title" tabindex="-1">${create ? "新增栏目" : "修改栏目"}</h2><p class="editor-introduction">保存后直接生效，请检查名称、说明与启用状态。</p>${beforeSubmitNotice}${current === "refreshed" ? '<div class="column-notice"><strong>已重新读取当前栏目</strong><p>下面是刚读取的内容。请重新填写并检查，不会自动提交此前的修改。</p></div>' : ""}${Object.keys(errors).length ? '<div class="column-notice error" role="alert">尚未保存，请先修改下方标出的内容。</div>' : ""}<form id="column-form" class="column-form" novalidate>${field("name", "栏目名称", "1–20 个字；去除首尾空白，不能与现存栏目重名。")}${field("key", "公开地址名", create ? "2–32 个小写英文字母、数字或连字符，以字母开头。创建后永久保留，不能修改或重新分配。" : "创建后不可修改；即使栏目以后被删除，此地址名也永久保留。", false, !create)}${field("description", "栏目说明（可不填）", "最多 100 个字，用于栏目页简介。", true)}<fieldset class="column-fieldset"${errors.enabled ? ' aria-describedby="enabled-error"' : ""}><legend>启用状态</legend><label class="column-choice"><input type="radio" name="enabled" value="true"${draft.enabled === true ? " checked" : ""} /><span>启用<small>栏目出现在公开导航，可以查看栏目页。</small></span></label><label class="column-choice"><input type="radio" name="enabled" value="false"${draft.enabled === false ? " checked" : ""} /><span>停用<small>从公开导航隐藏，栏目页不可访问，已上线文章仍可访问。</small></span></label>${errors.enabled ? `<p class="field-error" id="enabled-error">${esc(errors.enabled)}</p>` : ""}</fieldset><div class="column-actions"><button class="button primary" type="submit"${waiting ? " disabled" : ""}>${waiting ? "请稍后再试" : "确认保存"}</button><button class="button" type="button" data-action="list">取消</button></div></form>${previousInput ? `<details class="previous-input"><summary>查看此前未保存的输入</summary>${values(previousInput)}</details>` : ""}</section>${guide()}</div>`;
}
const messages = {
  loading: ["正在读取栏目", "检查当前权限后，将显示完整栏目列表。", "", true],
  "read-error": [
    "暂时无法读取栏目",
    "请重新加载后再查看。读取失败不代表没有栏目。",
    "reload-list",
    false,
    "重新加载",
  ],
  limited: [
    "暂时无法读取栏目",
    "暂时无法处理，请稍后再试。",
    "reload-list",
    false,
    "重新加载",
  ],
  "session-invalid": [
    "请重新登录",
    "当前登录已无法继续使用。管理内容已隐藏，请重新登录后再查看。",
    "login",
  ],
  denied: [
    "暂时无法使用栏目管理",
    "暂时无法读取栏目，请稍后再试。也可以先返回个人账户。",
    "account",
  ],
  "homepage-only": [
    "没有栏目维护权限",
    "首页编排与栏目维护分别授权。当前账户只有首页编排权限，不能读取或更改栏目。",
    "account",
  ],
  "permission-changed": [
    "管理权限已变化",
    "请重新检查当前账户及权限，再继续。",
    "recheck",
    false,
    "重新检查权限",
  ],
  "account-changed": [
    "登录状态已变化",
    "请先重新检查当前账户，再继续。",
    "recheck-account",
    false,
    "重新检查当前账户",
  ],
  unchanged: [
    "栏目没有变化",
    "名称、说明和启用状态都没有变化，无需保存。本次没有新增变更记录。",
    "edit-current",
    false,
    "返回编辑",
  ],
  "column-changed": [
    "栏目已被修改",
    "本次没有覆盖当前栏目。请先重新读取，再根据当前内容重新决定如何修改。",
    "refresh",
    false,
    "重新读取栏目",
  ],
  "refresh-loading": [
    "正在重新读取栏目",
    "读取完成前，暂不使用此前内容继续保存。",
    "",
    true,
  ],
  "refresh-error": [
    "暂时无法重新读取栏目",
    "此前输入尚未保存。重新读取当前内容后，再决定如何修改。",
    "refresh",
    false,
    "重新读取栏目",
  ],
  submitting: [
    "正在保存栏目",
    "保存请求已发出，请稍候。暂时不要重复保存；离开页面不会取消已发出的请求。",
    "",
    true,
  ],
  pending: [
    "保存结果尚未查明",
    "暂时还没有查询到结果，这不代表保存失败。请查询保存结果，不要重新新增栏目或重复保存。",
    "check",
    false,
    "查询保存结果",
  ],
  "create-pending": [
    "创建结果尚未查明",
    "这个新栏目可能已经创建。请查询保存结果，不要再次新增；当前列表中没有它，也不能证明创建失败。",
    "check",
    false,
    "查询保存结果",
  ],
  checking: ["正在查询保存结果", "正在查询本次保存的结果，请稍候。", "", true],
  "check-error": [
    "暂时无法查询保存结果",
    "查询失败不能说明保存没有发生。请稍后查询保存结果，不要重复保存。",
    "check",
    false,
    "查询保存结果",
  ],
  "check-limited": [
    "请稍后查询保存结果",
    "暂时无法查询保存结果。请等待后再查询，不要重复保存。",
    "check",
    false,
    "查询保存结果",
  ],
  "result-unavailable": [
    "暂时还没有查询到结果",
    "暂时还没有查询到本次保存的结果，这不代表保存失败。请保留当前页面，稍后查询保存结果，不要重复保存。",
    "check",
    false,
    "查询保存结果",
  ],
  "request-mismatch": [
    "查询信息与本次保存不一致",
    "已保留本次提交的信息。请用这些信息查询保存结果，暂时不要重新保存。",
    "check",
    false,
    "查询保存结果",
  ],
  "recovery-missing": [
    "暂时无法查询保存结果",
    "保存结果仍未确定。你可以先查看当前栏目，暂时不要再次新增或保存。",
    "read-current",
    false,
    "只查看当前栏目",
  ],
  "current-loading": ["正在读取当前栏目", "正在加载栏目，请稍候。", "", true],
  "target-unavailable": [
    "暂时无法查看这个栏目",
    "暂时无法找到这个栏目。请返回栏目列表查看可维护的内容。",
    "list",
    false,
    "返回栏目列表",
  ],
};
function messageView() {
  const [title, description, action, busy, label] =
    messages[current] || messages["read-error"];
  const limited = ["limited", "check-limited"].includes(current) && !waitEnded;
  const actionHtml =
    action === "login"
      ? '<a class="button primary" href="halo-authentication.html">重新登录</a>'
      : action === "account"
        ? '<a class="button primary" href="halo-account.html">返回个人账户</a>'
        : action
          ? button(action, limited ? "请稍后再试" : label, true, limited)
          : "";
  const privateSummary =
    operation &&
    [
      "submitting",
      "pending",
      "create-pending",
      "checking",
      "check-error",
      "check-limited",
      "result-unavailable",
      "request-mismatch",
    ].includes(current)
      ? `<div class="comparison"><h3>本次${operation.kind === "create" ? "创建" : "修改"}的内容</h3>${values(operation.input)}<p>只查询本次提交的保存结果，不能凭当前栏目判断本次保存是否完成。</p></div>`
      : previousInput && ["column-changed", "refresh-error"].includes(current)
        ? `<div class="comparison"><h3>此前未保存的输入</h3>${values(previousInput)}</div>`
        : "";
  return `<section class="columns-panel"><div class="column-state"${busy ? ' aria-busy="true"' : ' role="status"'}><span class="state-mark${busy ? " busy" : ""}" aria-hidden="true">${busy ? "" : "!"}</span><h2 id="state-title" tabindex="-1">${title}</h2><p>${description}</p>${limited ? "<p>请等待后再继续。</p>" : ""}${privateSummary}<div class="column-actions">${actionHtml}${busy ? "" : action === "login" || action === "account" || ["permission-changed", "account-changed"].includes(current) ? '<a class="text-action" href="halo-home.html">先浏览公开首页</a>' : action === "read-current" ? '<a class="text-action" href="halo-account.html">返回个人账户</a>' : ["list", "reload-list"].includes(action) ? "" : back()}</div></div></section>`;
}
function resultView() {
  const result = receipt || operation || fixtureOperation();
  const now = columns.find((c) => c.id === result.target);
  const loading = current === "completed-current-loading";
  const failed = current === "completed-current-error";
  const currentValue = loading
    ? '<div aria-busy="true"><p>正在读取当前栏目，请稍候。本次保存已完成，当前内容仍需另行读取。</p></div>'
    : failed
      ? `<p>暂时无法读取当前栏目。本次保存已完成；读取失败不代表栏目已经删除，也不需要重新保存。</p>${button("reload-current", "重新读取当前栏目")}`
      : now
        ? values(now)
        : "<p>当前列表中已无此栏目。这不影响上面已经完成的保存，也不表示该公开地址名可以重新使用。</p>";
  return `<div class="columns-grid"><section class="columns-panel"><div class="column-state" role="status"><span class="state-mark complete" aria-hidden="true">✓</span><h2 id="result-title" tabindex="-1">本次${result.kind === "create" ? "创建" : "修改"}已完成</h2><p>本次保存已生效，保存记录已保留。</p><div class="comparison"><h3>本次保存的内容</h3>${values(result.input)}<p><time datetime="2026-10-20T15:20:00+08:00">2026 年 10 月 20 日 15:20</time> · 本次保存已完成</p></div><div class="comparison"><h3>另行读取的当前栏目</h3>${currentValue}${current === "later-changed" ? "<p>当前栏目后来又有变化，不能用现在的值代替本次保存的记录。</p>" : ""}</div><div class="column-actions">${button("list", "返回栏目列表", true)}${now && !loading && !failed ? button("edit-current", "查看并修改当前栏目") : ""}</div></div></section>${guide()}</div>`;
}
function show(id, focus = true) {
  clearTimeout(timer);
  current = scenarios[id] ? id : "list";
  if (
    [
      "session-invalid",
      "denied",
      "homepage-only",
      "permission-changed",
      "account-changed",
    ].includes(current)
  ) {
    returnFocus = null;
    dialogAction = null;
    if (dialog.open) dialog.close();
    document.getElementById("dialog-summary").replaceChildren();
    document.getElementById("dialog-title").textContent = "";
    document.getElementById("dialog-description").textContent = "";
    content.replaceChildren();
    draft = null;
    initialDraft = null;
    previousInput = null;
    operation = null;
    receipt = null;
    unresolved = false;
    waitEnded = false;
  }
  picker.value = current;
  const kind = scenarios[current][1];
  content.innerHTML =
    kind === "list"
      ? listView()
      : kind === "form"
        ? formView()
        : kind === "result"
          ? resultView()
          : messageView();
  document.getElementById("state-basis").textContent = scenarios[current][2];
  document.getElementById("fill-example").hidden = kind !== "form";
  document.getElementById("end-wait").hidden =
    !waitingStates.has(current) || waitEnded;
  const params = new URLSearchParams({ state: current });
  if (kind === "form" && !createFormStates.has(current))
    params.set("id", selected);
  history.replaceState(null, "", "halo-columns.html?" + params);
  if (focus)
    (
      content.querySelector('[aria-invalid="true"]') ||
      content.querySelector("#state-title, #result-title, #editor-title") ||
      document.getElementById("page-title")
    ).focus();
}
function fixtureOperation() {
  return {
    kind: "edit",
    target: "culture",
    expected: "change-culture-1",
    id: "7741d575-f7ba-4493-a82b-d079a6f34397",
    input: Object.freeze({
      key: "culture",
      name: "四时风物",
      description: "循着时令，读懂身边的草木与风物。",
      enabled: false,
    }),
  };
}
function selectScenario(id, focus = true) {
  if (dialog.open) dialog.close();
  columns = structuredClone(initialColumns);
  permanentlyUsedKeys.clear();
  for (const key of [
    ...initialColumns.map((column) => column.key),
    "past-seasons",
  ])
    permanentlyUsedKeys.add(key);
  if (!columns.some((column) => column.id === selected)) selected = "culture";
  errors = {};
  draft = null;
  previousInput = null;
  operation = null;
  receipt = null;
  waitEnded = false;
  unresolved = false;
  if (id === "empty") columns = [];
  if (id === "at-limit")
    columns.push(
      ...Array.from({ length: 97 }, (_, index) => {
        const suffix = String(index + 4).padStart(3, "0");
        return {
          id: "sample-" + suffix,
          key: "sample-" + suffix,
          name: "草木札记" + suffix,
          description: "用于检查栏目数量上限的虚构示例。",
          enabled: true,
          change: "change-sample-" + suffix,
        };
      }),
    );
  if (createFormStates.has(id)) prepareForm(true);
  else if (["edit", "refreshed"].includes(id)) prepareForm(false);
  if (id === "field-errors") {
    draft = { key: "Culture Now", name: "", description: "", enabled: null };
    errors = {
      key: "请使用以小写字母开头的 2–32 位小写字母、数字或连字符。",
      name: "请填写栏目名称。",
      enabled: "请选择启用状态。",
    };
  }
  if (["key-in-use", "name-in-use"].includes(id)) {
    draft = {
      key: id === "key-in-use" ? "past-seasons" : "herbal-notes",
      name: id === "name-in-use" ? "器物与日常" : "草木小记",
      description: "记录身边的草木与生活。",
      enabled: true,
    };
    errors[id === "key-in-use" ? "key" : "name"] =
      id === "key-in-use"
        ? "这个地址名已被使用，已删除栏目使用过的地址名也不能复用。请选择另一个。"
        : "这个名称已被现存栏目使用，请换一个名称。";
  }
  if (["recovery-save-failed", "submit-limited"].includes(id))
    draft = {
      key: "botanical-notes",
      name: "草木小记",
      description: "记录身边的草木与生活。",
      enabled: true,
    };
  if (
    [
      "column-changed",
      "refresh-error",
      "refresh-loading",
      "refreshed",
    ].includes(id)
  )
    previousInput = {
      key: columns.find((column) => column.id === selected)?.key || "culture",
      name: "四时札记",
      description: "此前准备保存的说明。",
      enabled: false,
    };
  if (
    [
      "submitting",
      "pending",
      "checking",
      "check-error",
      "check-limited",
      "result-unavailable",
      "request-mismatch",
      "complete",
      "completed-current-loading",
      "completed-current-error",
      "later-changed",
      "later-unavailable",
    ].includes(id)
  )
    operation = fixtureOperation();
  if (id === "create-pending")
    operation = {
      kind: "create",
      target: "created-example",
      expected: null,
      id: "d2a315e4-1898-4d6c-82ce-39bb8b8ed7df",
      input: Object.freeze({
        key: "botanical-notes",
        name: "草木小记",
        description: "记录身边的草木与生活。",
        enabled: true,
      }),
    };
  if (
    [
      "submitting",
      "pending",
      "create-pending",
      "checking",
      "check-error",
      "check-limited",
      "result-unavailable",
      "request-mismatch",
      "recovery-missing",
      "current-loading",
      "current-only",
    ].includes(id)
  )
    unresolved = true;
  if (
    [
      "complete",
      "completed-current-loading",
      "completed-current-error",
      "later-changed",
      "later-unavailable",
    ].includes(id)
  ) {
    receipt = structuredClone(operation);
    Object.assign(columns[0], operation.input);
    if (id === "later-changed")
      Object.assign(columns[0], {
        name: "四季留痕",
        description: "这是后来重新读取的当前说明。",
        enabled: true,
        change: "change-culture-3",
      });
    if (id === "later-unavailable") columns.shift();
  }
  if (id === "refreshed") {
    const target =
      columns.find((column) => column.id === selected) || columns[0];
    Object.assign(target, {
      name: target.id === "life" ? "器物札记" : "四时风物",
      change: "change-refreshed-2",
    });
    prepareForm(false);
  }
  show(id, focus);
}
function readThen(target, interim = "loading") {
  show(interim);
  timer = setTimeout(() => show(target), 650);
}
function normalizeForm() {
  return {
    key: draft.key,
    name: draft.name.normalize("NFC").trim(),
    description: draft.description.normalize("NFC").trim(),
    enabled: draft.enabled,
  };
}
function validate(value, create) {
  const result = {},
    controls = /\p{Cc}/u;
  if (create && !/^[a-z][a-z0-9-]{1,31}$/.test(value.key))
    result.key = "请使用以小写字母开头的 2–32 位小写字母、数字或连字符。";
  if (
    !value.name ||
    Array.from(value.name).length > 20 ||
    controls.test(value.name)
  )
    result.name = "请填写 1–20 个字的栏目名称，不含控制字符。";
  if (
    Array.from(value.description).length > 100 ||
    controls.test(value.description)
  )
    result.description = "栏目说明最多 100 个字，不含控制字符。";
  if (typeof value.enabled !== "boolean") result.enabled = "请选择启用状态。";
  if (create && permanentlyUsedKeys.has(value.key))
    result.key = "这个地址名已被永久占用，即使原栏目已经删除也不能复用。";
  if (
    columns.some((c) => c.name === value.name && (create || c.id !== selected))
  )
    result.name = "这个名称已被现存栏目使用，请换一个名称。";
  return result;
}
function openDialog(
  title,
  description,
  confirm,
  action,
  summary = "",
  backLabel = "返回修改",
) {
  returnFocus = document.activeElement;
  dialogAction = action;
  document.getElementById("dialog-title").textContent = title;
  document.getElementById("dialog-description").textContent = description;
  document.getElementById("dialog-summary").innerHTML = summary;
  document.getElementById("dialog-confirm").textContent = confirm;
  document.getElementById("dialog-back").textContent = backLabel;
  dialog.showModal();
  document.getElementById("dialog-back").focus();
}
function finishSubmit() {
  const op = operation;
  if (op.kind === "create") {
    selected = op.target;
    columns.push({ id: selected, ...op.input, change: "change-created-1" });
    permanentlyUsedKeys.add(op.input.key);
  } else
    Object.assign(
      columns.find((c) => c.id === op.target),
      op.input,
      { change: "change-culture-2" },
    );
  receipt = structuredClone(op);
  unresolved = false;
  draft = null;
  show("completed-current-loading");
  timer = setTimeout(() => show("complete"), 650);
}
function submitForm(event) {
  event.preventDefault();
  if (unresolved || (current === "submit-limited" && !waitEnded)) return;
  const create = createFormStates.has(current),
    value = normalizeForm();
  if (create && columns.length >= 100) {
    show("at-limit");
    return;
  }
  errors = validate(value, create);
  if (Object.keys(errors).length) {
    draft = value;
    show(current);
    if (errors.enabled && !errors.name && !errors.key && !errors.description)
      content.querySelector('[name="enabled"]').focus();
    return;
  }
  const target = columns.find((c) => c.id === selected);
  if (
    !create &&
    JSON.stringify(editableFields(target)) === JSON.stringify(value)
  ) {
    show("unchanged");
    return;
  }
  draft = value;
  show(current, false);
  content.querySelector('[type="submit"]').focus();
  openDialog(
    create ? "确认创建这个栏目？" : "确认保存栏目修改？",
    `${value.enabled ? "栏目将启用，出现在公开导航中。" : "栏目将停用，不出现在公开导航中，栏目页不可访问；已经上线的文章仍可访问。"}保存后直接生效，不需要另行发布。`,
    create ? "确认创建" : "确认保存",
    () => {
      operation = {
        kind: create ? "create" : "edit",
        target: create ? "new-column-" + (columns.length + 1) : selected,
        expected: create ? null : target.change,
        id: "7741d575-f7ba-4493-a82b-d079a6f34397",
        input: Object.freeze(structuredClone(value)),
      };
      unresolved = true;
      show("submitting");
      timer = setTimeout(finishSubmit, 650);
    },
    values(value),
  );
}
function dirty() {
  return (
    draft &&
    scenarios[current][1] === "form" &&
    JSON.stringify(draft) !== initialDraft
  );
}
function leave(target) {
  const perform = () => {
    draft = null;
    if (
      target === "list" &&
      ["completed-current-loading", "completed-current-error"].includes(current)
    )
      readThen("list");
    else if (target === "list") show(unresolved ? "current-only" : "list");
    else location.href = target;
  };
  if (dirty() || unresolved) {
    openDialog(
      unresolved ? "保存结果尚未查明" : "放弃尚未保存的内容？",
      unresolved
        ? "离开不会取消已发出的保存请求。建议留在此页查询保存结果，不要重新新增栏目或重复保存。"
        : "这些修改还没有保存。离开后会丢弃当前输入，已保存的栏目不变。",
      "仍要离开",
      perform,
      "",
      unresolved ? "留在此页" : "继续编辑",
    );
  } else perform();
}
content.addEventListener("submit", (event) => {
  if (event.target.id === "column-form") submitForm(event);
});
content.addEventListener("input", (event) => {
  if (
    !draft ||
    !["name", "key", "description", "enabled"].includes(event.target.name)
  )
    return;
  const key = event.target.name;
  draft[key] =
    key === "enabled" ? event.target.value === "true" : event.target.value;
  const counter = document.getElementById(key + "-count");
  if (counter)
    counter.textContent =
      Array.from(draft[key]).length + " / " + (key === "name" ? 20 : 100);
});
document.addEventListener("click", (event) => {
  const local = event.target.closest("[data-local], [data-edit]");
  if (local) {
    event.preventDefault();
    if (
      unresolved ||
      (local.dataset.local === "create" && columns.length >= 100)
    )
      return;
    selected = local.dataset.edit || "culture";
    previousInput = null;
    operation = null;
    receipt = null;
    prepareForm(!local.dataset.edit);
    show(local.dataset.edit ? "edit" : "create");
    return;
  }
  const link = event.target.closest("a[href]");
  if (
    link &&
    !link.getAttribute("href").startsWith("#") &&
    (dirty() || unresolved)
  ) {
    event.preventDefault();
    leave(link.getAttribute("href"));
    return;
  }
  const action = event.target.closest("[data-action]")?.dataset.action;
  if (!action) return;
  if (action === "close-dialog") {
    dialog.close();
    return;
  }
  if (action === "list") {
    leave("list");
    return;
  }
  if (action === "fill-example") {
    draft = {
      key:
        current === "edit" || current === "refreshed"
          ? draft.key
          : "botanical-notes",
      name: "草木小记",
      description: "记录身边的草木与生活。",
      enabled: true,
    };
    errors = {};
    show(current);
    return;
  }
  if (action === "end-wait") {
    waitEnded = true;
    show(current, false);
    return;
  }
  if (action === "reload-list") {
    unresolved = false;
    readThen("list");
    return;
  }
  if (["recheck", "recheck-account"].includes(action)) {
    draft = null;
    operation = null;
    previousInput = null;
    unresolved = false;
    readThen(action === "recheck-account" ? "other-account" : "list");
    return;
  }
  if (action === "edit-current") {
    const target = columns.find((c) => c.id === (receipt?.target || selected));
    if (!target) {
      show("target-unavailable");
      return;
    }
    selected = target.id;
    previousInput = null;
    operation = null;
    receipt = null;
    prepareForm(false, target);
    show("edit");
    return;
  }
  if (action === "refresh") {
    previousInput ||= draft
      ? structuredClone(draft)
      : editableFields(columns[0]);
    show("refresh-loading");
    timer = setTimeout(() => {
      const target =
        columns.find((column) => column.id === selected) || columns[0];
      Object.assign(target, {
        name: target.id === "life" ? "器物札记" : "四时风物",
        change: "change-refreshed-2",
      });
      prepareForm(false);
      show("refreshed");
    }, 650);
    return;
  }
  if (action === "read-current") {
    unresolved = true;
    readThen("current-only", "current-loading");
    return;
  }
  if (action === "reload-current") {
    show("completed-current-loading");
    timer = setTimeout(() => show("complete"), 650);
    return;
  }
  if (action === "check") {
    if (!operation) {
      show("recovery-missing");
      return;
    }
    show("checking");
    timer = setTimeout(finishSubmit, 650);
  }
});
document.getElementById("dialog-confirm").addEventListener("click", () => {
  const action = dialogAction;
  dialog.close();
  action?.();
});
dialog.addEventListener("close", () => {
  if (returnFocus?.isConnected) returnFocus.focus();
});
previewToggle.addEventListener("click", () => {
  const expanded = previewToggle.getAttribute("aria-expanded") === "true";
  previewToggle.setAttribute("aria-expanded", String(!expanded));
  previewToggle.textContent = expanded ? "展开预览工具" : "收起预览工具";
});
picker.addEventListener("change", () => selectScenario(picker.value));
const params = new URLSearchParams(location.search);
selected = params.get("id") || "culture";
selectScenario(
  params.get("id") && !initialColumns.some((c) => c.id === selected)
    ? "target-unavailable"
    : params.get("state") || "list",
  false,
);
