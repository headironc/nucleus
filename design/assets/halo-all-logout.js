const picker = document.getElementById("scenario-picker");
const content = document.getElementById("logout-content");
const dialog = document.getElementById("logout-dialog");
const title = document.getElementById("page-title");
const introduction = document.getElementById("page-introduction");
const scenarios = {
  ready: "本人已读取 · 退出入口",
  "other-ready": "本人已读取 · 另一示例账户入口",
  "updated-ready": "本人已读取 · 更新后资料入口",
  "long-name": "长姓名",
  "avatar-failed": "仅头像图片失败",
  loading: "初读中 · 隐藏资料",
  "read-failed": "初读失败",
  "read-limited": "初读受限",
  "read-invalid": "初读 401",
  "read-forbidden": "初读 403",
  confirm: "明确确认 · 包含当前登录",
  "save-failed": "发送前保存失败 · 未发送",
  sending: "退出请求发送中",
  "submit-limited": "退出请求暂时受限",
  rejected: "退出请求被明确拒绝",
  unknown: "退出结果未知",
  "unknown-invalid": "原请求 401 · 原结果未知",
  "check-login": "恢复前检查当前登录",
  "check-failed": "登录状态检查失败",
  "check-limited": "登录状态检查受限",
  "login-required": "恢复需同账户重新登录",
  "recovery-ready": "同账户新登录 · 可查看原结果",
  "original-valid": "原登录仍有效 · 只读查看原结果",
  querying: "正在只读查看原结果",
  "query-failed": "原结果读取技术失败",
  "query-limited": "原结果读取受限",
  "query-empty": "原结果为空 · 仍不能确定",
  "query-invalid": "原结果读取 401",
  "query-forbidden": "原结果读取 403",
  "materials-missing": "原结果查询信息缺失",
  "other-account": "另一账户 · 隐藏旧操作",
  "current-account": "查询信息缺失 · 当前账户已读取",
  success: "本次全部退出已完成",
  recovered: "同账户新登录 · 已读到原完成",
  "late-success": "迟到成功 · 保留后来新登录",
  "request-mismatch": "请求与此前退出不一致 · 只读查看原结果",
  "materials-login-required": "查询信息缺失 · 当前需登录",
  "mismatch-result": "此前退出记录已读到 · 不判断全部退出范围",
};
const basis = {
  ready:
    "本人有效会话发起 DELETE /api/v1/sessions，无正文。入口独立于当前退出，不展示未经接口提供的设备信息。",
  confirm:
    "本人明确确认范围包含当前登录、网站和小程序；保留账户、实名、微信绑定和员工服务。",
  unknown:
    "断连、超时和 outcome_unknown 不能判断原 DELETE 是否提交。保留原操作，不换键或重发 DELETE。",
  "unknown-invalid":
    "原 DELETE 的 401 只说明本次认证不成立，不能据此判断原退出完成或失败。",
  "recovery-ready":
    "同账户新会话只能 GET /api/v1/revocations?operation_id=原标识；本次 GET 使用独立请求标识，绝不重新 DELETE。",
  "query-empty": "GET 的空列表不能证明原退出回滚或未执行，不提供重复退出。",
  "request-mismatch":
    "DELETE 的 request_mismatch 不代表原退出失败或首次未受理。保留原定位；同账户当前有效登录只能 GET 读取原退出，不重发 DELETE。",
  "mismatch-result":
    "GET 的原集合不携带退出范围。请求不一致后的读取只展示此前记录，不把其他范围的原完成当成本次全部退出成功；当前登录保留。",
  "other-account":
    "未读到原账户、或读到另一账户时，不展示旧操作类型、状态、姓名或离开提示。",
  recovered:
    "原结果封印当次会话集合，count 为会话数。原 GET 只读，不清理新 cookie，不撤销后来新登录。",
};
for (const [value, label] of Object.entries(scenarios)) {
  const option = document.createElement("option");
  option.value = value;
  option.textContent = label;
  picker.append(option);
}
let current = "ready";
let pending = false;
let originalValid = false;
let missingMaterials = false;
let resultScopeUnverified = false;
let nextLocation = "halo-account.html";
let timer;
let waitTimer;
let returnFocus;
let operationVisible = true;
let accountExample = "ready";
const accounts = {
  ready: { name: "陈思远", suffix: "0028", hash: "ready" },
  "long-name": {
    name: "阿依古丽·阿不都热依木",
    suffix: "0007",
    hash: "long-name",
  },
  "other-ready": { name: "林知夏", suffix: "0000", hash: "other-account" },
  "updated-ready": { name: "陈思宁", suffix: "0007", hash: "updated" },
  "avatar-failed": {
    name: "陈思远",
    suffix: "0028",
    hash: "avatar-unavailable",
  },
};
function accountHref() {
  const trusted = [
    ...Object.keys(accounts),
    "confirm",
    "save-failed",
    "submit-limited",
    "rejected",
    "recovery-ready",
    "original-valid",
    "query-failed",
    "query-limited",
    "query-empty",
    "current-account",
    "recovered",
    "late-success",
    "mismatch-result",
  ].includes(current);
  const target = trusted
    ? accounts[accountExample].hash
    : {
        "read-failed": "unavailable",
        "check-failed": "unavailable",
        "read-limited": "limited",
        "check-limited": "limited",
        "read-invalid": "session-invalid",
        "unknown-invalid": "session-invalid",
        "login-required": "session-invalid",
        "query-invalid": "session-invalid",
        "materials-login-required": "session-invalid",
        success: "session-invalid",
        "read-forbidden": "forbidden",
        "query-forbidden": "forbidden",
        "other-account": "other-account",
      }[current] || "loading";
  return `halo-account.html#${target}`;
}
const button = (action, label, primary = true) =>
  `<button type="button" class="button ${primary ? "primary" : "secondary"}" data-action="${action}">${label}</button>`;
const home =
  '<a class="button secondary" href="halo-home.html" data-leave>返回首页</a>';
const login = () =>
  `<a class="button primary" href="halo-authentication.html?return=all-logout-result${resultScopeUnverified ? "&example=scope-unverified" : ""}">手机号登录</a>`;
const ordinaryLogin =
  '<a class="button primary" href="halo-authentication.html">手机号登录</a>';
function person(account = accounts[accountExample], failed = false) {
  return `<div class="logout-person">${failed ? '<span class="logout-avatar" role="img" aria-label="头像暂时无法加载">头像</span>' : '<img src="assets/halo-avatar-current.webp" width="56" height="56" alt="虚构头像" />'}<div><strong>${account.name}</strong><p>本人账户 · 身份证尾号 ${account.suffix}</p></div></div>`;
}
function result(heading, text, actions = "", extra = "", success = false) {
  return `<section class="logout-panel logout-result"><span class="result-mark${success ? " success" : ""}" aria-hidden="true">${success ? "✓" : "·"}</span><h2>${heading}</h2><p class="lead">${text}</p>${extra}<div class="logout-actions">${actions}</div></section>`;
}
function wait(label) {
  return `<button type="button" class="button primary" data-wait disabled>60 秒后${label}</button>`;
}
function show(state, focus = true) {
  clearTimeout(timer);
  clearInterval(waitTimer);
  current = Object.hasOwn(scenarios, state) ? state : "ready";
  picker.value = current;
  if (Object.hasOwn(accounts, current)) accountExample = current;
  if (
    [
      "ready",
      "other-ready",
      "updated-ready",
      "long-name",
      "avatar-failed",
      "confirm",
      "save-failed",
      "submit-limited",
      "rejected",
      "loading",
      "read-failed",
      "read-limited",
      "read-invalid",
      "read-forbidden",
      "success",
    ].includes(current)
  ) {
    pending = false;
    originalValid = false;
    missingMaterials = false;
    resultScopeUnverified = false;
  } else if (current !== "other-account") {
    pending = !["recovered", "late-success", "mismatch-result"].includes(
      current,
    );
  }
  if (["request-mismatch", "mismatch-result"].includes(current))
    resultScopeUnverified = true;
  if (current === "original-valid") originalValid = true;
  if (["recovery-ready", "query-invalid", "other-account"].includes(current))
    originalValid = false;
  if (["materials-missing", "materials-login-required"].includes(current))
    missingMaterials = true;
  const hidden = [
    "loading",
    "read-failed",
    "read-limited",
    "read-invalid",
    "read-forbidden",
    "check-login",
    "check-failed",
    "check-limited",
    "other-account",
    "current-account",
    "materials-login-required",
  ].includes(current);
  operationVisible = !hidden;
  const completed = [
    "success",
    "recovered",
    "late-success",
    "mismatch-result",
  ].includes(current);
  title.textContent = hidden
    ? "个人账户"
    : pending || completed
      ? resultScopeUnverified
        ? "此前退出结果"
        : "本次退出结果"
      : "退出全部设备";
  document.title = `${title.textContent} · 司籍处`;
  if (hidden) introduction.textContent = "查看当前登录的本人账户。";
  else if (current === "mismatch-result")
    introduction.textContent = "以下为已查询到的此前退出记录。";
  else if (completed)
    introduction.textContent = "本次退出已完成，以下为当时的退出结果。";
  else if (current === "sending")
    introduction.textContent = "正在等待本次退出全部设备的结果。";
  else if (["materials-missing", "query-forbidden"].includes(current))
    introduction.textContent = "此前退出结果仍无法确认。";
  else if (pending)
    introduction.textContent = resultScopeUnverified
      ? "查看此前退出的记录，不会再次退出。"
      : "查看此前退出全部设备的结果，不会再次退出。";
  else introduction.textContent = "结束本人账户在网站和小程序中的全部登录。";
  document.getElementById("state-basis").textContent =
    basis[current] || basis[pending ? "unknown" : "ready"];
  const check = button("check-login", "检查登录状态") + home;
  const resultName = resultScopeUnverified ? "此前退出结果" : "本次退出结果";
  const query = button("query", `查看${resultName}`) + home;
  switch (current) {
    case "ready":
    case "other-ready":
    case "updated-ready":
    case "long-name":
    case "avatar-failed":
    case "confirm":
      content.innerHTML = `<section class="logout-panel">${person(accounts[accountExample], accountExample === "avatar-failed")}<h2>退出全部设备</h2><p class="lead">如果希望结束其他设备上的登录，可在此统一退出。</p><div class="logout-consequences"><strong>当前登录也会退出</strong><p>网站和小程序均需重新登录后才能继续使用。</p></div><p class="logout-preserved">账户、实名资料、微信绑定和员工服务均保留。</p><div class="logout-actions">${button("confirm", "退出全部设备")}<a class="button secondary" href="${accountHref()}">返回个人账户</a></div></section>`;
      break;
    case "loading":
      content.innerHTML = result(
        "正在读取当前账户",
        "读取完成后，再展示本人资料与退出入口。",
      );
      break;
    case "read-failed":
      content.innerHTML = result(
        "暂时无法读取账户",
        "登录状态暂时无法确认，请重新加载。",
        button("reload", "重新加载") + home,
      );
      break;
    case "read-limited":
      content.innerHTML = result(
        "暂时无法读取账户",
        "请稍后再试。",
        wait("重新加载") + home,
      );
      break;
    case "read-invalid":
      content.innerHTML = result(
        "请重新登录",
        "当前登录已无法继续使用，重新登录后再办理。",
        ordinaryLogin + home,
      );
      break;
    case "read-forbidden":
      content.innerHTML = result(
        "暂时无法打开此页",
        "当前无法读取账户资料，请稍后再试。",
        home,
      );
      break;
    case "save-failed":
      content.innerHTML = result(
        "尚未发送退出请求",
        "本次退出尚未提交。暂时无法开始退出，请稍后再试。",
        button("reload", "返回退出页面") + home,
      );
      break;
    case "sending":
      content.innerHTML = result(
        "正在提交退出请求",
        "请稍候，正在等待本次退出的结果。",
        home,
      );
      break;
    case "submit-limited":
      content.innerHTML = result(
        "暂时无法提交退出请求",
        "本次请求暂未受理。请等待后返回退出页面。",
        wait("返回退出页面") + home,
      );
      break;
    case "rejected":
      content.innerHTML = result(
        "本次退出请求未获允许",
        "本次请求已被拒绝。返回个人账户后，可重新查看当前情况。",
        '<a class="button primary" href="halo-account.html">返回个人账户</a>' +
          home,
      );
      break;
    case "unknown":
    case "unknown-invalid":
      content.innerHTML = result(
        "暂时无法确认退出结果",
        "请先检查登录状态，再查看本次退出结果。暂时不要再次退出。",
        check,
      );
      break;
    case "request-mismatch":
      content.innerHTML = result(
        "请查询此前退出的结果",
        "本次请求与此前退出不一致。请先检查当前登录，再查看此前退出的结果，暂时不要再次退出。",
        check,
      );
      break;
    case "check-login":
      content.innerHTML = result(
        "正在检查登录状态",
        "检查完成后，再显示当前可用的页面。",
      );
      break;
    case "check-failed":
      content.innerHTML = result(
        "暂时无法检查登录状态",
        "请重新检查，暂时无需重复登录。",
        check,
      );
      break;
    case "check-limited":
      content.innerHTML = result(
        "暂时无法检查登录状态",
        "请稍后再试。",
        wait("重新检查") + home,
      );
      break;
    case "login-required":
    case "query-invalid":
      content.innerHTML = result(
        "请登录原账户查看结果",
        "当前登录已无法继续使用。请用发起退出的同一账户重新登录，再返回此页检查登录状态。此前退出结果仍无法确认。",
        login() + button("check-login", "检查登录状态", false) + home,
      );
      break;
    case "recovery-ready":
      content.innerHTML = `<section class="logout-panel">${person()}<h2>查看此前退出的结果</h2><p class="lead">已重新登录同一账户。此前退出是否完成，还需单独查看。</p><div class="current-login"><strong>当前的新登录继续保留</strong><p>查看结果不会再次退出，也不会结束后来建立的登录。</p></div><div class="logout-actions">${query}</div></section>`;
      break;
    case "original-valid":
      content.innerHTML = `<section class="logout-panel">${person()}<h2>查看此前退出的结果</h2><p class="lead">当前登录仍可使用，但这不能证明此前退出的结果。</p><p class="minor">查看结果只读取${resultScopeUnverified ? "此前" : "本次"}记录，不会再次退出。</p><div class="logout-actions">${query}</div></section>`;
      break;
    case "querying":
      content.innerHTML = result(
        `正在查看${resultName}`,
        "只读取此前退出的结果，不会再次退出。",
        home,
      );
      break;
    case "query-failed":
      content.innerHTML = result(
        resultScopeUnverified
          ? "暂时无法查看此前退出结果"
          : "暂时无法查看退出结果",
        "此前退出是否完成仍无法确认。可重新查看，不会再次退出。",
        query,
      );
      break;
    case "query-limited":
      content.innerHTML = result(
        resultScopeUnverified
          ? "暂时无法查看此前退出结果"
          : "暂时无法查看退出结果",
        "请稍后再查看。此前退出结果仍无法确认。",
        wait("查看结果") + home,
      );
      break;
    case "query-empty":
      content.innerHTML = result(
        `暂未查到${resultName}`,
        "目前仍无法确认此前退出是否完成。请稍后再查看，暂时不要再次退出。",
        query,
      );
      break;
    case "query-forbidden":
      content.innerHTML = result(
        resultScopeUnverified
          ? "暂时无法查看此前退出结果"
          : "暂时无法查看退出结果",
        "此前是否已退出仍无法确认。你可以先返回首页浏览公开内容。",
        home,
      );
      break;
    case "materials-login-required":
      content.innerHTML = result(
        "请登录查看当前账户",
        "当前登录已无法继续使用。可重新登录查看当前账户。此前退出结果仍无法确认，暂时无法继续查看。",
        ordinaryLogin + home,
      );
      break;
    case "materials-missing":
      content.innerHTML = result(
        `暂时无法查看${resultName}`,
        "暂时无法继续查看此前退出结果。这不表示退出失败。你仍可检查当前登录状态。",
        button("check-login", "检查登录状态") +
          '<a class="button secondary" href="halo-authentication.html">手机号登录</a>' +
          home,
      );
      break;
    case "other-account":
      pending = false;
      content.innerHTML = `<section class="logout-panel">${person(accounts["other-ready"])}<h2>当前登录的账户</h2><p class="lead">可以打开个人账户，查看本人资料。</p><div class="logout-actions"><a class="button primary" href="halo-account.html#other-account">打开个人账户</a>${home}</div></section>`;
      break;
    case "current-account":
      content.innerHTML = `<section class="logout-panel">${person()}<h2>当前登录的账户</h2><p class="lead">此前退出结果仍无法确认，暂时无法继续查看。你可以打开当前账户。</p><div class="logout-actions"><a class="button primary" href="halo-account.html">打开个人账户</a>${home}</div></section>`;
      break;
    case "mismatch-result":
      content.innerHTML = result(
        "已查到此前退出记录",
        "以下是此前那次退出的结果，不表示本次退出全部设备已完成。",
        '<a class="button primary" href="halo-account.html">打开个人账户</a>' +
          home,
        '<dl class="result-meta"><div><dt>此前退出的登录</dt><dd>3 个</dd></div><div><dt>退出时间</dt><dd>2026 年 10 月 23 日 10:30</dd></div></dl><div class="current-login"><strong>当前登录继续保留</strong><p>读取此前记录不会再次退出。</p></div>',
        true,
      );
      break;
    case "success":
    case "recovered":
    case "late-success": {
      const recovered = current !== "success";
      const extra = `<dl class="result-meta"><div><dt>本次退出的登录</dt><dd>3 个</dd></div><div><dt>退出时间</dt><dd>2026 年 10 月 23 日 10:30</dd></div></dl>${recovered ? '<div class="current-login"><strong>当前的新登录继续保留</strong><p>本次结果仅对应当时的退出，之后建立的新登录不在本次结果范围内。</p></div>' : '<p class="minor">账户、实名资料、微信绑定和员工服务均保留。</p>'}`;
      content.innerHTML = result(
        "本次全部退出已完成",
        "已结束当时网站和小程序中尚未退出的全部登录，包括发起本次操作的登录。",
        recovered
          ? '<a class="button primary" href="halo-account.html">打开个人账户</a>' +
              home
          : ordinaryLogin + home,
        extra,
        true,
      );
      break;
    }
  }
  for (const link of document.querySelectorAll('a[href^="halo-account.html"]'))
    link.href = accountHref();
  const waitButton = content.querySelector("[data-wait]");
  if (waitButton) {
    let seconds = 60;
    const label =
      current === "read-limited"
        ? "重新加载"
        : current === "submit-limited"
          ? "返回退出页面"
          : current === "check-limited"
            ? "重新检查"
            : "查看结果";
    waitTimer = setInterval(() => {
      seconds -= 1;
      waitButton.textContent = seconds ? `${seconds} 秒后${label}` : label;
      if (!seconds) endWait();
    }, 1000);
  }
  if (focus) title.focus();
  if (current === "confirm") openDialog("confirm");
}
function endWait() {
  clearInterval(waitTimer);
  const control = content.querySelector("[data-wait]");
  if (!control) return;
  control.disabled = false;
  control.dataset.action = ["read-limited", "submit-limited"].includes(current)
    ? "reload"
    : current === "check-limited"
      ? "check-login"
      : "query";
  control.textContent =
    current === "submit-limited"
      ? "返回退出页面"
      : current === "read-limited"
        ? "重新加载"
        : current === "check-limited"
          ? "重新检查"
          : "查看结果";
}
function openDialog(kind) {
  returnFocus = document.activeElement;
  dialog.dataset.kind = kind;
  document.getElementById("dialog-title").textContent =
    kind === "confirm" ? "退出全部设备？" : "离开此页？";
  document.getElementById("dialog-description").textContent =
    kind === "confirm"
      ? "当前登录也将退出。网站和小程序均需重新登录后才能继续使用。"
      : `${resultScopeUnverified ? "此前" : "本次"}退出结果仍未确定。离开不会取消已经发出的请求。`;
  document.getElementById("dialog-detail").textContent =
    kind === "confirm"
      ? "账户、实名资料、微信绑定和员工服务均保留。"
      : current === "query-forbidden"
        ? "此前退出结果仍无法确认。暂时不要再次退出。"
        : `可返回此页，检查当前登录状态后继续查看${resultScopeUnverified ? "此前" : "本次"}结果。暂时不要再次退出。`;
  document.getElementById("dialog-actions").innerHTML =
    button("cancel", kind === "confirm" ? "暂不退出" : "留在此页", false) +
    button(
      kind === "confirm" ? "submit" : "leave",
      kind === "confirm" ? "确认退出全部设备" : "离开此页",
    );
  dialog.showModal();
  dialog.querySelector('#dialog-actions [data-action="cancel"]').focus();
}
dialog.addEventListener("close", () => {
  if (!dialog.open && returnFocus?.isConnected) returnFocus.focus();
});
dialog.addEventListener("keydown", (event) => {
  if (event.key !== "Tab") return;
  const controls = [...dialog.querySelectorAll("button:not(:disabled)")];
  const first = controls[0];
  const last = controls.at(-1);
  if (event.shiftKey && document.activeElement === first) {
    event.preventDefault();
    last.focus();
  } else if (!event.shiftKey && document.activeElement === last) {
    event.preventDefault();
    first.focus();
  }
});
document.querySelector(".preview-toggle").addEventListener("click", (event) => {
  const expanded = event.currentTarget.getAttribute("aria-expanded") === "true";
  event.currentTarget.setAttribute("aria-expanded", String(!expanded));
  event.currentTarget.textContent = expanded ? "展开预览工具" : "收起预览工具";
});
picker.addEventListener("change", () => {
  if (dialog.open) dialog.close();
  show(picker.value, false);
});
document.addEventListener("click", (event) => {
  const leave = event.target.closest("a[data-leave]");
  if (leave && pending && operationVisible && !missingMaterials) {
    event.preventDefault();
    nextLocation = leave.href;
    openDialog("leave");
    return;
  }
  const action = event.target.closest("[data-action]");
  if (!action || action.disabled) return;
  switch (action.dataset.action) {
    case "confirm":
      openDialog("confirm");
      break;
    case "cancel":
      dialog.close();
      break;
    case "submit": {
      const outcome = document.getElementById("submit-outcome").value;
      dialog.close();
      if (outcome === "save-failed") show(outcome);
      else {
        show("sending");
        timer = setTimeout(() => show(outcome), 700);
      }
      break;
    }
    case "leave":
      dialog.close();
      location.href = nextLocation;
      break;
    case "reload":
      show("loading");
      timer = setTimeout(() => show(accountExample), 500);
      break;
    case "check-login": {
      const outcome = document.getElementById("login-outcome").value;
      const missing = missingMaterials;
      show("check-login");
      timer = setTimeout(
        () =>
          show(
            missing && outcome === "login-required"
              ? "materials-login-required"
              : missing &&
                  ["recovery-ready", "original-valid"].includes(outcome)
                ? "current-account"
                : outcome,
          ),
        500,
      );
      break;
    }
    case "query": {
      const outcome = document.getElementById("query-outcome").value;
      show("querying");
      timer = setTimeout(
        () =>
          show(
            resultScopeUnverified && outcome === "recovered"
              ? "mismatch-result"
              : originalValid && outcome === "recovered"
                ? "success"
                : outcome,
          ),
        600,
      );
      break;
    }
    case "end-wait":
      endWait();
      break;
  }
});
const params = new URLSearchParams(location.search);
if (
  params.getAll("resume").length === 1 &&
  params.get("resume") === "all-logout-result"
) {
  const example =
    params.getAll("example").length === 1
      ? params.get("example")
      : "same-account";
  resultScopeUnverified = example === "scope-unverified";
  show("check-login", false);
  timer = setTimeout(
    () =>
      show(
        example === "other-account"
          ? "other-account"
          : example === "material-missing"
            ? "materials-missing"
            : "recovery-ready",
      ),
    500,
  );
} else show(params.get("scenario") || "ready", false);
