import * as vitest from "vitest";
import * as returns from "./return";

vitest.describe(`fixed all-logout login return`, () => {
  vitest.it.each([`/authentication/codes`, `/authentication/login`])(
    `preserves only the fixed all intent through %s`,
    (path) => {
      vitest
        .expect(
          returns.loginPath(
            path as `/authentication/codes` | `/authentication/login`,
            `?return=all-logout-result`,
          ),
        )
        .toBe(`${path}?return=all-logout-result`);
      vitest
        .expect(returns.allLogoutReturn(`?return=all-logout-result`))
        .toBe(true);
      vitest
        .expect(returns.logoutReturn(`?return=all-logout-result`))
        .toBe(false);
    },
  );
  vitest.it.each([
    `?return=logout-result`,
    `?return=https://evil.invalid`,
    `?return=/sessions/logout-all`,
    `?return=all-logout-result&return=all-logout-result`,
    `?operation_id=1&return=all-logout-result&return=x`,
    `?return=ALL-LOGOUT-RESULT`,
    ``,
  ])(`rejects an arbitrary or duplicate intent %s`, (search) => {
    vitest.expect(returns.allLogoutReturn(search)).toBe(false);
    vitest
      .expect(returns.returnIntent(search))
      .toBe(search === `?return=logout-result` ? `logout-result` : null);
    vitest
      .expect(returns.loginPath(`/authentication/codes`, search))
      .toBe(
        search === `?return=logout-result`
          ? `/authentication/codes?return=logout-result`
          : `/authentication/codes`,
      );
  });

vitest.describe(`fixed service termination login return`, () => {
  vitest.it.each([`/authentication/codes`, `/authentication/login`])(
    `preserves only the fixed termination intent through %s`,
    (path) => {
      vitest
        .expect(
          returns.loginPath(
            path as `/authentication/codes` | `/authentication/login`,
            `?return=service-termination`,
          ),
        )
        .toBe(`${path}?return=service-termination`);
      vitest
        .expect(returns.terminationReturn(`?return=service-termination`))
        .toBe(true);
      vitest
        .expect(returns.logoutReturn(`?return=service-termination`))
        .toBe(false);
      vitest
        .expect(returns.allLogoutReturn(`?return=service-termination`))
        .toBe(false);
    },
  );
  vitest.it.each([
    `?return=service-termination&return=service-termination`,
    `?return=https://evil.invalid`,
    `?return=/workforce/termination`,
    `?return=SERVICE-TERMINATION`,
    ``,
  ])(`rejects an arbitrary or duplicate termination intent %s`, (search) => {
    vitest.expect(returns.terminationReturn(search)).toBe(false);
    vitest
      .expect(returns.loginPath(`/authentication/codes`, search))
      .toBe(`/authentication/codes`);
  });
});
});
