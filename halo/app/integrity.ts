declare global {
  interface Window {
    __halo?: { reloadRequired: boolean };
  }
}

export function reloadRequired() {
  // Failure to load the early observer itself also prevents business writes.
  return window.__halo?.reloadRequired !== false;
}

export function requestReload() {
  window.__halo = { reloadRequired: true };
  window.dispatchEvent(new Event("halo:reload-required"));
}
