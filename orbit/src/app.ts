import { Controller } from "./controller";

App({
  onLaunch() {
    try { this.identity = new Controller(wx, ORBIT_API_ORIGIN); }
    catch { /* The page exposes configuration failure without sending requests. */ }
  },
  onShow() { this.identity?.resume(); },
  onHide() { this.identity?.suspend(); },
});
