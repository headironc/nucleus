const articles = [
  {
    id: "seasons",
    title: "四时有序，草木有时",
    summary: "从一草一木出发，读一读藏在四季里的传统文化。",
    column: "culture",
    image: "assets/halo-seasons.svg",
    coverAlt: "浅色纸面上的四时草木插画，枝叶与圆形日轮相映。",
    byline: "四时编辑部",
    published: "2026-10-16",
    updated: "2026-10-18",
  },
  {
    id: "table",
    title: "人间烟火，一味一故事",
    summary: "循着熟悉的器物，探寻日常生活中的风物与记忆。",
    column: "life",
    image: "assets/halo-table.svg",
    coverAlt: "纸色背景上的茶桌插画，茶壶、杯盏与枝叶相伴。",
    byline: "风物记录小组",
    published: "2026-10-12",
    updated: "2026-10-12",
  },
  {
    id: "courtyard",
    title: "一方庭院，收藏四时",
    summary: "循着一扇窗、一片叶，发现日常里安静的风景。",
    column: "culture",
    image: "assets/halo-botanical.svg",
    coverAlt: "弧形窗景中的草木插画，枝叶在纸色背景上舒展。",
    byline: "四时编辑部",
    published: "2026-10-08",
    updated: "2026-10-10",
  },
];
const columns = {
  all: {
    name: "四时拾记",
    description: "从草木风物，到日常生活。把值得细读的片刻，留在这里。",
  },
  culture: {
    name: "文化专栏",
    description: "循着草木与四时，读传统文化里的日常，也发现日常里的诗意。",
  },
  life: {
    name: "生活随笔",
    description: "一张桌子，一件旧物，一段记忆。在寻常的生活里，慢慢发现。",
  },
};
const params = new URLSearchParams(window.location.search);
const requestedArticle = params.get("article");
const article = articles.find((item) => item.id === requestedArticle);
const columnKey = params.get("column") || "all";
const column = columns[columnKey];
const picker = document.getElementById("scenario-picker");
const availableStates = [...picker.options].map((option) => option.value);
let state = params.get("state");
if (!availableStates.includes(state))
  state = requestedArticle ? "article-normal" : "list-normal";
if (requestedArticle && !article && state.startsWith("article-"))
  state = "article-not-found";
if (!column && state.startsWith("list-")) state = "list-unavailable";
const selectedArticle = article || articles[0];
const paginated =
  ["list-pagination", "list-page-out-of-range"].includes(state) ||
  params.has("page");
const requestedPage = params.get("page");
const invalidPage =
  params.has("page") &&
  (!/^[1-9]\d*$/.test(requestedPage) ||
    !Number.isSafeInteger(Number(requestedPage)));
const pageIndex =
  state === "list-page-out-of-range"
    ? 3
    : invalidPage
      ? 1
      : Number(requestedPage) || 1;
const pageSize = 2;
const visibleArticles = articles.filter(
  (item) => columnKey === "all" || item.column === columnKey,
);
if (
  ["list-normal", "list-pagination"].includes(state) &&
  paginated &&
  (invalidPage ||
    pageIndex > Math.max(1, Math.ceil(visibleArticles.length / pageSize)))
)
  state = "list-page-out-of-range";
function listAddress(page) {
  const query = new URLSearchParams();
  if (column && columnKey !== "all") query.set("column", columnKey);
  if (page !== undefined) {
    query.set("state", "list-pagination");
    query.set("page", String(page));
  }
  return `halo-reading.html${query.size ? `?${query}` : ""}`;
}
const listUrl = listAddress(paginated ? pageIndex : undefined);

function element(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text) node.textContent = text;
  return node;
}

function renderList() {
  document.getElementById("list-title").textContent = column.name;
  document.getElementById("list-description").textContent = column.description;
  document.title = `${column.name} · 司籍处`;
  document.querySelectorAll("[data-column]").forEach((link) => {
    if (link.dataset.column === columnKey)
      link.setAttribute("aria-current", "page");
  });
  document.getElementById("list-count").textContent =
    `共 ${visibleArticles.length} 篇文章 · 按首次发布时间排列`;
  if (paginated)
    document.querySelectorAll("[data-column]").forEach((link) => {
      const target = new URL(link.href);
      target.searchParams.set("state", "list-pagination");
      target.searchParams.set("page", "1");
      link.href = target.href;
    });
  const pageArticles = paginated
    ? visibleArticles.slice((pageIndex - 1) * pageSize, pageIndex * pageSize)
    : visibleArticles;
  const list = document.getElementById("article-list");
  pageArticles.forEach((item) => {
    const entry = element("article", "reading-entry");
    const link = element("a", "reading-entry-link");
    link.href = `halo-reading.html?article=${item.id}`;
    const image = element("img", "entry-image");
    image.src = item.image;
    image.alt = item.coverAlt;
    image.width = 560;
    image.height = 360;
    image.loading = "lazy";
    const copy = element("div", "entry-copy");
    const metadata = element("p", "entry-meta");
    const date = element("time", "", item.published.replaceAll("-", "."));
    date.dateTime = item.published;
    metadata.append(element("span", "", columns[item.column].name), date);
    copy.append(
      metadata,
      element("h2", "", item.title),
      element("p", "entry-summary", item.summary),
      element("span", "entry-action", "阅读全文 ↗"),
    );
    link.append(image, copy);
    entry.append(link);
    list.append(entry);
  });
  if (paginated) {
    document.getElementById("list-end").hidden = true;
    document.getElementById("list-pagination").hidden = false;
    document.getElementById("page-position").textContent =
      `第 ${pageIndex} / ${Math.ceil(visibleArticles.length / pageSize)} 页 · 共 ${visibleArticles.length} 篇`;
    const previous = document.getElementById("page-previous");
    previous.hidden = pageIndex <= 1;
    previous.href = listAddress(pageIndex - 1);
    document.getElementById("page-previous-disabled").hidden = pageIndex > 1;
    const next = document.getElementById("page-next");
    next.hidden = pageIndex * pageSize >= visibleArticles.length;
    next.href = listAddress(pageIndex + 1);
    document.getElementById("page-next-disabled").hidden =
      pageIndex * pageSize < visibleArticles.length;
  }
}

function renderArticle() {
  const item = selectedArticle;
  const disabled = state === "article-column-disabled";
  document.title = `${item.title} · 司籍处`;
  document.getElementById("article-title").textContent = item.title;
  document.getElementById("article-summary").textContent = item.summary;
  document.getElementById("article-byline").textContent =
    `署名：${item.byline}`;
  for (const field of ["published", "updated"]) {
    const time = document.getElementById(`article-${field}`);
    time.dateTime = item[field];
    time.textContent = item[field].replaceAll("-", ".");
  }
  document.getElementById("article-update").hidden =
    item.published === item.updated;
  const label = element(disabled ? "span" : "a", "", columns[item.column].name);
  if (!disabled) label.href = `halo-reading.html?column=${item.column}`;
  document.getElementById("article-column").append(label);
  document
    .getElementById("article-body")
    .append(document.getElementById(`body-${item.id}`).content.cloneNode(true));
  const back = document.getElementById("return-column");
  back.href = disabled
    ? "halo-reading.html"
    : `halo-reading.html?column=${item.column}`;
  back.textContent = disabled
    ? "返回全部文章 ←"
    : `返回${columns[item.column].name} ←`;
  document.getElementById("breadcrumb-separator").hidden = false;
  document.getElementById("breadcrumb-current").hidden = false;
  document.getElementById("breadcrumb-current").textContent = "正文";
}

const statuses = {
  "list-empty": [
    "暂无",
    "这里还没有文章",
    columnKey === "all"
      ? "这里暂时没有已发布的文章，稍后再来看看。"
      : "这个栏目暂时没有已发布的文章，先去其他栏目逛逛吧。",
    columnKey === "all" ? undefined : "浏览全部文章",
    columnKey === "all" ? undefined : "halo-reading.html",
  ],
  "list-page-out-of-range": [
    "404",
    "本页暂无文章",
    `${invalidPage ? "当前页码无效" : `第 ${pageIndex} 页超出范围`}。${columnKey === "all" ? "全部文章" : "这个栏目"}共 ${visibleArticles.length} 篇，返回第一页继续浏览。`,
    "返回第一页",
    listAddress(1),
  ],
  "list-loading": [
    "···",
    "正在加载文章列表",
    "请稍候，正在读取这个栏目的公开内容。",
  ],
  "list-error": [
    "稍候",
    "文章列表暂时读不到",
    "暂时无法读取文章列表，请稍后重试。",
    "重新加载",
    listUrl,
  ],
  "list-unavailable": [
    "404",
    "这个栏目暂不可用",
    "当前地址没有可供浏览的栏目。你可以返回全部文章，继续阅读。",
    "浏览全部文章",
    "halo-reading.html",
  ],
  "article-loading": [
    "···",
    "正在加载文章",
    "请稍候，正在读取这篇文章的公开内容。",
  ],
  "article-error": [
    "稍候",
    "文章暂时读不到",
    "暂时无法读取文章，请稍后重试。",
    "重新加载",
    `halo-reading.html?article=${selectedArticle.id}`,
  ],
  "article-not-found": [
    "404",
    "这篇文章暂不可用",
    "当前地址没有可供阅读的文章。你可以返回文章列表，看看其他内容。",
    "返回文章列表",
    "halo-reading.html",
  ],
};

picker.value = state;
document.getElementById("list-view").hidden =
  !state.startsWith("list-") ||
  ["list-unavailable", "list-page-out-of-range"].includes(state);
document.getElementById("list-content").hidden = ![
  "list-normal",
  "list-pagination",
].includes(state);
document.getElementById("article-view").hidden = ![
  "article-normal",
  "article-column-disabled",
].includes(state);
if (
  state.startsWith("list-") &&
  column &&
  !["list-unavailable", "list-page-out-of-range"].includes(state)
)
  renderList();
if (["article-normal", "article-column-disabled"].includes(state))
  renderArticle();
if (statuses[state]) {
  const [number, title, description, action, target] = statuses[state];
  document.title = `${title} · 司籍处`;
  document.getElementById("status-view").hidden = false;
  document.getElementById("status-number").textContent = number;
  document.getElementById("status-title").textContent = title;
  document.getElementById("status-description").textContent = description;
  const actionLink = document.getElementById("status-action");
  actionLink.hidden = !action;
  if (action) {
    actionLink.textContent = action;
    actionLink.href = target;
  }
  document
    .getElementById("status-view")
    .setAttribute("aria-busy", String(state.endsWith("loading")));
}

picker.addEventListener("change", () => {
  const next = new URLSearchParams();
  if (picker.value.startsWith("article-"))
    next.set("article", selectedArticle.id);
  else if (column && columnKey !== "all") next.set("column", columnKey);
  if (picker.value.startsWith("list-") && paginated)
    next.set("page", String(pageIndex));
  next.set("state", picker.value);
  window.location.href = `halo-reading.html?${next}`;
});
const previewToggle = document.querySelector(".preview-toggle");
previewToggle.addEventListener("click", () => {
  const expanded = previewToggle.getAttribute("aria-expanded") !== "true";
  previewToggle.setAttribute("aria-expanded", String(expanded));
  previewToggle.textContent = expanded ? "收起预览工具" : "展开预览工具";
});
const menu = document.querySelector(".menu-toggle");
const navigation = document.getElementById("mobile-navigation");
function closeMenu() {
  menu.setAttribute("aria-expanded", "false");
  menu.setAttribute("aria-label", "打开导航");
  navigation.hidden = true;
}
menu.addEventListener("click", () => {
  const expanded = menu.getAttribute("aria-expanded") !== "true";
  menu.setAttribute("aria-expanded", String(expanded));
  menu.setAttribute("aria-label", expanded ? "关闭导航" : "打开导航");
  navigation.hidden = !expanded;
});
document.addEventListener("keydown", (event) => {
  if (event.key === "Escape" && !navigation.hidden) {
    closeMenu();
    menu.focus();
  }
});
window.matchMedia("(min-width: 761px)").addEventListener("change", (event) => {
  if (event.matches) closeMenu();
});
