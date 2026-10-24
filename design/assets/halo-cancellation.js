const picker = document.getElementById("scenario-picker");
const content = document.getElementById("cancellation-content");
const pageTitle = document.getElementById("page-title");
const pageIntroduction = document.getElementById("page-introduction");
const pageKicker = document.getElementById("page-kicker");
const stateBasis = document.getElementById("state-basis");
const accountNavigation = document.getElementById("account-navigation");
const servicesNavigation = document.getElementById("services-navigation");
const accountBack = document.getElementById("account-back");
const previewToggle = document.querySelector(".preview-toggle");
const previewContent = document.getElementById("preview-tools-content");
const submitOutcome = document.getElementById("submit-outcome");
const currentOutcome = document.getElementById("current-outcome");
const themePicker = document.getElementById("theme-picker");
const confirmDialog = document.getElementById("confirm-dialog");
const confirmIdentity = document.getElementById("confirm-identity");
const confirmCancel = document.getElementById("confirm-cancel");
const leaveDialog = document.getElementById("leave-dialog");
const leaveTitle = document.getElementById("leave-title");
const leaveDescription = document.getElementById("leave-description");
const leaveStay = document.getElementById("leave-stay");

const fixtures = {
  primary: { name: "陈思远", suffix: "0028" },
  longName: { name: "阿依古丽·阿不都热依木", suffix: "0007" },
  updated: { name: "陈思宁", suffix: "0007" },
  other: { name: "林知夏", suffix: "0000" },
};

const receipt = {
  code: "ok",
  cancellation: {
    id: "8a33e9c7-4e51-4f6e-9c69-0a4e1f9a7d20",
    cancelledAt: "2026-10-24T01:20:00.000Z",
  },
};

const entryBasis =
  "入口先以 GET /api/v1/accounts/current 可信读取当前账户，只展示完整姓名和身份证尾四位；目标确认由 accountId、personId 与 revisionId 组成，不显示或补造手机号。";
const readBasis =
  "当前账户读取失败、受限、401、403 与选择变化分别处理。未取得可信本人资料前不展示旧资料，也不提交注销。401 不猜测具体原因。";
const submitBasis =
  "POST /api/v1/accounts/current/cancellations 只在可信本人确认后发出。首次 429 发生在业务执行前；409 target_changed 要求重新读取并再次确认，不自动更换目标。";
const successBasis =
  "只有可信 200、code ok、cancellation.id 与 cancelledAt 同时成立才展示成功。页面展示 cancelledAt；不因请求已发出或登录失效推断成功。";
const unknownBasis =
  "网络中断、超时、outcome_unknown 或不可信响应进入结果未知。停止自动重发，不提供注销结果查询；当前登录检查只读取账户，不能替代原办理结果。";
const pendingLeaveBasis =
  "提交中或结果未知时离开不取消已发出的注销申请；当前结果仍无法确认。";
const currentBasis =
  "检查当前登录只重新读取 accounts/current，不重提注销，也不取得注销历史读取权。选择变化和另一账户先隐藏旧资料与原注销上下文。";

const groups = [
  [
    "入口与确认",
    [
      "entry",
      "entry-long-name",
      "entry-updated",
      "entry-other",
      "confirm",
    ],
  ],
  [
    "检查当前账户",
    [
      "entry-loading",
      "entry-read-failed",
      "entry-limited",
      "entry-session-invalid",
      "entry-forbidden",
      "entry-selection-changed",
    ],
  ],
  [
    "提交结果",
    [
      "save-failed",
      "submitting",
      "success",
      "not-accepted",
      "submit-invalid",
      "submit-session-invalid",
      "submit-forbidden",
      "target-changed",
      "dependency-failed",
    ],
  ],
  ["结果待确认", ["unknown", "request-mismatch"]],
  [
    "检查登录状态",
    [
      "current-checking",
      "current-valid",
      "current-read-failed",
      "current-limited",
      "current-session-invalid",
      "current-no-selection",
      "current-selection-unavailable",
      "current-forbidden",
      "current-selection-changed",
      "current-other",
    ],
  ],
];

const possibilities = {
  "same-account": "current-valid",
  "read-failed": "current-read-failed",
  limited: "current-limited",
  "session-invalid": "current-session-invalid",
  "no-selection": "current-no-selection",
  "selection-unavailable": "current-selection-unavailable",
  forbidden: "current-forbidden",
  "selection-changed": "current-selection-changed",
  "other-account": "current-other",
};

const scenarios = {
  entry: {
    label: "本人资料 · 明确确认",
    kind: "entry",
    fixture: "primary",
    preSubmit: true,
    basis: entryBasis,
  },
  "entry-long-name": {
    label: "长姓名 · 明确确认",
    kind: "entry",
    fixture: "longName",
    preSubmit: true,
    basis: entryBasis,
  },
  "entry-updated": {
    label: "资料更新后 · 明确确认",
    kind: "entry",
    fixture: "updated",
    preSubmit: true,
    notice: "已重新读取更新后的账户资料，请再次核对。",
    basis: entryBasis,
  },
  "entry-other": {
    label: "另一示例账户 · 林知夏",
    kind: "entry",
    fixture: "other",
    preSubmit: true,
    basis: entryBasis,
  },
  confirm: {
    label: "确认注销弹窗",
    kind: "entry",
    fixture: "primary",
    preSubmit: true,
    openConfirm: true,
    basis: entryBasis,
  },
  "entry-loading": {
    label: "检查当前账户中",
    kind: "message",
    busy: true,
    title: "正在检查当前账户",
    description: "检查完成前不展示本人资料，也不会提交注销。",
    pageIntro: "正在检查当前账户，检查完成前不展示本人资料。",
    basis: readBasis,
    actions: [],
  },
  "entry-read-failed": {
    label: "当前账户读取失败",
    kind: "message",
    status: "账户检查",
    title: "暂时无法读取账户",
    description: "请重新加载后再办理注销。未取得可信资料前，不会提交注销。",
    pageIntro: "当前账户暂时无法读取，本次注销尚未提交。",
    basis: readBasis,
    actions: [
      { action: "retry-entry", label: "重新加载", primary: true },
      { href: "home", label: "返回首页" },
    ],
  },
  "entry-limited": {
    label: "当前账户读取受限",
    kind: "message",
    status: "账户检查",
    title: "请稍后再读取账户",
    description: "暂时无法处理，本次注销尚未提交。",
    pageIntro: "当前账户暂时无法读取，本次注销尚未提交。",
    limited: true,
    basis: readBasis,
    actions: [
      {
        action: "retry-entry",
        label: "等待后重新加载",
        waitLabel: "重新加载",
        primary: true,
        wait: true,
      },
      { href: "home", label: "返回首页" },
    ],
  },
  "entry-session-invalid": {
    label: "当前登录失效",
    kind: "message",
    status: "账户检查",
    title: "请重新登录",
    description: "当前登录已无法继续使用，本次注销尚未提交。",
    pageIntro: "当前登录已失效，本次注销尚未提交。",
    basis: readBasis,
    actions: [
      { href: "auth", label: "手机号登录", primary: true },
      { href: "home", label: "返回首页" },
    ],
  },
  "entry-forbidden": {
    label: "当前读取被拒绝",
    kind: "message",
    status: "账户检查",
    title: "暂时无法办理注销",
    description: "请稍后再试。本次注销尚未提交。",
    pageIntro: "暂时无法读取账户，本次注销尚未提交。",
    basis: readBasis,
    actions: [{ href: "home", label: "返回首页" }],
  },
  "entry-selection-changed": {
    label: "登录选择已变化",
    kind: "message",
    status: "账户检查",
    title: "登录状态已变化",
    description: "请重新检查当前账户。此前资料已隐藏，本次注销尚未提交。",
    pageIntro: "登录选择已变化，本次注销尚未提交。",
    fixture: "other",
    basis: readBasis,
    actions: [
      { action: "retry-entry", label: "重新检查当前账户", primary: true },
      { href: "home", label: "返回首页" },
    ],
  },
  "save-failed": {
    label: "发送前保存失败 · 未提交",
    kind: "message",
    status: "未提交",
    title: "本次尚未提交注销",
    description: "浏览器未能保存本次办理所需的信息，注销申请没有发出。",
    pageIntro: "本次注销尚未提交。",
    basis:
      "发送前保存失败时不得发出 POST。页面明确未提交，返回确认页后仍须由本人再次确认。",
    detail: () =>
      '<p class="cancellation-guidance">账户没有因此注销。返回确认页面后，可以重新核对并决定是否再次提交。</p>',
    actions: [
      { action: "return-entry", label: "返回确认页面", primary: true },
      { href: "home", label: "返回首页" },
    ],
  },
  submitting: {
    label: "提交中",
    kind: "message",
    status: "提交中",
    busy: true,
    title: "正在提交注销申请",
    description: "请稍候，不要重复提交。离开页面不会取消已经发出的申请。",
    pageIntro: "正在提交注销申请。",
    pending: true,
    basis:
      "提交中不提供第二次提交。本地等待结束不证明服务端已回滚；离开不取消已发出的申请。",
    actions: [
      { action: "none", label: "正在注销", primary: true, disabled: true },
      { href: "home", label: "返回首页" },
    ],
  },
  success: {
    label: "可信成功",
    kind: "message",
    status: "已完成",
    statusKind: "success",
    success: true,
    title: "账户已注销",
    description: "本次注销已经生效。",
    pageIntro: "本次注销已完成。",
    basis: successBasis,
    detail: () => {
      const time = formatTime(receipt.cancellation.cancelledAt);
      return `<p class="cancellation-time">注销时间：<time datetime="${receipt.cancellation.cancelledAt}">${time}</time></p>
        <ul class="result-consequences">
          <li>全部登录和本人服务已结束，账户不能恢复。</li>
          <li>必要业务记录和已保存的发放仍会保留；原工号不会自动释放。</li>
          <li>同一手机号重新注册是新账户，不继承原资格和历史。</li>
        </ul>`;
    },
    actions: [{ href: "home", label: "返回首页", primary: true }],
  },
  "not-accepted": {
    label: "首次受限 · 未受理",
    kind: "message",
    status: "未受理",
    title: "本次注销尚未受理",
    description: "本次注销没有开始办理，账户没有因此注销。",
    pageIntro: "本次注销尚未受理。",
    limited: true,
    basis:
      "首次 POST 的 429 rate_limited 发生在业务执行前，不产生注销事实或部分后果。遵守 Retry-After；等待后可返回确认页面，由本人明确确认后重新办理。",
    actions: [
      {
        action: "return-entry",
        label: "等待后可返回确认页面",
        waitLabel: "返回确认页面",
        primary: true,
        wait: true,
      },
      { href: "home", label: "返回首页" },
    ],
  },
  "submit-invalid": {
    label: "确认信息未被接受",
    kind: "message",
    status: "未办理",
    title: "本次注销未能办理",
    description: "提交的确认信息未被接受，本次注销没有办理。",
    pageIntro: "本次注销未能办理。",
    basis:
      "422 invalid_input 拒绝本次请求。重新读取当前账户后仍须本人再次确认；不自动重试。",
    actions: [
      { action: "retry-entry", label: "重新查看账户资料", primary: true },
      { href: "home", label: "返回首页" },
    ],
  },
  "submit-session-invalid": {
    label: "首次登录失效 · 未受理",
    kind: "message",
    status: "未受理",
    title: "当前登录已失效",
    description: "本次请求未被受理。请重新登录后再查看当前账户。",
    pageIntro: "当前登录已失效，本次请求未被受理。",
    basis:
      "首次 POST 的 401 只拒绝本次请求；此前没有未知结果。不猜测失效原因，也不据此宣称注销完成或失败。",
    actions: [
      { href: "auth", label: "手机号登录", primary: true },
      { href: "home", label: "返回首页" },
    ],
  },
  "submit-forbidden": {
    label: "首次请求被拒绝 · 未受理",
    kind: "message",
    status: "未办理",
    title: "本次注销未能办理",
    description: "暂时无法办理，请稍后再试。本次请求未被受理。",
    pageIntro: "本次注销未能办理。",
    basis:
      "首次 POST 的 403 permission_denied 拒绝本次请求，不产生注销事实。不推测账户暂停或注销原因。",
    actions: [{ href: "home", label: "返回首页" }],
  },
  "target-changed": {
    label: "账户资料已变化",
    kind: "message",
    status: "需要重新确认",
    title: "账户资料已变化",
    description: "本次注销没有办理。请重新查看当前账户资料，再确认是否注销。",
    pageIntro: "账户资料已变化，需要重新确认。",
    basis:
      "409 target_changed 表示确认资料与当前目标不一致。先遮蔽旧确认，重新读取后再由本人确认；不自动更换注销对象。",
    actions: [
      { action: "reread-updated", label: "重新查看账户资料", primary: true },
      { href: "home", label: "返回首页" },
    ],
  },
  "dependency-failed": {
    label: "已知依赖失败 · 未完成",
    kind: "message",
    status: "未完成",
    title: "本次注销没有完成",
    description: "系统暂时无法处理，本次没有产生注销结果。",
    pageIntro: "本次注销没有完成。",
    basis:
      "可信 503 dependency_unavailable 表示本次事务未完成、没有注销事实；不是结果未知。不会自动重试，新的办理须重新读取并明确确认。",
    detail: () =>
      '<p class="cancellation-guidance">可以稍后重新查看账户资料，再决定是否重新办理。</p>',
    actions: [
      { action: "retry-entry", label: "重新查看账户资料", primary: true },
      { href: "home", label: "返回首页" },
    ],
  },
  unknown: {
    label: "结果未知",
    kind: "message",
    status: "结果待确认",
    title: "暂时无法确认注销结果",
    description: "注销申请可能已经发出，但现在无法确认账户是否已注销。",
    pageIntro: "本次注销结果尚无法确认。",
    pending: true,
    basis: unknownBasis + pendingLeaveBasis,
    detail: () =>
      '<p class="cancellation-guidance">重新登录或使用同一手机号注册，也不能证明本次注销结果；新账户不承接本次注销。</p>',
    note: {
      kind: "warning",
      title: "登录失效不代表注销成功",
      text: "页面不会自动重复提交，也不提供注销结果查询。请先检查当前登录状态。",
    },
    actions: [
      { action: "check-login", label: "检查登录状态", primary: true },
      { href: "home", label: "返回首页" },
    ],
  },
  "request-mismatch": {
    label: "与此前请求不一致",
    kind: "message",
    status: "结果待确认",
    title: "暂时无法确认注销结果",
    description: "本次提交与已保存的信息不一致，当前无法确认账户是否已注销。",
    pageIntro: "本次注销结果尚无法确认。",
    pending: true,
    basis:
      "409 request_mismatch 不证明本次注销失败或首次未受理。保留结果未知，停止重提，不提供注销结果查询；检查当前登录不替代原结果。",
    note: {
      kind: "warning",
      title: "请勿重复提交",
      text: "登录失效也不代表注销成功，请先检查当前登录状态。",
    },
    actions: [
      { action: "check-login", label: "检查登录状态", primary: true },
      { href: "home", label: "返回首页" },
    ],
  },
  "current-checking": {
    label: "正在检查当前登录",
    kind: "message",
    busy: true,
    title: "正在检查当前登录",
    description: "检查只读取当前账户，不会重新提交注销。",
    pageTitle: "登录状态",
    pageIntro: "正在检查当前登录，不会重新提交注销。",
    pending: true,
    basis: currentBasis,
    actions: [
      { action: "none", label: "正在检查", primary: true, disabled: true },
      { href: "home", label: "返回首页" },
    ],
  },
  "current-valid": {
    label: "同一账户仍有效",
    kind: "message",
    status: "登录检查",
    title: "当前登录仍有效",
    description: "当前登录账户仍可使用。这不能单独确认本次注销结果。",
    pageTitle: "登录状态",
    pageIntro: "当前登录仍有效，注销结果仍待确认。",
    pending: true,
    basis: currentBasis,
    detail: () => currentAccountMarkup(),
    note: {
      title: "不能单独确认注销结果",
      text: "当前登录可用不能证明本次注销已经完成或没有完成。请勿重复提交注销；本页不提供注销结果查询。",
    },
    actions: [
      { action: "check-login", label: "重新检查登录状态", primary: true },
      { href: "home", label: "返回首页" },
    ],
  },
  "current-read-failed": {
    label: "检查登录读取失败",
    kind: "message",
    status: "登录检查",
    title: "暂时无法检查当前登录",
    description: "本次检查没有完成，注销结果仍待确认。",
    pageTitle: "登录状态",
    pageIntro: "当前登录暂时无法检查，注销结果仍待确认。",
    pending: true,
    basis: currentBasis,
    actions: [
      { action: "check-login", label: "重新检查登录状态", primary: true },
      { href: "home", label: "返回首页" },
    ],
  },
  "current-limited": {
    label: "检查登录暂时受限",
    kind: "message",
    status: "登录检查",
    title: "请稍后再检查登录",
    description: "暂时无法处理，注销结果仍待确认。",
    pageTitle: "登录状态",
    pageIntro: "当前登录暂时无法检查，注销结果仍待确认。",
    pending: true,
    limited: true,
    basis: currentBasis,
    actions: [
      {
        action: "check-login",
        label: "等待后检查登录状态",
        waitLabel: "检查登录状态",
        primary: true,
        wait: true,
      },
      { href: "home", label: "返回首页" },
    ],
  },
  "current-session-invalid": {
    label: "登录失效 · 失权",
    kind: "message",
    status: "登录检查",
    title: "当前登录已无法使用",
    description: "登录失效不代表注销成功，也不能确认本次注销结果。",
    pageTitle: "登录状态",
    pageIntro: "当前登录已失效，注销结果仍待确认。",
    pending: true,
    basis: currentBasis,
    note: {
      kind: "warning",
      title: "请勿用登录结果判断注销",
      text: "重新登录只检查当前账户，不会继续此前的注销。新注册或另一账户不承接本次注销。",
    },
    actions: [
      { href: "auth", label: "手机号登录", primary: true },
      { href: "home", label: "返回首页" },
    ],
  },
  "current-forbidden": {
    label: "检查登录被拒绝",
    kind: "message",
    status: "登录检查",
    title: "暂时无法检查当前登录",
    description: "请稍后再试。注销结果仍待确认。",
    pageTitle: "登录状态",
    pageIntro: "当前登录暂时无法检查，注销结果仍待确认。",
    pending: true,
    basis: currentBasis,
    actions: [{ href: "home", label: "返回首页" }],
  },
  "current-no-selection": {
    label: "没有当前登录",
    kind: "message",
    status: "登录检查",
    title: "需要登录",
    description: "当前没有可用的登录。登录后可以查看当前账户，但不能据此确认此前的注销结果。",
    pageTitle: "登录状态",
    pageIntro: "注销结果仍待确认。",
    pending: true,
    basis: currentBasis,
    actions: [
      { href: "auth", label: "手机号登录", primary: true },
      { href: "home", label: "返回首页" },
    ],
  },
  "current-selection-unavailable": {
    label: "登录选择存储不可用或损坏",
    kind: "message",
    status: "登录检查",
    title: "暂时无法读取登录信息",
    description: "浏览器中的登录信息无法使用。可以重新检查，或选择手机号登录。此前的注销结果仍无法确认。",
    pageTitle: "登录状态",
    pageIntro: "当前账户资料已隐藏。",
    pending: true,
    basis: currentBasis + "已确认选择存储不可用或损坏时，重新检查为主动作，显式手机号登录为次动作；普通技术读取失败不自动登录。",
    actions: [
      { action: "check-login", label: "重新检查登录状态", primary: true },
      { href: "auth", label: "手机号登录" },
      { href: "home", label: "返回首页" },
    ],
  },
  "current-selection-changed": {
    label: "登录选择变化 · 遮蔽旧内容",
    kind: "message",
    status: "当前账户",
    title: "登录状态已变化",
    description: "请重新检查当前账户。此前页面内容已隐藏。",
    pageTitle: "账户状态",
    pageIntro: "当前登录状态已变化。",
    pageKicker: "当前账户",
    neutral: true,
    fixture: "other",
    basis: currentBasis,
    actions: [
      { action: "check-login", label: "重新检查当前登录", primary: true },
      { href: "home", label: "返回首页" },
    ],
  },
  "current-other": {
    label: "另一账户 · 遮蔽旧办理",
    kind: "message",
    status: "当前账户",
    title: "已切换到另一账户",
    description: "可以打开个人账户继续查看。",
    pageTitle: "账户状态",
    pageIntro: "当前登录账户已更新。",
    pageKicker: "当前账户",
    fixture: "other",
    neutral: true,
    basis: currentBasis,
    actions: [
      { href: "account", label: "打开个人账户", primary: true },
      { href: "home", label: "返回首页" },
    ],
  },
};

const accountTargets = {
  primary: "halo-account.html",
  longName: "halo-account.html#long-name",
  updated: "halo-account.html#updated",
  other: "halo-account.html#other-account",
};

const accountFallbacks = {
  "entry-read-failed": "unavailable",
  "entry-limited": "limited",
  "entry-session-invalid": "session-invalid",
  "entry-forbidden": "forbidden",
  "entry-selection-changed": "account-changed",
  "submit-session-invalid": "session-invalid",
  "submit-forbidden": "forbidden",
  "target-changed": "profile-changed",
  "current-read-failed": "unavailable",
  "current-limited": "limited",
  "current-session-invalid": "session-invalid",
  "current-forbidden": "forbidden",
  "current-selection-changed": "account-changed",
};

const dateFormat = new Intl.DateTimeFormat("zh-CN", {
  dateStyle: "long",
  timeStyle: "short",
  timeZone: "Asia/Shanghai",
});

let current = "entry";
let fixture = "primary";
let readTimer;
let checkTimer;
let submitTimer;
let waitTimer;
let returnFocusConfirm;
let returnFocusLeave;
let leaveDestination;
let originalHidden = false;

function formatTime(value) {
  return dateFormat.format(new Date(value));
}

function accountHref() {
  const fallback = accountFallbacks[current];
  return fallback
    ? `halo-account.html#${fallback}`
    : accountTargets[fixture] || accountTargets.primary;
}

function clearTimers() {
  clearTimeout(readTimer);
  clearTimeout(checkTimer);
  clearTimeout(submitTimer);
  clearInterval(waitTimer);
  readTimer = undefined;
  checkTimer = undefined;
  submitTimer = undefined;
  waitTimer = undefined;
}

function entryState() {
  return (
    {
      longName: "entry-long-name",
      updated: "entry-updated",
      other: "entry-other",
    }[fixture] || "entry"
  );
}

function currentAccountMarkup() {
  const account = fixtures[fixture];
  return `<div class="current-account"><span>当前登录</span><strong>${account.name}</strong><span>身份证尾号 <span class="suffix">${account.suffix}</span></span></div>`;
}

function actionMarkup(item) {
  if (item.href) {
    const target =
      item.href === "account"
        ? accountHref()
        : item.href === "auth"
          ? "halo-authentication.html"
          : "halo-home.html";
    return `<a class="button ${item.primary ? "primary" : "secondary"}" href="${target}" data-leave>${item.label}</a>`;
  }
  const wait = item.wait
    ? ` data-wait data-wait-label="${item.waitLabel || item.label}"`
    : "";
  return `<button type="button" class="button ${item.primary ? "primary" : "secondary"}" data-action="${item.action}"${wait}${item.disabled ? " disabled" : ""}>${item.label}</button>`;
}

function actionsMarkup(actions = []) {
  if (!actions.length) return "";
  return `<div class="cancellation-actions">${actions.map(actionMarkup).join("")}</div>`;
}

function entryMarkup(state) {
  const account = fixtures[fixture];
  return `<div class="cancellation-columns">
    <section class="cancellation-panel" aria-labelledby="consequences-title">
      ${state.notice ? `<div class="cancellation-notice">${state.notice}</div>` : ""}
      <div class="identity-block">
        <p class="section-kicker">将要注销的账户</p>
        <h2 class="identity-name">${account.name}</h2>
        <p class="identity-meta">本人账户 · 身份证尾号 <span class="suffix">${account.suffix}</span></p>
      </div>
      <h2 id="consequences-title">注销后会发生什么</h2>
      <ul class="cancellation-consequences">
        <li>账户不能恢复，当前及所有设备的登录都会结束。</li>
        <li>本人服务会结束；尚未办理完的原申请停止办理。</li>
        <li>必要业务记录和已保存的发放仍会保留，原工号不会自动释放。</li>
      </ul>
      <div class="cancellation-note warning">
        <h3>重新注册不会恢复原账户</h3>
        <p>同一手机号重新注册会是新账户，不继承原账户的资格、记录或员工服务。</p>
      </div>
      ${actionsMarkup(state.actions || [
        { action: "request-confirm", label: "注销账户", primary: true },
        { href: "account", label: "返回个人账户" },
      ])}
    </section>
    <aside class="cancellation-guide" aria-labelledby="guide-title">
      <p class="section-kicker">注销前请确认</p>
      <h2 id="guide-title">这不是退出登录</h2>
      <p>如果只是暂时离开，可以返回个人账户，选择退出登录。注销会永久结束账户和本人服务，申请发出后不能撤回。</p>
      <ul>
        <li>只注销当前登录的本人账户。</li>
        <li>结果未知时，不要重复提交。</li>
      </ul>
    </aside>
  </div>`;
}

function messageMarkup(state) {
  const note = state.note
    ? `<div class="cancellation-note ${state.note.kind || ""}"><h3>${state.note.title}</h3><p>${state.note.text}</p></div>`
    : "";
  return `<section class="cancellation-panel cancellation-result${state.success ? " is-success" : ""}" aria-labelledby="result-title"${state.busy ? ' aria-busy="true"' : ""}>
    ${state.busy ? '<span class="reading-symbol" aria-hidden="true"></span>' : ""}
    ${state.status ? `<span class="result-status ${state.statusKind || ""}">${state.status}</span>` : ""}
    <h2 id="result-title">${state.title}</h2>
    <p class="stage-description">${state.description}</p>
    ${state.detail ? state.detail() : ""}
    ${note}
    ${actionsMarkup(state.actions)}
  </section>`;
}

function chrome(state) {
  if (state.neutral)
    return {
      kicker: state.pageKicker || "当前账户",
      title: state.pageTitle || "账户状态",
      introduction: state.pageIntro || "当前登录状态已更新。",
    };
  if (state.kind === "entry")
    return {
      kicker: "账户管理 · 仅限本人",
      title: "注销账户",
      introduction: "注销前，请核对当前账户并了解后果。",
    };
  if (state.success)
    return {
      kicker: "账户管理",
      title: "账户已注销",
      introduction: "本次注销已完成。",
    };
  if (state.group === "检查登录状态" || current.startsWith("current-"))
    return {
      kicker: "注销结果 · 检查登录",
      title: state.pageTitle || "登录状态",
      introduction:
        state.pageIntro || "检查只读取当前登录，不会重新提交注销。",
    };
  if (state.pending)
    return {
      kicker: "账户管理",
      title: state.pageTitle || "注销结果",
      introduction:
        state.pageIntro || "本次注销结果尚无法确认。",
    };
  return {
    kicker: "账户管理",
    title: state.pageTitle || "注销账户",
    introduction: state.pageIntro || "请按当前提示继续。",
  };
}

function beginWait(seconds = 60) {
  const buttons = [...content.querySelectorAll("[data-wait]")];
  if (!buttons.length) return;
  let remaining = seconds;
  const paint = () => {
    for (const button of buttons) {
      button.disabled = remaining > 0;
      button.textContent =
        remaining > 0
          ? `${remaining} 秒后${button.dataset.waitLabel}`
          : button.dataset.waitLabel;
    }
    if (remaining <= 0) {
      clearInterval(waitTimer);
      waitTimer = undefined;
    }
  };
  paint();
  waitTimer = setInterval(() => {
    remaining -= 1;
    paint();
  }, 1000);
}

function finishWait() {
  clearInterval(waitTimer);
  waitTimer = undefined;
  for (const button of content.querySelectorAll("[data-wait]")) {
    button.disabled = false;
    button.textContent = button.dataset.waitLabel;
  }
}

function closeConfirm(restore = true) {
  if (confirmDialog.open) confirmDialog.close();
  if (restore && returnFocusConfirm?.isConnected) returnFocusConfirm.focus();
}

function openConfirm() {
  if (confirmDialog.open) return;
  const account = fixtures[fixture];
  confirmIdentity.textContent = `${account.name} · 身份证尾号 ${account.suffix}`;
  returnFocusConfirm = document.activeElement;
  confirmDialog.showModal();
  confirmCancel.focus();
}

function closeLeave() {
  if (leaveDialog.open) leaveDialog.close();
  if (returnFocusLeave?.isConnected) returnFocusLeave.focus();
}

function show(stateName, options = {}) {
  clearTimers();
  if (confirmDialog.open) confirmDialog.close();
  if (leaveDialog.open) leaveDialog.close();
  current = Object.hasOwn(scenarios, stateName) ? stateName : "entry";
  let state = scenarios[current];
  if (state.kind === "entry") originalHidden = false;
  if (state.neutral) originalHidden = true;
  if (originalHidden && current.startsWith("current-") && !state.neutral) {
    if (current === "current-valid") {
      current = "current-other";
      state = scenarios[current];
    } else {
      state = {
        ...state,
        neutral: true,
        pending: false,
        pageTitle: "账户状态",
        pageKicker: "当前账户",
        pageIntro: "此前页面内容已隐藏。",
        description: {
          "current-checking": "正在检查当前选中的账户。",
          "current-read-failed": "本次检查没有完成，请重新检查。",
          "current-limited": "暂时无法处理，请等待后重新检查。",
          "current-session-invalid": "当前登录已无法使用，请重新登录。",
          "current-no-selection": "当前没有可用的登录，请选择手机号登录。",
          "current-selection-unavailable": "浏览器中的登录信息无法使用。可以重新检查，或选择手机号登录。",
          "current-forbidden": "暂时无法检查当前账户，请稍后再试。",
        }[current],
        note: undefined,
        detail: undefined,
      };
    }
  }
  if (state.fixture) fixture = state.fixture;
  picker.value = current;
  stateBasis.textContent = state.basis;
  const labels = chrome(state);
  pageKicker.textContent = labels.kicker;
  pageTitle.textContent = labels.title;
  pageIntroduction.textContent = labels.introduction;
  document.title = `${labels.title} · 司籍处`;
  accountNavigation.href = accountHref();
  accountBack.href = accountHref();
  accountNavigation.hidden = state.kind !== "entry";
  accountBack.hidden = state.kind !== "entry";
  servicesNavigation.hidden = state.kind !== "entry";
  content.innerHTML =
    state.kind === "entry" ? entryMarkup(state) : messageMarkup(state);
  if (state.limited) beginWait(60);
  if (state.openConfirm) {
    openConfirm();
  } else if (options.focus !== false) {
    pageTitle.focus();
  }
}

function loadEntry(stateName) {
  show("entry-loading");
  readTimer = window.setTimeout(() => show(stateName), 650);
}

function beginCurrentCheck() {
  show("current-checking");
  const result = possibilities[currentOutcome.value] || "current-valid";
  checkTimer = window.setTimeout(() => show(result), 700);
}

function completeSubmission() {
  const outcome = submitOutcome.value;
  confirmDialog.close();
  if (outcome === "save-failed") {
    show("save-failed");
    return;
  }
  show("submitting");
  submitTimer = window.setTimeout(
    () => show(Object.hasOwn(scenarios, outcome) ? outcome : "unknown"),
    900,
  );
}

function requestLeave(trigger, destination) {
  const state = scenarios[current];
  const pending = !originalHidden && Boolean(state.pending);
  leaveDestination = destination;
  returnFocusLeave = trigger;
  leaveTitle.textContent = pending ? "暂时离开？" : "离开注销？";
  leaveDescription.textContent = pending
    ? "注销申请可能已经发出。离开此页不会取消申请，当前结果仍无法确认。"
    : "本次注销尚未提交。离开此页不会注销账户。";
  leaveStay.textContent = pending ? "留在此页" : "继续办理";
  leaveDialog.showModal();
  leaveStay.focus();
}

previewToggle.addEventListener("click", () => {
  const expanded = previewToggle.getAttribute("aria-expanded") === "true";
  previewToggle.setAttribute("aria-expanded", String(!expanded));
  previewToggle.textContent = expanded ? "展开预览工具" : "收起预览工具";
  previewContent.hidden = expanded;
});

for (const [label, names] of groups) {
  const group = document.createElement("optgroup");
  group.label = label;
  for (const name of names) {
    group.append(new Option(scenarios[name].label, name));
  }
  picker.append(group);
}

picker.addEventListener("change", () => {
  originalHidden = false;
  show(picker.value);
});

themePicker.addEventListener("change", () => {
  if (themePicker.value === "system")
    delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = themePicker.value;
});

document.querySelector('[data-demo="end-wait"]').addEventListener("click", () => {
  finishWait();
});

confirmDialog.addEventListener("cancel", (event) => {
  event.preventDefault();
  closeConfirm();
});

leaveDialog.addEventListener("cancel", (event) => {
  event.preventDefault();
  closeLeave();
});

for (const dialog of [confirmDialog, leaveDialog]) {
  dialog.addEventListener("click", (event) => {
    if (event.target !== dialog) return;
    if (dialog === confirmDialog) closeConfirm();
    else closeLeave();
  });
}

document.addEventListener("click", (event) => {
  const link = event.target.closest("a[data-leave]");
  if (link && !originalHidden && (scenarios[current].pending || scenarios[current].preSubmit)) {
    event.preventDefault();
    requestLeave(link, link.href);
    return;
  }

  const button = event.target.closest("button[data-action]");
  if (!button || button.disabled) return;
  switch (button.dataset.action) {
    case "request-confirm":
      openConfirm();
      break;
    case "close-confirm":
      closeConfirm();
      break;
    case "confirm-cancellation":
      completeSubmission();
      break;
    case "retry-entry":
      loadEntry(entryState());
      break;
    case "reread-updated":
      fixture = "updated";
      loadEntry("entry-updated");
      break;
    case "return-entry":
      show(entryState());
      break;
    case "check-login":
      beginCurrentCheck();
      break;
    case "stay":
      closeLeave();
      break;
    case "leave":
      if (leaveDestination) {
        clearTimers();
        window.location.href = leaveDestination;
      }
      break;
  }
});

addEventListener("pagehide", () => {
  clearTimers();
});

function enter() {
  const params = new URLSearchParams(location.search);
  const theme = params.get("theme");
  if (["light", "dark"].includes(theme)) {
    document.documentElement.dataset.theme = theme;
    themePicker.value = theme;
  }
  const entryFromQuery = {
    primary: "entry",
    "long-name": "entry-long-name",
    updated: "entry-updated",
    other: "entry-other",
  }[params.get("example")];
  if (entryFromQuery) {
    fixture = {
      "entry-long-name": "longName",
      "entry-updated": "updated",
      "entry-other": "other",
    }[entryFromQuery] || "primary";
  }
  const initial = entryFromQuery || "entry";
  show("entry-loading", { focus: false });
  readTimer = window.setTimeout(() => show(initial, { focus: false }), 500);
}

enter();
