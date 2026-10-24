const picker = document.getElementById("scenario-picker");
const previewToggle = document.querySelector(".preview-toggle");

previewToggle.addEventListener("click", () => {
  const expanded = previewToggle.getAttribute("aria-expanded") === "true";
  previewToggle.setAttribute("aria-expanded", String(!expanded));
  previewToggle.textContent = expanded ? "展开预览工具" : "收起预览工具";
});
const content = document.getElementById("service-content");
const dialog = document.getElementById("service-dialog");
const pageTitle = document.getElementById("page-title");
const icons = {
  card: '<rect x="3" y="5" width="18" height="15" rx="2"/><path d="M9 5V3h6v2M14 11h4M14 15h4"/><circle cx="8" cy="11" r="2"/><path d="M5.5 17a2.5 2.5 0 0 1 5 0"/>',
  photo:
    '<rect x="3" y="3" width="18" height="18" rx="2"/><circle cx="8" cy="8" r="1.5"/><path d="m3 17 6-6 5 5 3-3 4 4"/>',
  invitation:
    '<rect x="3" y="6" width="18" height="14" rx="2"/><path d="m3 7 9 7 9-7M8 3h8"/>',
  pause: '<path d="M9 5v14M15 5v14"/>',
  ended: '<path d="M6 4h12v16H6zM10 8h4M10 12h4M10 16h4"/>',
  notice: '<circle cx="12" cy="12" r="9"/><path d="M12 7v6M12 17h.01"/>',
  login: '<path d="M14 3h6v18h-6M3 12h12m-4-4 4 4-4 4"/>',
};
const icon = (name) =>
  `<svg viewBox="0 0 24 24" aria-hidden="true">${icons[name]}</svg>`;
const scenarios = new Set([...picker.options].map((option) => option.value));
const activeScenarios = new Set([
  "active",
  "active-no-number",
  "active-empty",
  "number-unavailable",
  "applications-unavailable",
]);
const suspendedScenarios = new Set([
  "suspended",
  "suspended-empty",
  "suspended-unavailable",
]);
const terminatedScenarios = new Set([
  "terminated",
  "terminated-empty",
  "terminated-unavailable",
]);
const redactedScenarios = new Set([
  "account-unavailable",
  "session-expired",
  "account-changed",
]);
let current = "active";
let pendingRead;
let confirmAction;

const previews = {
  affiliation: [
    "政治面貌",
    "此处将进入政治面貌页面。未填写时可首次填写，已有值时查看当前值与历史；不作为工号或工牌办理的前提。该页面另行设计。",
  ],
  "login-methods": [
    "登录方式",
    "此处将进入本人登录方式页面，查看绑定情况并按条件办理变更。该页面另行设计。",
  ],
  number: [
    "我的工号",
    "工号详情页将显示当前可信的工号与人员类别，不补造办理日期。详情页另行设计。",
  ],
  "apply-number": [
    "申请工号",
    "下一页由本人选择人员类别，再核对并明确确认，不预选类别，也不增加审批环节。曾取得工号者提交时收到 number_present，申请页改为引导联系医院管理员核实，不再自助领取。本片只展示办理入口。",
  ],
  "apply-card": [
    "申请工牌",
    "下一页将准备照片、牌面和领取地点，再预览确认。新办、增办或换证由服务端判断；内页、外壳、整套选项的业务规则仍待补齐。本片不模拟提交。",
  ],
  photos: [
    "工牌照片",
    "此处将进入工牌照片选择与上传页面，与账户头像分开办理。照片页另行设计。",
  ],
  history: [
    "申请记录",
    "此处将进入本人有权查看的申请列表。完整列表、分页与详情另行设计，不根据首页示例推断历史读取权。",
  ],
  "pending-detail": [
    "待审核的工牌申请",
    "详情页将展示本次固定提交、领取地点与当前允许的操作。尚未发放时可取消；修改待审核申请先取消再重新办理。详情页另行设计。",
  ],
  "completed-detail": [
    "已发放的工牌",
    "详情页将展示成功保存的发放事实与固定领取地点。已发放不等于本人已领取，也不自动提供增办资格。详情页另行设计。",
  ],
  invitation: [
    "使用邀请码",
    "下一页将输入医院管理员提供的邀请码，查看可用摘要并确认开通。使用邀请码不表示人事入职；本片不执行开通。邀请码页及异常状态另行设计。",
  ],
};

function numberPanel() {
  if (current === "active-no-number")
    return `<section class="number-panel" aria-labelledby="number-title"><div><h2 class="number-label" id="number-title">我的工号</h2><p class="number-note">还没有工号？先选择人员类别，再确认申请。曾有工号的，请联系医院管理员核实。</p></div><button type="button" class="button primary" data-preview="apply-number">申请工号</button></section>`;
  if (current === "number-unavailable")
    return `<section class="inline-state" aria-labelledby="number-title"><h2 id="number-title" class="number-label">暂时无法读取工号</h2><p>请重新加载，确认工号后再办理工牌。</p><button type="button" class="subtle-button" data-action="retry-number">重新加载工号 <span aria-hidden="true">↻</span></button></section>`;
  return `<section class="number-panel" aria-labelledby="number-title"><div><h2 class="number-label" id="number-title">我的工号 <span class="number-category">临床</span></h2><p class="employee-number">12607</p><p class="number-note">工牌将使用你的工号</p></div><button type="button" class="subtle-button" data-preview="number">查看工号 <span aria-hidden="true">→</span></button></section>`;
}

function actionCards() {
  const withoutNumber = current === "active-no-number";
  const numberUnavailable = current === "number-unavailable";
  const disabled = withoutNumber || numberUnavailable;
  const description = withoutNumber
    ? "先申请工号，再准备照片和工牌信息。"
    : numberUnavailable
      ? "工号读取成功后，可继续准备工牌。"
      : "准备照片与工牌信息，预览后提交。";
  return `<section class="service-actions" aria-label="工牌办理"><div class="action-card">${icon("card")}<h2>申请工牌</h2><p id="application-prerequisite">${description}</p><button type="button" class="button primary" data-preview="apply-card"${disabled ? ' disabled aria-describedby="application-prerequisite"' : ""}>${withoutNumber ? "请先申请工号" : numberUnavailable ? "请先确认工号" : '申请工牌 <span aria-hidden="true">→</span>'}</button></div><div class="action-card secondary-action">${icon("photo")}<h2>工牌照片</h2><p>上传或选择证件照，用于工牌办理。</p><button type="button" class="subtle-button" data-preview="photos">管理照片 <span aria-hidden="true">→</span></button></div></section>`;
}

function applications(mode = "") {
  const suspended = Boolean(mode);
  const unavailable =
    current === "applications-unavailable" ||
    current === "suspended-unavailable" ||
    current === "terminated-unavailable";
  const empty = [
    "active-empty",
    "suspended-empty",
    "terminated-empty",
  ].includes(current);
  const heading = suspended ? "可取消的申请" : "最近申请";
  let entries;
  if (unavailable) {
    entries = `<div class="inline-state"><h3>暂时无法读取申请</h3><p>${suspended ? "重新加载后，可查看尚未发放的申请。" : "申请情况尚未确认，请稍后重新加载。"}</p><button type="button" class="subtle-button" data-action="retry-applications">重新加载申请 <span aria-hidden="true">↻</span></button></div>`;
  } else if (empty) {
    entries = `<div class="empty-applications"><h3>${suspended ? "暂无可取消的申请" : "还没有工牌申请"}</h3><p>${suspended ? "目前没有尚未发放且可由你取消的申请。" : "提交申请后，可以在这里查看进度。"}</p></div>`;
  } else if (suspended) {
    entries = `<div class="application-list"><article class="application-row"><div class="application-heading"><h3>新办工牌</h3><span class="status-tag pending">审核通过</span></div><div class="application-bottom"><p>10 月 08 日提交 · 尚未发放</p><button type="button" class="subtle-button" data-action="cancel-application">取消申请</button></div></article></div>`;
  } else {
    entries = `<div class="application-list">${current === "active-no-number" ? "" : `<article class="application-row"><div class="application-heading"><h3>换证 · 呼吸内科</h3><span class="status-tag pending">待审核</span></div><p>10 月 10 日提交</p><div class="application-bottom"><p>领取地点：门诊一楼服务台</p><button type="button" class="subtle-button" data-preview="pending-detail" aria-label="查看待审核申请">查看详情 <span aria-hidden="true">→</span></button></div></article>`}<article class="application-row"><div class="application-heading"><h3>新办 · 呼吸内科</h3><span class="status-tag">已发放</span></div><p>09 月 26 日提交</p><div class="application-bottom"><p>领取地点：门诊一楼服务台</p><button type="button" class="subtle-button" data-preview="completed-detail" aria-label="查看已发放申请">查看详情 <span aria-hidden="true">→</span></button></div></article></div>`;
  }
  return `<section class="applications-panel" aria-labelledby="applications-title"><div class="section-heading"><h2 id="applications-title">${heading}</h2>${suspended || empty || unavailable ? "" : '<button type="button" class="subtle-button" data-preview="history">全部记录 <span aria-hidden="true">→</span></button>'}</div>${suspended && !empty && !unavailable ? `<p class="suspended-note">${mode === "terminated" ? "以下申请已随服务结束停止办理，原阶段保留。你仍可取消尚未发放的申请，取消不会恢复或继续办理。" : "以下申请已暂停办理，原审核结果保留。你仍可取消尚未发放的申请。"}</p>` : ""}${entries}</section>`;
}

function statePanel({
  title,
  description,
  symbol = "notice",
  tone = "",
  actions = "",
  detail = "",
}) {
  return `<section class="state-panel ${tone}" aria-labelledby="state-title"><div class="state-symbol" aria-hidden="true">${symbol === "loading" ? '<span class="loading-mark"></span>' : icon(symbol)}</div><h2 id="state-title">${title}</h2><p class="state-description">${description}</p>${actions ? `<div class="state-actions">${actions}</div>` : ""}${detail ? `<div class="state-detail">${detail}</div>` : ""}</section>`;
}

function show(id, focus = true, updateHistory = true, rechecking = false) {
  clearTimeout(pendingRead);
  if (dialog.open) dialog.close();
  current = scenarios.has(id) ? id : "active";
  picker.value = current;
  if (updateHistory) history.replaceState(null, "", `#${current}`);
  const active = activeScenarios.has(current);
  const suspended = suspendedScenarios.has(current);
  const accountAvailable = !rechecking && !redactedScenarios.has(current);
  document.querySelectorAll("[data-account]").forEach((node) => {
    node.hidden = !accountAvailable;
  });
  document.querySelector("[data-content-management]").hidden =
    !accountAvailable || current !== "content-manager";
  document.getElementById("page-introduction").textContent = rechecking
    ? "正在确认登录状态，请稍候。"
    : accountAvailable
      ? active
        ? "陈思远，你好。工号、工牌和申请进度，都在这里。"
        : "陈思远，你好。"
      : "登录后，查看你的工号、工牌和申请进度。";
  document.getElementById("account-note").textContent = active
    ? "账户资料与员工服务分别管理。"
    : "个人资料与登录设置仍可正常使用。";
  document.getElementById("service-bottom").hidden = !active && !suspended;
  const status = document.getElementById("service-status");
  status.hidden =
    !active &&
    !suspended &&
    current !== "not-enabled" &&
    current !== "content-manager" &&
    !terminatedScenarios.has(current);
  status.textContent = active
    ? "已开通"
    : suspended
      ? "已暂停"
      : terminatedScenarios.has(current)
        ? "已结束"
        : "未开通";
  status.dataset.tone = active ? "active" : suspended ? "paused" : "neutral";
  content.setAttribute("aria-busy", String(current === "loading"));
  let basis;
  if (active) {
    content.innerHTML = numberPanel() + actionCards() + applications();
    basis =
      "可信 active 开放服务；工号和申请分别读取。空值与读取失败分别展示，工号未确认时不开放工牌申请。当前工号读取对从未取得与已被撤销都返回空值，首页无法区分，由申请页按 number_present 引导联系管理员。已有申请不阻止继续申请。工号为空不代表没有申请，申请区只按申请读取结果显示；尚无工号示例保留一条已发放的历史申请。示例中的人员类别来自当前工号，不表示任职档案。";
  } else if (suspended) {
    content.innerHTML =
      statePanel({
        title: "员工服务已暂停",
        description:
          "暂时无法申请工号、工牌，也不能继续办理已有申请。如需恢复，请联系医院管理员。",
        symbol: "pause",
        tone: "paused",
        detail:
          "<h3>个人账户仍可使用</h3><p>你可以管理个人资料和登录方式，也可以取消未发放的申请或结束员工服务。</p>",
      }) + applications("suspended");
    basis =
      "仅在可信 suspended 时展示。正常账户仍可结束员工服务及取消未发放申请；暂停时不读取完整工牌资料，只展示取消所需摘要。账户暂停不适用这些例外。";
  } else if (current === "not-enabled" || current === "content-manager") {
    content.innerHTML =
      statePanel({
        title: "开通你的员工服务",
        description:
          "使用医院管理员提供的邀请码，开通后即可按需办理工号与工牌。",
        symbol: "invitation",
        actions:
          '<button type="button" class="button primary" data-preview="invitation">使用邀请码 <span aria-hidden="true">→</span></button>',
        detail:
          "<h3>还没有邀请码？</h3><p>请联系医院管理员。服务开通前，你仍可管理个人账户，浏览医院文化与科普。</p>",
      }) +
      '<section class="service-guide" aria-label="员工服务介绍"><div><h3>申请工号</h3><p>按自己的人员类别申请工号，用于后续工牌办理。</p></div><div><h3>办理工牌</h3><p>准备证件照与工牌信息，预览确认后提交，随时查看进度。</p></div></section>';
    basis =
      "只有可信 404 grant_absent 表示尚未开通。普通注册不自动建立 Grant。邀请码接纳不表示员工身份认证或人事入职；邀请页将单独设计。";
    if (current === "content-manager")
      basis +=
        " 此示例另经当前正常账户与 institution.articles 权限核对，显示栏目管理入口；权限来自机构自定义角色，不要求员工服务资格。其他状态不推定具有内容维护权限。";
  } else if (terminatedScenarios.has(current)) {
    content.innerHTML =
      statePanel({
        title: "员工服务已结束",
        description: "本次员工服务已结束，原来尚未发放的工牌申请无法继续办理。",
        symbol: "ended",
        tone: "ended",
        actions:
          '<button type="button" class="button primary" data-preview="invitation">使用邀请码重新开通 <span aria-hidden="true">→</span></button>',
        detail:
          "<h3>以后还需要办理？</h3><p>可向医院管理员获取邀请码，重新开通。原申请不会恢复，旧工牌也不能用于增办；需要工牌时，请重新申请。</p>",
      }) +
      applications("terminated") +
      '<div class="inline-state"><h3>个人账户仍可使用</h3><p>服务结束不影响管理个人资料和登录方式。已有工号也不会因服务结束而自动改变。</p></div>';
    basis =
      "可信 terminated 表示原 Grant 已结束。重新加入建立新 Grant，不恢复旧申请或旧工牌的增办来源资格；工号属于 Person，不因结束服务自动删除。不把旧申请阶段改写为取消或结束。账户可用时本人仍可取消未发放的原申请，取消不恢复也不继续办理；本页只展示取消所需摘要。";
  } else if (current === "loading") {
    content.innerHTML = statePanel({
      title: rechecking ? "正在确认登录状态" : "正在读取服务状态",
      description: "请稍候，确认后将显示可办理的事项。",
      symbol: "loading",
    });
    basis = rechecking
      ? "从浏览器往返缓存返回时，先复查当前登录与账户，再读取服务状态；复查完成前不展示姓名、个人账户入口、工号或申请。"
      : "已确认账户正常，Grant 尚在读取。此时不猜测为未开通，不展示旧工号与旧申请；个人账户仍可进入。该状态由工具栏选中后保持，以便检查。";
  } else if (current === "unavailable") {
    content.innerHTML = statePanel({
      title: "暂时无法读取服务状态",
      description: "请重新加载后再办理。个人账户和公开内容仍可查看。",
      actions:
        '<button type="button" class="button primary" data-action="retry-service">重新加载 <span aria-hidden="true">↻</span></button>',
    });
    basis =
      "依赖或读取失败不能解释为 grant_absent、suspended 或 terminated。账户已可信读取，服务状态未知时隐藏工号与申请；重新加载只演示读取。";
  } else if (current === "account-unavailable") {
    content.innerHTML = statePanel({
      title: "暂时无法确认登录状态",
      description:
        "请检查网络后重新加载。确认当前账户前，不显示个人资料和员工服务。",
      actions:
        '<button type="button" class="button primary" data-action="reread-account">重新加载 <span aria-hidden="true">↻</span></button><a class="subtle-button" href="halo-home.html">先浏览公开内容</a>',
    });
    basis =
      "GET /api/v1/accounts/current 断连或 dependency_unavailable：当前账户尚未可靠复查，保持遮蔽姓名、账户入口、工号与申请，不解释为登录失效或服务状态失败。重新加载走遮蔽的复查后再显示。";
  } else {
    const changed = current === "account-changed";
    content.innerHTML = statePanel({
      title: changed ? "账户已切换" : "请重新登录",
      description: changed
        ? "你已在其他页面切换账户。确认当前账户后，再查看它的员工服务。"
        : "当前登录已失效。重新登录后，再继续查看员工服务。",
      symbol: "login",
      actions: `${changed ? '<button type="button" class="button primary" data-action="reread-account">查看当前账户 <span aria-hidden="true">↻</span></button>' : '<a class="button primary" href="halo-authentication.html">重新登录 <span aria-hidden="true">→</span></a>'}<a class="subtle-button" href="halo-home.html">先浏览公开内容</a>`,
    });
    basis =
      "会话失效或账户切换立即清除旧姓名、工号和申请展示。账户切换不要求重新认证：重新读取浏览器当前选择、本人账户与服务状态，复查完成前保持遮蔽；只有登录失效才进入登录页。本稿没有实际会话与跨标签页监听；通过状态选择器检查遮蔽效果。";
  }
  document.getElementById("state-basis").textContent = basis;
  if (focus) pageTitle.focus({ preventScroll: true });
}

function openDialog({
  title,
  description,
  confirm = "知道了",
  back,
  action,
  summary,
  note,
}) {
  confirmAction = action;
  document.getElementById("dialog-context").textContent = back
    ? "操作确认"
    : "设计预览 · 后续页面";
  document.getElementById("dialog-title").textContent = title;
  document.getElementById("dialog-description").textContent = description;
  document.getElementById("dialog-confirm").textContent = confirm;
  const backButton = document.getElementById("dialog-back");
  backButton.hidden = !back;
  backButton.textContent = back || "返回";
  const summaryNode = document.getElementById("dialog-summary");
  summaryNode.hidden = !summary;
  summaryNode.innerHTML = summary || "";
  const noteNode = document.getElementById("dialog-note");
  noteNode.hidden = !note;
  noteNode.textContent = note || "";
  dialog.showModal();
  if (back) backButton.focus();
}

function showBoundary(title, description) {
  openDialog({ title, description });
}

function retry(button, target) {
  const origin = current;
  button.disabled = true;
  button.textContent = "正在读取…";
  button.parentElement.setAttribute("aria-busy", "true");
  pendingRead = setTimeout(() => {
    if (current === origin) show(target);
  }, 700);
}

document.addEventListener("click", (event) => {
  const button = event.target.closest("[data-preview], [data-action]");
  if (!button) return;
  const target = button.dataset.preview;
  if (target === "profile") {
    location.href = "halo-account.html";
    return;
  }
  if (target && Object.hasOwn(previews, target)) {
    showBoundary(...previews[target]);
    return;
  }
  switch (button.dataset.action) {
    case "close-dialog":
      dialog.close();
      break;
    case "retry-number":
      retry(button, "active");
      break;
    case "retry-applications":
      retry(
        button,
        suspendedScenarios.has(current)
          ? "suspended"
          : terminatedScenarios.has(current)
            ? "terminated"
            : "active",
      );
      break;
    case "reread-account":
      show("loading", true, false, true);
      pendingRead = setTimeout(() => show("active", false), 700);
      break;
    case "retry-service":
      show("loading", true, false);
      pendingRead = setTimeout(() => {
        if (current === "loading") show("active");
      }, 700);
      break;
    case "leave-service":
      location.href = `halo-service-termination.html#${suspendedScenarios.has(current) ? "suspended" : "active"}`;
      break;
    case "cancel-application":
      openDialog({
        title: "取消这份工牌申请？",
        description:
          "取消后，这份申请将停止办理，不能恢复。其他申请和员工服务状态不会改变。",
        confirm: "确认取消",
        back: "保留申请",
        summary:
          "<strong>新办工牌 · 审核通过</strong><p>10 月 08 日提交 · 尚未发放</p>",
        action: () =>
          showBoundary(
            "取消确认已展示",
            "本稿未取消申请。正式办理须重新确认申请尚未发放，并核对本次取消结果；超时不代表取消失败。",
          ),
      });
      break;
    case "sign-out":
      openDialog({
        title: "退出登录？",
        description:
          "退出后，重新登录才能查看个人资料和员工服务。已提交的申请不受影响。",
        confirm: "退出登录",
        back: "留在此页",
        action: () => {
          location.href = "halo-authentication.html";
        },
      });
      break;
  }
});
document.getElementById("dialog-confirm").addEventListener("click", () => {
  const action = confirmAction;
  dialog.close();
  action?.();
});
picker.addEventListener("change", () => show(picker.value, false));
addEventListener("popstate", () => {
  const id = location.hash.slice(1);
  if (scenarios.has(id)) show(id, true, false);
});
addEventListener("pagehide", () => {
  clearTimeout(pendingRead);
  content.replaceChildren();
  document.getElementById("page-introduction").textContent = "";
});
function restore() {
  const recorded = location.hash.slice(1);
  const target = scenarios.has(recorded) ? recorded : current;
  if (redactedScenarios.has(target)) {
    show(target, false);
    return;
  }
  show("loading", false, false, true);
  pendingRead = setTimeout(
    () => show(target === "loading" ? "active" : target, false),
    700,
  );
}

addEventListener("pageshow", (event) => {
  if (event.persisted) restore();
});
for (const link of document.querySelectorAll('a[href="#services"]'))
  link.addEventListener("click", (event) => {
    event.preventDefault();
    document.getElementById("services").focus();
  });
restore();
