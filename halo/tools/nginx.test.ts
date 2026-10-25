import { spawn } from "node:child_process";
import { once } from "node:events";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, it } from "vitest";

it("proxies avatar bodies up to 2 MiB while other API routes retain 8 KiB", async () => {
  const directory = await mkdtemp(join(tmpdir(), "halo-nginx-"));
  const received: { url: string; bytes: Buffer }[] = [];
  const upstream = createServer(async (request, response) => {
    const chunks: Buffer[] = [];
    for await (const chunk of request) chunks.push(Buffer.from(chunk));
    received.push({ url: request.url!, bytes: Buffer.concat(chunks) });
    response.writeHead(200, { "Cache-Control": "private, max-age=60" });
    response.end("upstream");
  });
  upstream.listen(0, "127.0.0.1");
  await once(upstream, "listening");
  const address = upstream.address();
  if (!address || typeof address === "string")
    throw Error("Missing upstream port");
  const reservation = createServer();
  reservation.listen(0, "127.0.0.1");
  await once(reservation, "listening");
  const target = reservation.address();
  if (!target || typeof target === "string") throw Error("Missing proxy port");
  await new Promise<void>((resolve) => reservation.close(() => resolve()));
  const production = await readFile(
    new URL("../deploy/halo.nginx.conf", import.meta.url),
    "utf8",
  );
  const routes = production
    .replace("include /srv/halo/current/build/nginx-assets.conf;", "")
    .replaceAll("http://127.0.0.1:8080", `http://127.0.0.1:${address.port}`)
    .replace("http://127.0.0.1:3080", `http://127.0.0.1:${address.port}`);
  const config = join(directory, "nginx.conf");
  await writeFile(
    config,
    `daemon off; master_process off;
pid ${directory}/nginx.pid;
error_log stderr crit;
events { worker_connections 32; }
http {
    client_body_temp_path ${directory}/body;
    proxy_temp_path ${directory}/proxy;
    fastcgi_temp_path ${directory}/fastcgi;
    scgi_temp_path ${directory}/scgi;
    uwsgi_temp_path ${directory}/uwsgi;
    server { listen 127.0.0.1:${target.port}; ${routes} }
}
`,
  );
  const child = spawn(
    process.env.HALO_NGINX ?? "/usr/sbin/nginx",
    ["-c", config, "-p", directory],
    {
      stdio: ["ignore", "ignore", "pipe"],
    },
  );
  let diagnostics = "";
  child.stderr!.on("data", (chunk) => (diagnostics += String(chunk)));
  const exit = new Promise<void>((resolve) => {
    child.once("exit", () => resolve());
    child.once("error", (error) => {
      diagnostics += error.message;
      resolve();
    });
  });
  const origin = `http://127.0.0.1:${target.port}`;
  try {
    await expect
      .poll(
        async () => {
          try {
            const response = await fetch(origin, {
              signal: AbortSignal.timeout(500),
            });
            await response.text();
            return response.status;
          } catch {
            return diagnostics || "starting";
          }
        },
        { timeout: 5_000 },
      )
      .toBe(200);
    received.length = 0;
    const avatar = "/api/v1/accounts/current/avatar";
    // A real static square WebP exercises the original proxy failure boundary.
    const image = await readFile(
      new URL("../../assets/avatars/0-cat.webp", import.meta.url),
    );
    expect(image.length).toBeGreaterThan(8 * 1024);
    const send = (path: string, bytes: Buffer) =>
      fetch(origin + path, {
        method: "PUT",
        headers: { "Content-Type": "image/webp" },
        body: new Uint8Array(bytes),
        signal: AbortSignal.timeout(5_000),
      });
    for (const bytes of [image, Buffer.alloc(2 * 1024 * 1024)]) {
      const response = await send(avatar + "?fixture=upload", bytes);
      expect(response.status).toBe(200);
      expect(await response.text()).toBe("upstream");
      expect(response.headers.get("cache-control")).toBe("private, max-age=60");
      expect(response.headers.get("x-content-type-options")).toBe("nosniff");
      expect(response.headers.get("referrer-policy")).toBe("no-referrer");
      expect(received.at(-1)).toEqual({
        url: avatar + "?fixture=upload",
        bytes,
      });
    }
    for (const [path, size] of [
      [avatar, 2 * 1024 * 1024 + 1],
      ["/api/v1/accounts/current", 8 * 1024 + 1],
      [avatar + "/completions", 8 * 1024 + 1],
    ] as const) {
      const before = received.length;
      const response = await send(path, Buffer.alloc(size));
      expect(response.status, path).toBe(413);
      await response.text();
      expect(received.length, path).toBe(before);
    }
    const ordinary = await send(
      "/api/v1/accounts/current",
      Buffer.alloc(8 * 1024),
    );
    expect(ordinary.status).toBe(200);
    await ordinary.text();
  } finally {
    if (child.exitCode === null && child.signalCode === null)
      child.kill("SIGQUIT");
    await exit;
    upstream.closeAllConnections();
    await new Promise<void>((resolve) => upstream.close(() => resolve()));
    await rm(directory, { recursive: true, force: true });
  }
}, 20_000);
