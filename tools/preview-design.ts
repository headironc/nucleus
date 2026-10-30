import { createServer } from "node:http";
import { fileURLToPath } from "node:url";
import handler from "serve-handler";

export function createDesignPreview(
  publicRoot = fileURLToPath(new URL("../design/", import.meta.url)),
) {
  return createServer((request, response) => {
    void handler(request, response, {
      public: publicRoot,
      cleanUrls: false,
      // The pinned handler's MIME database predates the AVIF mapping.
      headers: [
        {
          source: "**/*.avif",
          headers: [{ key: "Content-Type", value: "image/avif" }],
        },
      ],
    }).catch((error: unknown) => {
      console.error("Design preview request failed:", error);
      if (response.headersSent) {
        response.destroy();
        return;
      }
      response.writeHead(500, { "Content-Type": "text/plain; charset=utf-8" });
      response.end("Internal Server Error\n");
    });
  });
}

if (import.meta.main) {
  const server = createDesignPreview();
  server.on("error", (error) => {
    const code = "code" in error ? ` (${error.code})` : "";
    console.error(`Design preview failed${code}: ${error.message}`);
    process.exitCode = 1;
    if (server.listening) server.close();
  });
  for (const signal of ["SIGINT", "SIGTERM"] as const) {
    process.once(signal, () => {
      // Bun clears its native handle in close(), so end active connections first.
      server.closeAllConnections();
      server.close();
    });
  }
  server.listen(4174, "127.0.0.1", () => {
    console.log("Design preview: http://127.0.0.1:4174/halo-home.html");
  });
}
