const picker = document.getElementById("scenario-picker");
const content = document.getElementById("logout-content");
const leaveDialog = document.getElementById("leave-dialog");
const previewToggle = document.querySelector(".preview-toggle");
const logoutBasis =
  "revocation.md：current 只撤销本次认证会话。账户、实名、绑定、授权与已提交办理保留；可信 200 completion 表示这次退出已完成。";
const pendingBasis =
  "revocation.md 响应丢失与读取、recovery.md：保留原退出；等待结束、旧会话 401 或一次查无记录都不能证明原退出失败或成功，不自动换键重办。";
const currentBasis =
  "current.md：当前账户须独立可信读取；读取故障、限流、401、403 与选择变化均不交付旧资料。503 不证明已退出，401 不猜测具体原因。";
const queryBasis =
  "revocation.md：同账户有效会话 GET 读取原退出，不撤销新登录。完整原集合才构成确定结果；空列表、技术失败和本次请求拒绝分别表达。";
const states = {
  loading: {
    label: "首次核对当前登录",
    group: "退出前",
    busy: true,
    title: "正在读取登录状态",
    description: "读取完成后，你可以选择是否退出本次登录。",
    basis: currentBasis,
  },
  unavailable: {
    label: "首次读取失败",
    group: "退出前",
    title: "暂时无法读取登录状态",
    description: "请重新读取后再决定是否退出。暂时无法读取不表示已经退出。",
    actions: [
      ["read-initial", "重新读取"],
      ["account", "返回个人账户"],
    ],
    basis: currentBasis,
  },
  limited: {
    label: "首次读取受限",
    group: "退出前",
    title: "暂时无法读取登录状态",
    description: "暂时无法处理，请稍后再试。尚未提交本次退出。",
    limited: true,
    actions: [
      ["read-initial", "稍后重新读取"],
      ["home", "返回首页"],
    ],
    basis: currentBasis,
  },
  "initial-session-invalid": {
    label: "首次登录无效",
    group: "退出前",
    title: "当前登录已无法继续使用",
    description: "尚未提交本次退出。需要使用账户时，请重新登录。",
    actions: [
      ["login", "重新登录"],
      ["home", "返回首页"],
    ],
    basis: currentBasis,
  },
  forbidden: {
    label: "首次读取被拒绝",
    group: "退出前",
    title: "暂时无法读取登录状态",
    description: "暂时无法读取账户资料，请稍后再试。尚未提交本次退出。",
    actions: [["home", "返回首页"]],
    basis:
      "current.md：403 同源条件不成立，不清理选择、不宣称退出，不猜测账户状态。",
  },
  ready: {
    label: "退出前明确确认",
    group: "退出前",
    title: "确认退出本次登录？",
    description: "退出后，在这里查看本人资料或办理事务时，需要重新登录。",
    identity: "original",
    notice: "仅结束本次登录，不退出其他设备，也不取消已提交的办理。",
    actions: [
      ["submit", "确认退出本次登录"],
      ["account", "取消退出"],
    ],
    basis: logoutBasis,
  },
  "other-ready": {
    label: "退出前明确确认 · 林知夏",
    group: "退出前",
    title: "确认退出本次登录？",
    description: "退出后，在这里查看本人资料或办理事务时，需要重新登录。",
    identity: "original",
    fixture: "other",
    notice: "仅结束本次登录，不退出其他设备，也不取消已提交的办理。",
    actions: [
      ["submit", "确认退出本次登录"],
      ["account", "取消退出"],
    ],
    basis: logoutBasis,
  },
  "updated-ready": {
    label: "退出前明确确认 · 陈思宁",
    group: "退出前",
    title: "确认退出本次登录？",
    description: "退出后，在这里查看本人资料或办理事务时，需要重新登录。",
    identity: "original",
    fixture: "updated",
    notice: "仅结束本次登录，不退出其他设备，也不取消已提交的办理。",
    actions: [
      ["submit", "确认退出本次登录"],
      ["account", "取消退出"],
    ],
    basis: logoutBasis,
  },
  "long-name": {
    label: "长姓名与前导零尾号",
    group: "退出前",
    title: "确认退出本次登录？",
    description: "退出后，在这里查看本人资料或办理事务时，需要重新登录。",
    identity: "original",
    long: true,
    notice: "仅结束本次登录，不退出其他设备，也不取消已提交的办理。",
    actions: [
      ["submit", "确认退出本次登录"],
      ["account", "取消退出"],
    ],
    basis: logoutBasis,
  },
  "avatar-unavailable": {
    label: "当前资料可信 · 头像单独失败",
    group: "退出前",
    title: "确认退出本次登录？",
    description: "头像图片暂时无法加载，不影响退出本次登录。",
    identity: "original",
    avatarFailed: true,
    actions: [
      ["submit", "确认退出本次登录"],
      ["account", "取消退出"],
    ],
    basis:
      "current.md、avatar.md：可信本人资料与图片加载分别判断。图片失败不等于登录失效，不阻止本次退出。",
  },
  "storage-failed": {
    label: "首次材料保存失败 · 尚未提交",
    group: "退出前",
    title: "本次尚未提交退出",
    description:
      "暂时无法保存本次退出所需的信息。请检查浏览器后，再重新读取登录状态。",
    notice: "退出请求尚未发出。重新读取后，你可以选择是否退出。",
    actions: [
      ["read-initial", "重新读取登录状态"],
      ["account", "取消退出"],
    ],
    basis:
      "recovery.md：首次写入前先保存最小恢复材料；保存失败不发送 DELETE，不自动补发。",
  },
  "submit-denied": {
    label: "首次退出请求被拒绝",
    group: "退出前",
    title: "暂时无法退出本次登录",
    description: "本次退出未能完成。你可以返回首页浏览公开内容。",
    notice: "本次请求没有退出登录，也未更改账户资料。",
    actions: [["home", "返回首页"]],
    basis:
      "revocation.md：首次 DELETE 可信 403 拒绝本次请求，不清 cookie；不推测账户暂停、注销或用户操作原因。",
  },
  "submit-invalid": {
    label: "首次退出输入被拒绝",
    group: "退出前",
    title: "本次退出未能完成",
    description: "请返回个人账户，重新选择是否退出本次登录。",
    actions: [["account", "返回个人账户"]],
    basis:
      "revocation.md、HTTP：首次请求 invalid_input 只拒绝本次请求；无用户字段可标红，不据此修改会话。",
  },
  "submit-limited": {
    label: "首次退出请求受限 · 未受理",
    group: "退出前",
    title: "本次退出未能完成",
    description: "请等待后重新读取登录状态，再选择是否退出本次登录。",
    notice: "重新读取后，你可以选择是否退出本次登录。",
    limited: true,
    actions: [
      ["read-initial", "稍后读取登录状态"],
      ["home", "返回首页"],
    ],
    basis:
      "首次 DELETE 的可信 rate_limited 拒绝本次受理，不是原退出未知后的核对受限。遵守 Retry-After，示例 60 秒；等待后先重新读取当前账户并明确确认，不自动提交。",
  },
  "submit-session-invalid": {
    label: "首次退出请求认证无效 · 未受理",
    group: "退出前",
    title: "当前登录已无法继续使用",
    description: "本次退出未能完成。需要使用账户时，请重新登录。",
    actions: [
      ["login", "重新登录"],
      ["home", "返回首页"],
    ],
    basis:
      "首次 DELETE 的可信 authentication_invalid 拒绝本次请求，不猜测失效原因。本状态此前没有未知退出；已有未知退出时继续使用 original-session-invalid，401 不能证明原退出已完成。",
  },
  "request-mismatch": {
    label: "退出请求与此前信息不一致",
    group: "提交与结果",
    pending: true,
    title: "请查询此前退出的结果",
    description:
      "当前请求与此前退出的信息不一致。此前退出是否完成仍未确定，请先读取当前账户，再查询退出结果。",
    notice: "不要再次提交退出，也不要因此退出当前的新登录。",
    actions: [
      ["read-new", "读取当前账户后查询"],
      ["home", "暂时返回首页"],
    ],
    basis:
      pendingBasis +
      " DELETE 409 request_mismatch 不代表首次未受理或原退出失败。保留原定位和输入；同账户当前会话只 GET 读取原退出，不重发 DELETE。尚未读取的身份、其他账户和材料缺失分别使用已有遮蔽分支。",
  },
  submitting: {
    label: "正在提交退出",
    group: "提交与结果",
    busy: true,
    pending: true,
    title: "正在退出本次登录",
    description: "正在提交本次退出，请勿重复操作。",
    actions: [["stop-waiting", "暂时停止等待"]],
    basis: logoutBasis + " 本地停止等待不取消已接纳工作，随后保持待确认。",
  },
  complete: {
    label: "本次退出确定完成",
    group: "提交与结果",
    success: true,
    title: "本次退出已完成",
    description: "本次登录已结束。账户与已提交的办理仍会保留。",
    notice: "其他登录不因本次退出而结束。",
    completion: true,
    actions: [
      ["home", "返回首页"],
      ["login", "重新登录"],
    ],
    basis: logoutBasis,
  },
  pending: {
    label: "这次退出结果待确认",
    group: "提交与结果",
    pending: true,
    title: "本次退出结果尚未确定",
    description:
      "退出请求已经发出，但暂时还没有结果。请查询本次退出结果，无需重复退出。",
    notice:
      "查询前会先读取登录状态。若已无法继续使用，请重新登录同一账户后查询。",
    actions: [
      ["check-original", "查询退出结果"],
      ["home", "暂时返回首页"],
    ],
    basis: pendingBasis,
  },
  "material-missing": {
    label: "原退出定位缺失",
    group: "提交与结果",
    pending: true,
    title: "暂时无法确认是否已退出",
    description: "你可以返回首页；需要使用个人账户时，请重新登录。",
    notice: "这不表示退出失败，请不要再次提交退出。",
    actions: [
      ["home", "返回首页"],
      ["login", "前往登录"],
    ],
    basis: pendingBasis + " 恢复材料缺失不开放猜测性重做。",
  },
  "original-reading": {
    label: "待确认 · 核对原登录",
    group: "继续确认",
    busy: true,
    title: "正在读取登录状态",
    description: "正在读取当前登录状态，请稍候。",
    basis: pendingBasis + currentBasis,
  },
  "original-valid": {
    label: "待确认 · 原登录仍有效",
    group: "继续确认",
    pending: true,
    title: "查询本次退出结果",
    description: "当前登录暂时仍可使用，但本次退出结果尚未确定。",
    notice: "接下来只读取本次退出结果，不再次提交退出。",
    actions: [
      ["query-original", "查询本次退出结果"],
      ["home", "暂时返回首页"],
    ],
    basis: queryBasis + " 原会话有效不等于原退出已确定失败。",
  },
  "original-session-invalid": {
    label: "原登录无效 · 退出仍待确认",
    group: "继续确认",
    pending: true,
    title: "请重新登录同一账户",
    description: "当前登录已无法继续使用，但本次退出结果尚未确定。",
    notice: "请登录退出时使用的账户，再查询退出结果。查询不会退出新的登录。",
    actions: [
      ["login", "前往登录"],
      ["home", "暂时返回首页"],
    ],
    basis:
      pendingBasis +
      " 旧会话 401 不能证明旧 DELETE 已提交；新会话只能 GET 核对，不能带旧定位重发 DELETE。",
  },
  "new-reading": {
    label: "新登录 · 当前资料读取中",
    group: "同账户新登录",
    busy: true,
    title: "正在读取登录账户",
    description: "读取完成后，可以查看当前账户资料。",
    basis: currentBasis + queryBasis,
  },
  "new-unavailable": {
    label: "新登录 · 当前资料读取失败",
    group: "同账户新登录",
    title: "暂时无法读取登录账户",
    description: "请重新读取后继续。读取失败不表示登录已失效。",
    actions: [
      ["read-new", "重新读取当前账户"],
      ["home", "暂时返回首页"],
    ],
    basis: currentBasis,
  },
  "new-limited": {
    label: "新登录 · 当前资料读取受限",
    group: "同账户新登录",
    limited: true,
    title: "暂时无法读取登录账户",
    description: "请等待后重新读取当前账户资料。",
    actions: [
      ["read-new", "稍后重新读取"],
      ["home", "暂时返回首页"],
    ],
    basis: currentBasis + " Retry-After 示例 60 秒。",
  },
  "new-invalid": {
    label: "新登录 · 当前登录无效",
    group: "同账户新登录",
    title: "请重新登录",
    description: "当前登录已无法继续使用。需要使用账户时，请重新登录。",
    actions: [
      ["login", "前往登录"],
      ["home", "暂时返回首页"],
    ],
    basis: currentBasis + pendingBasis,
  },
  "new-denied": {
    label: "新登录 · 当前资料读取被拒绝",
    group: "同账户新登录",
    title: "暂时无法读取登录账户",
    description: "暂时无法读取账户资料，请稍后再试。",
    actions: [["home", "返回首页"]],
    basis:
      currentBasis +
      " 403 不清会话、不显示旧资料，不要求重新确认账户以修复同源条件。",
  },
  "resume-ready": {
    label: "同账户新登录已可信读取",
    group: "同账户新登录",
    pending: true,
    identity: "new",
    title: "查询此前退出的结果",
    description: "你已重新登录同一账户，可以继续查询此前退出的结果。",
    notice: "接下来只读取此前退出的结果，不会退出当前的新登录。",
    actions: [
      ["query", "查询此前退出结果"],
      ["account", "暂时返回个人账户"],
    ],
    basis: currentBasis + queryBasis,
  },
  querying: {
    label: "新登录 · 原退出查询中",
    group: "只读核对",
    busy: true,
    pending: true,
    title: "正在查询此前退出结果",
    description: "只读取此前退出的结果，当前新登录不会因此退出。",
    basis: queryBasis,
  },
  "querying-original": {
    label: "原登录 · 原退出查询中",
    group: "只读核对",
    busy: true,
    pending: true,
    title: "正在查询退出结果",
    description: "只读取本次退出结果，不再次提交退出。",
    basis: queryBasis,
  },
  "original-query-unavailable": {
    label: "原登录 · 退出结果读取失败",
    group: "只读核对",
    pending: true,
    title: "暂时无法查询退出结果",
    description: "查询暂未成功，不表示退出失败。请稍后再查询本次退出结果。",
    notice: "这里只读取本次退出结果，不再次提交退出。",
    actions: [
      ["query-original", "查询退出结果"],
      ["home", "暂时返回首页"],
    ],
    basis:
      queryBasis +
      " 原会话刚刚可信读取为有效，以原登录 GET 原退出遇到技术失败时保留原办理；不称当前新登录，不把读取故障解释为退出。",
  },
  "original-query-limited": {
    label: "原登录 · 退出结果读取受限",
    group: "只读核对",
    pending: true,
    limited: true,
    title: "请稍后查询退出结果",
    description: "暂时无法查询，请等待后再试。本次退出结果尚未确定。",
    notice: "请等待后只重新读取本次退出结果，不再次提交退出。",
    actions: [
      ["query-original", "稍后查询退出结果"],
      ["home", "暂时返回首页"],
    ],
    basis:
      queryBasis +
      " 原会话仍有效时 GET 原退出受限，遵守 Retry-After，示例 60 秒。只读重查，不建立新退出，也不推断原退出失败。",
  },
  "original-query-empty": {
    label: "原登录 · 暂无可读退出结果",
    group: "只读核对",
    pending: true,
    title: "暂时还没有查询到本次退出结果",
    description:
      "暂时还没有查询到结果，不表示退出失败。请稍后再查询，或暂时返回首页。",
    notice: "当前登录暂时仍可使用。本次只查询结果，不会重新提交退出。",
    actions: [
      ["query-original", "查询退出结果"],
      ["home", "暂时返回首页"],
    ],
    basis:
      queryBasis +
      " 原会话仍有效不等于原退出确定失败；GET 空列表只表示暂无本账户可读的原集合，不能据此重新 DELETE。",
  },
  "query-unavailable": {
    label: "原退出查询失败",
    group: "只读核对",
    pending: true,
    identity: "new",
    title: "暂时无法查询退出结果",
    description:
      "查询暂未成功，请稍后再试。查询此前的退出结果不会结束当前的新登录。",
    actions: [
      ["query", "查询退出结果"],
      ["account", "暂时返回个人账户"],
    ],
    basis: queryBasis + " 读取技术失败不是空列表或原退出失败。",
  },
  "query-limited": {
    label: "原退出查询受限",
    group: "只读核对",
    pending: true,
    limited: true,
    identity: "new",
    title: "请稍后查询退出结果",
    description:
      "暂时无法查询，请等待后再试。此前退出的结果仍未确定，当前新登录仍可使用。",
    actions: [
      ["query", "稍后查询退出结果"],
      ["account", "暂时返回个人账户"],
    ],
    basis:
      queryBasis + " rate_limited 遵守 Retry-After，示例 60 秒；不自动循环。",
  },
  "query-empty": {
    label: "原退出暂无可读结果",
    group: "只读核对",
    pending: true,
    identity: "new",
    title: "暂时还没有查询到此前退出结果",
    description:
      "暂时还没有查询到结果，不表示此前退出失败。请稍后再查询，或先返回当前账户。",
    notice: "当前新登录仍有效。本页不会因此重新提交退出。",
    actions: [
      ["query", "查询退出结果"],
      ["account", "暂时返回个人账户"],
    ],
    basis:
      queryBasis +
      " GET count:0、revocations:[] 只表示无本账户可读的原集合，不是 404 或确定回滚。",
  },
  "query-denied": {
    label: "原退出查询被拒绝",
    group: "只读核对",
    pending: true,
    title: "暂时无法查询退出结果",
    description: "此前是否已退出仍不确定。你可以先返回首页浏览公开内容。",
    actions: [["home", "返回首页"]],
    basis:
      queryBasis +
      " GET 403 拒绝本次读取，不清选择，不推断原退出结局，也不公开内部同源字段。",
  },
  "query-invalid": {
    label: "原退出查询输入被拒绝",
    group: "只读核对",
    pending: true,
    title: "暂时无法查询退出结果",
    description: "此前是否已退出仍不确定。你可以先返回首页浏览公开内容。",
    actions: [["home", "返回首页"]],
    basis:
      queryBasis +
      " invalid_input 只拒绝本次 GET，不改原定位、不建立新退出、不标用户字段错。",
  },
  "query-complete": {
    label: "此前退出完成 · 新登录仍有效",
    group: "已确认",
    success: true,
    identity: "new",
    completion: true,
    title: "此前退出已完成",
    description:
      "已确认此前那次登录已退出。当前的新登录仍然有效，可以继续使用。",
    notice: "查询此前退出结果没有结束当前的新登录。",
    actions: [
      ["account", "查看当前个人账户"],
      ["home", "返回首页"],
    ],
    basis: queryBasis + " 不使用泛化“你已退出”；原完成和当前登录分别表达。",
  },
  "late-complete": {
    label: "原退出迟到成功 · 新登录保留",
    group: "已确认",
    success: true,
    identity: "new",
    completion: true,
    title: "此前退出已完成",
    description: "已收到此前退出成功的结果。你后来建立的本次新登录仍然有效。",
    notice: "这只是此前退出的结果，不会结束或替换当前的新登录。",
    actions: [
      ["account", "查看当前个人账户"],
      ["home", "返回首页"],
    ],
    basis:
      "revocation.md：可信迟到成功只清理原准确 cookie；本地 Session ID 与选择修订不匹配时，不清除后来选择或新登录尝试。",
  },
  "account-changed": {
    label: "账户选择已变化 · 全部遮蔽",
    group: "账户变化",
    title: "登录状态已变化",
    description: "请先读取当前账户资料。",
    actions: [
      ["read-other", "读取当前账户资料"],
      ["home", "返回首页"],
    ],
    basis:
      "recovery.md、current.md：选择变化立即隐藏旧资料与受限恢复材料；不使用旧响应，不跨账户读取旧退出。",
  },
  "other-reading": {
    label: "账户切换 · 重新读取中",
    group: "账户变化",
    busy: true,
    title: "正在读取登录账户",
    description: "读取完成后，可以查看当前账户资料。",
    basis: currentBasis,
  },
  "other-account": {
    label: "其他账户已可信读取",
    group: "账户变化",
    identity: "other",
    title: "当前账户可以继续使用",
    description: "你可以查看当前账户资料，或返回首页。",
    actions: [
      ["account", "查看当前个人账户"],
      ["home", "返回首页"],
    ],
    basis:
      "recovery.md：受限恢复信息按账户隔离。其他账户不能凭旧退出定位读取结果；不尝试查询再泄露归属。",
  },
};

const avatarIcon =
  '<svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="8" r="3.5"/><path d="M5 21v-2a7 7 0 0 1 14 0v2"/></svg>';
const avatarSource = "assets/halo-avatar-current.webp";
let current = "loading";
let confirmation = "ready";
let pendingRead;
let waitClock;
let remaining = 0;
let leaveTarget = "halo-home.html";
const concealedStates = new Set([
  "original-reading",
  "new-reading",
  "new-unavailable",
  "new-limited",
  "new-invalid",
  "new-denied",
  "account-changed",
  "other-reading",
  "other-account",
]);
const resumeParams = new URLSearchParams(location.search);
const returningToResult =
  resumeParams.getAll("resume").length === 1 &&
  resumeParams.get("resume") === "logout-result";
const returnExample =
  resumeParams.getAll("example").length === 1 &&
  ["same-account", "other-account", "material-missing"].includes(
    resumeParams.get("example"),
  )
    ? resumeParams.get("example")
    : "same-account";

function readReturningAccount(example = returnExample) {
  const target =
    example === "other-account"
      ? "other-account"
      : example === "material-missing"
        ? "material-missing"
        : "resume-ready";
  read("new-reading", target);
}

function stopTimers() {
  clearTimeout(pendingRead);
  clearInterval(waitClock);
}

function identityPanel(state) {
  const other = state.identity === "other" || state.fixture === "other";
  const updated = state.fixture === "updated";
  const name = other
    ? "林知夏"
    : updated
      ? "陈思宁"
      : state.long
        ? "阿依古丽·阿不都热依木"
        : "陈思远";
  const suffix = other ? "0000" : updated || state.long ? "0007" : "0028";
  const label =
    state.identity === "other"
      ? "当前账户"
      : state.identity === "new"
        ? "当前的新登录"
        : "即将退出的登录";
  return `<section class="logout-identity" aria-label="${label}"><p>${label}</p><div class="profile-summary"><div class="avatar-slot"${state.avatarFailed ? ' role="img" aria-label="头像暂时无法加载"' : ""}>${state.avatarFailed ? avatarIcon : `<img src="${avatarSource}" width="56" height="56" alt="虚构头像示例" />`}</div><div class="profile-name"><h3>${name}</h3><p>身份证尾号 <span class="suffix">${suffix}</span></p></div></div></section>`;
}

function render(focus = true) {
  const state = states[current];
  const concealed = concealedStates.has(current);
  const originalResult = [
    "original-valid",
    "querying-original",
    "original-query-unavailable",
    "original-query-limited",
    "original-query-empty",
  ].includes(current);
  const previousResult =
    originalResult ||
    state.identity === "new" ||
    ["querying", "query-denied", "query-invalid"].includes(current);
  const originalOutcome = state.pending || state.completion;
  const queryRejected = ["query-denied", "query-invalid"].includes(current);
  const pageTitle = concealed
    ? "账户状态"
    : previousResult
      ? "此前退出结果"
      : originalOutcome
        ? "本次退出结果"
        : "退出本次登录";
  document.getElementById("page-title").textContent = pageTitle;
  document.getElementById("page-introduction").textContent = concealed
    ? "读取当前账户资料后再继续。"
    : queryRejected
      ? "本次查询未能完成，此前退出结果仍未确定。"
      : originalResult
        ? "只查询此前退出的结果，不会再次提交退出。"
        : previousResult
          ? "查询此前退出的结果，不会退出当前的新登录。"
          : state.completion
            ? "已确认本次登录退出，账户与已提交的办理仍会保留。"
            : current === "submitting"
              ? "退出请求正在处理，请勿重复操作。"
              : originalOutcome
                ? "本次退出结果尚未确定，请勿重新提交退出。"
                : "只结束本次登录，保留账户与已提交的办理。";
  document.getElementById("logout-guide").hidden = concealed;
  document.getElementById("result-guide").hidden =
    !state.pending || current === "material-missing" || queryRejected;
  document.getElementById("logout-columns").dataset.neutral = String(concealed);
  document.title = `${pageTitle} · 司籍处`;
  picker.value = current;
  document.getElementById("state-basis").textContent = state.basis;
  content.setAttribute("aria-busy", String(Boolean(state.busy)));
  content.innerHTML = `${state.busy ? '<span class="reading-symbol" aria-hidden="true"></span>' : ""}<h2 id="stage-title">${state.title}</h2><p class="stage-description">${state.description}</p>${state.identity ? identityPanel(state) : ""}${state.notice ? `<p class="logout-notice${state.success ? " success" : ""}">${state.notice}</p>` : ""}${state.completion ? '<dl class="logout-facts"><div><dt>退出范围</dt><dd>此前确认的那次登录</dd></div><div><dt>退出时间</dt><dd><time datetime="2026-10-20T10:30:00+08:00">2026年10月20日 10:30</time>（北京时间）</dd></div></dl>' : ""}${state.actions ? `<div class="logout-actions">${state.actions.map(([action, label], index) => `<button class="button${index === 0 ? " primary" : ""}" type="button" data-action="${action}"${state.limited && index === 0 ? " data-wait disabled" : ""}>${label}</button>`).join("")}</div>` : ""}`;
  if (state.limited) {
    remaining = 60;
    tickWait();
    waitClock = setInterval(() => {
      remaining = Math.max(0, remaining - 1);
      tickWait();
    }, 1000);
  }
  if (focus) document.getElementById("page-title").focus();
}

function tickWait() {
  const button = content.querySelector("[data-wait]");
  if (!button) return;
  button.disabled = remaining > 0;
  button.textContent = remaining
    ? `${remaining} 秒后可继续`
    : states[current].actions[0][1];
  if (!remaining) clearInterval(waitClock);
}

function show(id, focus = true) {
  stopTimers();
  if (leaveDialog.open) leaveDialog.close();
  current = Object.hasOwn(states, id) ? id : "ready";
  render(focus);
}

function read(loading, target) {
  show(loading);
  pendingRead = setTimeout(() => show(target), 650);
}

function accountHref() {
  const state =
    states[
      [
        "storage-failed",
        "submit-denied",
        "submit-invalid",
        "submit-limited",
      ].includes(current)
        ? confirmation
        : current
    ];
  const target =
    state.identity === "other" || state.fixture === "other"
      ? "other-account"
      : state.fixture === "updated"
        ? "updated"
        : state.long
          ? "long-name"
          : state.avatarFailed
            ? "avatar-unavailable"
            : state.identity
              ? "ready"
              : {
                  unavailable: "unavailable",
                  limited: "limited",
                  forbidden: "forbidden",
                  "initial-session-invalid": "session-invalid",
                  "submit-session-invalid": "session-invalid",
                  complete: "session-invalid",
                  "original-session-invalid": "session-invalid",
                  "new-unavailable": "unavailable",
                  "new-limited": "limited",
                  "new-invalid": "session-invalid",
                  "new-denied": "forbidden",
                  "query-denied": "forbidden",
                  "account-changed": "account-changed",
                  "other-reading": "account-changed",
                }[current] || "loading";
  return `halo-account.html#${target}`;
}

function requestLeave(href) {
  if (href === "halo-account.html") href = accountHref();
  if (states[current].pending && current !== "material-missing") {
    leaveTarget = href;
    document.getElementById("leave-description").textContent = [
      "query-denied",
      "query-invalid",
    ].includes(current)
      ? "此前退出结果尚未确定，离开此页不会取消此前的请求。请勿重复提交退出。"
      : "此前的退出请求已经发出，离开此页不会取消请求。此前退出结果尚未确定，请使用同一账户继续查询。";
    leaveDialog.showModal();
    return;
  }
  location.href = href;
}

for (const [id, state] of Object.entries(states)) {
  let group = Array.from(picker.children).find(
    (node) => node.label === state.group,
  );
  if (!group) {
    group = document.createElement("optgroup");
    group.label = state.group;
    picker.append(group);
  }
  const option = document.createElement("option");
  option.value = id;
  option.textContent = state.label;
  group.append(option);
}
picker.addEventListener("change", () => {
  confirmation = "ready";
  show(picker.value, false);
});
previewToggle.addEventListener("click", () => {
  const expanded = previewToggle.getAttribute("aria-expanded") === "true";
  previewToggle.setAttribute("aria-expanded", String(!expanded));
  previewToggle.textContent = expanded ? "展开预览工具" : "收起预览工具";
});
document.addEventListener("click", (event) => {
  const demo = event.target.closest("[data-demo]");
  if (demo) {
    if (demo.dataset.demo === "same-account")
      readReturningAccount("same-account");
    if (demo.dataset.demo === "other-account") show("account-changed");
    if (demo.dataset.demo === "material-missing")
      readReturningAccount("material-missing");
    if (demo.dataset.demo === "end-wait") {
      remaining = 0;
      tickWait();
    }
    return;
  }
  const button = event.target.closest("[data-action]");
  if (button && !button.disabled) {
    switch (button.dataset.action) {
      case "submit": {
        confirmation = current;
        const target = document.getElementById("submit-outcome").value;
        if (target === "storage-failed") show(target);
        else read("submitting", target);
        break;
      }
      case "stop-waiting":
        show("pending");
        break;
      case "read-initial":
        read(
          "loading",
          ["storage-failed", "submit-limited"].includes(current)
            ? confirmation
            : "ready",
        );
        break;
      case "check-original":
        read("original-reading", "original-session-invalid");
        break;
      case "query-original":
        read("querying-original", "original-query-empty");
        break;
      case "read-new":
        readReturningAccount();
        break;
      case "query":
        read("querying", "query-complete");
        break;
      case "read-other":
        read("other-reading", "other-account");
        break;
      case "account":
        requestLeave("halo-account.html");
        break;
      case "home":
        requestLeave("halo-home.html");
        break;
      case "login":
        requestLeave(
          ["original-session-invalid", "new-invalid"].includes(current)
            ? "halo-authentication.html?return=logout-result"
            : "halo-authentication.html",
        );
        break;
      case "stay":
        leaveDialog.close();
        break;
      case "leave":
        location.href = leaveTarget;
        break;
    }
    return;
  }
  const link = event.target.closest("a[href]");
  if (!link || link.getAttribute("href").startsWith("#")) return;
  if (
    link.getAttribute("href") === "halo-account.html" ||
    states[current].pending
  ) {
    event.preventDefault();
    requestLeave(link.getAttribute("href"));
  }
});
addEventListener("pagehide", () => {
  stopTimers();
  content.replaceChildren();
});
function enter() {
  if (returningToResult) {
    readReturningAccount();
    return;
  }
  const selected = location.hash.slice(1);
  const target = Object.hasOwn(states, selected) ? selected : "ready";
  show("loading", false);
  if (target !== "loading")
    pendingRead = setTimeout(() => show(target, false), 650);
}
addEventListener("pageshow", (event) => {
  if (event.persisted) enter();
});
enter();
