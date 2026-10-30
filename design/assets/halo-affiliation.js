(() => {
  "use strict";

  const affiliations = Object.freeze([
    { code: "01", name: "中共党员" },
    { code: "02", name: "中共预备党员" },
    { code: "03", name: "共青团员" },
    { code: "04", name: "民革会员" },
    { code: "05", name: "民盟盟员" },
    { code: "06", name: "民建会员" },
    { code: "07", name: "民进会员" },
    { code: "08", name: "农工党党员" },
    { code: "09", name: "致公党党员" },
    { code: "10", name: "九三学社社员" },
    { code: "11", name: "台盟盟员" },
    { code: "12", name: "无党派民主人士" },
    { code: "13", name: "群众" },
  ]);
  const affiliationByCode = new Map(
    affiliations.map((affiliation) => [affiliation.code, affiliation]),
  );
  const people = Object.freeze({
    primary: Object.freeze({ name: "陈思远" }),
    long: Object.freeze({ name: "阿依古丽·阿不都热依木" }),
    other: Object.freeze({ name: "林知夏" }),
  });
  const currentFixtures = Object.freeze({
    primary: Object.freeze({
      code: "05",
      createdAt: "2026-06-18T10:20:00+08:00",
    }),
    long: Object.freeze({
      code: "12",
      createdAt: "2026-09-02T09:10:00+08:00",
    }),
    other: Object.freeze({
      code: "03",
      createdAt: "2025-11-03T14:40:00+08:00",
    }),
    unknown: Object.freeze({
      code: "03",
      createdAt: "2026-04-09T11:05:00+08:00",
    }),
  });

  const scenarioGroups = Object.freeze([
    {
      label: "身份与当前值",
      states: [
        ["loading", "正在读取当前账户"],
        ["identity-error", "当前账户读取失败"],
        ["identity-limited", "当前账户读取受限"],
        ["reading", "正在读取当前政治面貌"],
        ["read-error", "当前政治面貌读取失败"],
        ["read-limited", "当前政治面貌读取受限"],
        ["value", "当前政治面貌（有值）"],
        ["value-long", "当前政治面貌（长姓名）"],
        ["unset", "尚未填写"],
        ["dictionary-loading", "正在读取填写选项"],
        ["dictionary-error", "填写选项读取失败"],
        ["dictionary-limited", "填写选项读取受限"],
        ["value-dictionary-error", "有值但名称读取失败"],
      ],
    },
    {
      label: "账户限制与复查",
      states: [
        ["session-invalid", "需要登录（401）"],
        ["forbidden", "同源检查拒绝（403）"],
        ["account-changed", "账户选择变化"],
        ["other-account", "复查后另一账户"],
      ],
    },
    {
      label: "首次填写",
      states: [
        ["form", "首次填写（未选择）"],
        ["selected", "首次填写（已选一项）"],
        ["selected-long", "首次填写（最长选项）"],
        ["confirm", "确认填写"],
        ["submitting", "正在提交"],
        ["submit-limited", "首次提交受限（429）"],
        ["submit-conflict", "首次提交时已有记录（409）"],
        ["submit-invalid", "首次提交内容无效（422）"],
        ["submit-failed", "首次明确失败（dependency_unavailable）"],
        ["submit-unavailable", "首次未取得可信结果（503）"],
      ],
    },
    {
      label: "可信成功",
      states: [
        ["success", "已确认填写"],
        ["success-reading", "成功 · 正在读取当前值"],
        ["success-current", "成功 · 当前值已读取"],
        ["success-current-error", "成功 · 当前值读取失败"],
      ],
    },
    {
      label: "结果未确认",
      states: [
        ["unknown", "结果未知 · 原输入可重试"],
        ["unknown-reading", "未知 · 正在读取当前值"],
        ["unknown-current-value", "未知 · 当前有值"],
        ["unknown-current-unset", "未知 · 当前未填写"],
        ["unknown-current-error", "未知 · 当前读取失败"],
        ["unknown-no-material", "未知 · 原材料缺失"],
      ],
    },
    {
      label: "重试与遮蔽",
      states: [
        ["retry-submitting", "重试正在提交"],
        ["retry-limited", "原重试受限（429）"],
        ["retry-mismatch", "原重试无法对应（request_mismatch）"],
        ["retry-conflict", "原重试遇到已有值（state_conflict）"],
        ["unknown-session-invalid", "未知后遮蔽 · 需要登录（401）"],
        ["unknown-forbidden", "未知后遮蔽 · 同源检查拒绝（403）"],
      ],
    },
  ]);

  const scenarios = Object.freeze({
    loading: { page: "identity-loading" },
    "identity-error": { page: "identity-error" },
    "identity-limited": { page: "identity-limited" },
    reading: { page: "reading" },
    "read-error": { page: "read-error" },
    "read-limited": { page: "read-limited" },
    value: { page: "value", current: "primary" },
    "value-long": { page: "value", person: "long", current: "long" },
    unset: { page: "unset" },
    "dictionary-loading": { page: "dictionary-loading" },
    "dictionary-error": { page: "dictionary-error" },
    "dictionary-limited": { page: "dictionary-limited" },
    "value-dictionary-error": {
      page: "value-dictionary-error",
      current: "primary",
    },
    "session-invalid": { page: "session-invalid" },
    forbidden: { page: "forbidden" },
    "account-changed": { page: "account-changed" },
    "other-account": {
      page: "value",
      person: "other",
      current: "other",
      notice: "已重新读取当前账户，请核对本人资料。",
      basis:
        "账户选择变化后已重新核对；页面只展示新选择的当前账户，不沿用上一账户的值。",
    },
    form: { page: "form" },
    selected: { page: "form", selectedCode: "01" },
    "selected-long": {
      page: "form",
      person: "long",
      selectedCode: "12",
    },
    confirm: { page: "confirm", selectedCode: "12" },
    submitting: { page: "submitting", selectedCode: "12" },
    "submit-limited": { page: "submit-limited", selectedCode: "12" },
    "submit-conflict": { page: "submit-conflict" },
    "submit-invalid": { page: "submit-invalid", selectedCode: "12" },
    "submit-failed": { page: "submit-failed", selectedCode: "12" },
    "submit-unavailable": { page: "unknown", selectedCode: "12",
      basis: "503 不足以证明写入未完成；保留原操作绑定并按未知结果呈现，不换键重办。" },
    success: { page: "success" },
    "success-reading": {
      page: "success",
      currentPanel: "loading",
    },
    "success-current": {
      page: "success",
      currentPanel: "value",
      current: "long",
    },
    "success-current-error": {
      page: "success",
      currentPanel: "error",
    },
    unknown: { page: "unknown", selectedCode: "12" },
    "unknown-reading": {
      page: "unknown",
      selectedCode: "12",
      currentPanel: "loading",
    },
    "unknown-current-value": {
      page: "unknown",
      selectedCode: "12",
      currentPanel: "value",
      current: "unknown",
    },
    "unknown-current-unset": {
      page: "unknown",
      selectedCode: "12",
      currentPanel: "unset",
    },
    "unknown-current-error": {
      page: "unknown",
      selectedCode: "12",
      currentPanel: "error",
    },
    "unknown-no-material": {
      page: "unknown",
      selectedCode: "12",
      hasMaterial: false,
    },
    "retry-submitting": { page: "retry-submitting", selectedCode: "12" },
    "retry-limited": { page: "retry-limited", selectedCode: "12" },
    "retry-mismatch": { page: "retry-mismatch", selectedCode: "12" },
    "retry-conflict": { page: "retry-conflict", selectedCode: "12" },
    "unknown-session-invalid": {
      page: "session-invalid",
      basis:
        "此前的结果仍未由本页确认；401 后已先遮蔽原办理类型、原输入和旧提示。登录只恢复会话，不证明填写完成。",
    },
    "unknown-forbidden": {
      page: "forbidden",
      basis:
        "403 或同源拒绝先遮蔽原办理内容；失败不证明原填写结果，也不猜测退出。",
    },
  });

  const maskedPages = new Set([
    "identity-loading",
    "identity-error",
    "identity-limited",
    "session-invalid",
    "forbidden",
    "account-changed",
  ]);
  const draftPages = new Set(["form", "confirm", "submit-limited", "submit-invalid", "submit-failed"]);
  const pageDescriptions = Object.freeze({
    "identity-loading": "正在确认当前账户。",
    "identity-error": "当前账户暂时无法读取。",
    "identity-limited": "当前账户暂时无法读取。",
    reading: "正在读取当前政治面貌。",
    "read-error": "当前政治面貌暂时无法读取。",
    "read-limited": "当前政治面貌暂时无法读取。",
    value: "查看当前记录。已有记录后，本人不能自行更正或清空。",
    "value-long": "查看当前记录。已有记录后，本人不能自行更正或清空。",
    unset: "这是一项可选资料。未填写不影响员工服务和工牌。",
    "dictionary-loading": "当前尚未填写，正在读取填写选项。",
    "dictionary-error": "当前尚未填写，填写选项暂时无法读取。",
    "dictionary-limited": "当前尚未填写，填写选项暂时无法读取。",
    "value-dictionary-error": "当前政治面貌暂时无法完整显示。",
    "session-invalid": "登录后，再查看个人账户。",
    forbidden: "请从个人账户重新进入。",
    "account-changed": "账户资料已切换，正在重新确认。",
    form: "请选择与你本人一致的一项。提交后，本人不能自行修改或清空。",
    confirm: "请核对本人和所选内容，再明确提交。",
    submitting: "填写正在提交，请稍候。",
    "submit-limited": "本次填写尚未受理，请稍后再试。",
    "submit-conflict": "当前已有记录，不能再次填写。",
    "submit-invalid": "本次填写未提交成功，请重新核对。",
    "submit-failed": "本次填写未完成，请稍后再试。",
    success: "本次填写已确认完成。",
    unknown: "本次填写结果尚未确认。",
    "retry-submitting": "正在重试本次填写。",
    "retry-limited": "本次重试未被受理，此前填写结果仍未确认。",
    "retry-mismatch": "本次重试无法继续，此前填写结果仍未确认。",
    "retry-conflict": "当前已有政治面貌记录，此前填写结果仍未确认。",
    "other-account": "查看当前记录。已有记录后，本人不能自行更正或清空。",
  });
  const pageBasis = Object.freeze({
    "identity-loading": "身份与当前值分行读取；账户未确认前不展示本人资料。",
    "identity-error":
      "当前账户读取未取得可信结果。身份读取失败不证明登录失效，也不显示政治面貌。",
    "identity-limited":
      "当前账户读取受限，遵守 Retry-After 示例；受限不解释为退出或未填写。",
    reading:
      "当前账户已核对。正在读取当前政治面貌；完成前不继承旧值，也不把未取得当成未填写。",
    "read-error":
      "当前值读取未取得可信结果。技术失败不映射为 404 affiliation_unset。",
    "read-limited":
      "当前值读取受限，遵守 Retry-After 示例；受限不表示未填写。",
    value:
      "当前值读取与固定名称读取均已完成；页面只展示该次可信快照。",
    "value-long":
      "当前值读取与固定名称读取均已完成；页面展示完整姓名和完整取值名称。",
    unset:
      "仅可信 404 affiliation_unset 进入未填写；固定名称读取完成后才开放首次填写。",
    "dictionary-loading":
      "当前值可信未填写，固定名称仍在读取；名称未完成前不开放表单。",
    "dictionary-error":
      "固定名称读取失败，不猜名称、不开放表单；当前未填写状态仍来自可信 404。",
    "dictionary-limited":
      "固定名称读取受限，不猜名称、不开放表单，也不把受限改成有值。",
    "value-dictionary-error":
      "当前值读取可信，但固定名称未取得；不以代码猜测完整名称，也不改成未填写。",
    "session-invalid":
      "401 立即遮蔽账户资料。登录成功只证明会话可用，不证明此前填写结果。",
    forbidden:
      "403 或同源拒绝遮蔽受限资料。选择复查失败不猜测退出，也不读取政治面貌。",
    "account-changed":
      "账户选择变化先遮蔽旧值并重新核对；复查完成前不沿用上一账户资料。",
    form:
      "未选择时继续不可用；本次选择只在本页内临时保留，不进入 URL、存储或日志。",
    confirm:
      "确认页展示虚构姓名和完整取值；提交前明确提示不可自行修改，可返回改选。",
    submitting:
      "提交中不重复发送，也不自动重试。离开不会取消已经发送的填写。",
    "submit-limited":
      "首次明确 429 尚未受理，遵守 Retry-After；不自动重试，也不把受限当成成功或失败。",
    "submit-conflict":
      "首次明确 state_conflict 表示当前已有记录；不继续首次填写，另行读取当前值。",
    "submit-invalid":
      "首次明确 invalid_input 拒绝本次输入；回到选择核对，不将协议错误当作已填写。",
    "submit-failed":
      "首次可信 dependency_unavailable 且原执行已确定未提交，呈现明确失败；保留本次选择，用户明确重试仍使用原键和原输入。已有未知结果后的依赖失败不能据此改写原结局。",
    success:
      "可信成功只返回 code: ok。页面不把原选择复制成当前值，也不编造完成时间或结果 ID。",
    unknown:
      "结果未知时保留原操作和原输入才提供重试。重试是原 PUT 写操作，不是只读查询，不自动执行。",
    "no-material":
      "原材料已不可用，因此不提供原重试或原结果查询；只提供独立当前读取。",
    "retry-submitting":
      "正在按原操作、原键和原规范输入重试。可能完成原填写，不承诺不写入。",
    "retry-limited":
      "原重试未被受理，不是原填写失败；此前结果仍未确认，等待期间不自动重试。",
    "retry-mismatch":
      "request_mismatch 停止原重试；不换键、不改输入、不猜原填写结果。",
    "retry-conflict":
      "state_conflict 表示当前已有记录，不代表原填写结果；只引导独立当前读取。",
  });

  const scenarioPicker = document.querySelector("#scenario-picker");
  const themePicker = document.querySelector("#theme-picker");
  const content = document.querySelector("#affiliation-content");
  const pageTitle = document.querySelector("#page-title");
  const pageKicker = document.querySelector("#page-kicker");
  const pageDescription = document.querySelector("#page-description");
  const stateBasis = document.querySelector("#state-basis");
  const leaveDialog = document.querySelector("#leave-dialog");
  const leaveTitle = document.querySelector("#leave-title");
  const leaveDescription = document.querySelector("#leave-description");
  const stayButton = document.querySelector("#stay-button");
  const leaveButton = document.querySelector("#leave-button");

  if (
    !scenarioPicker ||
    !themePicker ||
    !content ||
    !pageTitle ||
    !pageKicker ||
    !pageDescription ||
    !stateBasis ||
    !leaveDialog ||
    !leaveTitle ||
    !leaveDescription ||
    !stayButton ||
    !leaveButton
  ) {
    return;
  }

  const dateFormatter = new Intl.DateTimeFormat("zh-CN", {
    year: "numeric",
    month: "long",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
    timeZone: "Asia/Shanghai",
  });

  let model = null;
  let timer = 0;
  let pendingHref = "";
  let leaveTrigger = null;
  let leaving = false;

  const escapeHTML = (value) =>
    String(value).replace(
      /[&<>"']/g,
      (character) =>
        ({
          "&": "&amp;",
          "<": "&lt;",
          ">": "&gt;",
          '"': "&quot;",
          "'": "&#39;",
        })[character],
    );

  const affiliationText = (code) => {
    const affiliation = affiliationByCode.get(code);
    return affiliation ? `${affiliation.code} ${affiliation.name}` : "";
  };

  const formatCreatedAt = (createdAt) => {
    const date = new Date(createdAt);
    return Number.isNaN(date.getTime()) ? "" : dateFormatter.format(date);
  };

  function buildScenarioPicker() {
    for (const group of scenarioGroups) {
      const optionGroup = document.createElement("optgroup");
      optionGroup.label = group.label;
      for (const [id, label] of group.states) {
        const option = document.createElement("option");
        option.value = id;
        option.textContent = label;
        optionGroup.append(option);
      }
      scenarioPicker.append(optionGroup);
    }
  }

  function applyTheme(value) {
    const selected = ["light", "dark"].includes(value) ? value : "system";
    if (selected === "system") {
      document.documentElement.removeAttribute("data-theme");
    } else {
      document.documentElement.dataset.theme = selected;
    }
    themePicker.value = selected;
  }

  function buildModel(id) {
    const config = scenarios[id];
    const page = config.page;
    const masked = maskedPages.has(page);
    const hasMaterial =
      typeof config.hasMaterial === "boolean"
        ? config.hasMaterial
        : page === "unknown";
    let selectedCode = config.selectedCode || "";
    if (
      masked ||
      page === "success" ||
      (page === "unknown" && !hasMaterial)
    ) {
      selectedCode = "";
    }
    const currentPanel = config.currentPanel || null;
    const currentContext =
      page === "success" ? "success" : page === "unknown" ? "unknown" : null;
    return {
      scenarioId: id,
      page,
      person: masked ? null : people[config.person || "primary"],
      selectedCode,
      hasMaterial,
      currentPanel,
      currentContext,
      readValue:
        currentPanel === "value" || page === "value"
          ? currentFixtures[config.current || "primary"]
          : null,
      notice: config.notice || "",
      customBasis: config.basis || "",
    };
  }

  function pageHeading(state) {
    if (maskedPages.has(state.page)) {
      return {
        title: "个人账户",
        kicker: "个人账户",
        description: pageDescriptions[state.page],
        documentTitle: "个人账户 · 司籍处",
      };
    }
    return {
      title: "政治面貌",
      kicker: "个人账户 · 本人资料",
      description: pageDescriptions[state.page],
      documentTitle: "政治面貌 · 司籍处",
    };
  }

  function basisFor(state) {
    if (state.customBasis) {
      return state.customBasis;
    }
    if (state.page === "success" && state.currentPanel) {
      if (state.currentPanel === "loading") {
        return "可信成功已确认；正在另行读取当前值。读取未完成不会改写成功结论。";
      }
      if (state.currentPanel === "error") {
        return "可信成功已确认；后续当前值读取失败不把结果改回未知，也不否定已确认完成。";
      }
      return "可信成功已确认；当前值来自另行读取，不取代成功回执，也不由原选择伪造。";
    }
    if (state.page === "unknown" && state.currentPanel) {
      if (state.currentPanel === "loading") {
        return "原填写结果仍未确认。正在另行读取当前值；此读取不是重试原 PUT。";
      }
      if (state.currentPanel === "error") {
        return "当前值读取失败不说明原 PUT 是否完成，原未知提示保持独立。";
      }
      if (state.currentPanel === "unset") {
        return "当前读取未填，但不证明原 PUT 未完成；本页不开放换键重办。";
      }
      return "当前有值只说明另行读取结果，不证明原 PUT 成败，也不改写未知提示。";
    }
    if (state.page === "unknown" && !state.hasMaterial) {
      return pageBasis["no-material"];
    }
    return pageBasis[state.page] || "本页只演示已确认的界面状态。";
  }

  function personContext(person) {
    if (!person) {
      return "";
    }
    return `<p class="person-context"><span>本人</span><strong>${escapeHTML(
      person.name,
    )}</strong></p>`;
  }

  function actionButton(label, action, options = {}) {
    const primary = options.primary !== false;
    const disabled = Boolean(options.disabled);
    return `<button type="button" class="button ${
      primary ? "primary" : "secondary"
    }" data-action="${action}"${disabled ? " disabled" : ""}>${escapeHTML(
      label,
    )}</button>`;
  }

  function linkButton(label, href, options = {}) {
    const primary = options.primary !== false;
    return `<a class="button ${
      primary ? "primary" : "secondary"
    }" href="${href}" data-leave>${escapeHTML(label)}</a>`;
  }

  function accountLink() {
    return linkButton("返回个人账户", "halo-account.html", { primary: false });
  }

  function homeLink() {
    return linkButton("返回首页", "halo-home.html", { primary: false });
  }

  function statePanel(options) {
    const kicker = options.kicker
      ? `<p class="panel-kicker">${escapeHTML(options.kicker)}</p>`
      : "";
    const busy = Boolean(options.busy);
    const symbol = busy
      ? '<span class="reading-symbol" aria-hidden="true"></span>'
      : "";
    const person = personContext(options.person);
    const actions = options.actions
      ? `<div class="panel-actions">${options.actions}</div>`
      : "";
    return `<section class="affiliation-panel state-panel ${
      options.className || ""
    }"${busy ? ' aria-busy="true"' : ""}>
      ${symbol}
      ${kicker}
      <h2 class="state-title" data-focus tabindex="-1">${escapeHTML(
        options.title,
      )}</h2>
      <p class="state-copy">${escapeHTML(options.body)}</p>
      ${person}
      ${options.extra || ""}
      ${actions}
    </section>`;
  }

  function recordPanel(record, person) {
    if (!record) {
      return statePanel({
        title: "当前政治面貌暂时无法显示",
        body: "当前记录无法完整显示，请稍后重新读取。",
        person,
        actions: actionButton("重新读取", "reload-read"),
      });
    }
    const affiliation = affiliationByCode.get(record.code);
    if (!affiliation) {
      return statePanel({
        title: "当前政治面貌暂时无法显示",
        body: "当前记录无法完整显示，请稍后重新读取。",
        person,
        actions: actionButton("重新读取", "reload-read"),
      });
    }
    const createdAt = formatCreatedAt(record.createdAt);
    return `<section class="affiliation-panel record-panel">
      <div class="record-heading">
        <div>
          <p class="panel-kicker">当前记录</p>
          <h2 class="state-title" data-focus tabindex="-1">当前政治面貌</h2>
        </div>
        <span class="read-only-label">只读</span>
      </div>
      ${personContext(person)}
      <div class="record-display">
        <span class="record-code" aria-hidden="true">${escapeHTML(
          affiliation.code,
        )}</span>
        <strong class="record-name">${escapeHTML(affiliation.name)}</strong>
      </div>
      <dl class="record-meta">
        <dt>生效时间</dt>
        <dd><time datetime="${escapeHTML(record.createdAt)}">${escapeHTML(
          createdAt,
        )}</time></dd>
      </dl>
      <p class="record-note">已有记录后，本人不能自行更正、撤回或清空。</p>
    </section>`;
  }

  function unsetPanel(person) {
    return `<section class="affiliation-panel state-panel">
      <p class="panel-kicker">本人资料</p>
      <h2 class="state-title" data-focus tabindex="-1">尚未填写政治面貌</h2>
      ${personContext(person)}
      <p class="state-copy">这是可选资料。未填写不影响员工服务和工牌。</p>
      <p class="page-note">你可以自愿填写。提交后，本人不能自行修改、撤回或清空。</p>
      <div class="panel-actions">${actionButton(
        "填写政治面貌",
        "start-form",
      )}</div>
    </section>`;
  }

  function optionsMarkup(selectedCode) {
    return affiliations
      .map(
        (affiliation) => `<label class="affiliation-option">
          <input
            id="affiliation-${affiliation.code}"
            type="radio"
            name="affiliation"
            value="${affiliation.code}"
            ${selectedCode === affiliation.code ? "checked" : ""}
          />
          <span class="affiliation-option-text">
            <span class="option-code" aria-hidden="true">${affiliation.code}</span>
            <span class="option-name">${escapeHTML(affiliation.name)}</span>
          </span>
        </label>`,
      )
      .join("");
  }

  function selectionMarkup(selectedCode) {
    if (!selectedCode) {
      return "尚未选择";
    }
    return `已选择：<strong>${escapeHTML(affiliationText(selectedCode))}</strong>`;
  }

  function formPanel(state) {
    const selected = state.selectedCode;
    return `<section class="affiliation-panel form-panel">
      <p class="panel-kicker">首次填写</p>
      <h2 class="state-title" data-focus tabindex="-1">填写政治面貌</h2>
      ${personContext(state.person)}
      <p class="state-copy">请选择与你本人一致的一项。提交后，本人不能自行修改或清空。</p>
      <form id="affiliation-form" class="affiliation-form" autocomplete="off">
        <fieldset aria-describedby="option-hint">
          <legend>选择一项（共 13 项）</legend>
          <p class="option-hint" id="option-hint">未选择时不能继续。</p>
          <div class="affiliation-options">${optionsMarkup(selected)}</div>
        </fieldset>
        <p class="selection-status" id="selection-status" aria-live="polite">${selectionMarkup(
          selected,
        )}</p>
        <div class="form-actions">
          <button type="submit" class="button primary" ${
            selected ? "" : "disabled"
          }>继续</button>
          ${accountLink()}
        </div>
      </form>
    </section>`;
  }

  function confirmationPanel(state) {
    const affiliation = affiliationByCode.get(state.selectedCode);
    if (!affiliation) {
      return statePanel({
        title: "确认内容暂时无法显示",
        body: "请返回选择后重试。",
        person: state.person,
        actions: actionButton("返回选择", "back-to-form"),
      });
    }
    return `<section class="affiliation-panel confirm-panel">
      <p class="panel-kicker">确认填写</p>
      <h2 class="state-title" data-focus tabindex="-1">确认填写政治面貌</h2>
      ${personContext(state.person)}
      <p class="state-copy">请核对本人和所选内容，再明确提交。</p>
      <dl class="confirm-summary">
        <div class="confirm-row">
          <dt>本人</dt>
          <dd>${escapeHTML(state.person.name)}</dd>
        </div>
        <div class="confirm-row">
          <dt>将填写</dt>
          <dd class="confirm-choice">${escapeHTML(
            affiliationText(affiliation.code),
          )}</dd>
        </div>
      </dl>
      <div class="confirm-warning">
        <svg viewBox="0 0 24 24" aria-hidden="true">
          <path d="M12 3 2.8 20h18.4L12 3Z" />
          <path d="M12 9v5" />
          <path d="M12 17.5h.01" />
        </svg>
        <span>提交后，你不能自行修改、撤回或清空。</span>
      </div>
      <div class="panel-actions">
        ${actionButton("确认填写", "confirm-submit")}
        ${actionButton("返回选择", "back-to-form", { primary: false })}
      </div>
    </section>`;
  }

  function pendingSummary(code) {
    if (!code) {
      return "";
    }
    return `<div class="pending-summary"><span>本次填写</span><strong>${escapeHTML(
      affiliationText(code),
    )}</strong></div>`;
  }

  function successPanel(state) {
    const action = state.currentPanel
      ? ""
      : actionButton("查看当前政治面貌", "view-current");
    const body = state.currentPanel
      ? "本次填写已完成。"
      : "本次填写已完成。需要查看当前记录时，可以继续读取。";
    return `<section class="affiliation-panel success-panel">
      <span class="success-mark" aria-hidden="true">
        <svg viewBox="0 0 24 24"><path d="m5 12.5 4.3 4.3L19 7" /></svg>
      </span>
      <p class="panel-kicker success-kicker">已确认完成</p>
      <h2 class="state-title" data-focus tabindex="-1">政治面貌已填写</h2>
      ${personContext(state.person)}
      <p class="state-copy">${body}</p>
      <div class="panel-actions">${action}${accountLink()}</div>
    </section>`;
  }

  function unknownPanel(state) {
    const currentAction = state.currentPanel
      ? ""
      : actionButton("查看当前政治面貌", "view-current", {
          primary: !state.hasMaterial,
        });
    const retryAction = state.hasMaterial
      ? actionButton("重试本次填写", "retry-write")
      : "";
    const summary = state.hasMaterial
      ? pendingSummary(state.selectedCode)
      : "";
    const materialNote = state.hasMaterial
      ? `<p class="retry-note"><strong>重试本次填写</strong>会再次提交上面的选项，可能完成本次填写。请不要另选一项。</p>`
      : `<p class="retry-note">本页已无法重试原填写。你仍可查看当前记录。</p>`;
    return `<section class="affiliation-panel unknown-panel">
      <p class="panel-kicker">结果尚未确认</p>
      <h2 class="state-title" data-focus tabindex="-1">暂时无法确认填写结果</h2>
      ${personContext(state.person)}
      <p class="state-copy">本次填写可能已经完成。离开本页不会取消已经发出的填写。</p>
      ${summary}
      ${materialNote}
      <p class="page-note">查看当前政治面貌只读取当前记录，不能证明本次填写是否完成。</p>
      <div class="panel-actions">${retryAction}${currentAction}${accountLink()}</div>
    </section>`;
  }

  function currentPanelMarkup(state) {
    const successContext = state.currentContext === "success";
    const contextNote = (mode) => {
      if (successContext) {
        if (mode === "loading") {
          return "读取完成前不显示记录。已确认的填写结果不受影响。";
        }
        if (mode === "error") {
          return "已确认的填写结果仍然有效；读取失败只影响当前值展示。";
        }
        if (mode === "unset") {
          return "已确认的填写结果仍然有效；这次读取只补充当前记录。";
        }
        return "这次读取只补充当前记录，不改变本次已完成的结果。";
      }
      if (mode === "loading") {
        return "读取完成前不显示记录。原填写结果仍未确认。";
      }
      if (mode === "error") {
        return "本次读取失败不说明原填写是否完成，原结果仍未确认。";
      }
      if (mode === "unset") {
        return "这次读取没有找到记录，但不能证明原填写未完成，也不会在本页开始新的填写。";
      }
      return "当前有值只来自这次另行读取，不证明原填写已经完成；原结果仍未确认。";
    };

    if (state.currentPanel === "loading") {
      return `<section class="affiliation-panel current-panel state-panel" aria-busy="true">
        <span class="reading-symbol" aria-hidden="true"></span>
        <p class="panel-kicker">另行读取</p>
        <h2 class="state-title" data-focus tabindex="-1">正在读取当前政治面貌</h2>
        <p class="state-copy">${escapeHTML(contextNote("loading"))}</p>
      </section>`;
    }

    if (state.currentPanel === "error") {
      return `<section class="affiliation-panel current-panel state-panel">
        <p class="panel-kicker">另行读取</p>
        <h2 class="state-title" data-focus tabindex="-1">暂时无法读取当前政治面貌</h2>
        <p class="state-copy">请稍后重新读取。</p>
        <p class="page-note">${escapeHTML(contextNote("error"))}</p>
        <div class="panel-actions">${actionButton(
          "重新读取当前政治面貌",
          "reload-current",
        )}</div>
      </section>`;
    }

    if (state.currentPanel === "unset") {
      return `<section class="affiliation-panel current-panel state-panel">
        <p class="panel-kicker">另行读取</p>
        <h2 class="state-title" data-focus tabindex="-1">当前尚未填写</h2>
        <p class="state-copy">这次读取没有找到政治面貌记录。</p>
        <p class="page-note">${escapeHTML(contextNote("unset"))}</p>
      </section>`;
    }

    const record = state.readValue;
    const affiliation = record
      ? affiliationByCode.get(record.code)
      : undefined;
    if (!record || !affiliation) {
      return "";
    }
    return `<section class="affiliation-panel current-panel record-panel">
      <div class="record-heading">
        <div>
          <p class="panel-kicker">另行读取</p>
          <h2 class="state-title" data-focus tabindex="-1">当前政治面貌</h2>
        </div>
        <span class="read-only-label">只读</span>
      </div>
      <div class="record-display">
        <span class="record-code" aria-hidden="true">${escapeHTML(
          affiliation.code,
        )}</span>
        <strong class="record-name">${escapeHTML(affiliation.name)}</strong>
      </div>
      <dl class="record-meta">
        <dt>生效时间</dt>
        <dd><time datetime="${escapeHTML(record.createdAt)}">${escapeHTML(
          formatCreatedAt(record.createdAt),
        )}</time></dd>
      </dl>
      <p class="record-note">${escapeHTML(contextNote("value"))}</p>
    </section>`;
  }

  function mainPanel(state) {
    switch (state.page) {
      case "identity-loading":
        return statePanel({
          className: "identity-panel",
          busy: true,
          title: "正在读取当前账户",
          body: "确认完成后，再显示本人资料。",
        });
      case "identity-error":
        return statePanel({
          className: "identity-panel",
          title: "暂时无法读取当前账户",
          body: "请稍后重新检查。暂时无法读取不代表登录已失效。",
          actions: `${actionButton(
            "重新检查",
            "reload-identity",
          )}${homeLink()}`,
        });
      case "identity-limited":
        return statePanel({
          className: "identity-panel",
          title: "暂时无法读取当前账户",
          body: "当前检查暂时无法进行，请稍后再试。",
          actions: `${actionButton("稍后重新检查", "", {
            disabled: true,
          })}${homeLink()}`,
        });
      case "reading":
        return statePanel({
          busy: true,
          title: "正在读取当前政治面貌",
          body: "读取完成前，不显示旧值，也不判断是否未填写。",
          person: state.person,
        });
      case "read-error":
        return statePanel({
          kicker: "当前记录",
          title: "暂时无法读取当前政治面貌",
          body: "请稍后重新读取。读取失败不代表未填写。",
          person: state.person,
          actions: `${actionButton("重新读取", "reload-read")}${accountLink()}`,
        });
      case "read-limited":
        return statePanel({
          kicker: "当前记录",
          title: "暂时无法读取当前政治面貌",
          body: "当前读取暂时无法进行，请稍后再试。这次未取得结果，不代表未填写。",
          person: state.person,
          actions: `${actionButton("稍后重新读取", "", {
            disabled: true,
          })}${accountLink()}`,
        });
      case "value":
        return recordPanel(state.readValue, state.person);
      case "unset":
        return unsetPanel(state.person);
      case "dictionary-loading":
        return statePanel({
          busy: true,
          kicker: "尚未填写",
          title: "正在读取填写选项",
          body: "选项读取完成后，才能开始填写。",
          person: state.person,
        });
      case "dictionary-error":
        return statePanel({
          kicker: "尚未填写",
          title: "暂时无法读取填写选项",
          body: "请稍后重新读取。选项尚未取得时不开放填写。",
          person: state.person,
          actions: `${actionButton(
            "重新读取选项",
            "reload-dictionary",
          )}${accountLink()}`,
        });
      case "dictionary-limited":
        return statePanel({
          kicker: "尚未填写",
          title: "暂时无法读取填写选项",
          body: "当前选项读取暂时无法进行，请稍后再试。",
          person: state.person,
          actions: `${actionButton("稍后重新读取", "", {
            disabled: true,
          })}${accountLink()}`,
        });
      case "value-dictionary-error":
        return statePanel({
          kicker: "当前记录",
          title: "当前政治面貌暂时无法显示",
          body: "当前政治面貌记录已读取，但完整名称暂未取得。请稍后重新读取。",
          person: state.person,
          actions: `${actionButton("重新读取", "reload-read")}${accountLink()}`,
        });
      case "session-invalid":
        return statePanel({
          className: "identity-panel",
          title: "请先登录",
          body: "登录后，再查看个人账户。",
          actions: `${linkButton(
            "手机号登录",
            "halo-authentication.html",
          )}${homeLink()}`,
        });
      case "forbidden":
        return statePanel({
          className: "identity-panel",
          title: "暂时无法继续访问",
          body: "本次访问暂时未能继续，请从个人账户重新进入。如仍无法打开，请稍后再试。",
          actions: `${linkButton(
            "打开个人账户",
            "halo-account.html",
          )}${homeLink()}`,
        });
      case "account-changed":
        return statePanel({
          className: "identity-panel",
          title: "当前账户需要重新确认",
          body: "此前的账户资料已隐藏。请重新确认当前账户。",
          actions: `${actionButton(
            "重新确认当前账户",
            "recheck-account",
          )}${homeLink()}`,
        });
      case "form":
        return formPanel(state);
      case "confirm":
        return confirmationPanel(state);
      case "submitting":
        return statePanel({
          className: "submitting-panel",
          busy: true,
          kicker: "请稍候",
          title: "正在提交填写",
          body: "暂时不用重复操作。",
          person: state.person,
        });
      case "submit-limited":
        return statePanel({
          kicker: "本次填写尚未受理",
          title: "请稍后再试",
          body: "本次提交未被受理，尚未收到填写完成的结果。此页会保留你的选择，等待期间不会自动提交。",
          person: state.person,
          extra: pendingSummary(state.selectedCode),
          actions: `${actionButton("稍后可重新提交", "", {
            disabled: true,
          })}${accountLink()}`,
        });
      case "submit-conflict":
        return statePanel({
          kicker: "不能再次填写",
          title: "当前已有政治面貌记录",
          body: "本次填写未提交成功。你可以查看当前记录，不能再次填写。",
          person: state.person,
          actions: `${actionButton("查看当前政治面貌", "reload-read")}${accountLink()}`,
        });
      case "submit-invalid":
        return statePanel({
          kicker: "本次填写未提交成功",
          title: "请重新核对所选内容",
          body: "本次提交的内容未能通过检查。请返回选择后重新确认。",
          person: state.person,
          extra: pendingSummary(state.selectedCode),
          actions: `${actionButton("返回选择", "back-to-form")}${accountLink()}`,
        });
      case "submit-failed":
        return statePanel({
          kicker: "本次填写未完成",
          title: "暂时无法完成填写",
          body: "本次填写未完成。服务恢复后，你可以按本次选择重试。",
          person: state.person,
          extra: pendingSummary(state.selectedCode),
          actions: `${actionButton("重试本次填写", "confirm-submit")}${accountLink()}`,
        });
      case "success":
        return `${successPanel(state)}${currentPanelMarkup(state)}`;
      case "unknown":
        return `${unknownPanel(state)}${currentPanelMarkup(state)}`;
      case "retry-submitting":
        return statePanel({
          className: "submitting-panel",
          busy: true,
          kicker: "结果尚未确认",
          title: "正在重试本次填写",
          body: "请稍候，暂时不用重复操作。重试会按原选项再次提交。",
          person: state.person,
          extra: pendingSummary(state.selectedCode),
        });
      case "retry-limited":
        return statePanel({
          kicker: "结果尚未确认",
          title: "暂时无法重试本次填写",
          body: "本次重试未被受理，请稍后再试。此前填写结果仍未确认。",
          person: state.person,
          extra: `${pendingSummary(
            state.selectedCode,
          )}<p class="retry-note">未获受理不等于原填写失败；等待期间不会自动重试。</p>`,
          actions: `${actionButton("稍后可重试本次填写", "", {
            disabled: true,
          })}${accountLink()}`,
        });
      case "retry-mismatch":
        return statePanel({
          kicker: "结果尚未确认",
          title: "本次重试无法继续",
          body: "当前无法把本次重试与原先填写对应，已停止重试。此前填写结果仍未确认。",
          person: state.person,
          extra: `${pendingSummary(
            state.selectedCode,
          )}<p class="retry-note">不能改选其他选项来代替本次填写。</p>`,
          actions: `${actionButton(
            "查看当前政治面貌",
            "view-current",
          )}${accountLink()}`,
        });
      case "retry-conflict":
        return statePanel({
          kicker: "结果尚未确认",
          title: "当前已有政治面貌记录",
          body: "本次重试未被受理。此前填写结果仍未确认。",
          person: state.person,
          extra: `${pendingSummary(
            state.selectedCode,
          )}<p class="retry-note">当前记录需要另行读取，不能用它判断原填写是否完成。</p>`,
          actions: `${actionButton(
            "查看当前政治面貌",
            "view-current",
          )}${accountLink()}`,
        });
      default:
        return statePanel({
          className: "identity-panel",
          title: "暂时无法显示本页",
          body: "请返回个人账户后重试。",
          actions: homeLink(),
        });
    }
  }

  function asideMarkup(heading, paragraphs) {
    return `<aside class="affiliation-aside" aria-labelledby="aside-heading">
      <div class="aside-rule" aria-hidden="true"></div>
      <h2 id="aside-heading">${escapeHTML(heading)}</h2>
      ${paragraphs.map((paragraph) => `<p>${paragraph}</p>`).join("")}
    </aside>`;
  }

  function asideFor(state) {
    if (
      maskedPages.has(state.page) ||
      state.page === "account-changed"
    ) {
      return "";
    }
    if (["reading", "read-error", "read-limited"].includes(state.page)) {
      return "";
    }
    if (state.page === "value") {
      return "";
    }
    if (state.page === "unset") {
      return "";
    }
    if (
      ["dictionary-loading", "dictionary-error", "dictionary-limited"].includes(
        state.page,
      )
    ) {
      return "";
    }
    if (state.page === "value-dictionary-error") {
      return "";
    }
    if (
      ["form", "confirm", "submitting", "submit-limited"].includes(state.page)
    ) {
      return asideMarkup("填写前请确认", [
        "这是可选资料，未填写不影响员工服务和工牌。",
        "未填写不等同于选择“群众”。请按本人情况选择，不确定时可暂不填写。",
      ]);
    }
    if (state.page === "success") {
      return "";
    }
    if (state.page === "unknown" && !state.hasMaterial) {
      return "";
    }
    if (state.page === "unknown" || state.page === "retry-submitting") {
      return "";
    }
    if (state.page === "retry-limited") {
      return "";
    }
    if (state.page === "retry-mismatch") {
      return "";
    }
    if (state.page === "retry-conflict") {
      return "";
    }
    return "";
  }

  function focusAfterRender(selector) {
    window.requestAnimationFrame(() => {
      const target = content.querySelector(selector) || pageTitle;
      if (target instanceof HTMLElement) {
        target.focus();
      }
    });
  }

  function render(options = {}) {
    if (!model) {
      return;
    }
    const heading = pageHeading(model);
    const aside = asideFor(model);
    const notice = model.notice
      ? `<p class="inline-notice" role="status">${escapeHTML(
          model.notice,
        )}</p>`
      : "";
    pageTitle.textContent = heading.title;
    pageKicker.textContent = heading.kicker;
    pageDescription.textContent = heading.description;
    document.title = heading.documentTitle;
    content.innerHTML = `<div class="affiliation-columns${
      aside ? "" : " single"
    }"><div class="affiliation-main">${notice}${mainPanel(
      model,
    )}</div>${aside}</div>`;
    stateBasis.textContent = basisFor(model);
    if (options.focus) {
      focusAfterRender(options.focusSelector || "[data-focus]");
    }
  }

  function clearTimer() {
    if (timer) {
      window.clearTimeout(timer);
      timer = 0;
    }
  }

  function schedule(callback) {
    clearTimer();
    timer = window.setTimeout(() => {
      timer = 0;
      callback();
    }, 700);
  }

  function enterScenario(id, options = {}) {
    clearTimer();
    if (leaveDialog.open) {
      leaveDialog.close();
    }
    model = buildModel(id);
    scenarioPicker.value = id;
    render({ focus: Boolean(options.focus) });
  }

  function startForm() {
    clearTimer();
    model.page = "form";
    model.selectedCode = "";
    model.currentPanel = null;
    model.readValue = null;
    model.currentContext = null;
    model.notice = "";
    model.customBasis = "";
    render({ focus: true });
  }

  function returnToForm() {
    if (!model.selectedCode) {
      startForm();
      return;
    }
    model.page = "form";
    render({ focus: false });
    window.requestAnimationFrame(() => {
      const selectedRadio = content.querySelector(
        'input[name="affiliation"]:checked',
      );
      if (selectedRadio instanceof HTMLElement) {
        selectedRadio.focus();
      } else {
        pageTitle.focus();
      }
    });
  }

  function beginSubmit(page) {
    clearTimer();
    model.page = page;
    model.currentPanel = null;
    model.readValue = null;
    model.currentContext = null;
    model.notice = "";
    model.customBasis = "";
    render({ focus: true });
    schedule(() => {
      if (!model) {
        return;
      }
      model.page = "success";
      model.selectedCode = "";
      model.hasMaterial = false;
      model.currentPanel = null;
      model.readValue = null;
      model.currentContext = "success";
      model.notice = "";
      render({ focus: true });
    });
  }

  function beginCurrentRead(context) {
    if (!model) {
      return;
    }
    clearTimer();
    if (model.page === "retry-mismatch" || model.page === "retry-conflict") {
      model.page = "unknown";
      model.selectedCode = "";
      model.hasMaterial = false;
    }
    model.currentContext = context;
    model.currentPanel = "loading";
    model.readValue = null;
    model.notice = "";
    model.customBasis = "";
    render({
      focus: true,
      focusSelector: ".current-panel [data-focus]",
    });
    schedule(() => {
      if (!model) {
        return;
      }
      model.currentPanel = "value";
      model.readValue =
        context === "success" ? currentFixtures.long : currentFixtures.unknown;
      render({
        focus: true,
        focusSelector: ".current-panel [data-focus]",
      });
    });
  }

  function reloadValue() {
    clearTimer();
    model.page = "reading";
    model.currentPanel = null;
    model.readValue = null;
    model.currentContext = null;
    model.notice = "";
    model.customBasis = "";
    render({ focus: true });
    schedule(() => {
      if (!model) {
        return;
      }
      model.page = "value";
      model.readValue = currentFixtures.primary;
      render({ focus: true });
    });
  }

  function reloadDictionary() {
    clearTimer();
    model.page = "dictionary-loading";
    model.selectedCode = "";
    model.currentPanel = null;
    model.readValue = null;
    model.currentContext = null;
    model.notice = "";
    model.customBasis = "";
    render({ focus: true });
    schedule(() => {
      if (!model) {
        return;
      }
      model.page = "form";
      render({ focus: true });
    });
  }

  function reloadIdentity() {
    clearTimer();
    model.page = "identity-loading";
    model.person = null;
    model.selectedCode = "";
    model.hasMaterial = false;
    model.currentPanel = null;
    model.readValue = null;
    model.currentContext = null;
    model.notice = "";
    model.customBasis = "";
    render({ focus: true });
    schedule(() => {
      if (!model) {
        return;
      }
      model.page = "unset";
      model.person = people.primary;
      render({ focus: true });
    });
  }

  function recheckAccount() {
    clearTimer();
    model.page = "identity-loading";
    model.person = null;
    model.selectedCode = "";
    model.hasMaterial = false;
    model.currentPanel = null;
    model.readValue = null;
    model.currentContext = null;
    model.notice = "";
    model.customBasis = "";
    render({ focus: true });
    schedule(() => {
      if (!model) {
        return;
      }
      model.page = "value";
      model.person = people.other;
      model.readValue = currentFixtures.other;
      model.notice = "已重新读取当前账户，请核对本人资料。";
      model.customBasis =
        "账户选择变化后已重新核对；页面只展示新选择的当前账户，不沿用上一账户的值。";
      render({ focus: true });
    });
  }

  function updateSelection(code) {
    model.selectedCode = code;
    const status = content.querySelector("#selection-status");
    if (status) {
      status.innerHTML = selectionMarkup(code);
    }
    const submit = content.querySelector(
      '#affiliation-form button[type="submit"]',
    );
    if (submit instanceof HTMLButtonElement) {
      submit.disabled = !code;
    }
  }

  function leaveMode() {
    if (!model || maskedPages.has(model.page)) {
      return "";
    }
    if (
      model.page === "submitting" ||
      model.page === "retry-submitting" ||
      model.page === "unknown" ||
      model.page.startsWith("retry-")
    ) {
      return "pending";
    }
    if (draftPages.has(model.page) && model.selectedCode) {
      return "draft";
    }
    return "";
  }

  function openLeaveDialog(target, trigger, mode) {
    if (leaveDialog.open) {
      return;
    }
    pendingHref = target;
    leaveTrigger = trigger;
    leaving = false;
    if (mode === "draft") {
      leaveTitle.textContent = "放弃当前选择？";
      leaveDescription.textContent =
        "当前选择尚未提交。离开后需要重新选择。";
    } else {
      leaveTitle.textContent = "离开前，请留意填写结果";
      leaveDescription.textContent =
        "离开不会取消已经发出的填写。返回后无法用本页原输入重试。";
    }
    leaveDialog.showModal();
    window.requestAnimationFrame(() => stayButton.focus());
  }

  function clearSensitivePage() {
    clearTimer();
    if (leaveDialog.open) {
      leaveDialog.close();
    }
    model = null;
    pendingHref = "";
    leaveTrigger = null;
    leaving = false;
    content.innerHTML = "";
    stateBasis.textContent = "";
    pageTitle.textContent = "个人账户";
    pageKicker.textContent = "个人账户";
    pageDescription.textContent = "正在重新确认当前账户。";
    document.title = "个人账户 · 司籍处";
  }

  buildScenarioPicker();

  const query = new URLSearchParams(window.location.search);
  applyTheme(query.get("theme"));
  themePicker.addEventListener("change", () => {
    applyTheme(themePicker.value);
  });
  scenarioPicker.addEventListener("change", () => {
    enterScenario(scenarioPicker.value, { focus: false });
  });

  document.addEventListener("click", (event) => {
    if (!(event.target instanceof Element)) {
      return;
    }
    const control = event.target.closest("[data-action]");
    if (control instanceof HTMLElement && !control.hasAttribute("disabled")) {
      const action = control.dataset.action;
      if (action === "start-form") {
        startForm();
      } else if (action === "confirm-submit") {
        beginSubmit("submitting");
      } else if (action === "back-to-form") {
        returnToForm();
      } else if (action === "retry-write") {
        beginSubmit("retry-submitting");
      } else if (action === "view-current") {
        beginCurrentRead(model?.page === "success" ? "success" : "unknown");
      } else if (action === "reload-current") {
        beginCurrentRead(model?.currentContext || "unknown");
      } else if (action === "reload-read") {
        reloadValue();
      } else if (action === "reload-dictionary") {
        reloadDictionary();
      } else if (action === "reload-identity") {
        reloadIdentity();
      } else if (action === "recheck-account") {
        recheckAccount();
      } else if (action === "close-leave") {
        leaveDialog.close();
      }
      return;
    }

    if (
      event.defaultPrevented ||
      event.button !== 0 ||
      event.metaKey ||
      event.ctrlKey ||
      event.shiftKey ||
      event.altKey
    ) {
      return;
    }
    const anchor = event.target.closest("a[data-leave]");
    if (!(anchor instanceof HTMLAnchorElement)) {
      return;
    }
    const mode = leaveMode();
    if (!mode) {
      return;
    }
    event.preventDefault();
    openLeaveDialog(anchor.href, anchor, mode);
  });

  content.addEventListener("change", (event) => {
    if (!(event.target instanceof HTMLInputElement)) {
      return;
    }
    if (
      event.target.name !== "affiliation" ||
      event.target.type !== "radio"
    ) {
      return;
    }
    updateSelection(event.target.value);
  });

  content.addEventListener("submit", (event) => {
    if (
      !(event.target instanceof HTMLFormElement) ||
      event.target.id !== "affiliation-form"
    ) {
      return;
    }
    event.preventDefault();
    if (!model || !model.selectedCode) {
      return;
    }
    model.page = "confirm";
    render({ focus: true });
  });

  stayButton.addEventListener("click", () => {
    leaveDialog.close();
  });

  leaveButton.addEventListener("click", () => {
    const target = pendingHref;
    leaving = true;
    leaveDialog.close();
    if (target) {
      window.location.href = target;
    }
  });

  leaveDialog.addEventListener("close", () => {
    if (!leaving && leaveTrigger?.isConnected) {
      leaveTrigger.focus();
    }
    pendingHref = "";
    leaveTrigger = null;
    leaving = false;
  });

  window.addEventListener("pagehide", () => {
    clearSensitivePage();
  });

  window.addEventListener("pageshow", (event) => {
    if (event.persisted) {
      enterScenario("unset", { focus: false });
    }
  });

  const requestedState = query.get("state");
  const initialState =
    requestedState && Object.hasOwn(scenarios, requestedState)
      ? requestedState
      : "unset";
  enterScenario(initialState, { focus: false });
})();
