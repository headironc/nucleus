import { startTransition } from "react";
import { hydrateRoot } from "react-dom/client";
import { HydratedRouter } from "react-router/dom";
import { requestReload } from "./integrity";

startTransition(() => {
  hydrateRoot(document, <HydratedRouter />, {
    onRecoverableError() {
      requestReload();
    },
  });
});
