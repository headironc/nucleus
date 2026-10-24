const content = document.getElementById("mobile-content");
const picker = document.getElementById("scenario-picker");
const dialog = document.getElementById("leave-dialog");
const states = {
  current: "填写当前手机号",
  "current-code": "验证当前手机号",
  new: "填写新手机号",
  "new-code": "填写新号验证码",
  confirm: "更换前明确确认",
  loading: "当前本人读取中",
  "initial-failed": "当前本人读取失败",
  "initial-limited": "当前本人读取受限",
  "initial-invalid": "进入前登录失效",
  "long-name": "长姓名与窄屏输入",
  "challenge-failed": "安全验证未通过",
  "target-changed": "填写的当前号不符",
  sending: "当前号短信仍在发送",
  "delivery-unknown": "当前号短信发送结果未知",
  "send-unknown": "当前号请求结果未知",
  "send-failed": "当前号短信明确失败",
  "send-limited": "当前号首次发码受限",
  "new-sending": "新号短信仍在发送",
  "new-delivery-unknown": "新号短信发送结果未知",
  "new-send-unknown": "新号请求结果未知",
  "new-send-failed": "新号短信明确失败",
  "new-send-limited": "新号首次发码受限",
  "send-query-limited": "发码原结果查询受限",
  "send-material-missing": "发送结果材料不可用",
  "send-mismatch": "发送原输入不符",
  establishing: "正在验证当前号",
  "establish-unknown": "当前号验证结果未知",
  "current-wrong-code": "当前号验证码错误",
  "current-required": "当前号验证码失效或耗尽",
  "same-number": "新号与当前号相同",
  "new-wrong-code": "新号验证码错误",
  "new-required": "新号验证码失效或耗尽",
  "mobile-taken": "验证码通过后新号被占用",
  "transfer-expired": "办理到期或被取代",
  "save-failed": "提交前保存失败",
  submitting: "正在提交更换",
  unknown: "更换结果未确定",
  querying: "正在查询本次更换",
  "query-failed": "更换结果查询失败",
  "query-limited": "更换结果查询受限",
  mismatch: "完成请求信息不一致",
  success: "可信完成 · 需新号登录",
  "login-required": "结果未知 · 原登录已失效",
  returned: "同账户新登录 · 可查询原办理",
  pending: "原办理仍未完成 · 原会话可继续",
  "pending-new-session": "新登录只读原未完成状态",
  expired: "原办理已不能继续",
  completed: "已查询到原完成",
  unavailable: "没有可读原结果",
  "material-missing": "原办理定位缺失",
  forbidden: "同源拒绝 · 遮蔽资料",
  "account-changed": "账户选择变化 · 遮蔽资料",
  "other-account": "另一账户 · 遮蔽原办理",
  "current-after-success-failed": "已知成功 · 当前资料读取失败",
  "known-query-failed": "已知成功 · 再次查询暂不可用",
};
for (const [value, label] of Object.entries(states))
  picker.add(new Option(label, value));
let state = "current";
let phase = "current";
let currentNumber = "";
let newNumber = "";
let code = "";
let previous = "current";
let hasTransfer = false;
let sameSession = true;
let knownComplete = false;
let leaveTarget = "";
let returnFocus;
const escape = (value) =>
  String(value).replace(
    /[&<>"']/g,
    (c) =>
      ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[
        c
      ],
  );
const action = (target, label, secondary = false, disabled = false) =>
  `<button type="button" data-go="${target}" class="${secondary ? "secondary" : ""}" ${disabled ? "disabled" : ""}>${label}</button>`;
const link = (href, label, secondary = true) =>
  `<a href="${href}" class="${secondary ? "secondary" : ""}" data-leave>${label}</a>`;
const account = (secondary = true) =>
  link("halo-account.html", "打开个人账户", secondary);
const home = () => link("halo-home.html", "返回首页");
const notice = (text, kind = "") =>
  `<div class="mobile-notice ${kind}"><p>${text}</p></div>`;
const login = () =>
  link(
    `halo-authentication.html?return=mobile-change${knownComplete ? "&known=completed" : ""}`,
    knownComplete ? "用新手机号登录" : "手机号登录",
    false,
  );
const maskedStates = new Set([
  "loading",
  "initial-failed",
  "initial-limited",
  "initial-invalid",
  "forbidden",
  "account-changed",
  "other-account",
  "login-required",
]);
const unknownStates = new Set([
  "sending",
  "new-sending",
  "delivery-unknown",
  "new-delivery-unknown",
  "establishing",
  "submitting",
  "send-unknown",
  "new-send-unknown",
  "send-query-limited",
  "send-material-missing",
  "send-mismatch",
  "establish-unknown",
  "unknown",
  "querying",
  "query-failed",
  "query-limited",
  "mismatch",
  "unavailable",
  "material-missing",
]);

function result(title, text, actions, extra = "", status = "本次办理") {
  return `<section class="mobile-panel"><span class="mobile-status">${status}</span><h2>${title}</h2><p>${text}</p>${extra}<div class="mobile-actions">${actions}</div></section>`;
}
function guide() {
  if (knownComplete)
    return `<aside class="mobile-guide"><h2>本次办理已完成</h2><p>今后请使用新手机号登录原账户。</p><p>重新登录或查看记录不会再次更换手机号，也不会改变本次完成时间。</p></aside>`;
  if (!sameSession || state === "pending-new-session" || state === "returned")
    return `<aside class="mobile-guide"><h2>本次登录只查询记录</h2><p>此前登录建立的办理，不能在本次登录中继续。</p><p>请先查看原办理结果，再决定后续操作。查询不会发送短信或再次更换手机号。</p></aside>`;
  return `<aside class="mobile-guide"><h2>更换后，用新号登录</h2><p>完成更换后，所有设备上的本账户登录都会退出。</p><p>姓名、头像、微信绑定和已有业务记录会保留。</p><hr><h2>在本次登录中完成</h2><p>当前号验证通过后，请在 10 分钟内完成。中途重新登录时，只能查询原办理。</p></aside>`;
}
function steps(index) {
  return `<ol class="mobile-steps" aria-label="办理步骤">${["1 验证当前号", "2 验证新号", "3 确认更换"].map((label, i) => `<li ${i === index ? 'aria-current="step"' : ""}>${label}</li>`).join("")}</ol>`;
}
function form(isCode = false, error = "") {
  const old = phase === "current";
  const number = old ? currentNumber : newNumber;
  const numberLabel = old ? "当前手机号" : "新手机号";
  const title = isCode
    ? `输入${old ? "当前" : "新"}手机号验证码`
    : `填写${numberLabel}`;
  const description = isCode
    ? `短信已提交发送，请查看 ${escape(number || (old ? "13800000000" : "13900000000"))} 收到的短信。受理不代表已送达。`
    : old
      ? "请输入本账户目前使用的中国大陆手机号。"
      : "请输入准备使用的新手机号。";
  const deadline = old
    ? ""
    : notice("当前手机号已验证。请在 21:50 前完成本次更换。", "success");
  return `<section class="mobile-panel">${steps(old ? 0 : 1)}<h2>${title}</h2><p>${description}</p>${deadline}
    <form id="mobile-form" novalidate autocomplete="off"><div class="mobile-field"><label for="mobile-input">${isCode ? "六位验证码" : numberLabel}</label><input id="mobile-input" ${isCode ? 'class="code" type="text" inputmode="numeric" maxlength="6"' : 'type="tel" inputmode="tel" maxlength="11"'} autocomplete="off" value="${escape(isCode ? code : number)}" placeholder="${isCode ? "请输入验证码" : "请输入 11 位手机号"}" aria-describedby="input-hint input-error" ${error ? 'aria-invalid="true"' : ""}><p id="input-hint" class="field-hint">${isCode ? "仅用于本次更换手机号，登录验证码不能用于此处。" : old ? "填写后，系统会核对是否为本账户的当前手机号。" : "请使用本人能接收短信的号码。"}</p><p id="input-error" class="field-error" ${error ? "" : "hidden"}>${error}</p></div>
    ${isCode ? "" : '<label class="mobile-challenge"><input id="challenge" type="checkbox">安全验证（设计演示）</label>'}
    <div class="mobile-actions"><button type="submit">${isCode ? (old ? "验证并继续" : "继续") : `获取${numberLabel}验证码`}</button>${isCode ? action("resend", "重新获取验证码", true) : account()}</div></form>
    ${isCode ? "" : old ? '<p class="field-hint">当前号码无法接收短信时，本人换号无法继续。请联系医院管理员。</p>' : ""}</section>`;
}
function render() {
  picker.value = state;
  const masked = maskedStates.has(state);
  document.title = masked ? "个人账户 · 司籍处" : "更换手机号 · 司籍处";
  document.getElementById("page-title").textContent = masked
    ? "个人账户"
    : "更换手机号";
  document.getElementById("page-context").textContent = masked
    ? "个人账户"
    : "个人账户 · 本人办理";
  document.getElementById("page-description").textContent = masked
    ? "请先检查当前登录，确认后再查看本人资料。"
    : "先验证当前手机号，再验证新手机号。";
  let html = "";
  if (
    [
      "current",
      "new",
      "long-name",
      "same-number",
      "target-changed",
      "challenge-failed",
    ].includes(state)
  ) {
    html += form(
      false,
      state === "same-number"
        ? "新手机号与当前手机号相同，请填写其他号码。"
        : state === "target-changed"
          ? "填写的号码与账户当前手机号不一致，请重新核对。"
          : state === "challenge-failed"
            ? "安全验证未通过，请重新完成验证。"
            : "",
    );
    if (state === "long-name")
      html = html.replace(
        '<section class="mobile-panel">',
        '<section class="mobile-panel">' +
          notice("阿依古丽·阿不都热依木 · 本人账户"),
      );
  } else if (
    [
      "current-code",
      "new-code",
      "current-wrong-code",
      "new-wrong-code",
    ].includes(state)
  ) {
    html = form(
      true,
      state.endsWith("wrong-code")
        ? "验证码不正确，请查看该号码最新收到的换号短信。"
        : "",
    );
  } else if (state === "confirm") {
    html = `<section class="mobile-panel">${steps(2)}<h2>确认更换手机号</h2><p>本账户今后将使用以下号码登录：</p><div class="mobile-number">${escape(newNumber || "13900000000")}</div><ul class="mobile-consequences"><li>完成后，所有设备上的本账户登录都会退出。</li><li>请用新手机号重新登录。姓名、头像和业务记录会保留。</li></ul>${notice("点击确认后，系统将验证新号验证码并办理更换。请在 21:50 前完成。")}<div class="mobile-actions">${action("submit", "确认更换手机号")}${action("new-code", "返回修改", true)}</div></section>`;
  } else if (
    [
      "sending",
      "new-sending",
      "delivery-unknown",
      "new-delivery-unknown",
    ].includes(state)
  ) {
    const sending = state.endsWith("sending");
    html = result(
      sending ? "短信仍在发送" : "暂时无法确认短信发送结果",
      `请先查看${phase === "current" ? "当前" : "新"}手机号收到的短信。${sending ? "请勿重复获取验证码。" : "如果已收到验证码，可以继续填写。"}`,
      action("send-query", "查看发送结果") +
        action("code-entry", "已收到，填写验证码", true),
      notice("查看结果只处理本次发送，不会另外发送短信。"),
    );
  } else if (
    ["send-unknown", "new-send-unknown", "send-query-limited"].includes(state)
  ) {
    html = result(
      "暂时无法确认验证码是否已发送",
      "请先查看手机短信。当前尚未取得可用于验证的发送结果，请勿再次获取验证码。",
      action(
        "send-query",
        state === "send-query-limited" ? "60 秒后查看发送结果" : "查看发送结果",
        false,
        state === "send-query-limited",
      ) + account(),
      notice("只查询本次发送，保持原填写内容。", "warning"),
    );
  } else if (["send-failed", "new-send-failed"].includes(state)) {
    html = result(
      "本次短信发送失败",
      "可以重新填写号码并完成安全验证，再由你获取验证码。",
      action("resend", "重新填写并验证") + account(),
    );
  } else if (["send-limited", "new-send-limited"].includes(state)) {
    html = result(
      "本次发送申请尚未受理",
      "请稍后再获取验证码。",
      action("resend", "60 秒后重新填写", false, true) + account(),
    );
  } else if (["send-material-missing", "send-mismatch"].includes(state)) {
    html = result(
      "暂时无法查看本次发送结果",
      "不表示短信没有发送。请先查看手机短信，当前页面暂时无法继续验证。",
      account(false) + home(),
      notice(
        state === "send-mismatch"
          ? "本次请求与先前填写内容不一致，已停止继续提交。"
          : "本页已无法继续查询本次短信申请。",
        "warning",
      ),
    );
  } else if (
    ["current-required", "new-required", "transfer-expired"].includes(state)
  ) {
    html = result(
      "本次验证已不能继续",
      "验证码或本次办理已不再适用，请重新验证当前手机号。",
      action("restart", "重新验证当前手机号") + account(),
    );
  } else if (state === "mobile-taken") {
    html = result(
      "新手机号已被使用",
      "本次未更换手机号。请使用其他号码，并重新获取新号验证码。",
      action("new", "填写其他新手机号") + account(),
    );
  } else if (
    ["establishing", "submitting", "querying", "loading"].includes(state)
  ) {
    const titles = {
      establishing: "正在验证当前手机号",
      submitting: "正在提交更换申请",
      querying: "正在查询本次更换",
      loading: "正在检查当前登录",
    };
    html = result(
      titles[state],
      state === "loading"
        ? "检查完成前，暂不显示本人资料。"
        : "请稍候，暂时不用重复操作。",
      action("none", "请稍候", false, true),
    );
  } else if (state === "save-failed") {
    html = result(
      "本次申请尚未提交",
      "当前页面暂时无法保存办理信息，请稍后再试。",
      action("previous", "返回填写页面") + account(),
    );
  } else if (state === "establish-unknown") {
    html = result(
      "暂时无法确认当前号验证结果",
      "请先查看本次验证结果，暂时不要修改验证码或重新获取短信。",
      action("establish-query", "查看验证结果") + account(),
    );
  } else if (["unknown", "query-failed", "query-limited"].includes(state)) {
    html = result(
      "暂时无法确认更换结果",
      "请先查询本次办理。返回个人账户或离开页面不会取消已提交的申请。",
      action(
        "query",
        state === "query-limited" ? "60 秒后查询更换结果" : "查询更换结果",
        false,
        state === "query-limited",
      ) + account(),
      notice("暂时不要重新提交更换申请。", "warning"),
    );
  } else if (state === "mismatch") {
    html = result(
      "本次填写内容与原申请不一致",
      "已停止继续提交。请查询原办理，当前填写内容不会覆盖原申请。",
      action("query", "查询更换结果") + account(),
    );
  } else if (
    [
      "success",
      "completed",
      "current-after-success-failed",
      "known-query-failed",
    ].includes(state)
  ) {
    html = result(
      state === "completed" ? "本次手机号更换已完成" : "手机号已更换",
      state === "success"
        ? "所有设备上的原登录已退出。请使用新手机号重新登录。"
        : "原办理完成于 2026 年 10 月 24 日 21:45。这个结果仅说明本次更换已经完成。",
      state === "success"
        ? login() + home()
        : state === "current-after-success-failed"
          ? action("current-read", "重新读取当前账户") + home()
          : state === "known-query-failed"
            ? action("query", "重新查询记录") + account()
            : account(false),
      state === "current-after-success-failed"
        ? notice("当前账户资料暂时无法读取。本次更换完成的结果仍然有效。")
        : state === "known-query-failed"
          ? notice("暂时无法再次读取办理记录，已确认的完成结果仍然有效。")
          : notice("账户、实名和业务记录保留。", "success"),
      "已确认完成",
    );
  } else if (state === "returned") {
    html = result(
      knownComplete ? "本次更换已完成" : "现在可以查询原办理",
      knownComplete
        ? "已重新登录原账户，可查看本次更换记录。"
        : "已重新登录原账户。登录成功不代表换号已经完成，请查询本次办理。",
      action("query", "查询更换结果") + account(),
    );
  } else if (["pending", "pending-new-session"].includes(state)) {
    html = result(
      "本次更换尚未完成",
      state === "pending"
        ? "请在 21:50 前，在本次登录中继续验证新手机号。"
        : "本次更换尚未完成。当前登录可以查询记录，无法继续此前登录建立的办理。",
      (state === "pending"
        ? action("new", "继续验证新手机号")
        : action("query", "再次查询更换结果")) + account(),
    );
  } else if (state === "expired") {
    html = result(
      "本次办理已不能继续",
      "这份办理已失效。重新更换时，请从验证当前手机号开始。",
      action("restart", "重新验证当前手机号") + account(),
    );
  } else if (["unavailable", "material-missing"].includes(state)) {
    html = result(
      knownComplete ? "本次手机号更换已完成" : "暂时无法查看本次更换结果",
      knownComplete
        ? state === "unavailable"
          ? "暂时无法读取本次更换记录，已确认的完成结果仍然有效。"
          : "本页已无法继续查询记录，已确认的完成结果仍然有效。"
        : state === "unavailable"
          ? "目前无法查到本次更换记录。暂时不能确定是否已完成。"
          : "本页已无法继续查询本次更换。暂时不能确定是否已完成。",
      (state === "unavailable" ? action("query", "重新查询") : "") +
        account(state === "unavailable") +
        home(),
      "",
      knownComplete ? "已确认完成" : "本次办理",
    );
  } else if (state === "login-required") {
    html = result(
      "请重新登录",
      "当前登录已不可用。请先重新登录，再查看本人服务。",
      link(
        `halo-authentication.html?return=mobile-change${knownComplete ? "&known=completed" : ""}`,
        "手机号登录",
        false,
      ) + home(),
      "",
      "个人账户",
    );
  } else if (state === "initial-invalid") {
    html = result(
      "请先登录",
      "登录后再管理本人账户。",
      link("halo-authentication.html", "手机号登录", false) + home(),
      "",
      "个人账户",
    );
  } else if (state === "forbidden") {
    html = result(
      "暂时无法打开账户页面",
      "本次访问未通过检查。本人资料已隐藏，请从本站入口重新打开。",
      account(false) + home(),
      "",
      "访问未完成",
    );
  } else if (["account-changed", "other-account"].includes(state)) {
    html = result(
      "请重新打开个人账户",
      "当前登录已变化。读取当前账户后，再继续使用本人服务。",
      account(false) + home(),
      "",
      "个人账户",
    );
  } else {
    html = result(
      "暂时无法读取当前账户",
      "目前无法确认当前登录，请稍后重新检查。",
      action(
        "initial-read",
        state === "initial-limited" ? "60 秒后重新检查" : "重新检查",
        false,
        state === "initial-limited",
      ) + home(),
      "",
      "个人账户",
    );
  }
  content.innerHTML = `<div class="mobile-columns">${html}${masked ? "" : guide()}</div>`;
  if (masked)
    content.querySelector(".mobile-columns").style.gridTemplateColumns =
      "minmax(0, 1fr)";
}
function show(next, focus = true) {
  if (!Object.hasOwn(states, next)) return;
  state = next;
  if (
    next.startsWith("new") ||
    ["same-number", "mobile-taken", "confirm"].includes(next)
  ) {
    phase = "new";
    hasTransfer = true;
  }
  if (next.startsWith("current") || next === "long-name") phase = "current";
  if (
    [
      "success",
      "completed",
      "current-after-success-failed",
      "known-query-failed",
    ].includes(next)
  )
    knownComplete = true;
  render();
  if (focus) document.getElementById("page-title").focus();
}
function query() {
  const outcome = document.getElementById("query-outcome").value;
  if (knownComplete && outcome !== "completed" && !maskedStates.has(outcome)) {
    show("known-query-failed");
    return;
  }
  show(outcome === "pending" && !sameSession ? "pending-new-session" : outcome);
}
function go(target) {
  if (target === "resend") {
    code = "";
    show(phase);
    return;
  }
  if (target === "code-entry" || target === "send-query") {
    show(`${phase}-code`);
    return;
  }
  if (target === "query") {
    query();
    return;
  }
  if (target === "establish-query") {
    hasTransfer = true;
    show("new");
    return;
  }
  if (target === "previous") {
    show(previous);
    return;
  }
  if (target === "restart") {
    currentNumber = "";
    newNumber = "";
    code = "";
    hasTransfer = false;
    knownComplete = false;
    sameSession = true;
    show("current");
    return;
  }
  if (target === "initial-read") {
    show("current");
    return;
  }
  if (target === "current-read") {
    location.href = "halo-account.html";
    return;
  }
  if (target === "submit") {
    const outcome = document.getElementById("submit-outcome").value;
    previous = "confirm";
    show(
      outcome === "wrong-code"
        ? "new-wrong-code"
        : outcome === "verification-required"
          ? "new-required"
          : outcome,
    );
    return;
  }
  show(target);
}
content.addEventListener("submit", (event) => {
  event.preventDefault();
  const input = document.getElementById("mobile-input");
  const value = input.value.trim();
  const codeForm = input.classList.contains("code");
  let error = "";
  if (codeForm && !/^\d{6}$/.test(value)) error = "请输入六位数字验证码。";
  if (
    !codeForm &&
    !/^(?:13[0-9]|14[01456879]|15[0-35-9]|16[2567]|17[0-8]|18[0-9]|19[0-35-9])[0-9]{8}$/.test(
      value,
    )
  )
    error = "请输入正确的中国大陆手机号。";
  if (!codeForm && !document.getElementById("challenge").checked)
    error ||= "请先完成安全验证。";
  if (error) {
    input.setAttribute("aria-invalid", "true");
    const message = document.getElementById("input-error");
    message.textContent = error;
    message.hidden = false;
    input.focus();
    return;
  }
  if (!codeForm) {
    if (phase === "current") currentNumber = value;
    else newNumber = value;
    if (phase === "new" && newNumber === currentNumber) {
      show("same-number");
      return;
    }
    previous = phase;
    const outcome = document.getElementById("send-outcome").value;
    if (outcome === "sent") show(`${phase}-code`);
    else
      show(
        phase === "new" && outcome !== "save-failed"
          ? `new-${outcome}`
          : outcome,
      );
  } else {
    code = value;
    if (phase === "new") {
      show("confirm");
      return;
    }
    previous = "current-code";
    const outcome = document.getElementById("submit-outcome").value;
    if (outcome === "success" || outcome === "mobile-taken") {
      hasTransfer = true;
      code = "";
      show("new");
    } else
      show(
        outcome === "wrong-code"
          ? "current-wrong-code"
          : outcome === "verification-required"
            ? "current-required"
            : outcome === "unknown"
              ? "establish-unknown"
              : outcome,
      );
  }
});
document.addEventListener("click", (event) => {
  const button = event.target.closest("[data-go]");
  if (button && !button.disabled) go(button.dataset.go);
  const anchor = event.target.closest("a[data-leave]");
  if (
    !anchor ||
    maskedStates.has(state) ||
    knownComplete ||
    ["expired", "mobile-taken", "send-failed", "new-send-failed"].includes(
      state,
    )
  )
    return;
  const input = document.getElementById("mobile-input");
  const dirty =
    hasTransfer ||
    unknownStates.has(state) ||
    currentNumber ||
    newNumber ||
    input?.value;
  if (!dirty) return;
  event.preventDefault();
  leaveTarget = anchor.href;
  returnFocus = anchor;
  document.getElementById("leave-title").textContent = unknownStates.has(state)
    ? "离开前，请留意办理结果"
    : hasTransfer
      ? "暂时离开换号办理？"
      : "放弃当前填写？";
  document.getElementById("leave-description").textContent = unknownStates.has(
    state,
  )
    ? "本次结果尚未确认。离开不会取消已发送的申请，返回后请先查看原结果。"
    : hasTransfer
      ? "离开不会撤销已经建立的办理，有效期仍继续计算。请在同一次登录中及时完成。"
      : "当前填写尚未提交，离开后需要重新填写。";
  dialog.showModal();
});
document.getElementById("stay-button").onclick = () => dialog.close();
document.getElementById("leave-button").onclick = () => {
  location.href = leaveTarget;
};
dialog.addEventListener("close", () => returnFocus?.focus());
picker.addEventListener("change", () => {
  sameSession = !["returned", "pending-new-session"].includes(picker.value);
  knownComplete = false;
  hasTransfer = ["pending", "confirm"].includes(picker.value);
  show(picker.value);
});
document.getElementById("theme-picker").addEventListener("change", (event) => {
  document.documentElement.dataset.theme = event.target.value;
});
document.querySelector(".preview-toggle").addEventListener("click", (event) => {
  const expanded = event.target.getAttribute("aria-expanded") === "true";
  event.target.setAttribute("aria-expanded", String(!expanded));
  document
    .querySelector(".preview-tools")
    .classList.toggle("expanded", !expanded);
});
const params = new URLSearchParams(location.search);
if (params.get("resume") === "mobile-change") {
  sameSession = false;
  knownComplete = params.get("known") === "completed";
  show(
    params.get("example") === "other-account"
      ? "other-account"
      : params.get("example") === "material-missing"
        ? "material-missing"
        : "returned",
    false,
  );
} else
  show(
    Object.hasOwn(states, location.hash.slice(1))
      ? location.hash.slice(1)
      : "current",
    false,
  );
