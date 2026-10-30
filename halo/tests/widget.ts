export const widget = `
window.__widgets = [];
window.turnstile = {
  render(element, options) {
    const id = String(window.__widgets.length);
    const button = document.createElement("button");
    button.type = "button";
    button.textContent = "完成安全验证（测试替身）";
    button.onclick = () => {
      const prefix = options.action === "send_mobile_change_code" ? "mobile:"
        : options.action === "send_mobile_binding_code" ? "binding:" : "";
      options.callback(prefix + options.cData);
    };
    element.append(button);
    window.__widgets.push({ options, element, button, removed: false, resets: 0 });
    return id;
  },
  reset(id) {
    const widget = window.__widgets[Number(id)];
    widget.resets++;
    widget.options["expired-callback"]();
  },
  remove(id) {
    const widget = window.__widgets[Number(id)];
    widget.removed = true;
    widget.element.replaceChildren();
  }
};`;
