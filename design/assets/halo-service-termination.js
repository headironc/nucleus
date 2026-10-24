const content = document.getElementById("termination-content");
const picker = document.getElementById("scenario-picker");
const dialog = document.getElementById("termination-dialog");
const scenarios = {
  active: "正常服务 · 结束入口",
  suspended: "暂停服务 · 结束入口",
  "long-name": "长姓名 · 结束入口",
  loading: "本人及服务读取中",
  "read-failed": "初读失败",
  "read-limited": "初读暂时受限",
  "read-forbidden": "当前读取被拒绝 · 隐藏资料",
  confirm: "结束前确认",
  "save-failed": "发送前保存失败",
  sending: "正在提交",
  "submit-limited": "首次提交受限 · 尚未受理",
  unknown: "办理结果未确定",
  querying: "正在核对本次办理",
  "query-failed": "核对失败",
  "query-limited": "核对暂时受限",
  "query-forbidden": "核对被拒绝 · 隐藏受限资料",
  mismatch: "请求信息不一致",
  rejected: "明确拒绝",
  "success-loading": "原成功 · 当前服务读取中",
  "success-read-failed": "原成功 · 当前服务读取失败",
  "success-ended": "原成功 · 当前服务已结束",
  "success-rejoined": "原成功 · 已重新开通服务",
  "selection-changed": "选择变化 · 遮蔽旧资料",
  "login-required": "原账户需重新登录",
  "same-account": "同账户新登录 · 恢复入口",
  "other-account": "另一账户 · 遮蔽旧办理",
  "materials-missing": "查询材料不可用",
  "materials-expired": "查询材料已失效 · 不推断失败",
  "materials-current": "材料不可用 · 当前服务已读取",
  "materials-login": "材料不可用 · 当前需登录",
  "current-reading": "此前待确认 · 读取当前服务",
  "current-read-failed": "此前待确认 · 当前读取失败",
  "current-active": "此前待确认 · 当前已开通",
  "current-suspended": "此前待确认 · 当前已暂停",
  "current-ended": "此前待确认 · 当前已结束",
};
for (const [value, label] of Object.entries(scenarios))
  picker.add(new Option(label, value));
const button = (action, label, primary = true, disabled = false) =>
  `<button type="button" class="button ${primary ? "primary" : "secondary"}" data-action="${action}"${disabled ? " disabled" : ""}>${label}</button>`;
const link = (href, label, primary = false) =>
  `<a class="button ${primary ? "primary" : "secondary"}" href="${href}" data-leave>${label}</a>`;
const home = () => link("halo-home.html", "返回首页");
const serviceHref = () =>
  `halo-services.html#${current === "other-account" || current === "selection-changed" ? "account-changed" : current === "login-required" || current === "materials-login" ? "session-expired" : current === "success-ended" ? "terminated" : current === "success-rejoined" || current === "materials-current" ? "active" : "loading"}`;
const service = (primary = false) =>
  pendingStates.has(current)
    ? button("check-current-pending", "查看当前员工服务", primary)
    : link(serviceHref(), "查看当前员工服务", primary);
const note = (heading, text, kind = "") =>
  `<section class="termination-note ${kind}"><h3>${heading}</h3><p>${text}</p></section>`;
let current = "active";
let origin = "active";
let pendingReturn = "unknown";
let timer;
let waitTimer;
let returnFocus;
let dialogAction;
const pendingStates = new Set([
  "sending",
  "unknown",
  "querying",
  "query-failed",
  "query-limited",
  "mismatch",
  "same-account",
  "current-reading",
  "current-read-failed",
  "current-active",
  "current-suspended",
  "current-ended",
]);
function result(heading, text, actions, extra = "", status = "办理结果") {
  return `<section class="termination-panel termination-result"><span class="termination-status">${status}</span><h2>${heading}</h2><p>${text}</p>${extra}<div class="termination-actions">${actions}</div></section>`;
}
function openDialog(heading, description, label, action, cancel = "暂不结束") {
  returnFocus = document.activeElement;
  document.getElementById("dialog-title").textContent = heading;
  document.getElementById("dialog-description").textContent = description;
  document.getElementById("dialog-confirm").textContent = label;
  document.getElementById("dialog-cancel").textContent = cancel;
  dialogAction = action;
  dialog.showModal();
  document.getElementById("dialog-cancel").focus();
}
function closeDialog() {
  dialog.close();
  if (returnFocus?.isConnected) returnFocus.focus();
}
function confirmation() {
  openDialog(
    "结束员工服务？",
    "结束后，将停止工号与工牌办理。尚未发放的原申请无法继续，重新开通也不会恢复。个人账户、当前登录和已有工号保持。",
    "确认结束",
    () => {
      closeDialog();
      const outcome = document.getElementById("submit-outcome").value;
      if (outcome === "save-failed") return show(outcome);
      show("sending");
      timer = setTimeout(() => show(outcome), 800);
    },
  );
}
function show(state, focus = true) {
  clearTimeout(timer);
  clearInterval(waitTimer);
  current = Object.hasOwn(scenarios, state) ? state : "active";
  picker.value = current;
  if (["active", "suspended", "long-name"].includes(current)) origin = current;
  history.replaceState(null, "", `#${current}`);
  let html;
  const progress = (label) => button("none", label, true, true) + home();
  const check = () => button("check", "继续核对") + service();
  const unknown =
    "暂时无法确认员工服务是否已结束。请勿重复提交，可以继续核对本次办理。";
  if (["active", "suspended", "long-name", "confirm"].includes(current)) {
    html = `<div class="termination-columns"><section class="termination-panel"><div class="termination-person"><strong>${origin === "long-name" ? "阿依古丽·阿不都热依木" : "陈思远"}</strong><p>本人账户 · 当前员工服务${origin === "suspended" ? "已暂停" : "已开通"}</p></div><h2>结束后会发生什么</h2><ul class="termination-consequences"><li>停止办理工号和工牌，尚未发放的原申请无法继续。</li><li>以后重新开通服务，也不会恢复原申请或旧工牌的增办资格。</li></ul>${note("个人账户仍可使用", "当前登录、机构角色和已有工号不会因结束员工服务而改变。")}<div class="termination-actions">${button("confirm", "结束员工服务")}${link(`halo-services.html#${origin === "suspended" ? "suspended" : "active"}`, "返回员工服务")}</div></section><aside class="termination-guide"><h2>只结束本人的员工服务</h2><p>这不是注销账户，也不是退出登录。</p><p>服务已暂停时，正常账户仍可主动结束。结束后的原申请仍按原有记录保留。</p></aside></div>`;
  } else {
    switch (current) {
      case "loading":
      case "selection-changed":
        html = result(
          "正在检查登录与员工服务",
          "检查完成前暂不展示个人资料。",
          progress("正在检查"),
          "",
          "账户检查",
        );
        break;
      case "read-failed":
        html = result(
          "暂时无法读取员工服务",
          "尚不能确认当前服务状态，请重新加载。",
          button("read", "重新加载") + home(),
        );
        break;
      case "read-limited":
        html = result(
          "请稍后再读取员工服务",
          "暂时无法处理，请等待后重新加载。",
          button("read", "60 秒后重新加载", true, true) + home(),
        );
        break;
      case "read-forbidden":
      case "query-forbidden":
        html = result(
          "暂时无法使用员工服务",
          "暂时无法访问此服务，请稍后再查看。",
          home(),
          "",
          "账户状态",
        );
        break;
      case "save-failed":
        html = result(
          "尚未提交结束申请",
          "本次申请没有发出。请返回确认页面后再继续。",
          button("return-confirm", "返回确认页面") + service(),
          note(
            "保存未完成",
            "浏览器未能保存本次办理所需信息，因此没有提交申请。",
          ),
        );
        break;
      case "sending":
        html = result(
          "正在提交结束申请",
          "请稍候，不要重复提交。离开页面不表示申请已取消。",
          progress("正在提交"),
        );
        break;
      case "querying":
        html = result(
          "正在核对办理结果",
          "只核对此前提交的结束申请，不会结束后来新开通的服务。",
          progress("正在核对"),
        );
        break;
      case "unknown":
      case "query-failed":
        html = result("暂时无法确认是否已结束", unknown, check());
        break;
      case "submit-limited":
        html = result(
          "本次申请尚未受理",
          "本次结束申请没有开始办理。请等待后返回确认页面，再决定是否提交。",
          button("return-confirm", "60 秒后返回确认页面", true, true) +
            service(),
        );
        break;
      case "query-limited":
        html = result(
          "请稍后继续核对",
          "本次办理结果尚未确定。等待后继续核对，不要重新发起结束申请。",
          button("check", "60 秒后继续核对", true, true) + service(),
        );
        break;
      case "mismatch":
        html = result(
          "暂时无法查看本次办理结果",
          "核对信息与此前不一致，不能据此判断员工服务是否已结束。请先查看当前服务状态。",
          button("check-current-pending", "查看当前员工服务") + home(),
        );
        break;
      case "rejected":
        html = result(
          "本次申请未完成",
          "当前服务状态不适用于本次结束申请。请查看最新的员工服务状态。",
          service() + home(),
        );
        break;
      case "success-loading":
      case "success-read-failed":
      case "success-ended":
      case "success-rejoined": {
        let detail;
        if (current === "success-loading")
          detail =
            note(
              "正在读取当前员工服务",
              "此前办理已完成，当前状态仍在读取。确认后会在此展示。",
            ) + button("none", "正在读取", true, true);
        if (current === "success-read-failed")
          detail =
            note(
              "当前员工服务暂时无法读取",
              "此前结束办理已完成，不受本次读取失败影响。",
            ) + button("read-current", "重新读取当前状态");
        if (current === "success-ended")
          detail = note(
            "当前员工服务已结束",
            "个人账户仍可使用；已有工号保持。尚未发放的原申请无法继续，你仍可取消符合条件的原申请。",
            "success",
          );
        if (current === "success-rejoined")
          detail = note(
            "当前员工服务已重新开通",
            "你现在使用的是重新开通的服务。本次核对只确认此前的结束申请，不会结束当前服务。",
            "success",
          );
        html = result(
          "本次结束申请已完成",
          "结束时间：2026 年 10 月 24 日 09:20。",
          service(
            current === "success-ended" || current === "success-rejoined",
          ) + link("halo-account.html", "打开个人账户"),
          detail,
          "此前办理 · 已完成",
        );
        break;
      }
      case "login-required":
      case "materials-login":
        html = result(
          "请先重新登录",
          "登录后才能查看本人的员工服务。",
          link(
            "halo-authentication.html?return=service-termination",
            "手机号登录",
            true,
          ) + home(),
          "",
          "登录状态",
        );
        break;
      case "same-account":
        html = result(
          "可以继续核对此前办理",
          "已确认当前仍是本人账户。继续使用此前申请的信息核对，不会重新发起结束申请。",
          check(),
        );
        break;
      case "other-account":
        html = result(
          "当前账户已更新",
          "可以打开当前账户的员工服务或个人账户。",
          service() + link("halo-account.html#other-account", "打开个人账户"),
          "",
          "当前账户",
        );
        break;
      case "materials-current":
        html = result(
          "当前员工服务已开通",
          "这是当前服务的读取结果，不能证明此前的结束申请成功或失败。",
          service() + link("halo-account.html", "打开个人账户"),
          "",
          "当前服务",
        );
        break;
      case "current-reading":
        html = result(
          "正在读取当前员工服务",
          "此前结束申请仍未确认。当前状态不会代替本次办理结果。",
          progress("正在读取"),
          "",
          "当前服务",
        );
        break;
      case "current-read-failed":
        html = result(
          "暂时无法读取当前员工服务",
          "此前结束申请的结果仍未确定，请勿重复提交。",
          button("check-current-pending", "重新读取当前状态") +
            button("return-pending", "返回办理结果", false),
        );
        break;
      case "current-active":
      case "current-suspended":
      case "current-ended":
        html = result(
          `当前员工服务${current === "current-active" ? "已开通" : current === "current-suspended" ? "已暂停" : "已结束"}`,
          "这是当前服务状态，不能单凭它确认此前结束申请的结果。",
          button("return-pending", "返回办理结果") +
            link("halo-account.html", "打开个人账户"),
          note(
            "此前办理仍待确认",
            "继续核对只使用此前的申请，不会结束后来新开通的服务。",
            "warning",
          ),
          "当前服务",
        );
        break;
      default:
        html = result(
          "暂时无法查看本次办理结果",
          "本次办理所需的信息已不可用，不能继续核对。请先查看当前员工服务状态。",
          button("check-current", "查看当前员工服务", true) + home(),
          note(
            "不表示办理失败",
            "当前状态与此前办理结果分别确认，请勿重复提交结束申请。",
          ),
        );
    }
  }
  content.innerHTML = html;
  document.getElementById("service-navigation").href = serviceHref();
  document.getElementById("service-back").href = serviceHref();
  document.getElementById("account-navigation").href =
    `halo-account.html#${current === "other-account" ? "other-account" : current === "selection-changed" ? "account-changed" : current === "login-required" || current === "materials-login" ? "session-invalid" : "loading"}`;
  const masked = [
    "loading",
    "read-failed",
    "read-limited",
    "read-forbidden",
    "query-forbidden",
    "selection-changed",
    "login-required",
    "materials-login",
    "other-account",
  ].includes(current);
  const completed = current.startsWith("success-");
  document.getElementById("page-title").textContent = completed
    ? "此前的员工服务已结束"
    : masked
      ? "员工服务"
      : "结束员工服务";
  document.querySelector(".termination-heading .eyebrow").textContent = masked
    ? "本人服务"
    : "员工服务 · 本人办理";
  document.title = `${completed ? "此前的员工服务已结束" : masked ? "员工服务" : "结束员工服务"} · 司籍处`;
  document.getElementById("page-introduction").textContent = completed
    ? "本次结束申请已完成；当前员工服务另行读取与确认。"
    : masked
      ? "登录与本人资料检查完成后，显示当前可用的服务。"
      : ["active", "suspended", "long-name", "confirm"].includes(current)
        ? "结束前，请了解对工号和工牌办理的影响。"
        : "请按当前提示继续办理或查看状态。";
  if (focus) document.getElementById("page-title").focus();
  if (current === "success-loading")
    timer = setTimeout(
      () => show(document.getElementById("current-outcome").value),
      900,
    );
  if (["read-limited", "submit-limited", "query-limited"].includes(current)) {
    const waitButton = content.querySelector("button:disabled");
    let remaining = 60;
    const label =
      current === "read-limited"
        ? "重新加载"
        : current === "submit-limited"
          ? "返回确认页面"
          : "继续核对";
    waitTimer = setInterval(() => {
      remaining -= 1;
      waitButton.textContent =
        remaining > 0 ? `${remaining} 秒后${label}` : label;
      if (remaining <= 0) {
        clearInterval(waitTimer);
        waitButton.disabled = false;
      }
    }, 1000);
  }
  if (current === "confirm") confirmation();
}
picker.addEventListener("change", () => show(picker.value));
document.getElementById("theme-picker").addEventListener("change", (event) => {
  if (event.target.value === "system")
    delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = event.target.value;
});
document.querySelector(".preview-toggle").addEventListener("click", (event) => {
  const expanded = event.currentTarget.getAttribute("aria-expanded") === "true";
  event.currentTarget.setAttribute("aria-expanded", String(!expanded));
  document
    .querySelector(".preview-tools")
    .classList.toggle("expanded", !expanded);
});
document
  .getElementById("dialog-confirm")
  .addEventListener("click", () => dialogAction?.());
dialog.addEventListener("cancel", (event) => {
  event.preventDefault();
  closeDialog();
});
document.addEventListener("click", (event) => {
  const action = event.target.closest("[data-action]")?.dataset.action;
  if (action === "close-dialog") closeDialog();
  if (action === "confirm") confirmation();
  if (action === "return-confirm") {
    show(origin);
    confirmation();
  }
  if (action === "read") {
    show("loading");
    timer = setTimeout(() => show(origin), 700);
  }
  if (action === "check") {
    show("querying");
    timer = setTimeout(
      () => show(document.getElementById("query-outcome").value),
      800,
    );
  }
  if (action === "read-current") show("success-loading");
  if (action === "check-current") show("materials-current");
  if (action === "return-pending") show(pendingReturn);
  if (action === "check-current-pending") {
    if (!current.startsWith("current-")) pendingReturn = current;
    show("current-reading");
    const outcome = document.getElementById("current-outcome").value;
    timer = setTimeout(
      () =>
        show(
          outcome === "success-read-failed"
            ? "current-read-failed"
            : outcome === "success-ended"
              ? "current-ended"
              : "current-active",
        ),
      700,
    );
  }
  const target = event.target.closest("a[data-leave]");
  if (target && pendingStates.has(current)) {
    event.preventDefault();
    openDialog(
      "暂时离开？",
      "本次结束申请的结果还未确定。离开不会取消已提交的申请，请勿重复提交。",
      "离开页面",
      () => {
        location.href = target.href;
      },
      "继续核对",
    );
  }
});
const params = new URLSearchParams(location.search);
if (["light", "dark"].includes(params.get("theme"))) {
  document.documentElement.dataset.theme = params.get("theme");
  document.getElementById("theme-picker").value = params.get("theme");
}
const resume =
  params.getAll("resume").length === 1 &&
  params.get("resume") === "service-termination";
const resumeState =
  {
    "same-account": "same-account",
    "other-account": "other-account",
    "material-missing": "materials-missing",
  }[params.get("example")] || "login-required";
show(resume ? resumeState : location.hash.slice(1) || "active", false);
