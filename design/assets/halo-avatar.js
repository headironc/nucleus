const picker = document.getElementById("scenario-picker");
const content = document.getElementById("avatar-content");
const dialog = document.getElementById("leave-dialog");
const exampleButton = document.querySelector('[data-action="use-example"]');
const currentExample = "assets/halo-avatar-current.webp";
const candidateExample = "assets/halo-avatar-candidate.webp";
const uploadBasis =
  "仅本人有效会话。原始静态 PNG/JPEG/WebP，格式与声明一致，正方形 1–1024 px，最多 2 MiB；不裁剪、缩放原图或按 EXIF 旋转。";
const recoveryBasis =
  "保留本次更换的原定位，只核对原完成与当前头像。未取得可读完成不等于失败，不直接重传或自动建立新更换。";
const scenarios = {
  ready: { kind: "selection", basis: uploadBasis },
  loading: {
    kind: "message",
    masked: true,
    busy: true,
    title: "正在读取当前头像",
    description: "读取完成后，再选择图片。",
    basis:
      "进入页面先取得可信本人投影。读取完成前不展示此前账户、头像或候选图片。",
  },
  unavailable: {
    kind: "message",
    masked: true,
    title: "暂时无法读取当前头像",
    description: "请重新加载后再试。暂时无法读取当前登录账户的头像。",
    action: "reload",
    label: "重新加载",
    basis:
      "普通读取失败或 dependency_unavailable 不是退出或头像不存在。遮蔽旧图片，只重读当前本人。",
  },
  limited: {
    kind: "message",
    masked: true,
    limited: true,
    title: "暂时无法读取当前头像",
    description: "暂时无法处理，请稍后再试。",
    action: "reload",
    label: "稍后重新加载",
    basis:
      "rate_limited 按 Retry-After 等待，不归因为用户操作频繁。设计示例等待 60 秒，可用预览工具演示等待结束。",
  },
  "session-invalid": {
    kind: "message",
    masked: true,
    title: "请重新登录",
    description: "当前登录已无法继续使用。重新登录后，再查看当前头像。",
    login: true,
    basis:
      "401 authentication_invalid：隐藏旧头像与候选，停止更换和旧账户核对，不推断会话失效的具体原因。",
  },
  forbidden: {
    kind: "message",
    masked: true,
    title: "暂时无法读取头像",
    description: "暂时无法读取账户资料，请稍后再试。",
    action: "home",
    label: "返回首页",
    basis:
      "403 permission_denied 是同源条件拒绝；不清理会话，不称登录失效，也不要求重新确认账户。",
  },
  "account-changed": {
    kind: "message",
    masked: true,
    title: "登录状态已变化",
    description: "登录账户已变化。请先重新读取当前账户后继续。",
    action: "confirm-account",
    label: "重新读取当前账户",
    basis:
      "账户选择变化后立即遮蔽旧图片、候选与原办理内容，旧响应不能覆盖新选择。新账户需重新读取后才可选图。",
  },
  "other-account": {
    kind: "selection",
    other: true,
    basis:
      "已重新核对当前选中的另一个虚构账户。未沿用此前账户的候选或更换定位。",
  },
  "image-unavailable": {
    kind: "selection",
    image: "failed",
    basis:
      "当前本人 JSON 投影可信，只有图片失败。img.onerror 不能证明过期、退出或更换失败；重取当前链接并最多重试一次。",
  },
  "image-retry-failed": {
    kind: "selection",
    image: "retried",
    basis:
      "图片重取链接后已重试一次，仍失败时保留占位，不无限重试、不重新上传。可信账户与当前头像事实仍保留。",
  },
  "file-reading": {
    kind: "selection",
    busy: true,
    basis:
      "只在本地读取所选原始字节，完成格式、动画、尺寸与解码检查后才显示候选；期间不能提交。",
  },
  preview: { kind: "selection", candidate: true, basis: uploadBasis },
  "format-error": {
    kind: "selection",
    error: "请选择静态 PNG、JPEG 或 WebP，图片实际格式须与文件类型一致。",
    basis:
      "415 avatar_format_unsupported：不支持的格式或声明与实际格式不符。保留原头像，允许重新选择。",
  },
  "size-error": {
    kind: "selection",
    error: "图片超过 2 MiB，请选择不超过此大小的原始文件。",
    basis:
      "413 avatar_too_large：上传原始字节最多 2 MiB，不以处理后文件大小替代。",
  },
  "dimensions-error": {
    kind: "selection",
    error: "请选择正方形原图，宽高均须为 1–1024 像素。这里不会自动裁剪或缩放。",
    basis: "400 invalid_input：按原图尺寸判断，宽高相同且边长 1–1024 px。",
  },
  "damaged-error": {
    kind: "selection",
    error: "无法完整读取这张图片，请检查文件是否损坏，或选择其他图片。",
    basis:
      "400 invalid_input：损坏或无法完整解码的图片被拒绝。错误不改变原头像。",
  },
  "animation-error": {
    kind: "selection",
    error: "不支持动画图片，请选择静态 PNG、JPEG 或 WebP。",
    basis: "400 invalid_input：动画明确拒绝，不截取第一帧冒充静态图片。",
  },
  "local-unavailable": {
    kind: "selection",
    error: "暂时无法在本地预览图片，请重新选择或换用支持图片预览的浏览器。",
    basis:
      "本地预览能力不可用不等于原文件不合规，也不推断服务端更换结果。未提交。",
  },
  "exif-unavailable": {
    kind: "selection",
    error:
      "本设计预览无法准确展示原始方向，可用示例演示。图片尚未提交，这不表示文件不合规。",
    basis:
      "设计工具限制：检测到 EXIF 元数据时不展示或确认该候选。原文件字节不变；正式客户端仍按原始方向处理，不按 EXIF 旋转。",
  },
  submitting: {
    kind: "message",
    busy: true,
    pending: true,
    title: "正在更换头像",
    description: "更换请求已发出，请稍候。暂时不要重复提交。",
    basis: "仅在本人明确确认候选后模拟提交。进行中固定原输入，不换键、不重传。",
  },
  pending: {
    kind: "message",
    pending: true,
    title: "暂时无法确认更换结果",
    description:
      "更换请求已发出，但暂时还没有结果。请查询本次更换的结果，无需重新上传图片。",
    action: "check",
    label: "查询更换结果",
    basis: recoveryBasis,
  },
  checking: {
    kind: "message",
    pending: true,
    busy: true,
    title: "正在查询更换结果",
    description: "正在查询本次更换的结果与当前头像，请稍候。",
    basis: "每次明确操作只发起一次有界结果核对；本稿模拟读取，不自动循环。",
  },
  "pending-session-invalid": {
    kind: "message",
    masked: true,
    pending: true,
    reauthRequired: true,
    login: true,
    title: "请重新登录后查询结果",
    description:
      "当前登录已无法继续使用。此前的图片已隐藏，更换结果尚未确定。请登录更换头像时使用的账户，再查询结果。",
    basis:
      "原结果核对遇到 401：保留原更换责任，隐藏旧图片。只能由重新确认的同一账户继续核对；不新建更换。预览工具只模拟重新登录，不实现生产恢复。",
  },
  "pending-reauthenticating": {
    kind: "message",
    masked: true,
    reauthRequired: true,
    busy: true,
    title: "正在读取登录账户",
    description: "读取完成后，再继续。",
    basis:
      "账户读取期间不展示旧账户的操作类型、状态或图片，也不触发旧操作离开提示。仅模拟私下保留原定位；读取确认为同账户后才恢复原结果查询。",
  },
  "pending-reauthenticated": {
    kind: "message",
    pending: true,
    title: "已登录更换头像时使用的账户",
    description:
      "当前登录的是更换头像时使用的账户。更换结果尚未确定，请查询结果。",
    action: "check",
    label: "查询更换结果",
    basis:
      "同账户重新登录并可信确认本人后，只允许继续核对原更换。本人确认本身不证明更换完成。",
  },
  "pending-other-account": {
    kind: "message",
    masked: true,
    reauthRequired: true,
    other: true,
    title: "已切换账户",
    description: "请先查看当前账户，再继续。",
    action: "confirm-account",
    label: "读取当前账户",
    basis:
      "另一账户的页面不展示或使用旧账户的受限记录，包括操作类型、状态、图片及旧操作离开提示。原记录仅以设计工具标志私下保留，重新读到同账户后才恢复；不实现生产存储。",
  },
  "pending-forbidden": {
    kind: "message",
    masked: true,
    pending: true,
    title: "暂时无法查询更换结果",
    description: "本次查询未能完成。更换结果仍未确定，请稍后查询。",
    action: "check",
    label: "查询更换结果",
    basis:
      "原结果 GET 的 403 permission_denied 只拒绝本次同源查询，不证明此前 PUT 失败或会话失效。隐藏图片但保留原更换责任；明确操作只查询原完成，不开放选图或上传。",
  },
  "check-error": {
    kind: "message",
    pending: true,
    title: "暂时无法查询更换结果",
    description:
      "更换结果尚未确定，请稍后查询结果。暂时无法查询，不代表更换失败。",
    action: "check",
    label: "查询更换结果",
    basis:
      "结果读取 dependency_unavailable 或断连：保留原更换定位。普通依赖失败不能伪造 result_unavailable。",
  },
  "check-limited": {
    kind: "message",
    pending: true,
    limited: true,
    title: "请稍后查询更换结果",
    description: "暂时无法查询本次更换的结果。更换结果尚未确定，请稍后再试。",
    action: "check",
    label: "查询更换结果",
    basis:
      "结果读取 rate_limited 按 Retry-After 等待，只核对原更换，不开放直接重传。",
  },
  "result-unavailable": {
    kind: "message",
    pending: true,
    title: "暂时无法查询更换结果",
    description:
      "尚未查到本次更换的结果，这不代表更换失败。请保留当前页面，稍后查询结果。",
    action: "check",
    label: "查询更换结果",
    basis:
      "404 result_unavailable 不证明原事务回滚，不得据此自动新建更换或重复上传。",
  },
  "locator-missing": {
    kind: "message",
    pending: true,
    title: "暂时无法确认更换结果",
    description: "你可以先查看当前头像，但这不能确认本次更换的结果。",
    action: "read-current",
    label: "只查看当前头像",
    basis:
      "恢复定位缺失不等于失败。只读当前头像不能证明原更换完成；本页不开放直接重传。",
  },
  "current-only": {
    kind: "selection",
    currentOnly: true,
    pending: true,
    title: "当前头像已读取",
    description: "这是当前使用的头像，本次更换的结果仍未确定。",
    basis:
      "缺少原定位时允许只读当前头像；当前头像不能替代原完成。本页不开放新提交。",
  },
  "request-mismatch": {
    kind: "message",
    pending: true,
    title: "提交内容与本次更换不一致",
    description:
      "已保留原来的更换信息。请查询本次更换的结果，暂时不要重新上传。",
    action: "check",
    label: "查询更换结果",
    basis: "409 request_mismatch：不覆盖原定位、不替换原输入、不换键自动重传。",
  },
  "submit-error": {
    kind: "selection",
    error: "本次未更换头像，原头像已保留。请重新选择图片后再确认。",
    basis: "此示例只表示已确定的输入错误，不将普通 503 或未知结果当作未提交。",
  },
  "submit-limited": {
    kind: "selection",
    limited: true,
    error: "暂时无法更换头像，请稍后再试。本次未更换，原头像已保留。",
    basis:
      "示例为确定拒绝的新更换 rate_limited，等待后再明确确认。无需变更、失败和重放不计实际更换次数。",
  },
  "avatar-changed": {
    kind: "message",
    title: "当前头像已变化",
    description:
      "本次尚未更换头像。请先重新读取当前头像，再对比新头像并确认更换。",
    action: "reload-changed",
    label: "重新读取当前头像",
    basis:
      "409 avatar_changed：原期望修订已失效。先重读当前，不能无提示覆盖其他更换。候选仅在同一已确认账户下保留。",
  },
  "current-reloading": {
    kind: "message",
    busy: true,
    title: "正在重新读取当前头像",
    description: "正在读取当前头像，读取完成前不显示旧图片。",
    basis:
      "只重读当前头像。重读成功前不将旧头像继续标为当前，也不自动再次提交。",
  },
  reconfirm: {
    kind: "selection",
    candidate: true,
    changedCurrent: true,
    title: "请重新确认新头像",
    description: "已重新读取当前头像。请对比两张图片，再决定是否更换。",
    basis: "当前头像可信重读后重新展示候选；只有新的明确确认才演示新更换。",
  },
  success: {
    kind: "result",
    changed: true,
    title: "本次更换已完成",
    description: "当前头像已更新。",
    basis:
      "completion.changed=true 表示本次确实更换；当前 avatar 来自同次决定性读取。",
  },
  unchanged: {
    kind: "result",
    changed: false,
    title: "本次无需更换",
    description: "所选图片与当时的头像相同，本次未更换头像。",
    basis: "completion.changed=false：当前头像、头像修订和实际更换次数不变。",
  },
  "later-changed": {
    kind: "result",
    changed: true,
    later: true,
    title: "本次更换已完成",
    description: "完成之后，当前头像又有变化。下方展示重新读取时的当前头像。",
    basis:
      "原 completion 与当前 avatar 的修订可以不同。后来的更换不否认原完成，当前图片不能替代原完成记录。",
  },
  "unchanged-later-changed": {
    kind: "result",
    changed: false,
    later: true,
    title: "本次无需更换",
    description:
      "所选图片与当时的头像相同，本次未更换头像。此后头像发生变化；下方展示重新读取的当前头像。",
    basis:
      "原 completion.changed=false 只说明本次无需变更。后来当前头像变化不改变本次完成，不表示本次候选曾更换成功。",
  },
};
let current = "loading";
let candidate = null;
let currentSource = currentExample;
let selectionVersion = 0;
let pendingTimer;
let imageRetry = false;
let waiting = false;
let leaveTarget = "halo-account.html";
let returnFocus;
let resultSource = candidateExample;
let currentOnly = false;
let retainedReplacement = false;
let originalAccount = true;

const escapeHtml = (value) =>
  String(value).replace(
    /[&<>"']/g,
    (character) =>
      ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[
        character
      ],
  );
const personIcon =
  '<svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="8" r="3.5"/><path d="M5 21v-2a7 7 0 0 1 14 0v2"/></svg>';
function button(action, label, primary = false, disabled = false) {
  return `<button type="button" class="button${primary ? " primary" : ""}" data-action="${action}"${disabled ? " disabled" : ""}>${label}</button>`;
}
function imageCard(title, source, options = {}) {
  const failed = options.failed;
  return `<figure class="image-card"><h3>${title}</h3><div class="avatar-picture${options.candidate ? " candidate" : ""}">${failed ? personIcon : `<img src="${escapeHtml(source)}" alt="${title}"${options.current ? ' data-current-image="true"' : ""} />`}</div>${options.caption ? `<figcaption>${options.caption}</figcaption>` : ""}${failed ? `<p class="image-failure">头像图片暂不可用</p>${!imageRetry ? '<button class="text-action" type="button" data-action="retry-image">重新加载图片</button>' : '<p class="image-failure">已重试一次，稍后再查看。</p>'}` : ""}</figure>`;
}
function selectionPanel(state) {
  const busy = current === "file-reading";
  const failedImage = state.image || imageRetry;
  if (currentOnly)
    return `<div class="avatar-stage-heading"><h2>当前头像已读取</h2><p>这是当前使用的头像，本次更换的结果仍未确定。</p></div>
      <div class="avatar-pair single">${imageCard("当前头像", currentSource, { current: true, failed: failedImage, caption: "刚刚读取的当前头像" })}</div>
      <p class="result-note">暂不开始新的更换，以免重复提交。</p>
      <div class="avatar-actions">${button("show-unresolved", "查看待确认说明")}<a class="text-action" href="halo-account.html">返回个人账户</a></div>`;
  return `<div class="avatar-stage-heading"><h2>${state.title || (candidate ? "预览后，确认更换" : "先选择一张图片")}</h2><p>${state.description || "更换前，请对比当前头像与新头像。只有确认更换后才会提交。"}</p></div>
    <div class="avatar-pair${candidate ? "" : " single"}">
      ${imageCard("当前头像", currentSource, { current: true, failed: failedImage })}
      ${candidate ? imageCard("新头像", candidate.url, { candidate: true, caption: `${escapeHtml(candidate.width)} × ${escapeHtml(candidate.height)} 像素 · ${escapeHtml(candidate.type)}` }) : ""}
    </div>
    ${busy ? '<div class="selection-empty" aria-busy="true">正在读取本地图片，完成后再展示预览。</div>' : !candidate && !state.error ? '<p class="selection-empty">还没有选择图片。原头像会保留，选图本身不会更换头像。</p>' : ""}
    <div class="file-field"><label for="avatar-file">${candidate ? "重新选择图片" : "选择本地图片"}</label><input type="file" id="avatar-file" accept="image/png,image/jpeg,image/webp" aria-describedby="file-hint${state.error ? " file-error" : ""}"${state.error ? ' aria-invalid="true"' : ""}${busy || waiting || currentOnly ? " disabled" : ""} /><p id="file-hint" class="file-hint">静态 PNG、JPEG 或 WebP；正方形 1–1024 像素；最多 2 MiB。</p>${state.error ? `<p id="file-error" class="file-error" role="alert">${state.error}</p>` : ""}${candidate ? `<p class="selection-meta">已选择：${escapeHtml(candidate.name)}<br />仅在本地预览，尚未提交。</p>` : ""}</div>
    <div class="avatar-actions">${candidate && !currentOnly ? button("submit", "确认更换", true, busy || waiting) + button("clear", "取消选择", false, busy || waiting) : ""}<a class="text-action" href="halo-account.html">返回个人账户</a></div>`;
}
function messagePanel(state) {
  return `<div class="avatar-message"${state.busy ? ' aria-busy="true"' : ""}>${state.busy ? '<span class="reading-symbol" aria-hidden="true"></span>' : '<span class="status-seal" aria-hidden="true">!</span>'}<h2>${state.title}</h2><p>${state.description}</p>${waiting ? '<p class="file-hint">请等待后再继续。设计工具可演示等待结束。</p>' : ""}<div class="avatar-actions">${state.login ? '<a class="button primary" href="halo-authentication.html">重新登录</a>' : state.action ? button(state.action, state.label, true, waiting) : ""}${state.busy ? "" : '<a class="text-action" href="halo-account.html">返回个人账户</a>'}</div></div>`;
}
function resultPanel(state) {
  const source = state.later || !state.changed ? currentSource : resultSource;
  const laterNote = state.changed
    ? "本次更换已完成；当前头像此后又有变化。"
    : "本次未更换头像；当前头像此后又有变化。";
  return `<div class="avatar-stage-heading"><span class="status-seal complete" aria-hidden="true">✓</span><h2 class="result-title">${state.title}</h2><p>${state.description}</p></div><div class="avatar-pair single">${imageCard("当前头像", source, { current: true, failed: imageRetry, caption: "刚刚读取的当前头像" })}</div><div class="result-note"><strong>本次更换：${state.changed ? "已完成" : "无需更换"}</strong><br /><time datetime="2026-10-20T14:30:00+08:00">2026 年 10 月 20 日 14:30</time>${state.later ? `<br />${laterNote}` : ""}</div><div class="avatar-actions"><a class="button primary" href="halo-account.html">返回个人账户</a>${button("start-new", "重新选择图片")}</div>`;
}
function render(focus = false) {
  const state = scenarios[current];
  picker.value = current;
  document.getElementById("state-basis").textContent = state.basis;
  content.innerHTML =
    state.kind === "selection"
      ? selectionPanel(state)
      : state.kind === "result"
        ? resultPanel(state)
        : messagePanel(state);
  exampleButton.disabled =
    state.kind !== "selection" || state.busy || waiting || currentOnly;
  let waitButton = document.getElementById("end-wait");
  if (!waitButton) {
    waitButton = document.createElement("button");
    waitButton.id = "end-wait";
    waitButton.type = "button";
    waitButton.className = "sample-tool";
    waitButton.dataset.action = "end-wait";
    waitButton.textContent = "演示等待结束";
    exampleButton.after(waitButton);
  }
  waitButton.hidden = !waiting;
  for (const action of ["simulate-same-account", "simulate-other-account"]) {
    const tool = document.querySelector(`[data-action="${action}"]`);
    tool.hidden = !retainedReplacement;
    tool.disabled = Boolean(state.busy);
  }
  if (focus)
    (
      content.querySelector('[aria-invalid="true"]') ||
      document.getElementById("page-title")
    ).focus();
}
function show(key, focus = true) {
  clearTimeout(pendingTimer);
  current = key;
  const state = scenarios[key];
  if (state.pending && originalAccount) retainedReplacement = true;
  if (state.kind === "result") {
    candidate = null;
    if (originalAccount) retainedReplacement = false;
  }
  if (state.masked) {
    candidate = null;
    selectionVersion += 1;
    resultSource = candidateExample;
    currentOnly = false;
  }
  waiting = Boolean(state.limited);
  render(focus);
}
function readThen(target, interim = "loading") {
  show(interim);
  pendingTimer = setTimeout(() => show(target), 650);
}
function exampleCandidate(source = candidateExample) {
  return {
    url: source,
    width: 512,
    height: 512,
    type: "WebP",
    name: source === currentExample ? "虚构猫头像.webp" : "虚构狗头像.webp",
  };
}
function chooseScenario(key) {
  selectionVersion += 1;
  candidate = null;
  imageRetry = key === "image-retry-failed";
  originalAccount = !scenarios[key].other;
  retainedReplacement = Boolean(
    scenarios[key].pending || scenarios[key].reauthRequired,
  );
  currentSource =
    key === "other-account" ||
    key === "reconfirm" ||
    key === "unchanged-later-changed"
      ? candidateExample
      : currentExample;
  currentOnly = Boolean(scenarios[key].currentOnly);
  resultSource = candidateExample;
  if (scenarios[key].candidate)
    candidate = exampleCandidate(
      key === "reconfirm" ? currentExample : candidateExample,
    );
  show(key, false);
}

// Container inspection does not decode pixels or implement production validation.
function imageFormat(bytes) {
  const ascii = (offset, length) =>
    String.fromCharCode(...bytes.subarray(offset, offset + length));
  if (
    bytes.length >= 8 &&
    [137, 80, 78, 71, 13, 10, 26, 10].every(
      (value, index) => bytes[index] === value,
    )
  )
    return "image/png";
  if (
    bytes.length >= 3 &&
    bytes[0] === 255 &&
    bytes[1] === 216 &&
    bytes[2] === 255
  )
    return "image/jpeg";
  if (bytes.length >= 12 && ascii(0, 4) === "RIFF" && ascii(8, 4) === "WEBP")
    return "image/webp";
  return null;
}
function animated(bytes, type) {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let offset = type === "image/png" ? 8 : 12;
  while (offset + 8 <= bytes.length) {
    const png = type === "image/png";
    const length = view.getUint32(offset + (png ? 0 : 4), !png);
    const tag = String.fromCharCode(
      ...bytes.subarray(offset + (png ? 4 : 0), offset + (png ? 8 : 4)),
    );
    if (
      (png && tag === "acTL") ||
      (!png &&
        (tag === "ANIM" ||
          tag === "ANMF" ||
          (tag === "VP8X" && length > 0 && bytes[offset + 8] & 2)))
    )
      return true;
    const next = offset + 8 + length + (png ? 4 : length % 2);
    if (next > bytes.length || next <= offset) break;
    offset = next;
  }
  return false;
}
function hasExif(bytes, type) {
  const ascii = (offset, length) =>
    String.fromCharCode(...bytes.subarray(offset, offset + length));
  if (type === "image/jpeg") {
    let offset = 2;
    while (offset < bytes.length) {
      if (bytes[offset++] !== 255) continue;
      while (bytes[offset] === 255) offset += 1;
      const marker = bytes[offset++];
      if (marker === 217) break;
      if (
        marker === 0 ||
        marker === 1 ||
        marker === 216 ||
        (marker >= 208 && marker <= 215)
      )
        continue;
      if (offset + 2 > bytes.length) break;
      const length = (bytes[offset] << 8) | bytes[offset + 1];
      if (marker === 225 && ascii(offset + 2, 6) === "Exif\0\0") return true;
      if (length < 2 || offset + length > bytes.length) break;
      offset += length;
    }
    return false;
  }
  const png = type === "image/png";
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let offset = png ? 8 : 12;
  while (offset + 8 <= bytes.length) {
    const length = view.getUint32(offset + (png ? 0 : 4), !png);
    const tag = ascii(offset + (png ? 4 : 0), 4);
    if (
      tag === (png ? "eXIf" : "EXIF") ||
      (!png && tag === "VP8X" && length > 0 && bytes[offset + 8] & 8)
    )
      return true;
    const next = offset + 8 + length + (png ? 4 : length % 2);
    if (next > bytes.length || next <= offset) break;
    offset = next;
  }
  return false;
}
async function selectFile(file) {
  if (!file) return;
  const version = ++selectionVersion;
  candidate = null;
  show("file-reading", false);
  const fail = (key) => {
    if (version === selectionVersion) show(key);
  };
  if (file.size > 2 * 1024 * 1024) {
    fail("size-error");
    return;
  }
  if (!["image/png", "image/jpeg", "image/webp"].includes(file.type)) {
    fail("format-error");
    return;
  }
  let bitmap;
  try {
    const bytes = new Uint8Array(await file.arrayBuffer());
    if (version !== selectionVersion) return;
    const type = imageFormat(bytes);
    if (!type || type !== file.type) {
      fail("format-error");
      return;
    }
    if (hasExif(bytes, type)) {
      fail("exif-unavailable");
      return;
    }
    if (type !== "image/jpeg" && animated(bytes, type)) {
      fail("animation-error");
      return;
    }
    if (typeof createImageBitmap !== "function") {
      fail("local-unavailable");
      return;
    }
    bitmap = await createImageBitmap(file);
    if (version !== selectionVersion) return;
    if (
      bitmap.width !== bitmap.height ||
      bitmap.width < 1 ||
      bitmap.width > 1024
    ) {
      fail("dimensions-error");
      return;
    }
    const canvas = document.createElement("canvas");
    canvas.width = bitmap.width;
    canvas.height = bitmap.height;
    const context = canvas.getContext("2d");
    if (!context) {
      fail("local-unavailable");
      return;
    }
    context.drawImage(bitmap, 0, 0);
    candidate = {
      url: canvas.toDataURL("image/png"),
      width: bitmap.width,
      height: bitmap.height,
      type:
        type === "image/jpeg" ? "JPEG" : type === "image/png" ? "PNG" : "WebP",
      name: file.name,
      file,
    };
    show("preview");
  } catch {
    fail("damaged-error");
  } finally {
    bitmap?.close();
  }
}
function canCheckReplacement() {
  const state = scenarios[current];
  return (
    state.pending &&
    !state.busy &&
    !state.reauthRequired &&
    !waiting &&
    !currentOnly &&
    current !== "locator-missing"
  );
}
function requestLeave(href, trigger) {
  const state = scenarios[current];
  const unresolved = state.pending || currentOnly;
  if (!candidate && !unresolved) {
    location.href = href;
    return;
  }
  leaveTarget = href;
  returnFocus = trigger;
  document.getElementById("leave-title").textContent = unresolved
    ? "更换结果尚未确定"
    : "放弃这张新头像？";
  document.getElementById("leave-description").textContent = unresolved
    ? "离开不会取消已发出的更换请求。更换结果尚未确定，请查询结果。"
    : "新头像还没有提交。离开后，本地选择会清除，原头像不变。";
  const stayButton = document.getElementById("stay-button");
  stayButton.textContent = unresolved ? "留在此页" : "继续选择";
  stayButton.dataset.action = "dismiss";
  document.getElementById("leave-button").textContent = unresolved
    ? "仍要离开"
    : "放弃并离开";
  dialog.showModal();
  document.getElementById("stay-button").focus();
}
picker.addEventListener("change", () => chooseScenario(picker.value));
document.querySelector(".preview-toggle").addEventListener("click", (event) => {
  const expanded = event.currentTarget.getAttribute("aria-expanded") !== "true";
  event.currentTarget.setAttribute("aria-expanded", String(expanded));
  event.currentTarget.textContent = expanded ? "收起预览工具" : "展开预览工具";
});
document.addEventListener("change", (event) => {
  if (event.target.id === "avatar-file") selectFile(event.target.files[0]);
});
document.addEventListener(
  "error",
  (event) => {
    if (event.target.matches?.("[data-current-image]")) {
      event.target.parentElement.innerHTML = personIcon;
      const card = content.querySelector(".image-card");
      const failure = document.createElement("p");
      failure.className = "image-failure";
      failure.textContent = imageRetry
        ? "头像图片暂不可用，已重试一次。"
        : "头像图片暂不可用。";
      card.append(failure);
      if (!imageRetry) {
        const retry = document.createElement("button");
        retry.type = "button";
        retry.className = "text-action";
        retry.dataset.action = "retry-image";
        retry.textContent = "重新加载图片";
        card.append(retry);
      }
    }
  },
  true,
);
document.addEventListener("click", (event) => {
  const link = event.target.closest("a[href]");
  if (
    link &&
    !link.getAttribute("href").startsWith("#") &&
    !event.metaKey &&
    !event.ctrlKey &&
    !event.shiftKey &&
    event.button === 0
  ) {
    event.preventDefault();
    requestLeave(link.getAttribute("href"), link);
    return;
  }
  const trigger = event.target.closest("button[data-action]");
  if (!trigger || trigger.disabled) return;
  switch (trigger.dataset.action) {
    case "reload":
      readThen("ready");
      break;
    case "confirm-account":
      originalAccount = false;
      currentSource = candidateExample;
      imageRetry = false;
      readThen("other-account");
      break;
    case "home":
      location.href = "halo-home.html";
      break;
    case "use-example":
      selectionVersion += 1;
      candidate = exampleCandidate();
      show("preview");
      break;
    case "clear":
      candidate = null;
      selectionVersion += 1;
      show("ready");
      break;
    case "submit":
      if (
        !candidate ||
        scenarios[current].kind !== "selection" ||
        waiting ||
        currentOnly
      )
        break;
      resultSource = candidate.url;
      show("submitting");
      pendingTimer = setTimeout(() => {
        candidate = null;
        show("success");
      }, 850);
      break;
    case "check":
      if (canCheckReplacement()) readThen("success", "checking");
      break;
    case "simulate-same-account":
    case "simulate-other-account":
      if (!retainedReplacement || scenarios[current].busy) break;
      originalAccount = trigger.dataset.action === "simulate-same-account";
      readThen(
        trigger.dataset.action === "simulate-same-account"
          ? "pending-reauthenticated"
          : "pending-other-account",
        "pending-reauthenticating",
      );
      break;
    case "read-current":
      currentOnly = true;
      readThen("current-only", "current-reloading");
      break;
    case "show-unresolved":
      show("locator-missing");
      break;
    case "reload-changed":
      show("current-reloading");
      pendingTimer = setTimeout(() => {
        currentSource = candidateExample;
        if (!candidate) candidate = exampleCandidate(currentExample);
        show("reconfirm");
      }, 650);
      break;
    case "retry-image":
      imageRetry = true;
      trigger.disabled = true;
      trigger.textContent = "正在重新加载…";
      pendingTimer = setTimeout(() => {
        if (scenarios[current].kind === "selection") show("image-retry-failed");
        else render();
      }, 650);
      break;
    case "start-new":
      currentSource =
        scenarios[current].later || !scenarios[current].changed
          ? currentSource
          : resultSource;
      candidate = null;
      currentOnly = false;
      show("ready");
      break;
    case "end-wait":
      waiting = false;
      render();
      break;
    case "dismiss":
      dialog.close();
      returnFocus?.focus();
      break;
    case "leave":
      candidate = null;
      selectionVersion += 1;
      clearTimeout(pendingTimer);
      location.href = leaveTarget;
      break;
  }
});
dialog.addEventListener("cancel", () => returnFocus?.focus());
const initial = location.hash.slice(1);
chooseScenario(Object.hasOwn(scenarios, initial) ? initial : "loading");
if (!Object.hasOwn(scenarios, initial))
  pendingTimer = setTimeout(() => show("ready", false), 650);
