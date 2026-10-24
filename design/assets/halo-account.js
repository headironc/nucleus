const picker = document.getElementById("scenario-picker");
const previewToggle = document.querySelector(".preview-toggle");
const content = document.getElementById("account-content");
const notice = document.getElementById("page-notice");
const dialog = document.getElementById("account-dialog");

const identityBasis =
  "GET accounts/current：可信本人投影仅含完整姓名、身份证尾四位、头像和当前绑定事实。当前资料与实名更正历史分别读取。";
const historyBasis =
  "本人更正历史：before/after 姓名完整、证件仅尾四位，无办理人；按更正成功顺序倒序。page、size、count 分别解释页码、每页条数和总数。";
const scenarios = {
  ready: { basis: identityBasis },
  unbound: { basis: identityBasis, unbound: true },
  "long-name": { basis: identityBasis, fixture: "long" },
  loading: {
    masked: true,
    busy: true,
    title: "正在读取当前账户",
    description: "确认完成后，再展示本人资料与实名更正记录。",
    basis: "进入页面先核对当前账户。未取得可信结果前，不展示旧资料和历史。",
  },
  unavailable: {
    masked: true,
    title: "暂时无法加载账户资料",
    description: "请刷新后重试。登录状态仍待确认，无需重复登录。",
    action: "reload-account",
    actionLabel: "重新加载",
    basis:
      "当前读取断连或 dependency_unavailable：遮蔽资料，保留原选择，只重读当前账户。不伪装成退出、未绑定或空资料。",
  },
  limited: {
    masked: true,
    title: "暂时无法读取账户",
    description: "暂时无法处理，请稍后再试。",
    action: "reload-account",
    actionLabel: "稍后重新加载",
    limited: true,
    basis:
      "rate_limited 按 Retry-After 等待，示例为 60 秒。不归因为用户频繁操作，也不要求重复登录。",
  },
  "session-invalid": {
    masked: true,
    title: "请重新登录",
    description: "当前登录已无法继续使用。重新登录后，再查看本人资料。",
    login: true,
    basis:
      "401 authentication_invalid 不区分过期、撤销、账户不可用或其他原因。遮蔽全部私有资料，不推断暂停或注销。",
  },
  forbidden: {
    masked: true,
    title: "暂时无法查看账户资料",
    description: "暂时无法读取账户资料，请稍后再试。",
    action: "return-home",
    actionLabel: "返回首页",
    basis:
      "当前读取的 403 permission_denied 表示网站同源条件不成立；遮蔽资料与历史并保留返回入口，不引导重新确认账户或推断账户状态。",
  },
  "account-changed": {
    masked: true,
    title: "登录状态已变化",
    description: "请重新确认当前账户。此前的账户资料与记录已隐藏。",
    action: "confirm-account",
    actionLabel: "重新确认当前账户",
    basis:
      "浏览器选择变化后立即遮蔽原账户资料与历史。重新读取当前选中的账户后才显示新资料，旧响应不能覆盖新选择。",
  },
  "other-account": { basis: identityBasis, fixture: "other" },
  "profile-changed": {
    masked: true,
    title: "账户资料已更新",
    description: "请重新加载，核对最新资料与实名更正记录。此前内容已隐藏。",
    action: "confirm-profile",
    actionLabel: "重新加载资料与记录",
    basis:
      "示例为可信本人重读发现实名版本已变化；先清理旧资料与历史，再读取当前内容。不虚构普通 GET 的 data_changed 响应或本人编辑实名入口。",
  },
  updated: {
    basis: identityBasis,
    fixture: "updated",
    notice: "已重新读取当前资料与实名更正记录，请核对下方内容。",
  },
  "avatar-unavailable": {
    basis:
      "本人 JSON 投影已可信读取，只有原生图片加载失败。不能从 img.onerror 推断链接过期、会话退出或更换失败；重新取当前链接且最多重试一次。",
    avatar: "failed",
  },
  "avatar-retry-failed": {
    basis:
      "已重新取得当前头像链接并重试一次；图片仍不可用时保留明确占位，不自动无限重试，不重新上传。",
    avatar: "retried",
  },
  "history-loading": { basis: historyBasis, history: "loading" },
  "history-page-two": { basis: historyBasis, page: 2 },
  "history-empty": { basis: historyBasis, empty: true },
  "history-unavailable": {
    basis:
      "历史读取失败只在记录区提供重读，不显示 count: 0 或暂无记录。当前账户资料仍有自己的可信结果。",
    history: "error",
  },
  "history-limited": {
    basis:
      "历史 rate_limited 等待后只重读历史，示例 Retry-After 为 60 秒；不影响已经可靠读取的当前账户资料。",
    history: "limited",
    limited: true,
  },
  "history-out-of-range": {
    basis:
      "示例 page=4、size=2、count=5 且 corrections=[]；这是越界页，不是无更正历史。返回第一页重新读取，不更改可信总数。",
    page: 4,
  },
};

const value = (name, suffix) => ({ name, idNumber: { suffix } });
const changes = [
  {
    before: value("陈思源", "0028"),
    after: value("陈思远", "0028"),
    correctedAt: "2026-10-12T09:30:00+08:00",
  },
  {
    before: value("陈思源", "0007"),
    after: value("陈思源", "0028"),
    correctedAt: "2026-06-18T15:20:00+08:00",
  },
  {
    before: value("陈思圆", "0007"),
    after: value("陈思源", "0007"),
    correctedAt: "2025-10-09T10:00:00+08:00",
  },
  {
    before: value("陈思圆", "0000"),
    after: value("陈思圆", "0007"),
    correctedAt: "2025-05-02T14:10:00+08:00",
  },
  {
    before: value("陈思园", "0000"),
    after: value("陈思圆", "0000"),
    correctedAt: "2025-02-08T08:40:00+08:00",
  },
];
const fixtures = {
  primary: {
    ...value("陈思远", "0028"),
    binding: { revision: "13ad41be-5ce3-4e90-8a44-339d6e0ac040" },
    changes,
  },
  long: {
    ...value("阿依古丽·阿不都热依木", "0007"),
    binding: null,
    changes: [
      {
        before: value("阿依古丽·阿不都拉", "0007"),
        after: value("阿依古丽·阿不都热依木", "0007"),
        correctedAt: "2026-08-11T11:25:00+08:00",
      },
    ],
  },
  other: { ...value("林知夏", "0000"), binding: null, changes: [] },
  updated: {
    ...value("陈思宁", "0007"),
    binding: { revision: "13ad41be-5ce3-4e90-8a44-339d6e0ac040" },
    changes: [
      {
        before: value("陈思远", "0028"),
        after: value("陈思宁", "0007"),
        correctedAt: "2026-10-20T10:15:00+08:00",
      },
      ...changes,
    ],
  },
};
const maintenance = {
  avatar: [
    "更换头像",
    "此处将进入本人头像更换页面。头像与工牌照片分别办理，更换不影响实名、微信或员工服务。本片仅展示入口，未上传或更换头像。",
  ],
  affiliation: [
    "政治面貌",
    "进入本人政治面貌页面。未填写时可自愿首次填写；已有值只读。",
  ],
  "login-methods": [
    "登录方式",
    "此处将进入本人微信绑定与登录安全页面。绑定、解绑及退出全部设备将按各自条件明确确认。本片只展示当前可信绑定事实，不办理变更。",
  ],
  mobile: [
    "更换手机号",
    "网站本人更换手机号须先验证当前手机号，再验证新手机号。成功后需用新手机号重新登录。独立设计稿演示办理与原结果查询，不发送真实短信或更改账户。",
  ],
  logout: [
    "退出本次登录",
    "此处进入退出本次登录及结果确认设计稿，不执行真实退出。",
  ],
  "all-logout": ["退出全部设备"],
  cancellation: [
    "注销账户",
    "注销将结束账户、全部登录和本人服务；必要业务历史仍按规则保留，重新注册不继承旧资格。此处进入本人注销设计稿，不执行真实注销。",
  ],
};
const avatarIcon =
  '<svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="8" r="3.5"/><path d="M5 21v-2a7 7 0 0 1 14 0v2"/></svg>';
const avatarSource = "assets/halo-avatar-current.webp";
const dateFormat = new Intl.DateTimeFormat("zh-CN", {
  dateStyle: "medium",
  timeStyle: "short",
  timeZone: "Asia/Shanghai",
});
let current = "loading";
let fixture = "primary";
let avatar = "available";
let binding = null;
let historyEmpty = false;
let page = 1;
let size = 2;
let pendingRead;
let waitClock;

function stopTimers() {
  clearTimeout(pendingRead);
  clearInterval(waitClock);
}

function readState(title, description, action, label, options = {}) {
  const tag = options.history ? "h3" : "h2";
  return `<div class="reading-state"${options.busy ? ' aria-busy="true"' : ""}>
    ${options.busy ? '<span class="reading-symbol" aria-hidden="true"></span>' : ""}
    <${tag}>${title}</${tag}><p>${description}</p>
    ${options.login ? '<a class="button primary" href="halo-authentication.html">重新登录</a>' : action ? `<button class="button" type="button" data-action="${action}"${options.limited ? " data-wait disabled" : ""}>${label}</button>` : ""}
  </div>`;
}

function profilePanel() {
  const account = fixtures[fixture];
  const bound = binding !== null;
  return `<section class="profile-panel" aria-labelledby="profile-title">
    <div class="section-heading"><h2 id="profile-title">当前账户资料</h2><button type="button" class="text-action" data-action="reload-account">重新加载</button></div>
    <div class="profile-summary">
      <div class="avatar-slot"${avatar !== "available" ? ' role="img" aria-label="头像暂时无法加载"' : ""}>${avatar === "available" ? `<img src="${avatarSource}" width="72" height="72" alt="虚构头像示例" />` : avatarIcon}</div>
      <div class="profile-name"><h3>${account.name}</h3><p>本人账户</p></div>
    </div>
    ${avatar !== "available" ? `<p class="avatar-message">${avatar === "retried" ? "头像仍暂时无法加载，其他资料可继续查看。" : '图片暂时无法加载。<button type="button" class="text-action" data-action="reload-avatar">重新加载头像</button>'}</p>` : ""}
    <dl class="profile-facts">
      <div><dt>身份证号</dt><dd>尾号 <span class="suffix">${account.idNumber.suffix}</span></dd></div>
      <div><dt>微信</dt><dd><span class="binding-state${bound ? "" : " unbound"}">${bound ? "已绑定" : "未绑定"}</span></dd></div>
    </dl>
    <p class="profile-explanation">姓名和身份证号用于账户登记。如需更正，请联系医院管理员；下方可查看本人实名更正记录。</p>
  </section>`;
}

function maintenancePanel() {
  return `<aside class="maintenance-panel" aria-labelledby="maintenance-title"><h2 id="maintenance-title">账户管理</h2>
    <p>员工服务未开通、暂停或结束时，正常账户仍可使用。</p>
    <div class="maintenance-links">${Object.entries(maintenance)
      .map(
        ([key, [label]]) =>
          `<button type="button" data-preview="${key}"${key === "cancellation" ? ' class="danger-link"' : ""}><span>${label}</span><span aria-hidden="true">›</span></button>`,
      )
      .join("")}</div>
  </aside>`;
}

function recordItem(record) {
  return `<li class="change-record"><article>
    <div class="change-heading"><h3>实名资料更正</h3><time datetime="${record.correctedAt}">${dateFormat.format(new Date(record.correctedAt))}</time></div>
    <div class="change-values">
      <div><p class="value-label">更正前</p><p>${record.before.name}</p><p class="value-suffix">身份证尾号 <span class="suffix">${record.before.idNumber.suffix}</span></p></div>
      <span class="change-arrow" aria-hidden="true">→</span>
      <div><p class="value-label">更正后</p><p>${record.after.name}</p><p class="value-suffix">身份证尾号 <span class="suffix">${record.after.idNumber.suffix}</span></p></div>
    </div>
  </article></li>`;
}

function historyPanel() {
  const mode = scenarios[current].history;
  let body;
  if (mode === "loading")
    body = readState("正在读取更正记录", "读取完成后再展示记录。", null, null, {
      history: true,
      busy: true,
    });
  else if (mode === "error")
    body = readState(
      "暂时无法加载更正记录",
      "请刷新后重试。当前账户资料仍可查看。",
      "reload-history",
      "重新加载记录",
      { history: true },
    );
  else if (mode === "limited")
    body = readState(
      "暂时无法读取更正记录",
      "暂时无法处理，请稍后再试。当前账户资料仍可查看。",
      "reload-history",
      "稍后重新加载",
      { history: true, limited: true },
    );
  else {
    const records = historyEmpty ? [] : fixtures[fixture].changes;
    const count = records.length;
    const visible = records.slice((page - 1) * size, page * size);
    body = visible.length
      ? `<ol class="change-list">${visible.map(recordItem).join("")}</ol>`
      : count === 0
        ? readState(
            "暂无实名更正记录",
            "当前账户没有可展示的实名更正记录。",
            null,
            null,
            { history: true },
          )
        : readState(
            "这一页没有记录",
            "记录总数仍为 " + count + " 条，请返回第一页查看。",
            "first-page",
            "返回第一页",
            { history: true },
          );
    body += `<div class="history-controls"><span>共 ${count} 条记录</span><label class="page-size" for="page-size">每页<select id="page-size">${[2, 20, 50].map((value) => `<option value="${value}"${value === size ? " selected" : ""}>${value} 条</option>`).join("")}</select></label>
      <nav class="pagination" aria-label="实名更正记录分页"><button type="button" data-action="previous-page"${page <= 1 ? " disabled" : ""}>上一页</button><span aria-live="polite">第 ${page} 页</span><button type="button" data-action="next-page"${page * size >= count ? " disabled" : ""}>下一页</button></nav></div>`;
  }
  return `<section class="history-panel" id="correction-history" aria-labelledby="history-title">
    <div class="section-heading"><h2 id="history-title" tabindex="-1">实名更正记录</h2><button type="button" class="text-action" data-action="reload-history"${mode === "loading" || mode === "limited" ? " disabled" : ""}>刷新记录</button></div>
    <p class="history-explanation">按更正先后从新到旧展示，时间以北京时间显示。证件号仅展示尾四位。</p>${body}</section>`;
}

function render(focus = true) {
  const state = scenarios[current];
  if (dialog.open) dialog.close();
  picker.value = current;
  document.getElementById("state-basis").textContent = state.basis;
  notice.hidden = !state.notice;
  notice.textContent = state.notice || "";
  content.innerHTML = state.masked
    ? readState(
        state.title,
        state.description,
        state.action,
        state.actionLabel,
        state,
      )
    : `<div class="account-columns"><div class="account-main">${profilePanel()}${historyPanel()}</div>${maintenancePanel()}</div>`;
  if (state.limited) {
    let remaining = 60;
    const tick = () => {
      for (const button of content.querySelectorAll("[data-wait]")) {
        button.disabled = remaining > 0;
        button.textContent =
          remaining > 0 ? `${remaining} 秒后可重新加载` : "重新加载";
      }
      if (!remaining) clearInterval(waitClock);
      remaining -= 1;
    };
    tick();
    waitClock = setInterval(tick, 1000);
  }
  if (focus) document.getElementById("page-title").focus();
}

function show(id, focus = true) {
  stopTimers();
  current = Object.hasOwn(scenarios, id) ? id : "ready";
  const state = scenarios[current];
  fixture = state.fixture || "primary";
  binding = state.unbound ? null : fixtures[fixture].binding;
  avatar = state.avatar || "available";
  historyEmpty = Boolean(state.empty);
  page = state.page || 1;
  size = 2;
  render(focus);
}

function loadAccount(target) {
  show("loading");
  pendingRead = setTimeout(() => show(target), 650);
}

function loadHistory(targetPage = 1, targetSize = size) {
  stopTimers();
  current = "history-loading";
  render(false);
  pendingRead = setTimeout(() => {
    page = targetPage;
    size = targetSize;
    current = historyEmpty ? "history-empty" : "ready";
    render(false);
    document.getElementById("history-title").focus();
  }, 650);
}

previewToggle.addEventListener("click", () => {
  const expanded = previewToggle.getAttribute("aria-expanded") === "true";
  previewToggle.setAttribute("aria-expanded", String(!expanded));
  previewToggle.textContent = expanded ? "展开预览工具" : "收起预览工具";
});
picker.addEventListener("change", () => show(picker.value, false));
document.addEventListener("change", (event) => {
  if (event.target.id === "page-size")
    loadHistory(1, Number(event.target.value));
});
document.addEventListener("click", (event) => {
  const button = event.target.closest("[data-action], [data-preview]");
  if (!button || button.disabled) return;
  const preview = button.dataset.preview;
  if (preview && Object.hasOwn(maintenance, preview)) {
    if (preview === "avatar") {
      location.href = "halo-avatar.html";
      return;
    }
    if (preview === "mobile") {
      location.href = "halo-mobile-change.html";
      return;
    }
    if (preview === "affiliation") {
      location.href = "halo-affiliation.html";
      return;
    }
    if (preview === "logout") {
      const state = scenarios[current];
      const target = state.masked
        ? {
            unavailable: "unavailable",
            limited: "limited",
            "session-invalid": "initial-session-invalid",
            forbidden: "forbidden",
            "account-changed": "account-changed",
          }[current] || "loading"
        : fixture === "other"
          ? "other-ready"
          : fixture === "long"
            ? "long-name"
            : fixture === "updated"
              ? "updated-ready"
              : avatar !== "available"
                ? "avatar-unavailable"
                : "ready";
      location.href = `halo-logout.html#${target}`;
      return;
    }
    if (preview === "all-logout") {
      const target =
        fixture === "other"
          ? "other-ready"
          : fixture === "long"
            ? "long-name"
            : fixture === "updated"
              ? "updated-ready"
              : avatar !== "available"
                ? "avatar-failed"
                : "ready";
      location.href = `halo-all-logout.html?scenario=${target}`;
      return;
    }
    if (preview === "cancellation") {
      const example =
        fixture === "other"
          ? "other"
          : fixture === "long"
            ? "long-name"
            : fixture === "updated"
              ? "updated"
              : "primary";
      location.href = `halo-cancellation.html?example=${example}`;
      return;
    }
    const [title, description] = maintenance[preview];
    document.getElementById("dialog-title").textContent = title;
    document.getElementById("dialog-description").textContent = description;
    dialog.showModal();
    return;
  }
  switch (button.dataset.action) {
    case "close-dialog":
      dialog.close();
      break;
    case "return-home":
      location.href = "halo-home.html";
      break;
    case "confirm-account":
      loadAccount("other-account");
      break;
    case "confirm-profile":
      loadAccount("updated");
      break;
    case "reload-account": {
      const target =
        fixture === "other"
          ? "other-account"
          : fixture === "updated"
            ? "updated"
            : fixture === "long"
              ? "long-name"
              : !binding
                ? "unbound"
                : "ready";
      loadAccount(target);
      break;
    }
    case "reload-avatar":
      button.disabled = true;
      button.textContent = "正在重新加载…";
      pendingRead = setTimeout(() => {
        avatar = "retried";
        current = "avatar-retry-failed";
        render(false);
      }, 650);
      break;
    case "reload-history":
    case "first-page":
      loadHistory(1);
      break;
    case "previous-page":
      loadHistory(Math.max(1, page - 1));
      break;
    case "next-page":
      loadHistory(page + 1);
      break;
  }
});
for (const link of document.querySelectorAll('a[href="#account"]')) {
  link.addEventListener("click", (event) => {
    event.preventDefault();
    document.getElementById("account").focus();
  });
}
function enter() {
  const selected = location.hash.slice(1);
  const target = Object.hasOwn(scenarios, selected) ? selected : "ready";
  show("loading", false);
  if (target !== "loading")
    pendingRead = setTimeout(() => show(target, false), 650);
}
addEventListener("pagehide", () => {
  stopTimers();
  content.replaceChildren();
  notice.hidden = true;
  notice.textContent = "";
});
addEventListener("pageshow", (event) => {
  if (event.persisted) enter();
});
enter();
