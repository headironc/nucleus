import { spawn, spawnSync } from "node:child_process";
import { once } from "node:events";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { createServer } from "node:https";
import type { ServerResponse } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, afterEach, beforeAll, expect, it } from "vitest";
import { waitForNginx, waitForNodeReady } from "./browser-ready";

let temporary: string;
let certificate: Buffer;
let key: Buffer;
const children: ReturnType<typeof spawn>[] = [];
afterEach(() => {
  for (const child of children.splice(0))
    if (child.exitCode === null && child.signalCode === null) child.kill("SIGKILL");
});
beforeAll(async () => {
  temporary = await mkdtemp(join(tmpdir(), "halo-ready-"));
  const result = spawnSync(
    "openssl",
    [
      "req",
      "-x509",
      "-newkey",
      "rsa:2048",
      "-nodes",
      "-keyout",
      join(temporary, "key.pem"),
      "-out",
      join(temporary, "cert.pem"),
      "-days",
      "1",
      "-subj",
      "/CN=127.0.0.1",
      "-addext",
      "subjectAltName=IP:127.0.0.1",
    ],
    { stdio: "ignore", timeout: 10_000 },
  );
  expect(result.status).toBe(0);
  certificate = await readFile(join(temporary, "cert.pem"));
  key = await readFile(join(temporary, "key.pem"));
});
afterAll(async () => {
  if (temporary) await rm(temporary, { recursive: true, force: true });
});

async function fixture(handle: (response: ServerResponse) => void) {
  const server = createServer(
    { cert: certificate, key },
    (_request, response) => handle(response),
  );
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const address = server.address();
  if (!address || typeof address === "string")
    throw new Error("Missing test listener");
  const child = spawn(process.execPath, ["-e", "setInterval(() => {}, 1000)"], {
    stdio: "ignore",
  });
  const cancellation = new AbortController();
  return {
    child,
    cancellation,
    requested: once(server, "request"),
    wait: () =>
      waitForNginx(
        child,
        `https://127.0.0.1:${address.port}`,
        certificate,
        cancellation.signal,
      ),
    close: async () => {
      cancellation.abort();
      server.closeAllConnections();
      await new Promise<void>((resolve) => server.close(() => resolve()));
      if (child.exitCode === null && child.signalCode === null) {
        const exited = once(child, "exit");
        child.kill("SIGTERM");
        await exited;
      }
    },
  };
}

it("waits for the HTTPS response before accepting readiness", async () => {
  const proxy = await fixture(() => {});
  try {
    let ready = false;
    const waiting = proxy.wait().then(() => {
      ready = true;
    });
    const [, response] = await proxy.requested;
    expect(ready).toBe(false);
    response.end("{}");
    await waiting;
    expect(ready).toBe(true);
  } finally {
    await proxy.close();
  }
});

it("retries an unavailable upstream before accepting the TLS endpoint", async () => {
  let requests = 0;
  const proxy = await fixture((response) => {
    response.statusCode = ++requests === 1 ? 503 : 200;
    response.end();
  });
  try {
    await proxy.wait();
    expect(requests).toBe(2);
  } finally {
    await proxy.close();
  }
});

it.each(["exit", "cancel"])(
  "stops a pending readiness probe on %s",
  async (cause) => {
    const proxy = await fixture(() => {});
    try {
      const waiting = expect(proxy.wait()).rejects.toThrow(
        cause === "exit" ? "Nginx exited" : "Acceptance cancelled",
      );
      await proxy.requested;
      if (cause === "exit") proxy.child.kill("SIGTERM");
      else proxy.cancellation.abort(new Error("Acceptance cancelled"));
      await waiting;
    } finally {
      await proxy.close();
    }
  },
);

it("accepts Node readiness only after the marker", async () => {
  const child = spawn(
    process.execPath,
    [
      "-e",
      "setTimeout(() => { process.stdout.write('halo_ready'); setInterval(() => {}, 1000); }, 100)",
    ],
    { stdio: ["ignore", "pipe", "inherit"] },
  );
  children.push(child);
  await waitForNodeReady(child, "halo_ready", new AbortController().signal);
  expect(child.exitCode).toBeNull();
});

it("stops Node readiness when the process exits early", async () => {
  const child = spawn(process.execPath, ["-e", "process.exit(2)"], {
    stdio: ["ignore", "pipe", "inherit"],
  });
  children.push(child);
  await expect(
    waitForNodeReady(child, "halo_ready", new AbortController().signal),
  ).rejects.toThrow("Node exited");
});

it("stops Node readiness when acceptance is cancelled", async () => {
  const child = spawn(process.execPath, ["-e", "setInterval(() => {}, 1000)"], {
    stdio: ["ignore", "pipe", "inherit"],
  });
  children.push(child);
  const cancellation = new AbortController();
  const waiting = expect(
    waitForNodeReady(child, "halo_ready", cancellation.signal),
  ).rejects.toThrow("Acceptance cancelled");
  cancellation.abort(new Error("Acceptance cancelled"));
  await waiting;
});
