const r1Release = new URLSearchParams(location.search).get("release") === "r1";
const r2Release = new URLSearchParams(location.search).get("release") === "r2";

const phone = {
  screen: "phone",
  group: "手机号与发码",
  step: "手机号登录",
  title: "登录司籍处",
  description: r1Release
    ? "已有账户可使用手机号登录，注册功能暂未开放。"
    : "使用手机号验证登录。首次使用，验证后再填写注册资料。",
  basis:
    "C1：先完成安全验证并保存恢复材料，再发送验证码。安全验证区域为交互占位。",
};
const code = {
  screen: "code",
  group: "验证码与登录",
  step: "验证手机号",
  title: "输入验证码",
  description: "请留意手机短信，输入最新收到的验证码。",
  basis:
    "C1 / R1：验证码为 6 位 ASCII 数字。sent 只表示供应商受理，不能据此声称已送达；重发间隔和有效期取服务端。",
};
const registration = {
  screen: "registration",
  group: "首次注册",
  step: "首次使用 · 创建账户",
  title: "填写注册资料",
  description: "手机号已验证。填写本人资料，完成账户注册。",
  basis:
    "R1 返回 registration 后进入 R2。注册只接收姓名与 idNumber，不从凭据或缓存推导手机号展示，不自动加入员工服务。",
};
const result = {
  screen: "result",
  group: "登录确认",
  step: "登录司籍处",
  action: "phone",
  actionLabel: "返回登录",
  symbol: "!",
  basis: "网站会话：取得 Session 后，须成功读取本人账户，才能进入已登录页面。",
};
const currentAccount = {
  screen: "current",
  group: result.group,
  step: "个人账户",
  title: "当前账户",
  description: "当前登录的账户信息。",
  basis:
    "首批 R1：Session 交接后，可信 accounts/current 成功且仍匹配当前选择，才展示姓名、证件尾四位和当前头像。示例均为虚构，不读取员工服务资格。",
};
const wechat = {
  screen: "wechat",
  group: "微信登录 · 目标设计",
  step: "微信登录",
  title: "微信扫一扫",
  description: "已绑定微信的账户，可以扫码登录。",
  basis:
    "微信网站授权为尚未实施的目标设计；正式使用供应商组件。仅原结果与当前本人读取能确认登录，组件加载或扫码消息不能证明成功。",
};

const states = {
  phone: { ...phone, label: "登录 · 初始" },
  "phone-invalid": {
    ...phone,
    label: "手机号格式错误",
    errors: { mobile: "请填写正确的 11 位中国大陆手机号，不含空格或 +86。" },
  },
  "challenge-loading": {
    ...phone,
    label: "安全验证加载中",
    challenge: "loading",
    notice: "正在加载安全验证，请稍候。",
  },
  "challenge-expired": {
    ...phone,
    label: "安全验证已过期",
    notice: "安全验证已过期，请重新完成验证。",
    tone: "error",
    basis:
      "仅首次发码前过期时重新挑战。已发送的 C1 不因组件过期而换掉恢复材料。",
  },
  "challenge-unavailable": {
    ...phone,
    label: "安全验证加载失败",
    notice: "安全验证暂时无法加载。请检查网络后重试。",
    tone: "error",
  },
  "challenge-invalid": {
    ...phone,
    label: "安全验证未通过",
    notice: "安全验证未通过，请重新验证后再获取验证码。",
    tone: "error",
    basis:
      "C1 可信 verification_invalid 拒绝后，用户主动获取新证明；不把网络故障解释为验证未通过。",
  },
  "storage-unavailable": {
    ...result,
    group: phone.group,
    label: "发送前无法保存",
    title: "暂时无法继续",
    description: "浏览器暂时无法保存本次验证所需的信息。",
    detail: "请检查浏览器是否限制了此网站保存信息。本次尚未发送验证码。",
    actionLabel: "返回登录",
    basis:
      "首次写入前保存恢复材料失败，不发送 C1；不默认为已发送。设计按钮只演示返回。",
  },
  sending: {
    ...phone,
    label: "验证码发送中",
    notice: "正在发送验证码，请稍候。",
    locked: true,
    busy: true,
    transient: true,
    recovery: "send-pending",
  },
  "send-pending": {
    ...result,
    group: phone.group,
    missing: "send-proof-missing",
    label: "发送结果待确认",
    title: "本次发送结果暂时无法确定",
    symbol: "…",
    description: "暂时无法确定验证码是否已发送。请先查看手机短信。",
    detail: "你也可以查看本次发送的处理结果，不会另外发送一条短信。",
    action: "confirm",
    actionLabel: "查看发送结果",
    basis:
      "C1 断连、outcome_unknown、progress 或不可信响应：只重提原材料，不换键发码；progress 按 retryAfterSeconds 再核对。不把普通 503 当成确定未发送。",
  },
  "send-proof-invalid": {
    ...result,
    group: phone.group,
    label: "发送原证明无效",
    title: "暂时无法继续查看本次发送结果",
    description: "本次发送的查询信息已无法使用，不能据此判断发送结果。",
    detail:
      "请先查看是否已收到短信。需要时可返回登录，重新完成安全验证后再获取。",
    actionLabel: "返回登录",
    basis:
      "C1 authentication_invalid：本次重提的原人机证明不成立，结束自动恢复。不推断短信已发或未发，不归咎于用户，也不默认附上重新发码动作。",
  },
  "send-proof-missing": {
    ...result,
    group: phone.group,
    missing: "send-proof-missing",
    label: "发送恢复信息缺失",
    title: "暂时无法继续查看本次发送结果",
    description: "请查看手机短信，暂时不要重复获取验证码。",
    detail: "",
    action: "home",
    actionLabel: "返回首页",
    basis:
      "原 C1 恢复材料缺失或已到本地保留上限：停止自动恢复，不默认附上重新发送动作。",
  },
  code: { ...code, label: "验证码已发送 · 倒计时" },
  "code-ready": { ...code, label: "可重新获取验证码", resendReady: true },
  "send-in-progress": {
    ...code,
    label: "短信仍在发送",
    notice: "短信正在发送，请稍候。如果已收到，可以先输入验证码。",
    basis:
      "可信 message.delivery=sending 已提供 Message。验证码效力由 Check 判断，区别于还未取得 Message 的请求等待。",
  },
  "send-unknown": {
    ...code,
    label: "短信发送情况未确认",
    notice: "短信发送情况暂未确认。如果已经收到，请输入验证码。",
    basis:
      "可信 message.delivery=unknown 已有 Message，可使用有效码；区别于 C1 outcome_unknown。不得声称未发送。",
  },
  "send-failed": {
    ...code,
    label: "短信发送未成功",
    notice: "本次验证码发送未成功。请稍后重新获取。",
    tone: "error",
    basis:
      "可信 message.delivery=failed。原许可仍占额度；重新获取遵守服务端等待时间，旧码效力仍由 Check 判定。",
  },
  "send-limited": {
    ...result,
    group: phone.group,
    label: "发码暂时无法处理",
    title: "暂时无法获取验证码",
    description: "暂时无法处理，请稍后再试。",
    detail: "请稍后返回，重新完成安全验证后再获取验证码。",
    action: "limited",
    actionLabel: "返回获取验证码",
    limited: true,
    resumes: "send-pending",
    pendingResult: {
      title: "暂时无法查询发送结果",
      description: "本次发送的结果仍未确定。",
      detail:
        "此前获取验证码的结果仍未确定。等待后查询本次发送，无需重新获取验证码。",
      actionLabel: "继续查询发送结果",
    },
    basis:
      "可信 rate_limited：展示 Retry-After 的剩余等待时间，设计示例为 60 秒。它可能来自共享处理名额，不单独证明用户操作频繁，也不得据此推断此前发送失败。若是在确认结果未明的原发码时受限，等待后回到原待确认状态，不开放改号或重新发码。",
  },
  "send-mismatch": {
    ...result,
    group: phone.group,
    missing: "send-proof-missing",
    label: "发码资料不一致",
    title: "暂时无法确认验证码是否已发送",
    description: "请先查看手机短信，暂时不要重新获取验证码。",
    detail:
      "本次申请的信息与原记录不一致。返回首页不会重新发送短信，本次申请仍按原期限保留。",
    action: "home",
    actionLabel: "返回首页",
    basis:
      "C1 request_mismatch：保留原输入与证明至原截止，停止自动恢复及该原请求的继续核对入口，不提供新发码动作。返回公开首页不判断原结果；页面重进不新办。演示页刷新后材料缺失，显示先查看短信且不要重复获取的提示。",
  },

  "login-limited": {
    ...result,
    group: code.group,
    resumes: "login-pending",
    retry: "code",
    label: "登录验证暂时无法处理",
    title: "暂时无法登录",
    description: "暂时无法处理，请稍后再试。",
    detail: "等待结束后再继续验证。若验证码已失效，再重新获取。",
    action: "limited",
    actionLabel: "继续验证",
    limited: true,
    pendingResult: {
      title: "暂时无法查询登录结果",
      description: "本次登录的结果仍未确定。",
      detail:
        "此前登录的结果仍未确定。等待后继续查询本次登录的结果，无需重新获取验证码。",
      actionLabel: "继续查询登录结果",
    },
    basis:
      "R1 rate_limited：入口额度拒绝，可能来自共享处理名额，不单独证明用户操作频繁。等待 Retry-After 后用原 Operation-Id 与原材料重试；若是在确认原登录时受限，回到原待确认状态，不重新获取验证码。",
  },
  "login-mismatch": {
    ...result,
    group: code.group,
    resumes: "login-pending",
    label: "登录资料不一致",
    title: "请先确认原登录结果",
    description: "本次请求与先前提交的验证信息不一致。",
    detail:
      "原手机号与验证码保持不变。请先确认原登录结果，无需重新获取验证码。",
    action: "resume",
    actionLabel: "确认原登录结果",
    basis:
      "R1 request_mismatch 不改写原输入，不推断原结果，也不自动用新内容重新认证。按钮回到原登录的待确认状态。",
  },
  "code-invalid": {
    ...code,
    label: "验证码不正确",
    errors: { code: "验证码不正确，请核对最新短信后重新输入。" },
    basis:
      "R1 verification_invalid：仅确定的错码提示，不把供应商或网络故障算作输错。",
  },
  "login-proof-invalid": {
    ...result,
    group: code.group,
    label: "登录原证明无效",
    title: "登录状态待确认",
    description: "请先检查当前登录状态，暂时不用再次获取验证码。",
    detail: "",
    action: "open-current",
    actionLabel: "检查登录状态",
    secondaryHome: true,
    basis:
      "R1 authentication_invalid 仅拒绝本次原 secret，终止原自动恢复，不判断原登录结果，也不重新发码。页面保留原材料至原期限，打开独立当前账户；Current 成功不证明原登录完成。本设计不实现真实材料存储。",
  },
  "code-expired": {
    ...result,
    group: code.group,
    label: "验证码失效或次数用尽",
    title: "请重新获取验证码",
    description: "这个验证码已无法使用。",
    detail: "请重新完成安全验证，再获取新的短信验证码。",
    actionLabel: "重新获取验证码",
    basis:
      "R1 verification_required 可能为失效、被消费、过期或猜错上限，界面不臆测具体原因。",
  },
  verifying: {
    ...code,
    label: "验证码验证中",
    notice: "正在验证手机号，请稍候。",
    locked: true,
    busy: true,
    transient: true,
    recovery: "login-pending",
  },
  "login-pending": {
    ...result,
    group: code.group,
    missing: "result-unavailable",
    label: "登录结果待确认",
    title: "本次登录结果暂时无法确定",
    symbol: "…",
    description: "暂时无法确定本次登录是否完成。",
    detail: "请查看本次登录结果，无需重新获取验证码。",
    action: "confirm",
    actionLabel: "查看登录结果",
    basis:
      "R1 待核对：保留原 secret、Message、手机号与码；progress 按 retryAfterSeconds 再核对。未知结果不重新认证，也不进入新用户注册分支。",
  },
  "login-denied": {
    ...result,
    group: code.group,
    label: "登录被拒绝",
    title: "暂时无法登录",
    description: "本次登录未能完成。",
    detail: "请返回首页后重试。如果仍无法登录，请联系医院管理员。",
    action: "home",
    actionLabel: "返回首页",
    basis:
      "R1 permission_denied 可来自账户暂停，也可能来自来源检查。现有响应不能区分原因，用户页面不武断标成账户已暂停。",
  },
  registration: { ...registration, label: "注册 · 填写资料" },
  "registration-invalid": {
    ...registration,
    label: "注册资料格式错误",
    errors: {
      name: "请填写身份证上的完整姓名。",
      "id-number": "请核对 18 位居民身份证号。",
    },
    basis:
      "仅表示本地已经确定的字段格式错误，不代表服务端仅含 code 的通用 invalid_input。出生日期、校验位及可信办理日由服务端验证。",
  },
  "registration-storage-unavailable": {
    ...registration,
    label: "注册前无法保存 · 尚未提交",
    title: "暂时无法提交注册",
    description: "浏览器暂时无法保存本次注册所需的信息。",
    notice:
      "本次尚未提交注册。请检查浏览器是否限制了此网站保存信息，再继续填写。",
    submitLabel: "继续填写注册资料",
    recheckStorage: true,
    basis:
      "首次 R2 POST 前先确认最小恢复材料已保存。保存失败不发送注册；重新检查只回到填表，仍需用户明确提交，不套用 C1 的未发送验证码提示。",
  },
  "registration-input-rejected": {
    ...registration,
    label: "首次注册请求未受理 · 通用输入拒绝",
    notice: "本次注册请求未受理，请检查填写内容后再试。",
    tone: "error",
    basis:
      "首次请求的可信 invalid_input 不携带字段详情，可能来自头、JSON、媒体类型、大小或字段值。只提供表单级提示，不把姓名和身份证号同时标错。此前没有未知 R2。",
  },
  "registration-denied": {
    ...result,
    group: registration.group,
    label: "首次注册请求未受理 · 权限拒绝",
    title: "暂时无法注册",
    description: "本次注册未能完成。",
    detail: "你可以返回首页，继续浏览公开内容。",
    action: "home",
    actionLabel: "返回首页",
    basis:
      "首次 R2 的可信 permission_denied 只表示本次请求未受理；不猜测账户暂停、号码占用或实名问题，不泄露其他账户资料。此前没有未知 R2。",
  },
  registering: {
    ...registration,
    label: "注册提交中",
    title: "正在注册",
    description: "资料已提交，正在处理。",
    notice: "正在注册，请稍候。",
    locked: true,
    busy: true,
    transient: true,
    recovery: "registration-pending",
  },
  "registration-pending": {
    ...registration,
    missing: "result-unavailable",
    label: "注册结果待确认",
    title: "本次注册结果暂时无法确定",
    description: "本次注册已经提交，结果尚未确定。",
    notice: "请查看本次注册结果，不要重复提交。此前提交的资料暂时不能修改。",
    locked: true,
    pending: true,
    basis:
      "R2 待核对：冻结原姓名与身份证号；不因填表倒计时结束而清除已提交证明，不换键重新注册。",
  },
  "registration-recovery-read-failed": {
    ...registration,
    resumes: "registration-pending",
    missing: "result-unavailable",
    label: "注册待确认 · 恢复信息暂时无法读取",
    title: "本次注册结果尚未确定",
    description: "暂时无法查看本次注册结果。",
    notice: "请再试一次，不要重复提交注册。此前提交的资料暂时不能修改。",
    locked: true,
    pending: true,
    recheckRecovery: true,
    submitLabel: "继续查看注册结果",
    basis:
      "R2 已提交且结果未知后，本地材料暂时读取失败不证明原办理失败。保留原输入和待核对定位；重新读到原材料只回原 registration-pending，不自动新注册。真正丢失或到期沿 result-unavailable。",
  },
  "registration-recovery-write-failed": {
    ...registration,
    resumes: "registration-pending",
    missing: "result-unavailable",
    label: "注册待确认 · 恢复信息暂时无法更新",
    title: "本次注册结果尚未确定",
    description: "暂时无法查看本次注册结果。",
    notice: "请留在本页继续查看结果，不要重复提交或刷新页面。",
    locked: true,
    pending: true,
    recheckRecovery: true,
    submitLabel: "继续查看注册结果",
    basis:
      "R2 已提交且结果未知后，本地材料更新失败不覆盖或清空仍持有的原材料。重新检查仅回原待确认，不因保存失败猜测重办，也不把操作失败转为注册成功。",
  },
  "registration-confirm-input-rejected": {
    ...registration,
    resumes: "registration-pending",
    missing: "result-unavailable",
    label: "注册待确认 · 此次确认输入被拒绝",
    title: "本次注册结果尚未确定",
    description: "暂时无法查看本次注册结果。",
    notice: "请继续查看注册结果，不要重新填写或重复提交。",
    locked: true,
    pending: true,
    resumePending: true,
    basis:
      "此前 R2 结果未知时，可信通用 invalid_input 只拒绝本次核对调用。不标姓名或证件字段错误，不解锁原输入，不换键重办；按钮只回原待确认。",
  },
  "registration-confirm-denied": {
    ...registration,
    resumes: "registration-pending",
    missing: "result-unavailable",
    label: "注册待确认 · 此次确认被拒绝",
    title: "本次注册结果尚未确定",
    description: "暂时无法查看本次注册结果。",
    notice: "注册结果仍未确定。你可以稍后再查看，不要重新填写或重复提交。",
    locked: true,
    pending: true,
    resumePending: true,
    basis:
      "此前 R2 结果未知时，permission_denied 只拒绝本次确认请求。保留原输入与待核对责任，不退为首次失败，不推断暂停或占用，不自动清空后新注册。",
  },
  "registration-expired": {
    ...result,
    group: registration.group,
    label: "填写超时 · 尚未提交",
    title: "请重新验证手机号",
    description: "填写资料的时间已结束，请重新验证手机号后继续。",
    detail: "本次尚未提交注册。重新验证后，再填写注册资料。",
    actionLabel: "验证手机号",
    basis:
      "仅未提交 R2 的编辑阶段使用 Registration.expiresAt 倒计时。已提交请求继续按原操作恢复。",
  },
  "registration-id-taken": {
    ...registration,
    label: "身份证号已被使用",
    errors: {
      "id-number":
        "此身份证号已用于注册。请核对填写内容；如需帮助，请联系医院管理员。",
    },
    basis:
      "R2 id_number_taken：不披露占用者信息。改资料是新的 R2，原有效凭据仍可用；姓名与身份证号仍需本人确认。",
  },
  "registration-mobile-taken": {
    ...result,
    group: registration.group,
    label: "手机号已有账户",
    title: "这个手机号已注册",
    description: "请重新验证手机号，登录已有账户。",
    detail: "需要重新验证这个手机号，才能登录已有账户。",
    actionLabel: "前往登录",
    basis:
      "R2 mobile_taken：旧待注册凭据不能改为已有账户的登录证明，必须重新认证。",
  },
  "registration-limited": {
    ...result,
    group: registration.group,
    label: "注册暂时无法处理",
    title: "请稍后再试",
    description: "暂时无法处理，请稍后再试。",
    detail: "请等待后再试；若验证已过期，先重新验证手机号。",
    action: "limited",
    actionLabel: "继续填写注册资料",
    limited: true,
    resumes: "registration-pending",
    pendingResult: {
      title: "暂时无法查询注册结果",
      description: "本次注册的结果仍未确定。",
      detail:
        "此前注册的结果仍未确定。等待后继续查询本次注册的结果，无需重新验证手机号或重新填写资料。",
      actionLabel: "继续查询注册结果",
    },
    basis:
      "R2 rate_limited：可能来自共享处理名额，不单独证明用户操作频繁。等待 Retry-After；之后先核对原凭据期限，期满重新认证，不延长填表时间。若是在确认已提交注册时受限，等待后回到原待确认状态并保留原资料，不开放编辑。",
  },
  "registration-proof-invalid": {
    ...result,
    group: registration.group,
    label: "注册原证明无效",
    title: "暂时无法确认注册结果",
    description: "请先检查当前登录状态，暂时不要再次提交注册。",
    detail: "",
    action: "open-current",
    actionLabel: "检查登录状态",
    secondaryHome: true,
    basis:
      "R2 authentication_invalid 只拒绝本次原注册证明，不证明原注册失败。本片 R2 页面保留原材料至原期限，停止提供该原请求恢复入口，改为打开独立 Current；当前账户读取不证明原注册完成。这是当前页面的消费者选择，不表示协议一概禁止所有手动恢复。",
  },
  "registration-proof-missing": {
    ...result,
    group: registration.group,
    label: "注册后需重新登录",
    title: "请重新验证手机号",
    description: "请重新验证手机号，再按页面提示继续。",
    detail: "这不表示本次注册失败。",
    actionLabel: "验证手机号",
    basis:
      "R2 verification_required：终止原自动恢复后重新认证，不重复注册。仅凭本地材料丢失则使用下方结果无法确认状态。",
  },
  "result-unavailable": {
    ...result,
    group: registration.group,
    label: "办理结果无法确认",
    title: "暂时无法确认办理结果",
    description: "请先检查当前登录状态，暂时不要重复提交。",
    detail: "",
    action: "open-current",
    actionLabel: "检查登录状态",
    secondaryHome: true,
    basis:
      "本地恢复材料丢失、恢复期结束或 result_unavailable 不证明原办理失败。独立读取当前选择与本人账户，不恢复原 POST，也不从 Current 成功推断历史登录或注册完成。",
  },
  "request-mismatch": {
    ...result,
    group: registration.group,
    label: "注册资料不一致",
    title: "暂时无法确认注册结果",
    description: "请先检查当前登录状态，暂时不要再次提交注册。",
    detail: "",
    action: "open-current",
    actionLabel: "检查登录状态",
    secondaryHome: true,
    basis:
      "R2 request_mismatch 只拒绝本次输入，不改写原输入，也不证明原注册失败。本片 R2 页面保留原材料至原期限，停止提供该原请求恢复入口，改为打开独立 Current；当前账户读取不证明原注册完成。这是当前页面的消费者选择，不表示协议一概禁止所有手动恢复。",
  },
  "current-loading": {
    ...result,
    label: "读取本人资料中",
    title: "正在打开个人账户",
    description: "正在读取账户资料，请稍候。",
    detail: "",
    busy: true,
    transient: true,
    recovery: "current-unavailable",
    symbol: "",
    actionLabel: "正在读取…",
  },
  "current-unavailable": {
    ...result,
    label: "本人资料暂时无法读取",
    title: "暂时无法打开个人账户",
    description: "暂时无法读取账户资料。",
    detail: "请重新加载账户资料，无需重复登录。",
    action: "read-current",
    actionLabel: "重新加载账户资料",
    basis:
      "GET /accounts/current 的断连或 dependency_unavailable 只重读当前账户，不清空有效选择、不暴露旧资料。",
  },
  "current-limited": {
    ...result,
    label: "本人资料读取暂时受限",
    title: "暂时无法打开个人账户",
    description: "账户资料暂时无法读取，请等待后重新加载。",
    detail: "无需重复登录。",
    action: "read-current",
    actionLabel: "60 秒后可重新加载",
    limited: true,
    expired: "current-unavailable",
    basis:
      "GET /accounts/current 的 rate_limited：可能来自共享处理名额，不单独证明用户操作频繁。等待 Retry-After 后只重读当前账户，设计示例为 60 秒；不清空有效选择，也不重新登录。",
  },
  "session-expired": {
    ...result,
    label: "登录已过期",
    title: "请重新登录",
    description: "当前登录已失效，请验证手机号后继续。",
    detail: "请重新登录后继续。",
    actionLabel: "重新登录",
    basis:
      "本人读取明确 authentication_invalid，且仍是原选择时才清除该选择；不能清理后来选中的账户。",
  },
  "session-storage": {
    ...result,
    label: "浏览器无法保留登录",
    title: "暂时无法保留登录",
    description: "浏览器未能保留登录所需的信息。",
    detail: "请检查浏览器是否限制了此网站保存登录信息，再重新登录。",
    actionLabel: "重新登录",
    basis:
      "cookie 无法回传或浏览器持久选择保存失败时不进入已登录页；不得只凭 Session ID 认定已登录。",
  },
  "account-changed": {
    ...result,
    label: "其他标签页切换账户",
    title: "登录状态已变化",
    description: "当前登录的账户已变化，原资料已隐藏。",
    detail: "请重新读取当前账户。",
    action: "read-current",
    actionLabel: "重新读取账户资料",
    basis:
      "选择变化立即遮蔽旧私有内容。标签页通知仅触发重读，旧响应不覆盖新选择。",
  },
  "login-complete": {
    ...result,
    label: "登录完成",
    title: "登录成功",
    description: "欢迎回来。",
    detail: "现在可以继续查看账户资料和已开通的员工服务。",
    symbol: "✓",
    action: "next",
    actionLabel: "继续",
    basis:
      "仅 Session 交接且当前本人读取成功后显示。本稿只展示完成状态，下一步根据可信服务资格分流，不虚构员工授权。",
  },
  "registration-unavailable": {
    ...result,
    label: "首批 R1 · 注册尚未开放",
    step: "手机号验证",
    title: "手机号已验证",
    description: "注册功能尚未开放，暂时无法创建账户。",
    detail: "你可以返回首页，继续浏览公开内容。",
    symbol: "i",
    action: "home",
    actionLabel: "返回首页",
    basis:
      "首批 R1 的 registration 只确认本次手机号验证，不建立新账户或新会话。此阶段不显示号码、不采集实名、不提交 R2，也不清除浏览器可能已有的另一会话。",
  },
  "current-confirmed": {
    ...currentAccount,
    label: "首批 R1 · 当前账户已确认",
  },
  "current-avatar-unavailable": {
    ...currentAccount,
    label: "首批 R1 · 头像加载失败",
    avatar: "unavailable",
  },
  "current-avatar-retry-failed": {
    ...currentAccount,
    label: "首批 R1 · 头像重取后仍失败",
    avatar: "retry-failed",
  },
  "registration-complete": {
    ...currentAccount,
    group: registration.group,
    label: "注册完成",
    step: "注册完成 · 个人账户",
    description: "账户已创建。以下是当前登录的账户信息。",
    created: true,
    basis:
      "R2 的可信 Session 已接纳且当前本人读取成功、选择仍匹配后展示姓名、证件尾四位和头像。对应正式 /account，只读确认并返回公开首页，不承诺资料编辑或员工服务。",
  },
  wechat: { ...wechat, label: "微信 · 等待扫码" },
  "wechat-loading": {
    ...result,
    group: wechat.group,
    step: "微信登录",
    label: "微信登录加载中",
    title: "正在准备微信登录",
    description: "请稍候，登录二维码正在加载。",
    detail: "你也可以使用手机号登录。",
    busy: true,
    action: "phone",
    actionLabel: "使用手机号登录",
    allowAction: true,
    basis: wechat.basis,
  },
  "wechat-limited": {
    ...result,
    group: wechat.group,
    step: "微信登录",
    resumes: "wechat-processing",
    retry: "wechat",
    label: "微信登录暂时受限",
    title: "暂时无法发起微信登录",
    description: "暂时无法处理，请稍后再试。",
    detail: "你也可以先使用手机号登录。",
    action: "limited",
    actionLabel: "60 秒后可继续",
    limited: true,
    basis:
      "目标设计：发起或核对微信授权时 rate_limited，等待 Retry-After；若是在确认原授权时受限，回到原待确认状态，不重新扫码或交换 code。",
  },
  "wechat-expired": {
    ...result,
    group: wechat.group,
    step: "微信登录",
    label: "二维码已过期",
    title: "二维码已过期",
    description: "请更新二维码，再用微信扫码。",
    detail: "如果已在手机上完成确认，请先返回原登录页查看结果。",
    action: "wechat",
    actionLabel: "更新二维码",
    basis:
      "仅原授权确定过期且未进入交换时允许新授权；仍在交换或待确认时不重开。",
  },
  "wechat-processing": {
    ...result,
    group: wechat.group,
    missing: "wechat-proof-missing",
    step: "微信登录",
    label: "微信登录结果待确认",
    title: "正在确认微信登录",
    symbol: "…",
    description: "请稍候，登录结果还在确认中。",
    detail: "无需重复扫码，也不必更新二维码。",
    action: "confirm",
    actionLabel: "继续确认",
    basis:
      "回调或组件消息不证明登录成功。恢复只读原授权，不重新交换 code；原授权不会因为二维码到期而重开。",
  },
  "wechat-unbound": {
    ...result,
    group: wechat.group,
    step: "微信登录",
    label: "微信尚未绑定",
    title: "请先使用手机号登录",
    description: "这个微信尚未绑定司籍处账户。",
    detail: "登录后，可在账户中绑定微信，下次就能扫码登录。",
    actionLabel: "手机号登录",
    basis:
      "可信 wechat_not_bound。不得从读取失败推断未绑定，也不自动注册或绑定微信。",
  },
  "wechat-unavailable": {
    ...result,
    group: wechat.group,
    step: "微信登录",
    label: "微信登录暂不可用",
    title: "微信登录暂不可用",
    description: "暂时无法通过微信完成登录。",
    detail: "你可以先使用手机号登录。",
    actionLabel: "手机号登录",
    basis:
      "目标分支的可信确定拒绝；普通网络中断或原交换未明应留在微信结果待确认。手机号途径继续可用。",
  },
  "wechat-proof-missing": {
    ...result,
    group: wechat.group,
    step: "微信登录",
    label: "微信回跳信息缺失",
    title: "请回到发起登录的页面",
    description: "这个页面无法确认刚才的微信登录。",
    detail: "请回到最初打开二维码的页面继续，或使用手机号登录。",
    actionLabel: "手机号登录",
    basis:
      "网站完成页缺少原标签页证明时不推断失败、不重开授权、不重新交换 code。",
  },
  "wechat-cancelled": {
    ...result,
    group: wechat.group,
    step: "微信登录",
    label: "未完成微信授权",
    title: "未完成微信登录",
    description: "本次微信登录尚未完成。",
    detail: "你可以使用手机号登录，之后再选择是否绑定微信。",
    actionLabel: "手机号登录",
    basis:
      "仅在可信确定拒绝或用户确实取消授权时显示；供应商窗口关闭本身不能证明原操作未完成。",
  },
};

Object.assign(states, {
  "registration-session-storage": {
    ...result,
    group: registration.group,
    label: "账户已创建 · 登录选择无法保存",
    title: "账户已创建，请重新登录",
    description: "浏览器未能保留登录所需的信息。",
    detail: "请检查浏览器的存储设置，再重新登录。这不会重新提交注册。",
    actionLabel: "重新登录",
    created: true,
    basis:
      "已收到可信 R2 Session 后，浏览器选择保存失败不能否认账户已创建。遮蔽本人资料，检查浏览器后重新登录，不退为注册未知或重提 R2。",
  },
  "registration-cleanup-failed": {
    ...result,
    group: registration.group,
    label: "账户已创建 · 临时信息清理失败",
    title: "账户已创建",
    description: "暂时无法打开个人账户。",
    detail: "账户已经创建。请继续登录，无需重新注册。",
    action: "recheck-registration-cleanup",
    actionLabel: "继续登录",
    created: true,
    cleanupPending: true,
    basis:
      "可信 R2 Session 已接收后的本地临时凭据清理失败，不否认已创建事实。只重新检查本地清理，完成后读取当前账户；不再次提交注册。本稿只模拟可见状态。",
  },
  "registration-current-loading": {
    ...states["current-loading"],
    group: registration.group,
    label: "账户已创建 · 正在读取本人",
    title: "账户已创建，正在打开个人账户",
    description: "正在读取账户资料，请稍候。",
    detail: "",
    recovery: "registration-current-unavailable",
    created: true,
    basis:
      "可信 R2 Session 只说明已创建账户与会话。成功读取 accounts/current 前不展示本人资料，不进入员工服务。",
  },
  "registration-current-unavailable": {
    ...states["current-unavailable"],
    group: registration.group,
    label: "账户已创建 · 本人读取失败",
    description: "账户已创建，暂时无法读取账户资料。",
    detail: "请重新加载账户资料，无需重新注册。",
    actionLabel: "重新加载账户资料",
    created: true,
  },
  "registration-current-limited": {
    ...states["current-limited"],
    group: registration.group,
    label: "账户已创建 · 本人读取受限",
    detail: "账户已创建。请等待后重新读取本人资料，无需重新提交注册。",
    expired: "registration-current-unavailable",
    created: true,
  },
  "registration-session-invalid": {
    ...states["session-expired"],
    group: registration.group,
    label: "账户已创建 · 当前登录无效",
    description: "账户已创建，但当前登录已无法继续使用。",
    detail: "请重新登录，不必重新注册。",
    created: true,
  },
  "registration-current-denied": {
    ...result,
    group: registration.group,
    label: "账户已创建 · 本人读取被拒绝",
    title: "暂时无法查看账户资料",
    description: "账户已创建，暂时无法读取本人资料。",
    detail: "你可以返回首页，继续浏览公开内容。",
    action: "home",
    actionLabel: "返回首页",
    created: true,
    basis:
      "accounts/current 的 403 permission_denied 是同源拒绝。隐藏资料，不清理会话、不称登录失效，也不否认已创建账户。",
  },
  "registration-account-changed": {
    ...states["account-changed"],
    group: registration.group,
    label: "注册后当前账户选择已变化",
    detail:
      "此前的注册已完成，当前登录的账户已变化。原资料已隐藏，请重新读取当前账户。",
    actionLabel: "重新读取账户资料",
    selectedAccount: true,
    basis:
      "后来选择优先，原注册或本人读取的迟到响应不能覆盖它。原注册已创建事实与当前选中账户分别表达；重新读取只展示当前选择。",
  },
  "registration-selected-loading": {
    ...states["current-loading"],
    group: registration.group,
    label: "重新读取当前选中账户",
    title: "正在打开个人账户",
    detail: "",
    recovery: "registration-account-changed",
    selectedAccount: true,
  },
  "registration-selected-account": {
    ...currentAccount,
    group: registration.group,
    label: "当前选中账户已重新确认",
    selectedAccount: true,
    basis:
      "当前选中的账户已可信读取。这里只说明当前身份，不把先前注册完成归给后来选中的账户。旧资料未沿用。",
  },
  "registration-avatar-unavailable": {
    ...states["registration-complete"],
    label: "注册完成 · 头像图片加载失败",
    avatar: "unavailable",
    basis:
      "R2 后本人 JSON 已可信读取，只有图片失败。保留姓名与尾号，不推断退出或注册失败；重取当前链接并最多重试一次。",
  },
  "registration-avatar-retry-failed": {
    ...states["registration-complete"],
    label: "注册完成 · 头像重试一次仍失败",
    avatar: "retry-failed",
    basis:
      "已重取链接并重试一次，图片仍失败时保留占位和可信本人资料，不无限重试或重新注册。",
  },
});

const accountCheck = {
  ...result,
  group: "当前个人账户",
  step: "登录状态",
  inspectCurrent: true,
  action: "open-current",
  actionLabel: "重新检查",
  secondaryHome: true,
  detail: "",
  basis:
    "独立读取浏览器当前选择，再按 accounts/current 读取本人。成功只证明当前账户，不证明此前登录或注册完成；没有可信选择、读取失败、失效与拒绝分别展示，未成功前不显示旧资料。",
};
Object.assign(states, {
  "account-check-loading": {
    ...accountCheck,
    label: "个人账户 · 正在读取",
    title: "正在检查登录状态",
    description: "请稍候。",
    busy: true,
    transient: true,
    recovery: "account-check-unavailable",
    actionLabel: "正在检查…",
  },
  "account-check-none": {
    ...accountCheck,
    label: "个人账户 · 没有登录选择",
    title: "请使用手机号登录",
    description: "请验证手机号后继续。",
    detail: "将在新标签页打开登录，当前页面会保留。",
    action: "phone-new-tab",
    actionLabel: "手机号登录",
    basis:
      "独立 Current 没有可信登录选择。原生链接只在新标签页打开手机登录起点，用户须重新填写手机号并验证；不清理原页材料，不自动发码，原办理的未知事实不变。",
  },
  "account-check-storage": {
    ...accountCheck,
    label: "个人账户 · 登录存储不可用",
    title: "暂时无法检查登录状态",
    description: "浏览器无法读取登录信息。",
    detail: "请先重试。仍无法确认时，可在新标签页重新登录。",
    secondaryPhone: true,
    basis:
      "已确认浏览器选择存储不可用或记录损坏，按网站会话规则遮蔽旧资料，并提供用户显式重新登录。主操作仍重新检查，次操作以原生新标签页链接打开手机登录起点，用户重新填号并验证；不清理原页材料，也不改变原办理的未知事实。普通技术读取失败沿用 account-check-unavailable，不默认要求重新登录。",
  },
  "account-check-unavailable": {
    ...accountCheck,
    label: "个人账户 · 资料读取失败",
    title: "暂时无法检查登录状态",
    description: "请稍后重试，或先浏览首页。",
  },
  "account-check-limited": {
    ...accountCheck,
    label: "个人账户 · 读取受限",
    title: "请稍后再检查",
    description: "暂时无法检查登录状态，请稍后重试。",
    actionLabel: "60 秒后可重新检查",
    limited: true,
    expired: "account-check-unavailable",
  },
  "account-check-invalid": {
    ...accountCheck,
    label: "个人账户 · 当前登录失效",
    title: "请重新登录",
    description: "当前登录已失效。",
    detail: "将在新标签页打开登录，当前页面会保留。",
    action: "phone-new-tab",
    actionLabel: "手机号登录",
    basis:
      "独立 Current 的 authentication_invalid 只说明当前选择不可用。原生链接只在新标签页打开手机登录起点，用户须重新填写手机号并验证；不清理原页材料，不自动发码，原办理的未知事实不变。",
  },
  "account-check-denied": {
    ...accountCheck,
    label: "个人账户 · 读取被拒绝",
    title: "暂时无法完成检查",
    description: "本次检查未能完成。",
    detail: "你可以返回首页，继续浏览公开内容。",
    action: "home",
    actionLabel: "返回首页",
    secondaryHome: false,
  },
  "account-check-changed": {
    ...accountCheck,
    label: "个人账户 · 当前选择变化",
    title: "登录状态有变化",
    description: "请重新检查，再继续使用。",
    detail: "",
  },
  "account-check-confirmed": {
    ...currentAccount,
    group: accountCheck.group,
    label: "个人账户 · 当前账户已读取",
    title: "当前已登录",
    description: "以下是当前登录的账户。",
    step: "登录状态",
    inspectCurrent: true,
    basis: accountCheck.basis,
  },
});

const r1States = new Set([
  "registration-unavailable",
  "current-confirmed",
  "current-avatar-unavailable",
  "current-avatar-retry-failed",
]);
if (r1Release) {
  states["result-unavailable"].group = result.group;
  states["current-loading"].detail = "";
  states["session-expired"].detail = "请重新登录后继续。";
  document.querySelector(".preview-caption strong").textContent =
    "HALO · 首批 R1 预览";
  document.querySelector('#demo-path option[value="new"]').textContent =
    "新号码 · 注册未开放";
  document.getElementById("preview-protocol").textContent =
    "首批只演示已有账户短信登录与可信本人确认。新号码在验证后说明注册尚未开放，不进入实名表单。安全验证为展示占位，不调用供应商。";
  document.getElementById("preview-session").textContent =
    "取得 Session 后仍须读取本人成功。当前账户只读展示姓名、身份证尾号与头像；本稿不实现持久会话、跨页恢复或多标签页协调。";
  document.getElementById("preview-delivery").textContent =
    "首批结果只返回公开首页，不开放注册、微信登录、员工服务或退出操作。去掉 release=r1 参数可查看完整目标稿，两者均不代表生产功能已交付。";
  const introduction = document.querySelector(".service-introduction");
  introduction.setAttribute("aria-label", "账户登录说明");
  introduction.querySelector(".section-kicker").textContent =
    "账户登录 · 从这里开始";
  introduction.querySelector("h2").textContent = "账户登录";
  introduction.querySelector(".introduction-copy").textContent =
    "登录后确认当前账户。文化与科普内容无需登录即可浏览。";
  document.querySelector(".authentication-footer span").textContent =
    "司籍处 · 账户登录";
}
if (r2Release) {
  document.querySelector(".preview-caption strong").textContent =
    "HALO · 首批 R2 预览";
  document.getElementById("preview-protocol").textContent =
    "本片演示短信登录、首次注册和可信本人确认。首次提交前保存失败、已提交待确认与账户已创建分别展示；不调用供应商或业务接口。";
  document.getElementById("preview-session").textContent =
    "注册取得可信 Session 后仍须成功读取本人账户。只读确认姓名、身份证尾四位与头像，再返回公开首页；本稿不实现生产恢复、会话或跨标签页协调。";
  document.getElementById("preview-delivery").textContent =
    "首批 R2 不开放微信、员工服务或资料维护。注册完成与已有账户登录均进入只读当前账户，对应正式前端 /account；本稿使用同页模板演示。";
  const introduction = document.querySelector(".service-introduction");
  introduction.setAttribute("aria-label", "账户注册与登录说明");
  introduction.querySelector(".section-kicker").textContent =
    "本人账户 · 从这里开始";
  introduction.querySelector("h2").textContent = "注册与登录";
  introduction.querySelector(".introduction-copy").textContent =
    "验证手机号，完成注册或登录。";
  document.querySelector(".authentication-footer span").textContent =
    "司籍处 · 本人账户";
  for (const id of [
    "current-confirmed",
    "current-avatar-unavailable",
    "current-avatar-retry-failed",
  ])
    states[id] = {
      ...states[id],
      label: states[id].label.replace("首批 R1 · ", ""),
      basis:
        "已有账户短信登录后可信读取当前本人，且响应仍匹配当前选择；只读确认并返回公开首页，不读取员工服务资格。",
    };
}
const visibleStates = new Set(
  Object.entries(states)
    .filter(([id, state]) =>
      r1Release
        ? ![registration.group, wechat.group].includes(state.group) &&
          id !== "login-complete" &&
          id !== "registration-complete"
        : r2Release
          ? state.group !== wechat.group &&
            id !== "registration-unavailable" &&
            id !== "login-complete"
          : !r1States.has(id),
    )
    .map(([id]) => id),
);

const picker = document.getElementById("scenario-picker");
const pathPicker = document.getElementById("demo-path");
const currentPicker = document.getElementById("demo-current");
const previewToggle = document.querySelector(".preview-toggle");
const content = document.getElementById("form-content");
const notice = document.getElementById("state-notice");
const dialog = document.getElementById("navigation-dialog");
const model = { mobile: "", code: "", name: "", idNumber: "" };
const examples = {
  mobile: "13800000000",
  code: "012345",
  name: "示例用户",
  idNumber: "110101199001010015",
};
let current = "phone";
let challengePassed = false;
let transitionTimer;
let clockTimer;
let resendAt = 0;
let registrationUntil = 0;
let dialogAction;
let completedRegistration = false;
let r2Context = false;
let registeredIdentity = { name: examples.name, suffix: "0015" };
let selectedIdentity = { name: "另一位示例用户", suffix: "0007" };
let currentIdentity = { name: "示例用户", suffix: "012X" };
let recordedHash = location.hash;
let submittedRegistration = null;
let unconfirmed = "";
let limit = null;
const returnParams = new URLSearchParams(location.search);
const logoutReturnIntent =
  returnParams.getAll("return").length === 1 &&
  [
    "logout-result",
    "all-logout-result",
    "service-termination",
    "mobile-change",
  ].includes(returnParams.get("return"));
const serviceTerminationReturnIntent =
  logoutReturnIntent && returnParams.get("return") === "service-termination";
const mobileChangeReturnIntent =
  logoutReturnIntent && returnParams.get("return") === "mobile-change";
const logoutReturnExample = document.getElementById("logout-return-example");
document.getElementById("logout-return-settings").hidden = !logoutReturnIntent;
const allLogoutReturnIntent =
  logoutReturnIntent && returnParams.get("return") === "all-logout-result";
logoutReturnExample.querySelector('[value="scope-unverified"]').hidden =
  !allLogoutReturnIntent;
if (
  allLogoutReturnIntent &&
  returnParams.getAll("example").length === 1 &&
  returnParams.get("example") === "scope-unverified"
)
  logoutReturnExample.value = "scope-unverified";

function logoutReturnHref() {
  if (
    !logoutReturnIntent ||
    current !== "login-complete" ||
    pathPicker.value !== "existing"
  )
    return null;
  const allowedExamples = ["same-account", "other-account", "material-missing"];
  if (allLogoutReturnIntent) allowedExamples.push("scope-unverified");
  const example = allowedExamples.includes(logoutReturnExample.value)
    ? logoutReturnExample.value
    : "same-account";
  if (serviceTerminationReturnIntent)
    return `halo-service-termination.html?resume=service-termination&example=${example}`;
  if (mobileChangeReturnIntent)
    return `halo-mobile-change.html?resume=mobile-change&example=${example}${returnParams.get("known") === "completed" ? "&known=completed" : ""}`;
  return allLogoutReturnIntent
    ? `halo-all-logout.html?resume=all-logout-result&example=${example}`
    : `halo-logout.html?resume=logout-result&example=${example}`;
}

logoutReturnExample.addEventListener("change", () => {
  if (current === "login-complete") show(current, false, false);
});
let codeComposing = false;

previewToggle.addEventListener("click", () => {
  const expanded = previewToggle.getAttribute("aria-expanded") === "true";
  previewToggle.setAttribute("aria-expanded", String(!expanded));
  previewToggle.textContent = expanded ? "展开预览工具" : "收起预览工具";
});

for (const group of new Set([...visibleStates].map((id) => states[id].group))) {
  const options = document.createElement("optgroup");
  options.label = group;
  for (const [id, state] of Object.entries(states)) {
    if (!visibleStates.has(id) || state.group !== group) continue;
    const option = document.createElement("option");
    option.value = id;
    option.textContent = state.label;
    options.append(option);
  }
  picker.append(options);
}

function fieldError(id, message) {
  const inputs = id === "code" ? codeCells() : [document.getElementById(id)];
  const error = document.getElementById(`${id}-error`);
  for (const input of inputs)
    input.setAttribute("aria-invalid", String(Boolean(message)));
  error.hidden = !message;
  error.textContent = message || "";
}

function codeCells() {
  return [...content.querySelectorAll(".code-cell")];
}

function fillCode(start, text) {
  const cells = codeCells();
  const digits = text;
  start = Math.min(start, model.code.length);
  if (digits.length === cells.length) start = 0;
  if (!/^[0-9]+$/.test(digits) || start + digits.length > cells.length) {
    renderCode();
    fieldError("code", "只能填入数字验证码，请核对后重新粘贴。");
    return false;
  }
  model.code =
    model.code.slice(0, start) +
    digits +
    model.code.slice(start + digits.length);
  renderCode();
  fieldError("code", "");
  focusCode(start + digits.length);
  return true;
}

function renderCode() {
  codeCells().forEach((cell, index) => {
    cell.value = model.code[index] || "";
  });
}

function focusCode(index) {
  const cells = codeCells();
  const target = Math.max(
    0,
    Math.min(index, model.code.length, cells.length - 1),
  );
  cells[target].focus();
}

function setupCode(locked) {
  const cells = codeCells();
  codeComposing = false;
  const remove = (index) => {
    if (index < 0 || index >= model.code.length) return;
    model.code = model.code.slice(0, index) + model.code.slice(index + 1);
    renderCode();
    fieldError("code", "");
    focusCode(index);
  };
  cells.forEach((cell, index) => {
    cell.value = model.code[index] || "";
    cell.disabled = locked;
    let compositionEnded = false;
    cell.addEventListener("focus", () => {
      if (index > model.code.length) {
        focusCode(model.code.length);
        return;
      }
      cell.select();
    });
    cell.addEventListener("compositionstart", () => {
      codeComposing = true;
    });
    cell.addEventListener("compositionend", (event) => {
      codeComposing = false;
      compositionEnded = true;
      setTimeout(() => {
        compositionEnded = false;
      }, 0);
      if (event.data) fillCode(index, event.data);
      else renderCode();
    });
    cell.addEventListener("input", (event) => {
      if (event.isComposing || codeComposing) return;
      if (compositionEnded || event.inputType?.includes("Composition")) {
        renderCode();
        return;
      }
      if (event.inputType?.startsWith("delete")) {
        const backward = event.inputType.includes("Backward");
        remove(
          index < model.code.length ? index : backward ? index - 1 : index,
        );
        renderCode();
        return;
      }
      fillCode(index, event.data ?? cell.value);
    });
    cell.addEventListener("paste", (event) => {
      event.preventDefault();
      if (codeComposing) return;
      fillCode(index, event.clipboardData?.getData("text") || "");
    });
    cell.addEventListener("keydown", (event) => {
      if (event.isComposing || codeComposing) return;
      const target = {
        ArrowLeft: index - 1,
        ArrowRight: index + 1,
        Home: 0,
        End: model.code.length,
      }[event.key];
      if (target !== undefined) {
        event.preventDefault();
        focusCode(target);
      } else if (event.key === "Backspace" || event.key === "Delete") {
        event.preventDefault();
        remove(
          index < model.code.length
            ? index
            : event.key === "Backspace"
              ? index - 1
              : index,
        );
      }
    });
  });
}

function inform(message, tone) {
  notice.textContent = message || "";
  notice.hidden = !message;
  notice.dataset.tone = tone || "notice";
}

function show(id, focus = true, record = true, unavailableOperation = "") {
  if (!visibleStates.has(id)) id = "phone";
  const unavailableName =
    id === "result-unavailable"
      ? unavailableOperation ||
        (r1Release ||
        unconfirmed === "login-pending" ||
        states[current].group === code.group
          ? "登录"
          : unconfirmed === "registration-pending" ||
              states[current].group === registration.group ||
              r2Release
            ? "注册"
            : "办理")
      : "";
  if (
    states[id].screen === "registration" &&
    !states[id].locked &&
    registrationUntil &&
    registrationUntil <= Date.now()
  )
    id = "registration-expired";
  if (dialog.open) dialog.close();
  clearTimeout(transitionTimer);
  clearInterval(clockTimer);
  current = id;
  let state = states[id];
  if (unavailableName)
    state = {
      ...state,
      title:
        unavailableName === "登录"
          ? "登录状态待确认"
          : unavailableName === "注册"
            ? "暂时无法确认注册结果"
            : "暂时无法确认办理结果",
      description:
        unavailableName === "登录"
          ? "请先检查当前登录状态，暂时不用再次获取验证码。"
          : unavailableName === "注册"
            ? "请先检查当前登录状态，暂时不要再次提交注册。"
            : state.description,
    };
  if (state.group === registration.group) r2Context = true;
  if (state.created) {
    completedRegistration = true;
    if (!state.cleanupPending) clearRegistration();
  }
  if (state.selectedAccount) completedRegistration = false;
  if (state.selectedAccount || id === "account-changed")
    currentIdentity = { ...selectedIdentity };
  if (
    [
      "send-pending",
      "login-pending",
      "registration-pending",
      "wechat-processing",
    ].includes(id) ||
    (state.screen === "registration" && state.pending)
  )
    unconfirmed = state.screen === "registration" ? "registration-pending" : id;
  else if (
    !state.transient &&
    !(state.resumes && unconfirmed === state.resumes)
  )
    unconfirmed = "";
  const pendingLimited = Boolean(
    state.pendingResult && unconfirmed === state.resumes,
  );
  if (pendingLimited) state = { ...state, ...state.pendingResult };
  if (
    id === "registration-expired" &&
    !(registrationUntil && registrationUntil <= Date.now())
  )
    registrationUntil = Date.now() - 1;
  const entry = state.transient
    ? state.recovery
    : state.resumes && unconfirmed === state.resumes
      ? unconfirmed
      : id;
  if (record && entry) {
    recordedHash = `#${entry}`;
    history.replaceState(null, "", recordedHash);
  }
  picker.value = id;
  document.title = `${state.title} · 司籍处`;
  document.getElementById("page-step").textContent = state.step;
  document.getElementById("page-title").textContent = state.title;
  document.getElementById("page-description").textContent = state.description;
  document.getElementById("state-basis").textContent = state.basis;
  inform(state.notice, state.tone);
  content.replaceChildren(
    document.getElementById(`${state.screen}-template`).content.cloneNode(true),
  );
  content.setAttribute("aria-busy", String(Boolean(state.busy)));

  if (state.screen === "phone") {
    if (r1Release || r2Release) content.querySelector(".alternative").remove();
    if (
      [
        "challenge-loading",
        "challenge-expired",
        "challenge-unavailable",
        "challenge-invalid",
      ].includes(id)
    )
      challengePassed = false;
    const label = document.getElementById("challenge-label");
    if (challengePassed && !state.busy) {
      label.textContent = "验证已通过";
      document.getElementById("challenge-description").textContent =
        "可以继续获取验证码";
      content.querySelector(".challenge-symbol").textContent = "✓";
      const challenge = content.querySelector('[data-action="challenge"]');
      challenge.textContent = "已完成";
      challenge.disabled = true;
    }
    if (state.challenge === "loading") {
      label.textContent = "安全验证加载中";
      content.querySelector('[data-action="challenge"]').disabled = true;
      const submit = content.querySelector('[type="submit"]');
      submit.disabled = true;
      submit.textContent = "等待安全验证…";
    }
    if (state.busy) {
      label.textContent = "验证已通过";
      document.getElementById("challenge-description").textContent =
        "正在获取短信验证码";
      content.querySelector(".challenge-symbol").textContent = "✓";
      content.querySelector('[data-action="challenge"]').textContent = "已完成";
    }
    content.querySelector("form").addEventListener("submit", submitPhone);
  }
  if (state.screen === "code") {
    if (r1Release)
      content.querySelector(".form-help").textContent =
        "验证后，已有账户可登录；注册功能暂未开放。";
    if (!model.mobile) model.mobile = "13800000000";
    document.getElementById("mobile-summary").textContent =
      `${model.mobile.slice(0, 3)} **** ${model.mobile.slice(-4)}`;
    if (state.resendReady) resendAt = Date.now() - 1;
    else if (!resendAt) resendAt = Date.now() + 60_000;
    content.querySelector("form").addEventListener("submit", submitCode);
    setupCode(Boolean(state.locked));
    updateResend();
    clockTimer = setInterval(updateResend, 1000);
  }
  if (state.screen === "registration") {
    if (!registrationUntil) registrationUntil = Date.now() + 600_000;
    document.getElementById("registration-window").hidden = Boolean(
      state.locked,
    );
    if (state.locked) {
      const step = content.querySelector(".registration-steps [aria-current]");
      step.replaceChildren(
        step.querySelector("span"),
        document.createTextNode("查看结果"),
      );
    }
    content
      .querySelector("form")
      .addEventListener("submit", submitRegistration);
    if (!state.locked) {
      updateRegistrationClock();
      clockTimer = setInterval(updateRegistrationClock, 1000);
    }
  }
  if (state.screen === "registration" && state.locked) {
    submittedRegistration ??= {
      name: examples.name,
      idNumber: examples.idNumber,
    };
    Object.assign(model, submittedRegistration);
  }
  for (const input of content.querySelectorAll("input:not(.code-cell)")) {
    input.value = model[input.name];
    input.disabled = Boolean(state.locked);
    input.addEventListener("input", () => {
      model[input.name] = input.value;
      fieldError(input.id, "");
    });
  }
  for (const [field, message] of Object.entries(state.errors || {}))
    fieldError(field, message);
  if (state.locked) {
    for (const button of content.querySelectorAll("button")) {
      if (button.dataset.action === "leave-registration") continue;
      button.disabled = true;
    }
    const submit = content.querySelector('[type="submit"]');
    submit.textContent = state.pending
      ? "查看注册结果"
      : state.screen === "phone"
        ? "正在发送…"
        : state.screen === "code"
          ? "正在验证…"
          : "正在注册…";
    if (state.pending) submit.disabled = false;
  }
  if (state.screen === "registration" && state.submitLabel)
    content.querySelector('[type="submit"]').textContent = state.submitLabel;
  if (state.screen === "result") {
    const symbol = content.querySelector(".result-symbol");
    symbol.textContent = state.busy ? "" : state.symbol;
    symbol.dataset.busy = String(Boolean(state.busy));
    symbol.dataset.tone =
      state.symbol === "✓" || state.busy ? "success" : "notice";
    const detail = document.getElementById("result-detail");
    detail.textContent = state.detail;
    detail.hidden = !state.detail;
    document.getElementById("result-home").hidden = !state.secondaryHome;
    const button = document.getElementById("result-action");
    const link = document.getElementById("result-link");
    const newTabLogin = state.action === "phone-new-tab";
    const showPhoneLink = newTabLogin || Boolean(state.secondaryPhone);
    button.hidden = newTabLogin;
    link.hidden = !showPhoneLink;
    link.classList.toggle("primary", newTabLogin);
    link.classList.toggle("secondary", !newTabLogin);
    if (showPhoneLink) {
      link.textContent = "手机号登录";
      link.href = `halo-authentication.html${r1Release ? "?release=r1" : r2Release ? "?release=r2" : ""}#phone`;
    }
    button.textContent = state.actionLabel;
    button.dataset.action = state.action;
    if (logoutReturnHref()) {
      if (logoutReturnExample.value === "other-account") {
        button.textContent = "查看当前账户";
        detail.textContent = "登录已完成，接下来读取当前账户资料。";
      } else if (logoutReturnExample.value === "material-missing") {
        button.textContent = "返回登录前页面";
        detail.textContent = "登录已完成，可以返回之前的页面。";
      } else {
        button.textContent = mobileChangeReturnIntent
          ? "查看更换手机号结果"
          : serviceTerminationReturnIntent
            ? "继续核对服务结束结果"
            : "继续查看退出结果";
        detail.textContent = mobileChangeReturnIntent
          ? "登录已完成，可以查询本次手机号更换。登录成功不代表此前换号已经完成。"
          : serviceTerminationReturnIntent
            ? "登录已完成，可以继续核对此前结束员工服务的申请。不会结束后来新开通的服务。"
            : "登录已完成，现在可以继续查看此前退出的结果。不会退出当前的新登录。";
      }
    }
    button.disabled = Boolean(
      state.limited || (state.busy && !state.allowAction),
    );
    if (state.limited) {
      if (limit?.id !== id) limit = { id, until: Date.now() + 60_000 };
      const tick = () => {
        const remaining = Math.max(
          0,
          Math.ceil((limit.until - Date.now()) / 1000),
        );
        if (state.pendingResult) {
          const readyLabel = pendingLimited
            ? state.actionLabel
            : id === "send-limited"
              ? "返回获取验证码"
              : id === "login-limited"
                ? "继续验证"
                : registrationUntil > Date.now()
                  ? "继续填写注册资料"
                  : "重新验证手机号";
          const waitingLabel = pendingLimited
            ? "可查询"
            : id === "send-limited"
              ? "可返回获取验证码"
              : id === "login-limited"
                ? "可继续验证"
                : "可继续";
          button.textContent = remaining
            ? `${remaining} 秒后${waitingLabel}`
            : readyLabel;
        } else if (remaining < 60)
          button.textContent = state.inspectCurrent
            ? remaining
              ? `${remaining} 秒后可重新检查`
              : "重新检查"
            : remaining
              ? `${remaining} 秒后可继续`
              : "继续";
        button.disabled = Boolean(remaining);
        if (!remaining) clearInterval(clockTimer);
      };
      tick();
      clockTimer = setInterval(tick, 1000);
    }
  }
  if (state.screen === "current") {
    if (state.inspectCurrent) {
      document.getElementById("current-name").textContent =
        currentIdentity.name;
      document.getElementById("current-id-suffix").textContent =
        currentIdentity.suffix;
    } else if (state.group === registration.group) {
      const identity = state.selectedAccount
        ? selectedIdentity
        : registeredIdentity;
      document.getElementById("current-name").textContent = identity.name;
      document.getElementById("current-id-suffix").textContent =
        identity.suffix;
    }
    if (!state.inspectCurrent)
      currentIdentity = {
        name: document.getElementById("current-name").textContent,
        suffix: document.getElementById("current-id-suffix").textContent,
      };
    const failed = Boolean(state.avatar);
    content
      .querySelector(".current-avatar-image")
      .toggleAttribute("hidden", failed);
    content.querySelector(".current-avatar-placeholder").hidden = !failed;
    const error = content.querySelector(".current-avatar-error");
    error.hidden = !failed;
    error.textContent =
      state.avatar === "retry-failed"
        ? "头像仍暂时无法加载。已重新加载一次，这不影响当前账户确认。"
        : "头像暂时无法加载，这不影响当前账户确认。";
    content.querySelector('[data-action="retry-avatar"]').hidden =
      state.avatar !== "unavailable";
  }
  if (
    id.endsWith("-complete") ||
    state.screen === "current" ||
    id === "registration-unavailable"
  ) {
    for (const key of Object.keys(model)) model[key] = "";
    clearRegistration();
  }
  if (focus) document.getElementById("page-title").focus();
}

function updateResend() {
  const button = content.querySelector('[data-action="resend"]');
  if (!button) return;
  const remaining = Math.max(0, Math.ceil((resendAt - Date.now()) / 1000));
  button.disabled = Boolean(remaining || states[current].locked);
  button.textContent = remaining ? `${remaining} 秒后重新获取` : "重新获取";
}

function clearRegistration() {
  model.name = "";
  model.idNumber = "";
  registrationUntil = 0;
  submittedRegistration = null;
}

function expireRegistration() {
  model.name = "";
  model.idNumber = "";
  submittedRegistration = null;
  show("registration-expired");
}

function updateRegistrationClock() {
  const remaining = Math.max(
    0,
    Math.ceil((registrationUntil - Date.now()) / 1000),
  );
  const clock = document.getElementById("registration-clock");
  if (!clock) return;
  if (!remaining) {
    expireRegistration();
    return;
  }
  clock.textContent = `${String(Math.floor(remaining / 60)).padStart(2, "0")}:${String(remaining % 60).padStart(2, "0")}`;
}

function transition(id, next) {
  show(id);
  transitionTimer = setTimeout(() => {
    if (current === id) next();
  }, 750);
}

function submitPhone(event) {
  event.preventDefault();
  if (states[current].locked) return;
  if (
    !/^(?:13[0-9]|14[01456879]|15[0-35-9]|16[2567]|17[0-8]|18[0-9]|19[0-35-9])[0-9]{8}$/.test(
      model.mobile,
    )
  ) {
    fieldError("mobile", "请填写正确的 11 位中国大陆手机号，不含空格或 +86。");
    document.getElementById("mobile").focus();
    return;
  }
  if (!challengePassed) {
    inform("请先完成安全验证。");
    content.querySelector('[data-action="challenge"]').focus();
    return;
  }
  model.code = "";
  challengePassed = false;
  transition("sending", () => {
    resendAt = Date.now() + 60_000;
    show("code");
  });
}

function submitCode(event) {
  event.preventDefault();
  if (states[current].locked || codeComposing) return;
  if (!/^[0-9]{6}$/.test(model.code)) {
    fieldError("code", "请输入短信中的 6 位数字验证码。");
    const cells = codeCells();
    (cells.find((cell) => !cell.value) || cells[0]).focus();
    return;
  }
  transition("verifying", () => {
    model.code = "";
    completedRegistration = false;
    if (pathPicker.value === "new") {
      if (r1Release) {
        show("registration-unavailable");
        return;
      }
      registrationUntil = Date.now() + 600_000;
      show("registration");
    } else confirmCurrent();
  });
}

function confirmCurrent() {
  const registered = completedRegistration && !r1Release;
  transition(
    registered
      ? "registration-current-loading"
      : r2Context
        ? "registration-selected-loading"
        : "current-loading",
    () =>
      show(
        registered
          ? "registration-complete"
          : r2Context
            ? "registration-selected-account"
            : r1Release || r2Release
              ? "current-confirmed"
              : "login-complete",
      ),
  );
}

function openCurrentAccount() {
  const outcome = currentPicker.value;
  limit = null;
  transition("account-check-loading", () => {
    if (outcome === "changed") {
      currentIdentity = { name: "另一位示例用户", suffix: "0007" };
      currentPicker.value = "ready";
      show("account-check-changed");
    } else
      show(
        outcome === "ready"
          ? "account-check-confirmed"
          : `account-check-${outcome}`,
      );
  });
}

function submitRegistration(event) {
  event.preventDefault();
  if (states[current].recheckStorage) {
    show("registration");
    if (current === "registration") inform("请核对资料，再提交注册。");
    return;
  }
  if (states[current].pending) {
    if (states[current].recheckRecovery || states[current].resumePending) {
      show("registration-pending");
      return;
    }
    inform("本次注册结果尚未确定，请稍后继续查看。无需再次填写资料。");
    return;
  }
  if (states[current].locked) return;
  if (Date.now() >= registrationUntil) {
    expireRegistration();
    return;
  }
  const name = model.name.trim().normalize("NFC");
  const nameValid =
    name.length > 0 &&
    [...name].length <= 64 &&
    !/[\u0000-\u001f\u007f-\u009f]/.test(name);
  const idValid = /^[0-9]{17}[0-9Xx]$/.test(model.idNumber);
  fieldError(
    "name",
    nameValid ? "" : "请填写身份证上的完整姓名，最多 64 个字。",
  );
  fieldError(
    "id-number",
    idValid ? "" : "请核对 18 位居民身份证号，末位可为 X。",
  );
  if (!nameValid || !idValid) {
    document.getElementById(nameValid ? "id-number" : "name").focus();
    return;
  }
  completedRegistration = true;
  registeredIdentity = { name, suffix: model.idNumber.slice(-4).toUpperCase() };
  submittedRegistration = { name: model.name, idNumber: model.idNumber };
  transition("registering", () => {
    clearRegistration();
    confirmCurrent();
  });
}

function openDialog(
  title,
  description,
  confirmText,
  action,
  cancelText = "继续填写",
) {
  document.getElementById("dialog-title").textContent = title;
  document.getElementById("dialog-description").textContent = description;
  document.getElementById("dialog-confirm").textContent = confirmText;
  const cancel = dialog.querySelector(".secondary");
  cancel.hidden = !cancelText;
  cancel.textContent = cancelText || "";
  dialogAction = action;
  dialog.showModal();
}

function confirmLeaveRegistration() {
  const submitted = Boolean(states[current].locked);
  openDialog(
    "返回登录？",
    submitted
      ? "本次注册已经提交。离开不会撤销注册，请先查看注册结果。"
      : "尚未提交的姓名和身份证号将清除，下次需要重新填写。",
    submitted ? "查看注册结果" : "返回登录",
    () => {
      if (!submitted) returnToPhone();
      else if (states[current].locked) show("registration-pending");
    },
    submitted ? "留在此页" : "继续填写",
  );
}

function returnToPhone() {
  model.code = "";
  challengePassed = false;
  clearRegistration();
  completedRegistration = false;
  r2Context = false;
  show("phone");
}

document.addEventListener("click", (event) => {
  const button = event.target.closest("[data-action]");
  if (!button || button.disabled) return;
  switch (button.dataset.action) {
    case "challenge":
      challengePassed = true;
      document.getElementById("challenge-label").textContent = "验证已通过";
      document.getElementById("challenge-description").textContent =
        "可以继续获取验证码";
      content.querySelector(".challenge-symbol").textContent = "✓";
      button.textContent = "已完成";
      button.disabled = true;
      inform("");
      break;
    case "phone":
    case "change-mobile": {
      const created = Boolean(states[current].created);
      const mobileTaken = current === "registration-mobile-taken";
      returnToPhone();
      if (created || mobileTaken) pathPicker.value = "existing";
      if (created) {
        r2Context = true;
        selectedIdentity = { ...registeredIdentity };
      }
      break;
    }
    case "wechat":
      show("wechat");
      break;
    case "home":
      location.href = "halo-home.html";
      break;
    case "resend":
      returnToPhone();
      inform("请重新完成安全验证，再获取验证码。");
      break;
    case "resume":
      show(states[current].resumes);
      break;
    case "confirm":
      inform(
        current === "send-pending"
          ? "本次发送结果尚未确定，请稍后继续查看。无需重新获取验证码。"
          : current === "login-pending"
            ? "本次登录结果尚未确定，请稍后继续查看。无需重新获取验证码。"
            : "仍在确认，请稍后继续查看。无需重新发起。",
      );
      break;
    case "limited":
      continueAfterLimit(current);
      break;
    case "read-current":
      if (states[current].limited) limit = null;
      confirmCurrent();
      break;
    case "open-current":
      openCurrentAccount();
      break;
    case "recheck-registration-cleanup":
      clearRegistration();
      confirmCurrent();
      break;
    case "retry-avatar":
      show(
        states[current].group === registration.group
          ? "registration-avatar-retry-failed"
          : "current-avatar-retry-failed",
      );
      break;
    case "leave-registration":
      confirmLeaveRegistration();
      break;
    case "next":
      location.href = logoutReturnHref() || "halo-services.html#active";
      break;
    case "close-dialog":
      dialog.close();
      break;
  }
});

document.getElementById("dialog-confirm").addEventListener("click", () => {
  dialog.close();
  dialogAction?.();
});
function resetContext() {
  for (const key of Object.keys(model)) model[key] = "";
  clearRegistration();
  unconfirmed = "";
  limit = null;
  resendAt = 0;
  challengePassed = false;
  r2Context = false;
}

picker.addEventListener("change", () => {
  const target = states[picker.value];
  if (target.selectedAccount)
    selectedIdentity = { name: "另一位示例用户", suffix: "0007" };
  if (target.group !== states[current].group) {
    resetContext();
    if (target.group === registration.group)
      registrationUntil = Date.now() + 600_000;
  }
  completedRegistration = pathPicker.value === "new";
  show(picker.value, false);
});
pathPicker.addEventListener("change", () => {
  completedRegistration = pathPicker.value === "new";
});
document.querySelector(".skip-link").addEventListener("click", (event) => {
  event.preventDefault();
  document.getElementById("authentication").focus();
});
document.getElementById("fill-example").addEventListener("click", () => {
  for (const input of content.querySelectorAll("input:not(.code-cell)")) {
    if (input.disabled) continue;
    input.value = examples[input.name];
    model[input.name] = input.value;
    fieldError(input.id, "");
  }
  const cells = codeCells();
  if (cells.length && !cells[0].disabled) fillCode(0, examples.code);
});
window.addEventListener("popstate", () => {
  history.replaceState(
    null,
    "",
    `${location.pathname}${location.search}${recordedHash}`,
  );
});
window.addEventListener("beforeunload", (event) => {
  const state = states[current];
  if (
    state.screen === "registration" &&
    !state.locked &&
    (model.name || model.idNumber)
  )
    event.preventDefault();
});
window.addEventListener("pagehide", () => {
  clearTimeout(transitionTimer);
  clearInterval(clockTimer);
  const submitted =
    (states[current].screen === "registration" && states[current].locked) ||
    unconfirmed === "registration-pending";
  for (const key of Object.keys(model))
    if (!submitted || (key !== "name" && key !== "idNumber")) model[key] = "";
  if (!submitted) {
    submittedRegistration = null;
    for (const input of content.querySelectorAll("input")) input.value = "";
  }
});
function continueAfterLimit(id) {
  limit = null;
  if (unconfirmed) show(unconfirmed);
  else if (id === "registration-limited" && registrationUntil > Date.now())
    show("registration");
  else if (states[id].retry) show(states[id].retry);
  else returnToPhone();
}

function restore(id, memoryKept) {
  if (memoryKept && limit && limit.until > Date.now())
    show(limit.id, false, false);
  else if (memoryKept && states[id]?.expired) show(states[id].expired, false);
  else if (memoryKept && states[id]?.limited) continueAfterLimit(id);
  else if (
    !states[id]?.missing &&
    !Object.values(states).some((state) => state.recovery === id)
  )
    returnToPhone();
  else if (memoryKept) show(id, false, false);
  else if (states[id].missing)
    show(
      states[id].missing,
      false,
      true,
      states[id].group === registration.group
        ? "注册"
        : states[id].group === code.group
          ? "登录"
          : "办理",
    );
  else returnToPhone();
}

window.addEventListener("pageshow", (event) => {
  if (event.persisted) restore(location.hash.slice(1), true);
});
completedRegistration = pathPicker.value === "new";
if (
  ["back_forward", "reload"].includes(
    performance.getEntriesByType("navigation")[0]?.type,
  )
)
  restore(location.hash.slice(1), false);
else show(location.hash.slice(1) || "phone", false, false);
