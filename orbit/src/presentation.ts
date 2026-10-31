import type { View } from "./controller";

/** Copy for the two native self pages; operation availability remains with Controller. */
export function selfPresentation(view: View, page: "account" | "sessions") {
  let message = view.message;
  const recoveryLabel = view.logoutPending ? "查看退出结果" : view.recoveryKind === "login" ? "查看登录结果" :
    view.recoveryKind === "registration" ? "查看注册结果" : "";
  let heading = page === "sessions" ? "登录管理" : "个人账户";
  if (view.busy) {
    heading = view.phase === "checking" ? page === "account" ? "正在打开个人账户…" : "正在加载登录情况…" : view.message;
  } else if (view.phase === "complete") {
    heading = view.homeAction ? view.completedScope === "all" ? "已退出全部登录" : "已退出登录" :
      view.completedScope === "all" ? "此前的登录已全部退出" : "此前的登录已退出";
  }
  else if (view.phase === "pending") {
    const subject = view.logoutPending ? "退出" : view.recoveryKind === "login" ? "登录" : view.recoveryKind === "registration" ? "注册" : "";
    if (subject) heading = `暂时无法确认${subject}结果`;
    if (view.recoveryKind === "login") {
      if (message === "暂时无法确认是否已登录，请稍后查看登录结果。") message = "请稍后查看登录结果。";
      else if (message === "暂时无法确认此前的登录结果。") message = "";
    } else if (view.recoveryKind === "registration") {
      if (message === "暂时无法确认注册结果，请稍后查看结果，暂时不要再次提交。") message = "请稍后查看注册结果，暂时不要再次提交。";
      else if (message === "暂时无法确认此前的注册结果。") message = "";
    } else if (view.logoutPending && view.recoveryKind === "logout" &&
      (message === "暂时无法确认是否已退出，请查看退出结果。" || message === "暂时无法确认此前的退出结果，请稍后再查看。")) {
      message = "请查看退出结果。";
    }
  } else if (view.phase === "unavailable") heading = page === "account" ? "暂时无法打开个人账户" : "暂时无法打开登录管理";
  else if (page === "account") {
    if (view.phase === "registration") heading = "创建个人账户";
    else if (view.canLogin) heading = "登录个人账户";
  } else if (view.phase === "sessions" && !view.sessionsReady) heading = "登录情况暂时无法显示";
  return { heading, recoveryLabel, message };
}
