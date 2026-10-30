import { test, expect } from "@playwright/test";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { createServer, connect } from "node:net";
import { assertDocumentPolicy } from "./policy";

test("production documents carry the exact policy and avatar origin", async ({
  request,
}) => {
  for (const [path, turnstile] of [
    ["/", false],
    ["/authentication/codes", true],
    ["/authentication/login", false],
    ["/authentication/register", false],
    ["/account", false],
  ] as const) {
    const response = await request.get(path);
    expect(response.status(), path).toBe(200);
    assertDocumentPolicy(response.headers(), turnstile);
    if (path !== "/")
      expect(await response.text(), path).toContain('content="noindex"');
  }
});

test("the real browser refuses an unconfigured HTTPS image origin", async ({
  page,
}) => {
  let networkRequests = 0;
  // If policy regresses, intercept before DNS or an external network call.
  // A CSP refusal occurs before this transport route is reached.
  await page.route("https://unconfigured.invalid/**", async (route) => {
    networkRequests++;
    await route.fulfill({ status: 200, contentType: "image/webp", body: "" });
  });
  await page.goto("/account");
  const violation = await page.evaluate(
    () =>
      new Promise<{ directive: string; blocked: string; policy: string }>(
        (resolve, reject) => {
          const timer = setTimeout(
            () => reject(new Error("no securitypolicyviolation")),
            5_000,
          );
          document.addEventListener(
            "securitypolicyviolation",
            (event) => {
              clearTimeout(timer);
              const image = document.getElementById("unconfigured-avatar");
              image?.remove();
              resolve({
                directive: event.effectiveDirective,
                blocked: event.blockedURI,
                policy: event.violatedDirective,
              });
            },
            { once: true },
          );
          const image = new Image();
          image.id = "unconfigured-avatar";
          image.src = `https://unconfigured.invalid/api/v1/avatars/${"c".repeat(64)}.webp`;
          document.body.append(image);
        },
      ),
  );
  expect(violation.directive).toBe("img-src");
  expect(violation.policy.startsWith("img-src")).toBe(true);
  expect(violation.blocked).toContain("https://unconfigured.invalid");
  expect(networkRequests).toBe(0);
});

async function reserve() {
  const server = createServer();
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const address = server.address();
  if (!address || typeof address === "string")
    throw new Error("Missing startup fixture port");
  const port = address.port;
  await new Promise<void>((resolve, reject) =>
    server.close((error) => (error ? reject(error) : resolve())),
  );
  return port;
}

async function start(origin: string | undefined) {
  const port = await reserve();
  const env: NodeJS.ProcessEnv = {
    ...process.env,
    NODE_ENV: "production",
    HALO_PORT: String(port),
  };
  delete env.HALO_AVATAR_ORIGIN;
  if (origin !== undefined) env.HALO_AVATAR_ORIGIN = origin;
  const child = spawn(process.execPath, ["server.mjs"], {
    cwd: process.cwd(),
    env,
    stdio: ["ignore", "pipe", "pipe"],
  });
  let stdout = "";
  let stderr = "";
  child.stdout!.on("data", (chunk) => (stdout += String(chunk)));
  child.stderr!.on("data", (chunk) => (stderr += String(chunk)));
  let timer: ReturnType<typeof setTimeout> | undefined;
  const exit = once(child, "exit");
  let code: number | null;
  try {
    [code] = (await Promise.race([
      exit,
      new Promise<never>((_, reject) => {
        timer = setTimeout(
          () => reject(new Error("startup did not exit")),
          15_000,
        );
      }),
    ])) as [number | null, NodeJS.Signals | null];
  } finally {
    clearTimeout(timer);
    if (child.exitCode === null && child.signalCode === null) {
      child.kill("SIGKILL");
      await exit;
    }
  }
  const listening = await new Promise<boolean>((resolve) => {
    const socket = connect({ host: "127.0.0.1", port });
    socket.once("connect", () => {
      socket.destroy();
      resolve(true);
    });
    socket.once("error", () => resolve(false));
    socket.setTimeout(500, () => {
      socket.destroy();
      resolve(false);
    });
  });
  return { code, stdout, stderr, listening };
}

test("built Node fails before listening for a missing or invalid avatar origin", async () => {
  for (const origin of [
    undefined,
    "https://startup-secret.invalid/path?token=value",
    "https://startup-secret.invalid;upgrade-insecure-requests",
    "https://startup-secret.invalid,other",
  ]) {
    const result = await start(origin);
    expect(result.code).toBe(1);
    expect(result.stdout).not.toContain("halo_ready");
    expect(result.listening).toBe(false);
    expect(result.stderr).toContain(
      "HALO_AVATAR_ORIGIN must be an HTTPS origin without credentials, a path, query or fragment",
    );
    if (origin) expect(result.stderr).not.toContain("startup-secret.invalid");
  }
});
