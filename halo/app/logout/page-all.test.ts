import * as vitest from "vitest";
import * as React from "react";
import * as server from "react-dom/server";
import * as page from "./page";
import type * as recovery from "./recovery";
import type * as protocol from "../protocol/session";

const revision = `d4e3bb16-fec5-4d86-9d2b-d172b8ea5000`;
const time = `2026-10-23T00:00:00Z`;
const account: protocol.Account = {
  id: `e4e3bb16-fec5-4d86-9d2b-d172b8ea5000`,
  personId: `a4e3bb16-fec5-4d86-9d2b-d172b8ea5000`,
  revisionId: revision,
  name: `阿依古丽·阿不都热依木`,
  idNumber: { suffix: `0007` },
  binding: null,
  avatar: {
    revision,
    href: `https://fixture.invalid/avatar`,
    expiresAt: time,
    mediaType: `image/webp`,
    width: 512,
    height: 512,
  },
};
function render(state: recovery.State, detail: page.Detail | null = null) {
  const controller = {
    subscribe: () => () => {},
    snapshot: () => state,
    detail: () => detail,
    submit: vitest.vi.fn(),
    query: vitest.vi.fn(),
    start: vitest.vi.fn(),
    verify: vitest.vi.fn(),
    suspend: vitest.vi.fn(),
  };
  const html = server.renderToStaticMarkup(
    React.createElement(page.Content, {
      controller: controller as unknown as Parameters<
        typeof page.Content
      >[0][`controller`],
      scope: `all`,
    }),
  );
  return { html, controller };
}

vitest.describe(`the all-session logout page`, () => {
  vitest.it(
    `states the permanent scope and consequences before confirmation`,
    () => {
      const { html, controller } = render({
        kind: `ready`,
        account,
        selection: { id: revision, revision },
      });
      vitest.expect(html).toContain(`退出全部登录`);
      vitest.expect(html).toContain(account.name);
      vitest
        .expect(html)
        .toContain(`将退出网站和小程序的全部登录，包括本次登录。`);
      vitest.expect(html).toContain(`再次进入个人账户需要重新登录。`);
      vitest.expect(html).toContain(`账户资料和已提交的申请保留。`);
      vitest.expect(html).toContain(`>全部退出<`);
      vitest.expect(html).toContain(`暂不退出`);
      vitest.expect(controller.submit).not.toHaveBeenCalled();
    },
  );
  vitest.it(
    `does not claim success for a prior record after an unverified request`,
    () => {
      const detail = {
        count: null,
        binding: "mismatch" as const,
        record: { count: 2, revokedAt: time },
        revokedAt: null,
      };
      const { html } = render(
        {
          kind: `result`,
          outcome: `pending`,
          notice: `empty`,
          retryAt: 0,
          canRead: true,
          canStart: false,
          newLogin: false,
        },
        detail,
      );
      vitest.expect(html).toContain(`暂时无法确认此前的退出结果`);
      vitest.expect(html).toContain(`此前退出的登录`);
      vitest.expect(html).toContain(`查看退出结果`);
      vitest.expect(html).not.toContain(`已退出全部登录`);
      vitest.expect(html).toContain(`<h1>退出结果</h1>`);
    },
  );
  vitest.it(`reports the positive session count without a device list`, () => {
    const detail = {
      count: 3,
      binding: "matched" as const,
      record: null,
      revokedAt: time,
    };
    const { html } = render(
      {
        kind: `result`,
        outcome: `complete`,
        notice: `pending`,
        retryAt: 0,
        canRead: false,
        canStart: false,
        newLogin: false,
      },
      detail,
    );
    vitest.expect(html).toContain(`已退出全部登录`);
    vitest.expect(html).toContain(`class="primary" href="/"`);
    vitest
      .expect(html)
      .toContain(`class="quiet-link" href="/authentication/codes"`);
    vitest.expect(html).toContain(`已退出的登录`);
    vitest.expect(html).toContain(`3 条`);
    vitest.expect(html).not.toContain(`设备清单`);
    vitest.expect(html).toContain(`<h1>退出结果</h1>`);
  });
  vitest.it(`keeps a later login when reading the prior completion`, () => {
    const { html } = render(
      {
        kind: `result`,
        outcome: `complete`,
        notice: `pending`,
        retryAt: 0,
        canRead: false,
        canStart: true,
        newLogin: true,
      },
      { count: 2, binding: "matched", record: null, revokedAt: time },
    );
    vitest.expect(html).toContain(`此前的退出已完成`);
    vitest.expect(html).not.toContain(`当前的新登录继续保留`);
    vitest.expect(html).toContain(`打开个人账户`);
  });
  vitest.it(
    `does not promise recovery when original material is missing`,
    () => {
      const { html } = render({ kind: `missing` });
      vitest.expect(html).toContain(`暂时无法查看此前的退出结果`);
      vitest.expect(html).toContain(`href="/authentication/codes"`);
      vitest.expect(html).not.toContain(`return=all-logout-result`);
      vitest.expect(html).not.toContain(`>查看退出结果<`);
    },
  );
  vitest.it(
    `offers the public home after a query denial without a repair check`,
    () => {
      const { html } = render(
        {
          kind: `result`,
          outcome: `pending`,
          notice: `denied`,
          retryAt: 0,
          canRead: false,
          canStart: false,
          newLogin: false,
        },
        {
          count: null,
          binding: "mismatch",
          record: { count: 2, revokedAt: time },
          revokedAt: null,
        },
      );
      vitest.expect(html).toContain(`暂时无法查看此前的退出结果`);
      vitest.expect(html).toContain(`返回首页`);
      vitest.expect(html).not.toContain(`重新检查`);
      vitest.expect(html).not.toContain(`请稍后查看退出结果`);
      vitest.expect(html).not.toContain(`>查看退出结果<`);
      vitest.expect(html).not.toContain(`重新确认`);
      vitest.expect(html).not.toContain(`此前退出的登录`);
      vitest.expect(html).not.toContain(`本次尚未退出`);
    },
  );
  vitest.it(
    `propagates the fixed all-logout return for a pending original`,
    () => {
      const { html } = render({
        kind: `current`,
        current: {
          kind: `anonymous`,
        },
        pending: true,
      });
      vitest
        .expect(html)
        .toContain(`href="/authentication/codes?return=all-logout-result"`);
      vitest
        .expect(html)
        .toContain(
          `退出结果尚未确认。请使用原账户的手机号登录，继续查看退出结果。`,
        );
      vitest.expect(html).toContain(`<h1>账户管理</h1>`);
    },
  );
  vitest.it(
    `conceals the old operation and leave prompt after an account change`,
    () => {
      const { html } = render({ kind: `other` });
      vitest
        .expect(html)
        .not.toMatch(/退出全部登录|全部退出|return=all-logout-result/);
      vitest.expect(html).toContain(`href="/account"`);
      vitest.expect(html).toContain(`<h1>账户管理</h1>`);
    },
  );
});
