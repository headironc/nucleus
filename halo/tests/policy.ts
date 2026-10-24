import { expect } from "@playwright/test";

const NONCE = /^'nonce-[A-Za-z0-9+/=]+'$/;

/** Assert the production HTML policy that every rendered document must carry. */
export function assertDocumentPolicy(
  headers: Record<string, string | undefined>,
  turnstile: boolean,
) {
  const configured = process.env.HALO_AVATAR_ORIGIN;
  if (!configured)
    throw new Error(
      "HALO_AVATAR_ORIGIN is required to check production policy",
    );
  const avatar = new URL(configured).origin;
  expect(
    headers["cache-control"]?.split(",").map((value) => value.trim()),
  ).toContain("no-store");
  expect(headers["content-type"]).toBe("text/html; charset=utf-8");
  // The deployment entry repeats these upstream headers, so both lines arrive.
  expect(
    headers["x-content-type-options"]?.split(",").map((value) => value.trim()),
  ).toContain("nosniff");
  expect(
    headers["referrer-policy"]?.split(",").map((value) => value.trim()),
  ).toContain("no-referrer");
  const policy = headers["content-security-policy"];
  expect(policy).toBeTruthy();
  const directives = new Map(
    policy!
      .split("; ")
      .map((directive) => [
        directive.split(" ")[0],
        directive.split(" ").slice(1),
      ]),
  );
  expect([...directives.keys()]).toEqual([
    "default-src",
    "script-src",
    "style-src-elem",
    "style-src-attr",
    "img-src",
    "connect-src",
    "frame-src",
    "object-src",
    "frame-ancestors",
    "form-action",
    "base-uri",
  ]);
  expect(directives.get("default-src")).toEqual(["'self'"]);
  const script = directives.get("script-src") ?? [];
  expect(script[0]).toBe("'self'");
  expect(script[1]).toMatch(NONCE);
  expect(script.slice(2)).toEqual(
    turnstile ? ["https://challenges.cloudflare.com"] : [],
  );
  expect(directives.get("style-src-elem")).toEqual(["'self'"]);
  expect(directives.get("style-src-attr")).toEqual(["'unsafe-inline'"]);
  // The only external source is the deployer-configured exact HTTPS origin.
  expect(directives.get("img-src")).toEqual(["'self'", avatar]);
  expect(directives.get("connect-src")).toEqual(["'self'"]);
  expect(directives.get("frame-src")).toEqual([
    turnstile ? "https://challenges.cloudflare.com" : "'none'",
  ]);
  expect(directives.get("object-src")).toEqual(["'none'"]);
  expect(directives.get("frame-ancestors")).toEqual(["'none'"]);
  expect(directives.get("form-action")).toEqual(["'self'"]);
  expect(directives.get("base-uri")).toEqual(["'self'"]);
  expect(policy).not.toMatch(/\*|blob:|data:/);
}
