import { randomBytes } from "node:crypto";
import { renderToReadableStream } from "react-dom/server";
import {
  ServerRouter,
  type EntryContext,
  type RouterContextProvider,
} from "react-router";
import { avatarOriginContext } from "./avatar/origin";

export const buildId = __BUILD_ID__;
export { createLoadContext } from "./avatar/origin";

export default async function handleRequest(
  request: Request,
  status: number,
  headers: Headers,
  context: EntryContext,
  loadContext?: RouterContextProvider,
) {
  const nonce = randomBytes(24).toString("base64");
  const codes = context.staticHandlerContext.matches.some(
    ({ route }) => route.id === "routes/codes",
  );
  // Current-avatar views and client-side navigation keep the document's
  // policy, so every document must allow the configured media origin.
  const avatar = loadContext?.get(avatarOriginContext);
  headers.set(
    "Content-Security-Policy",
    [
      "default-src 'self'",
      `script-src 'self' 'nonce-${nonce}'${codes ? " https://challenges.cloudflare.com" : ""}`,
      "style-src-elem 'self'",
      "style-src-attr 'unsafe-inline'",
      `img-src 'self'${avatar ? ` ${avatar}` : ""}`,
      "connect-src 'self'",
      `frame-src ${codes ? "https://challenges.cloudflare.com" : "'none'"}`,
      "object-src 'none'",
      "frame-ancestors 'none'",
      "form-action 'self'",
      "base-uri 'self'",
    ].join("; "),
  );
  headers.set("Content-Type", "text/html; charset=utf-8");
  headers.set("Cache-Control", "no-store");
  headers.set("X-Content-Type-Options", "nosniff");
  headers.set("Referrer-Policy", "no-referrer");
  let failed = false;
  const stream = await renderToReadableStream(
    <ServerRouter context={context} url={request.url} nonce={nonce} />,
    {
      nonce,
      signal: request.signal,
      onError() {
        failed = true;
        console.error("halo_render_failed");
      },
    },
  );
  await stream.allReady;
  return new Response(request.method === "HEAD" ? null : stream, {
    status: failed ? 500 : status,
    headers,
  });
}

export function handleError() {
  // Never send request URLs, headers or response bodies to diagnostics.
  console.error("halo_request_failed");
}
