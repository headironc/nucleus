import { spawn } from "node:child_process";
import { once } from "node:events";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer, type IncomingHttpHeaders } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, it } from "vitest";

it("proxies exact avatar and badge write routes within their limits while other API routes retain 8 KiB", async () => {
  const directory = await mkdtemp(join(tmpdir(), "halo-nginx-"));
  const photo = "/api/v1/workforce/application-photos";
  const application = "/api/v1/workforce/applications";
  const received: {
    url: string;
    bytes: Buffer;
    method: string;
    headers: IncomingHttpHeaders;
  }[] = [];
  const upstream = createServer(async (request, response) => {
    const chunks: Buffer[] = [];
    for await (const chunk of request) chunks.push(Buffer.from(chunk));
    received.push({
      url: request.url!,
      bytes: Buffer.concat(chunks),
      method: request.method!,
      headers: request.headers,
    });
    const badge = [photo, application].includes(request.url!.split("?")[0]);
    const refused = request.url === photo + "?fixture=refusal";
    response.writeHead(refused ? 503 : badge ? 201 : 200, {
      "Cache-Control": badge ? "no-store" : "private, max-age=60",
      "Content-Type": refused ? "application/json" : "text/plain",
    });
    response.end(refused ? '{"code":"dependency_unavailable"}' : "upstream");
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
    const send = (
      path: string,
      bytes: Buffer,
      method = "PUT",
      media = "image/webp",
    ) =>
      fetch(origin + path, {
        method,
        headers: {
          "Content-Type": media,
          "Session-Id": "11111111-1111-4111-8111-111111111111",
          "Operation-Id": "22222222-2222-4222-8222-222222222222",
          Cookie: "fixture=badge",
          "X-Forwarded-For": "192.0.2.99",
          "X-Real-IP": "192.0.2.99",
        },
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
      const actual = received.at(-1)!;
      expect(actual.url).toBe(avatar + "?fixture=upload");
      expect(actual.method).toBe("PUT");
      expect(actual.bytes.length).toBe(bytes.length);
      expect(actual.bytes.equals(bytes)).toBe(true);
    }

    for (const bytes of [image, Buffer.alloc(8 * 1024 * 1024)]) {
      const response = await send(photo + "?fixture=upload", bytes, "POST");
      expect(response.status).toBe(201);
      expect(await response.text()).toBe("upstream");
      expect(response.headers.get("cache-control")).toBe("no-store");
      expect(response.headers.get("set-cookie")).toBeNull();
      expect(response.headers.get("x-content-type-options")).toBe("nosniff");
      expect(response.headers.get("referrer-policy")).toBe("no-referrer");
      const actual = received.at(-1)!;
      expect(actual.url).toBe(photo + "?fixture=upload");
      expect(actual.method).toBe("POST");
      expect(actual.bytes.length).toBe(bytes.length);
      expect(actual.bytes.equals(bytes)).toBe(true);
      expect(actual.headers).toMatchObject({
        "content-type": "image/webp",
        "session-id": "11111111-1111-4111-8111-111111111111",
        "operation-id": "22222222-2222-4222-8222-222222222222",
        cookie: "fixture=badge",
        "x-real-ip": "127.0.0.1",
      });
      expect(actual.headers["x-forwarded-for"]).toBeUndefined();
    }
    const refused = await send(photo + "?fixture=refusal", image, "POST");
    expect(refused.status).toBe(503);
    expect(refused.headers.get("content-type")).toBe("application/json");
    expect(refused.headers.get("cache-control")).toBe("no-store");
    expect(await refused.json()).toEqual({ code: "dependency_unavailable" });

    for (const size of [8 * 1024 + 1, 128 * 1024]) {
      const bytes = Buffer.from(
        JSON.stringify({ fixture: "a".repeat(size - 14) }),
      );
      expect(bytes.length).toBe(size);
      const response = await send(
        application + "?fixture=submit",
        bytes,
        "POST",
        "application/json",
      );
      expect(response.status).toBe(201);
      expect(await response.text()).toBe("upstream");
      expect(response.headers.get("cache-control")).toBe("no-store");
      const actual = received.at(-1)!;
      expect(actual.url).toBe(application + "?fixture=submit");
      expect(actual.method).toBe("POST");
      expect(actual.bytes.length).toBe(bytes.length);
      expect(actual.bytes.equals(bytes)).toBe(true);
      expect(actual.headers["content-type"]).toBe("application/json");
    }

    for (const [path, size, method] of [
      [avatar, 2 * 1024 * 1024 + 1, "PUT"],
      [photo, 8 * 1024 * 1024 + 1, "POST"],
      [application, 128 * 1024 + 1, "POST"],
      ["/api/v1/accounts/current", 8 * 1024 + 1, "PUT"],
      [avatar + "/completions", 8 * 1024 + 1, "PUT"],
      [photo + "/", 8 * 1024 + 1, "POST"],
      [photo + "/extra", 8 * 1024 + 1, "POST"],
      [photo + "-other", 8 * 1024 + 1, "POST"],
      [application + "/preview", 8 * 1024 + 1, "POST"],
      [application + "/", 8 * 1024 + 1, "POST"],
      [application + "/extra", 8 * 1024 + 1, "POST"],
      [application + "-other", 8 * 1024 + 1, "POST"],
    ] as const) {
      const before = received.length;
      const response = await send(path, Buffer.alloc(size), method);
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
