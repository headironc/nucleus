(() => {
  const source = document.currentScript?.src;
  if (!source) return;
  const assets = new URL(".", source);
  // This blocking script runs before stylesheet and module resources can fail.
  window.__halo ??= { reloadRequired: false };
  window.addEventListener(
    "error",
    (event) => {
      const target = event.target;
      const source =
        target instanceof HTMLScriptElement
          ? target.src
          : target instanceof HTMLLinkElement
            ? target.href
            : target instanceof HTMLImageElement
              ? target.currentSrc || target.src
              : undefined;
      if (!source) return;
      const url = new URL(source, location.href);
      if (
        url.origin === assets.origin &&
        url.pathname.startsWith(assets.pathname)
      ) {
        window.__halo.reloadRequired = true;
        window.dispatchEvent(new Event("halo:reload-required"));
      }
    },
    true,
  );
})();
