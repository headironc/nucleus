import type { View } from "../../controller";
import { selfPresentation } from "../../presentation";

function loginDate(value: string): string {
  const date = new Date(value);
  const two = (part: number) => String(part).padStart(2, "0");
  return `${date.getFullYear()}年${date.getMonth() + 1}月${date.getDate()}日 ${two(date.getHours())}:${two(date.getMinutes())}`;
}

Page({
  data: { phase: "checking", busy: false, canCheck: false, canLogin: false, canRecover: false,
    sessionsReady: false, logoutPending: false, homeAction: false, message: "正在加载登录列表…",
    heading: "正在加载登录情况…", recoveryLabel: "查看退出结果", recoveryKind: "",
    sessions: [] as { id: string; sourceLabel: string; date: string; current: boolean }[] },
  onShow() {
    const identity = getApp().identity;
    if (!identity) {
      this.setData({ phase: "unavailable", homeAction: true, heading: "暂时无法打开登录管理", message: "请返回首页后重新打开。" });
      return;
    }
    identity.subscribe((view: View) => {
      this.setData({ phase: view.phase, busy: view.busy, canCheck: view.canCheck, canLogin: view.canLogin,
        canRecover: view.canRecover, logoutPending: view.logoutPending, sessionsReady: view.sessionsReady,
        homeAction: view.homeAction, recoveryKind: view.recoveryKind, ...selfPresentation(view, "sessions"),
        sessions: view.sessions.map(item => ({ id: item.id, sourceLabel: item.source === "website" ? "网站" : "小程序",
          date: loginDate(item.createdAt), current: item.current })) });
    });
    void identity.show("sessions");
  },
  onHide() {
    getApp().identity?.hide();
    getApp().identity?.subscribe(() => {});
    this.setData({ sessions: [], sessionsReady: false });
  },
  onUnload() { this.onHide(); },
  refresh() { void getApp().identity?.check(); },
  recover() { void getApp().identity?.recover(); },
  logoutOther(event: { currentTarget: { dataset: { id: string } } }) {
    const identity = getApp().identity;
    const target = identity?.targetSelection(event.currentTarget.dataset.id);
    const row = this.data.sessions.find(item => item.id === target?.targetId);
    if (!identity || !target || !row) return;
    wx.showModal({ title: "退出所选登录？", content: `${row.sourceLabel}，登录于 ${row.date}。退出后，该处需重新登录。`, confirmText: "退出登录",
      success: result => { if (result.confirm) void identity.logoutOther(target); } });
  },
  logoutAll() {
    const identity = getApp().identity;
    const selection = identity?.selection();
    if (!identity || !selection) return;
    wx.showModal({ title: "退出全部登录？", content: "将退出网站和小程序的全部登录，包括本次登录。再次进入个人账户需要重新登录。", confirmText: "全部退出",
      success: result => { if (result.confirm) void identity.logout("all", selection); } });
  },
});
