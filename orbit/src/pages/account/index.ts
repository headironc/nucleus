import type { View } from "../../controller";

Page({
  data: { phase: "checking", busy: false, canLogin: false, canRecover: false, canCheck: false,
    canReset: false, logoutPending: false, message: "正在准备个人账户…", name: "", suffix: "", avatar: "", bound: false,
    inputName: "", inputIdNumber: "", avatarFailed: false },
  onShow() {
    const identity = getApp().identity;
    if (!identity) {
      this.setData({ phase: "unavailable", message: "暂时无法打开个人账户，请联系维护人员检查服务配置。" });
      return;
    }
    identity.subscribe((view: View) => {
      this.setData({ ...view, ...(view.phase !== "registration" ? { inputName: "", inputIdNumber: "" } : {}), avatarFailed: false });
    });
    void identity.show();
  },
  onHide() {
    getApp().identity?.hide();
    getApp().identity?.subscribe(() => {});
    this.setData({ inputName: "", inputIdNumber: "", name: "", suffix: "", avatar: "" });
  },
  onUnload() { this.onHide(); },
  authorize(event: { detail: { code?: string } }) { void getApp().identity?.authorize(event.detail.code); },
  inputName(event: { detail: { value: string } }) { this.setData({ inputName: event.detail.value }); },
  inputIdNumber(event: { detail: { value: string } }) { this.setData({ inputIdNumber: event.detail.value }); },
  submit() { void getApp().identity?.register(this.data.inputName, this.data.inputIdNumber); },
  check() { void getApp().identity?.check(); },
  recover() { void getApp().identity?.recover(); },
  restartLogin() {
    const identity = getApp().identity;
    const generation = identity?.resetConfirmation();
    if (generation === undefined) return;
    wx.showModal({ title: "重新验证手机号", content: "将移除设备上无法使用的登录信息。此前办理结果仍无法确认。", confirmText: "重新验证",
      success: result => { if (result.confirm) void identity!.restartLogin(generation); } });
  },
  switchAccount() {
    const identity = getApp().identity;
    const selection = identity?.selection();
    if (!identity || !selection) return;
    wx.showModal({ title: "切换账户", content: "将隐藏当前资料。另一账户需要重新授权手机号登录。", confirmText: "切换账户",
      success: result => { if (result.confirm) void identity.switchAccount(selection); } });
  },
  sessions() { wx.navigateTo({ url: "/pages/sessions/index" }); },
  logoutCurrent() {
    const identity = getApp().identity;
    const selection = identity?.selection();
    if (!identity || !selection) return;
    wx.showModal({ title: "退出本次登录？", content: "退出后，再次进入个人账户需要重新登录。其他登录不受影响。",
      confirmText: "退出登录", success: result => { if (result.confirm) void identity.logout("current", selection); } });
  },
  avatarError() { this.setData({ avatarFailed: true }); },
});
