import { createServer } from "node:http";
import { fileURLToPath } from "node:url";
import { createRequestListener } from "@react-router/node";
import { verifyRelease } from "./tools/release.mjs";

if (Number(process.versions.node.split(".")[0]) !== 26)
  throw new Error("Halo requires Node 26");
if (process.env.NODE_ENV !== "production")
  throw new Error("Start Halo with NODE_ENV=production");
const root = fileURLToPath(new URL(".", import.meta.url));
const manifest = await verifyRelease(root);
const port = Number(process.env.HALO_PORT ?? 3080);
if (!Number.isInteger(port) || port < 1 || port > 65535)
  throw new Error("Invalid HALO_PORT");
const build = await import("./build/server/index.js");
if (build.entry.module.buildId !== manifest.buildId)
  throw new Error("Node and assets belong to different builds");
const loadContext = build.entry.module.createLoadContext;
if (typeof loadContext !== "function")
  throw new Error("Halo build cannot validate HALO_AVATAR_ORIGIN");
const getLoadContext = loadContext(process.env.HALO_AVATAR_ORIGIN);
const listener = createRequestListener({
  build,
  mode: "production",
  getLoadContext,
});
const server = createServer((request, response) => {
  // Remove credentials before the framework, its Request conversion and any error handling.
  delete request.headers.cookie;
  delete request.headers.authorization;
  request.rawHeaders = request.rawHeaders.flatMap((value, index, values) =>
    index % 2 === 0 &&
    !["cookie", "authorization"].includes(value.toLowerCase())
      ? [value, values[index + 1]]
      : [],
  );
  response.setHeader("Cache-Control", "no-store");
  response.setHeader("X-Content-Type-Options", "nosniff");
  response.setHeader("Referrer-Policy", "no-referrer");
  if (!["GET", "HEAD"].includes(request.method)) {
    response.writeHead(405, {
      Allow: "GET, HEAD",
      "Content-Type": "text/plain; charset=utf-8",
    });
    response.end("Method not allowed");
    return;
  }
  if (request.url === "/build.json") {
    response.writeHead(200, { "Content-Type": "application/json" });
    response.end(
      request.method === "HEAD"
        ? undefined
        : JSON.stringify({ buildId: manifest.buildId }),
    );
    return;
  }
  if (request.url === "/robots.txt") {
    response.writeHead(200, { "Content-Type": "text/plain; charset=utf-8" });
    response.end(
      request.method === "HEAD" ? undefined : "User-agent: *\nDisallow: /\n",
    );
    return;
  }
  Promise.resolve(listener(request, response)).catch(() => {
    console.error("halo_response_failed");
    if (!response.headersSent)
      response.writeHead(500, { "Content-Type": "text/plain; charset=utf-8" });
    response.end("Page unavailable");
  });
});
server.headersTimeout = 10_000;
server.requestTimeout = 15_000;
server.listen(port, "127.0.0.1", () =>
  console.log(`halo_ready port=${port} build=${manifest.buildId}`),
);
for (const signal of ["SIGTERM", "SIGINT"]) {
  process.on(signal, () => {
    server.close(() => process.exit(0));
    setTimeout(() => {
      server.closeAllConnections();
      process.exit(1);
    }, 5000).unref();
  });
}
