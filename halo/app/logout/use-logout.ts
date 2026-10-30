import * as React from "react";
import * as dom from "react-dom";
import * as session from "../authentication/session";
import * as protocol from "../protocol/session";
import * as revocation from "../protocol/revocation";
import * as build from "../protocol/build";
import * as integrity from "../integrity";
import * as recovery from "./recovery";
import * as returns from "./return";
import * as codes from "../codes/material";
import type * as page from "./page";

export function useLogout(scope: page.Scope) {
  const [controller, setController] = React.useState<recovery.Logout>();
  React.useEffect(() => {
    const all = scope === `all`;
    const markerKey = all ? `haloAllLogout` : `haloLogout`;
    const sessions = new session.Sessions();
    const controller = new recovery.Logout(
      {
        storage: {
          getItem: (key) => sessionStorage.getItem(key),
          setItem: (key, value) => sessionStorage.setItem(key, value),
          removeItem: (key) => sessionStorage.removeItem(key),
        },
        sessions,
        current: protocol.current,
        revoke: all ? revocation.revokeAll : revocation.revoke,
        query: all ? revocation.readAllRevocations : revocation.readRevocations,
        build: build.currentBuild,
        now: Date.now,
        uuid: () => crypto.randomUUID(),
        marker: () => {
          const marker = history.state?.[markerKey];
          if (
            marker &&
            typeof marker === `object` &&
            typeof marker.preparing === `string` &&
            codes.UUID.test(marker.preparing)
          )
            return { preparing: marker.preparing };
          return marker ||
            (all
              ? returns.allLogoutReturn(location.search)
              : returns.logoutReturn(location.search))
            ? `submitted`
            : null;
        },
        mark: (value = `submitted`) =>
          history.replaceState(
            { ...history.state, [markerKey]: value },
            ``,
            value ? location.href : location.pathname,
          ),
      },
      scope,
    );
    setController(controller);
    const hide = () => dom.flushSync(() => controller.suspend());
    const reload = () => dom.flushSync(() => controller.reload());
    const check = () => {
      hide();
      if (!document.hidden) void controller.verify();
    };
    const unwatch = sessions.watch(check);
    window.addEventListener(`pagehide`, hide);
    window.addEventListener(`pageshow`, check);
    window.addEventListener(`focus`, check);
    document.addEventListener(`visibilitychange`, check);
    window.addEventListener(`halo:reload-required`, reload);
    if (integrity.reloadRequired()) controller.reload();
    else void controller.verify();
    return () => {
      controller.suspend();
      unwatch();
      window.removeEventListener(`pagehide`, hide);
      window.removeEventListener(`pageshow`, check);
      window.removeEventListener(`focus`, check);
      document.removeEventListener(`visibilitychange`, check);
      window.removeEventListener(`halo:reload-required`, reload);
      void sessions.close().catch(() => {});
    };
  }, [scope]);
  return controller;
}
