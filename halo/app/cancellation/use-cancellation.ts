import { useEffect, useState } from "react";
import { flushSync } from "react-dom";
import { Sessions } from "../authentication/session";
import { UUID } from "../codes/material";
import { reloadRequired } from "../integrity";
import { currentBuild } from "../protocol/build";
import { cancel } from "../protocol/cancellation";
import { current } from "../protocol/session";
import { Cancellation, type Marker } from "./controller";

const MARKER = "haloCancellation";

function marker(): Marker {
  const value = history.state?.[MARKER];
  if (
    value &&
    typeof value === "object" &&
    typeof value.preparing === "string" &&
    UUID.test(value.preparing)
  )
    return { preparing: value.preparing };
  return value ? "submitted" : null;
}

function mark(value: Marker) {
  history.replaceState(
    { ...history.state, [MARKER]: value },
    "",
    value ? location.href : location.pathname,
  );
}

export function useCancellation() {
  const [controller, setController] = useState<Cancellation>();
  useEffect(() => {
    const sessions = new Sessions();
    const instance = new Cancellation({
      storage: {
        getItem: (key) => sessionStorage.getItem(key),
        setItem: (key, value) => sessionStorage.setItem(key, value),
        removeItem: (key) => sessionStorage.removeItem(key),
      },
      sessions,
      current,
      cancel,
      build: currentBuild,
      now: Date.now,
      uuid: () => crypto.randomUUID(),
      marker,
      mark,
    });
    setController(instance);
    const hide = () => flushSync(() => instance.suspend());
    const reload = () => flushSync(() => instance.reload());
    const check = () => {
      hide();
      if (!document.hidden) void instance.verify();
    };
    const unwatch = sessions.watch(check);
    window.addEventListener("pagehide", hide);
    window.addEventListener("pageshow", check);
    window.addEventListener("focus", check);
    document.addEventListener("visibilitychange", check);
    window.addEventListener("halo:reload-required", reload);
    if (reloadRequired()) instance.reload();
    else if (!document.hidden) void instance.verify();
    return () => {
      instance.close();
      unwatch();
      window.removeEventListener("pagehide", hide);
      window.removeEventListener("pageshow", check);
      window.removeEventListener("focus", check);
      document.removeEventListener("visibilitychange", check);
      window.removeEventListener("halo:reload-required", reload);
      void sessions.close().catch(() => {});
    };
  }, []);
  return controller;
}
