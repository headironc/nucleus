import type { View } from "./controller";

/** Copy for the two native self pages; operation availability remains with Controller. */
export function selfPresentation(view: View, page: "account" | "sessions") {
  const recoveryLabel = view.logoutPending ? "查看退出结果" : view.recoveryKind === "login" ? "查看登录结果" :
    view.recoveryKind === "registration" ? "查看注册结果" : "查看办理结果";
  let heading = page === "sessions" ? "登录管理" : "个人账户";
  if (view.busy) {
    heading = view.phase === "checking" ? page === "account" ? "正在打开个人账户…" : "正在加载登录情况…" : view.message;
  } else if (view.phase === "complete") {
    heading = view.homeAction ? "退出已完成" : view.completedScope === "all" ? "此前的登录已全部退出" : "此前的登录已退出";
  }
  else if (view.phase === "pending") {
    const subject = view.logoutPending ? "退出" : view.recoveryKind === "login" ? "登录" : view.recoveryKind === "registration" ? "注册" : "";
    heading = `${subject}结果待确认`;
  } else if (view.phase === "unavailable") heading = "暂时无法继续";
  else if (page === "account") {
    if (view.phase === "registration") heading = "创建个人账户";
    else if (view.canLogin) heading = "登录个人账户";
  } else if (view.phase === "sessions" && !view.sessionsReady) heading = "登录情况暂时无法显示";
  return { heading, recoveryLabel };
}
