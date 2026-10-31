import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { Content } from "../routes/logout";
import type { Logout, State } from "./recovery";

vi.mock("../root", () => ({ Header: () => null, Footer: () => null }));

function page(state: State) {
  const controller = {
    subscribe: () => () => {},
    snapshot: () => state,
  } as unknown as Logout;
  return renderToStaticMarkup(createElement(Content, { controller }));
}

describe("logout page disclosure and recovery links", () => {
  it.each(["pending", "empty", "limited"] as const)(
    "shows safe result guidance for %s",
    (notice) => {
      const html = page({
        kind: "result",
        outcome: "pending",
        notice,
        retryAt: 0,
        canRead: true,
        canStart: false,
        newLogin: false,
      });
      expect(html).toContain("退出结果尚不确定");
      expect(html).toContain("查看退出结果");
      expect(html).toContain(
        notice === "limited"
          ? "暂时无法查看退出结果，请稍后再试。"
          : "请稍后查看退出结果，暂时不要重复退出。",
      );
      expect(html).not.toContain("重新确认退出");
      expect(html).toContain("<h1>退出结果</h1>");
    },
  );
  it("distinguishes a failed read from an unqueried result", () => {
    const html = page({
      kind: "result",
      outcome: "pending",
      notice: "unavailable",
      retryAt: 0,
      canRead: true,
      canStart: false,
      newLogin: false,
    });
    expect(html).toContain("暂时无法查看退出结果，请稍后再试。");
    expect(html).toContain("退出结果尚不确定");
    expect(html).not.toContain("已退出登录");
  });
  it("names the active submission and immediate completion as the current logout", () => {
    expect(page({ kind: "submitting", pending: true })).toContain(
      "正在退出登录…",
    );
    const html = page({
      kind: "result",
      outcome: "complete",
      notice: "pending",
      retryAt: 0,
      canRead: false,
      canStart: false,
      newLogin: false,
    });
    expect(html).toContain("<h1>退出结果</h1>");
    expect(html).toContain("已退出登录");
    expect(html).toContain('class="primary" href="/"');
    expect(html).not.toContain("此前的退出已完成");
  });
  it.each([
    "checking",
    "changed",
    "unavailable",
    "limited",
    "denied",
    "storage",
    "reload",
  ] as const)(
    "conceals prior logout during current-account state %s",
    (kind) => {
      const html = page({
        kind: "current",
        current: { kind, retryAt: 50_000 },
        pending: true,
      });
      expect(html).not.toMatch(/退出|return=logout-result|确认退出/);
      expect(html).toContain("账户管理");
    },
  );
  it("shows only the current account destination after an account change", () => {
    const html = page({ kind: "other" });
    expect(html).not.toMatch(/退出|return=logout-result/);
    expect(html).toContain('href="/account"');
  });
  it("does not promise recovery when the original material is missing", () => {
    const html = page({ kind: "missing" });
    expect(html).toContain('href="/authentication/codes"');
    expect(html).not.toMatch(/return=logout-result|>查看退出结果<|>退出登录</);
    expect(html).not.toContain("查询不会再次退出");
  });
  it("omits query guidance after a refused query", () => {
    const html = page({
      kind: "result",
      outcome: "pending",
      notice: "denied",
      retryAt: 0,
      canRead: false,
      canStart: false,
      newLogin: false,
    });
    expect(html).not.toContain("查询不会再次退出");
    expect(html).not.toContain("同一账户");
  });
  it("preserves the fixed return only for an unknown original with an expired session", () => {
    const html = page({
      kind: "current",
      current: {
        kind: "expired",
        selection: { id: "original-session", revision: "original-choice" },
      },
      pending: true,
    });
    expect(html).toContain('href="/authentication/codes?return=logout-result"');
    expect(html).toContain(
      "退出结果尚未确认。请使用原账户的手机号登录，继续查看退出结果。",
    );
  });
  it("separates the original completion from a newer login", () => {
    const html = page({
      kind: "result",
      outcome: "complete",
      notice: "pending",
      retryAt: 0,
      canRead: false,
      canStart: true,
      newLogin: true,
    });
    expect(html).toContain("此前的退出已完成");
    expect(html).toContain('href="/account"');
    expect(html).not.toContain("当前的新登录仍然有效");
    expect(html).not.toContain("查看退出结果");
  });
});
