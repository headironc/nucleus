const menuButton = document.querySelector(".menu-toggle");
const mobileNavigation = document.querySelector(".mobile-navigation");
const dialog = document.querySelector(".preview-dialog");

function closeNavigation() {
  menuButton.setAttribute("aria-expanded", "false");
  menuButton.setAttribute("aria-label", "打开导航");
  mobileNavigation.hidden = true;
}

menuButton.addEventListener("click", () => {
  const expanded = menuButton.getAttribute("aria-expanded") === "true";
  menuButton.setAttribute("aria-expanded", String(!expanded));
  menuButton.setAttribute("aria-label", expanded ? "打开导航" : "关闭导航");
  mobileNavigation.hidden = expanded;
});

mobileNavigation.querySelectorAll("a").forEach((link) => {
  link.addEventListener("click", () => {
    closeNavigation();
  });
});

document.addEventListener("keydown", (event) => {
  if (event.key === "Escape" && !mobileNavigation.hidden) {
    closeNavigation();
    menuButton.focus();
  }
});

window.matchMedia("(min-width: 761px)").addEventListener("change", (event) => {
  if (event.matches) closeNavigation();
});

document.querySelectorAll("[data-preview-title]").forEach((button) => {
  button.addEventListener("click", (event) => {
    event.preventDefault();
    document.getElementById("preview-title").textContent =
      button.dataset.previewTitle;
    document.getElementById("preview-description").textContent =
      button.dataset.previewDescription;
    dialog.showModal();
  });
});

document
  .querySelector(".dialog-close")
  .addEventListener("click", () => dialog.close());

const filterButtons = document.querySelectorAll("[data-filter]");
const stories = document.querySelectorAll(".story");
const homeScenario = document.getElementById("home-scenario");
function renderHomeContent() {
  const scenario = homeScenario.value;
  const selected = document.querySelector('[data-filter][aria-pressed="true"]');
  const category = selected.dataset.filter;
  let count = 0;
  stories.forEach((story, index) => {
    const unavailable =
      scenario === "block-empty" ||
      (scenario === "partial-unavailable" && index === 0);
    story.hidden =
      unavailable ||
      (category !== "all" && story.dataset.category !== category);
    if (!story.hidden) count += 1;
  });
  document.getElementById("reading").hidden = scenario === "block-empty";
  document.querySelectorAll(".explore-item").forEach((entry, index) => {
    entry.hidden =
      scenario === "block-empty" ||
      (scenario === "partial-unavailable" && index === 1);
  });
  document.getElementById("explore").hidden = scenario === "block-empty";
  document.querySelectorAll('[href="#explore"]').forEach((link) => {
    link.hidden = scenario === "block-empty";
  });
  document.getElementById("reading-status").textContent =
    `显示${selected.textContent.trim()} ${count} 篇推荐阅读`;
}
homeScenario.addEventListener("change", renderHomeContent);
filterButtons.forEach((button) => {
  button.addEventListener("click", () => {
    filterButtons.forEach((item) =>
      item.setAttribute("aria-pressed", String(item === button)),
    );
    renderHomeContent();
  });
});
