import { afterEach, expect, spyOn, test } from "bun:test";
import { spawn, spawnSync } from "node:child_process";
import { once } from "node:events";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { request, type Server } from "node:http";
import { createConnection, type Socket } from "node:net";
import { tmpdir } from "node:os";
import { extname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { createDesignPreview } from "./preview-design";

const root = fileURLToPath(new URL("../", import.meta.url));
const design = join(root, "design");
const script = join(root, "tools/preview-design.ts");
const url = "http://127.0.0.1:4174/halo-home.html";
const servers = new Set<Server>();
const directories: string[] = [];
const children = new Set<ReturnType<typeof cli>>();
const sockets = new Set<Socket>();
const browserTypes: Record<string, RegExp> = {
  ".html": /^text\/html(?:;|$)/,
  ".js": /^(?:application|text)\/javascript(?:;|$)/,
  ".css": /^text\/css(?:;|$)/,
  ".svg": /^image\/svg\+xml(?:;|$)/,
  ".jpg": /^image\/jpeg(?:;|$)/,
  ".jpeg": /^image\/jpeg(?:;|$)/,
  ".png": /^image\/png(?:;|$)/,
  ".webp": /^image\/webp(?:;|$)/,
  ".avif": /^image\/avif(?:;|$)/,
  ".gif": /^image\/gif(?:;|$)/,
  ".txt": /^text\/plain(?:;|$)/,
  ".md": /^text\/markdown(?:;|$)/,
  ".ttf": /^font\/ttf(?:;|$)/,
};

async function within<T>(promise: Promise<T>, label: string): Promise<T> {
  let timer: ReturnType<typeof setTimeout>;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_, reject) => {
        timer = setTimeout(
          () => reject(new Error(`${label} timed out`)),
          5_000,
        );
      }),
    ]);
  } finally {
    clearTimeout(timer!);
  }
}

afterEach(async () => {
  for (const socket of sockets) socket.destroy();
  sockets.clear();
  for (const child of children) {
    if (child.process.exitCode === null && child.process.signalCode === null)
      child.process.kill("SIGKILL");
    await within(child.exited, "Child cleanup");
  }
  children.clear();
  await Promise.all(
    [...servers].map(
      (server) =>
        new Promise<void>((resolve) => {
          if (!server.listening) return resolve();
          server.closeAllConnections();
          server.close(() => resolve());
        }),
    ),
  );
  servers.clear();
  for (const directory of directories.splice(0))
    rmSync(directory, { recursive: true, force: true });
});

function temporary() {
  const directory = mkdtempSync(join(tmpdir(), "nucleus design preview "));
  directories.push(directory);
  return directory;
}

async function listen(server = createDesignPreview(), port = 0) {
  servers.add(server);
  await within(
    new Promise<void>((resolve, reject) => {
      const failed = (error: Error) => {
        server.off("listening", ready);
        reject(error);
      };
      const ready = () => {
        server.off("error", failed);
        resolve();
      };
      server.once("error", failed);
      server.once("listening", ready);
      server.listen(port, "127.0.0.1");
    }),
    "Preview listen",
  );
  const address = server.address();
  if (!address || typeof address === "string")
    throw new Error("Missing TCP address");
  expect(address.address).toBe("127.0.0.1");
  expect(address.family).toBe("IPv4");
  return { server, port: address.port };
}

function get(port: number, path: string, method = "GET") {
  return within(
    new Promise<{
      status: number;
      headers: Record<string, string | string[] | undefined>;
      body: Buffer;
    }>((resolve, reject) => {
      // The raw path is intentional: URL normalization would hide traversal inputs.
      const req = request(
        { host: "127.0.0.1", port, path, method, agent: false },
        (response) => {
          const chunks: Buffer[] = [];
          response.on("data", (chunk) => chunks.push(Buffer.from(chunk)));
          response.on("end", () =>
            resolve({
              status: response.statusCode ?? 0,
              headers: response.headers,
              body: Buffer.concat(chunks),
            }),
          );
          response.on("error", reject);
        },
      );
      req.on("error", reject);
      req.setTimeout(5_000, () =>
        req.destroy(new Error("Preview request timed out")),
      );
      req.end();
    }),
    "Preview request",
  );
}

function cli() {
  const process = spawn(globalThis.process.execPath, [script], {
    cwd: tmpdir(),
    env: { ...globalThis.process.env, PORT: "9876", HOST: "0.0.0.0" },
    stdio: ["ignore", "pipe", "pipe"],
  });
  let stdout = "";
  let stderr = "";
  process.stdout.on("data", (data) => {
    stdout += String(data);
  });
  process.stderr.on("data", (data) => {
    stderr += String(data);
  });
  const exited = new Promise<{
    code: number | null;
    signal: NodeJS.Signals | null;
  }>((resolve, reject) => {
    process.once("error", reject);
    process.once("close", (code, signal) => resolve({ code, signal }));
  });
  const result = {
    process,
    exited,
    stdout: () => stdout,
    stderr: () => stderr,
    ready: () =>
      within(
        (async () => {
          while (!stdout.includes(url)) {
            await Promise.race([
              once(process.stdout, "data"),
              exited.then(() => {
                throw new Error(`Preview exited before readiness: ${stderr}`);
              }),
            ]);
          }
        })(),
        "CLI readiness",
      ),
  };
  children.add(result);
  return result;
}

function snapshot() {
  return new Map(
    readdirSync(design, { recursive: true, encoding: "utf8" })
      .filter((path) => statSync(join(design, path)).isFile())
      .map((path) => [path, readFileSync(join(design, path))]),
  );
}

test("every current design file retains its bytes and browser MIME", async () => {
  const before = snapshot();
  const { port } = await listen();
  expect(before.size).toBeGreaterThan(0);
  for (const [path, bytes] of before) {
    const response = await get(port, `/${path}`);
    expect(response.status, path).toBe(200);
    expect(response.body, path).toEqual(bytes);
    expect(response.headers["content-type"], path).toMatch(
      browserTypes[extname(path)],
    );
    expect(response.headers.location).toBeUndefined();
    expect(response.headers["content-encoding"]).toBeUndefined();
  }
  expect(snapshot()).toEqual(before);
}, 15_000);

test.each([
  ".html",
  ".js",
  ".css",
  ".svg",
  ".jpg",
  ".jpeg",
  ".png",
  ".webp",
  ".avif",
  ".gif",
  ".txt",
  ".md",
  ".ttf",
])(
  "accepted design files retain bytes and browser MIME: %s",
  async (extension) => {
    const directory = temporary();
    mkdirSync(join(directory, "assets/nested"), { recursive: true });
    const bytes = Buffer.from(
      `Preview transport fixture ${extension}\n\0`,
      "utf8",
    );
    const paths = [`fixture${extension}`, `assets/nested/fixture${extension}`];
    for (const path of paths) writeFileSync(join(directory, path), bytes);
    const { port } = await listen(createDesignPreview(directory));
    for (const path of paths) {
      const response = await get(port, `/${path}`);
      expect(response.status, path).toBe(200);
      expect(response.body, path).toEqual(bytes);
      expect(response.headers["content-type"], path).toMatch(
        browserTypes[extension],
      );
      expect(response.headers.location).toBeUndefined();
      expect(response.headers["content-encoding"]).toBeUndefined();
    }
  },
);

test("explicit HTML, queries, directories and HEAD keep static semantics", async () => {
  const { port } = await listen();
  const home = await get(port, "/halo-home.html?preview=example&state=active");
  expect(home.status).toBe(200);
  expect(home.body).toEqual(readFileSync(join(design, "halo-home.html")));
  expect(home.headers.location).toBeUndefined();
  for (const path of ["/", "/assets", "/assets/"]) {
    const directory = await get(port, path);
    expect(directory.status).toBe(200);
    expect(directory.body.toString()).toContain(
      path === "/" ? "halo-home.html" : "halo-home.css",
    );
  }
  const head = await get(port, "/assets/halo-lingzhi.jpg", "HEAD");
  expect(head.status).toBe(200);
  expect(head.body.length).toBe(0);
  expect(Number(head.headers["content-length"])).toBe(
    statSync(join(design, "assets/halo-lingzhi.jpg")).size,
  );
});

test("missing and malformed paths do not become SPA responses or escape the root", async () => {
  const { port } = await listen();
  for (const path of ["/missing.html", "/halo-home", "/authentication/login"]) {
    const response = await get(port, path);
    expect(response.status, path).toBe(404);
    expect(response.body).not.toEqual(
      readFileSync(join(design, "halo-home.html")),
    );
  }
  for (const path of [
    "/../Cargo.toml",
    "/%2e%2e/Cargo.toml",
    "/%2e%2e%2fCargo.toml",
    "/%ZZ",
  ]) {
    const response = await get(port, path);
    expect(response.status, path).toBe(400);
  }
});

test("font transport uses a synthetic fixture without supplying licensed fonts", async () => {
  const directory = temporary();
  mkdirSync(join(directory, "assets/fonts.local"), { recursive: true });
  const bytes = Buffer.from([0, 1, 0, 0, 0, 0, 71, 72, 73]);
  writeFileSync(join(directory, "assets/fonts.local/badge-main.ttf"), bytes);
  const fixture = await listen(createDesignPreview(directory));
  const response = await get(
    fixture.port,
    "/assets/fonts.local/badge-main.ttf",
  );
  expect(response.status).toBe(200);
  expect(response.headers["content-type"]).toBe("font/ttf");
  expect(response.body).toEqual(bytes);
  const actual = await listen();
  const present = existsSync(join(design, "assets/fonts.local/badge-main.ttf"));
  expect(
    (await get(actual.port, "/assets/fonts.local/badge-main.ttf")).status,
  ).toBe(present ? 200 : 404);
});

test.each([false, true])(
  "unexpected handler rejection is contained (headers sent: %j)",
  async (sent) => {
    const logged = spyOn(console, "error").mockImplementation(() => {});
    try {
      const server = createDesignPreview();
      const fault = new Error("controlled preview handler rejection");
      server.prependListener("request", (request, response) => {
        if (sent) {
          response.writeHead(200);
          response.write("partial");
        }
        const headers = request.headers;
        let pending = true;
        Object.defineProperty(request, "headers", {
          get() {
            if (pending) {
              pending = false;
              throw fault;
            }
            return headers;
          },
        });
      });
      const { port } = await listen(server);
      if (sent)
        await expect(get(port, "/halo-home.html")).rejects.toBeDefined();
      else {
        const response = await get(port, "/halo-home.html");
        expect(response.status).toBe(500);
        expect(response.body.toString()).toBe("Internal Server Error\n");
        expect(response.body.toString()).not.toContain(fault.message);
      }
      expect(logged).toHaveBeenCalledTimes(1);
      expect(logged).toHaveBeenCalledWith(
        "Design preview request failed:",
        fault,
      );
    } finally {
      logged.mockRestore();
    }
  },
);

test("the real CLI keeps its fixed loopback port, rejects a contender and releases on signals", async () => {
  for (const signal of ["SIGTERM", "SIGINT"] as const) {
    const running = cli();
    await running.ready();
    const response = await get(4174, "/halo-home.html");
    expect(response.status).toBe(200);
    expect(response.body).toEqual(readFileSync(join(design, "halo-home.html")));
    const contender = cli();
    const rejected = await within(contender.exited, "Occupied CLI exit");
    expect(rejected).toEqual({ code: 1, signal: null });
    expect(contender.stderr()).toContain("EADDRINUSE");
    expect(contender.stdout()).not.toContain("Design preview:");
    expect(() => process.kill(contender.process.pid!, 0)).toThrow();
    expect((await get(4174, "/halo-home.html")).status).toBe(200);
    const active = createConnection(4174, "127.0.0.1");
    sockets.add(active);
    await within(once(active, "connect"), "Active preview connection");
    active.write(
      "GET /halo-home.html HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 1048576\r\nConnection: keep-alive\r\n\r\nx",
    );
    const [responseChunk] = await within(
      once(active, "data"),
      "Active preview response",
    );
    expect(String(responseChunk)).toContain("HTTP/1.1 200");
    expect(active.destroyed).toBe(false);
    const closed = new Promise<void>((resolve) =>
      active.once("close", () => resolve()),
    );
    active.on("error", () => {});
    const slow = createConnection(4174, "127.0.0.1");
    sockets.add(slow);
    await within(once(slow, "connect"), "Slow preview connection");
    const [largePath] = [...snapshot()].sort(
      (left, right) => right[1].length - left[1].length,
    )[0];
    // A paused pipeline retains response bytes in flight without timing sleeps.
    slow.write(
      `GET /${encodeURI(largePath)} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: keep-alive\r\n\r\n`.repeat(
        16,
      ),
    );
    const [streaming] = await within(
      once(slow, "data"),
      "Slow preview response",
    );
    expect(String(streaming)).toContain("HTTP/1.1 200");
    slow.pause();
    const slowClosed = new Promise<void>((resolve) =>
      slow.once("close", () => resolve()),
    );
    slow.on("error", () => {});
    expect(running.process.kill(signal)).toBe(true);
    expect(await within(running.exited, "Preview signal exit")).toEqual({
      code: 0,
      signal: null,
    });
    expect(() => process.kill(running.process.pid!, 0)).toThrow();
    await within(closed, "Active connection shutdown");
    slow.resume();
    await within(slowClosed, "Slow connection shutdown");
    const released = await listen(createDesignPreview(), 4174);
    await new Promise<void>((resolve) =>
      released.server.close(() => resolve()),
    );
  }
}, 20_000);

test("the package command checks Bun first and preserves either command's failure", () => {
  const manifest = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));
  const directory = temporary();
  mkdirSync(join(directory, "tools"));
  writeFileSync(
    join(directory, "package.json"),
    JSON.stringify({
      scripts: { "preview:design": manifest.scripts["preview:design"] },
    }),
  );
  writeFileSync(
    join(directory, "tools/check-bun.ts"),
    'console.log("pin checked"); process.exit(Number(process.env.PIN_STATUS));\n',
  );
  writeFileSync(
    join(directory, "tools/preview-design.ts"),
    'console.log("preview started"); process.exit(23);\n',
  );
  for (const status of [0, 17]) {
    const result = spawnSync(process.execPath, ["run", "preview:design"], {
      cwd: directory,
      env: { ...process.env, PIN_STATUS: String(status) },
      encoding: "utf8",
      timeout: 5_000,
    });
    expect(result.error).toBeUndefined();
    expect(result.status, result.stderr).toBe(status || 23);
    expect(result.stdout).toContain("pin checked");
    if (status) expect(result.stdout).not.toContain("preview started");
    else expect(result.stdout).toContain("preview started");
  }
});
