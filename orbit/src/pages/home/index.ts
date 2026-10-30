Page({
  onShow() { getApp().identity?.hide(); getApp().identity?.resume(); },
  openAccount() { wx.navigateTo({ url: "/pages/account/index" }); },
});
