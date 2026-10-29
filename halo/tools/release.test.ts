import { mkdtemp, mkdir, writeFile, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import { describe, it, expect } from "vitest";
import { verifyRelease, digest } from "./release.mjs";

describe("release integrity", () => {
  it.each(["24.0.0", "25.0.0", "27.0.0"])(
    "rejects unsupported Node %s before release startup",
    (version) => {
      const result = spawnSync(
        process.execPath,
        [
          "--input-type=module",
          "--eval",
          `Object.defineProperty(process.versions, "node", { value: ${JSON.stringify(version)} }); await import(${JSON.stringify(new URL("../server.mjs", import.meta.url).href)});`,
        ],
        {
          encoding: "utf8",
          timeout: 10_000,
          env: { ...process.env, NODE_ENV: "production" },
        },
      );
      expect(result.error).toBeUndefined();
      expect(result.status).not.toBe(0);
      expect(result.stderr).toContain("Halo requires Node 26");
      expect(result.stdout).not.toContain("halo_ready");
    },
  );
  it.each(["asset", "server", "runtime", "manifest", "extra"])(
    "rejects %s drift before serving",
    async (fault) => {
      const root = await mkdtemp(join(tmpdir(), "halo-release-"));
      try {
        await mkdir(join(root, "build/client"), { recursive: true });
        await mkdir(join(root, "build/server"), { recursive: true });
        await mkdir(join(root, "tools"));
        const resources = [
          {
            file: "fixed.js",
            path: "/assets/00000000000000000000/fixed.js",
            sha256: digest("fixed bytes"),
            mediaType: "text/javascript",
          },
        ];
        const server = [{ file: "index.js", sha256: digest("server bytes") }];
        const runtime = [
          "server.mjs",
          "tools/release.mjs",
          "package.json",
          "bun.lock",
        ].map((file) => ({ file, sha256: digest("runtime bytes") }));
        for (const entry of runtime)
          await writeFile(join(root, entry.file), "runtime bytes");
        await writeFile(join(root, "build/client/fixed.js"), "fixed bytes");
        await writeFile(join(root, "build/server/index.js"), "server bytes");
        const manifest = {
          version: 1,
          buildId: "00000000000000000000",
          resources,
          server,
          runtime,
        };
        await writeFile(
          join(root, "build/release.json"),
          JSON.stringify(manifest),
        );
        await expect(verifyRelease(root)).resolves.toEqual(manifest);
        if (fault === "asset")
          await writeFile(
            join(root, "build/client/fixed.js"),
            "different bytes",
          );
        if (fault === "server")
          await writeFile(
            join(root, "build/server/index.js"),
            "different bytes",
          );
        if (fault === "runtime")
          await writeFile(join(root, "server.mjs"), "different bytes");
        if (fault === "manifest") {
          manifest.runtime = [];
          await writeFile(
            join(root, "build/release.json"),
            JSON.stringify(manifest),
          );
        }
        if (fault === "extra")
          await writeFile(
            join(root, "build/client/unlisted.js"),
            "unlisted bytes",
          );
        await expect(verifyRelease(root)).rejects.toThrow();
      } finally {
        await rm(root, { recursive: true, force: true });
      }
    },
  );
  it("ships precompiled validators without a runtime schema compiler", async () => {
    const packageJson = JSON.parse(
      await readFile(new URL("../package.json", import.meta.url), "utf8"),
    );
    expect(packageJson.dependencies).not.toHaveProperty("ajv");
    // Standalone validators use precompiled functions; their small runtime helpers are bundled.
    const validators = await readFile(
      new URL("../app/protocol/generated/validators.cjs", import.meta.url),
      "utf8",
    );
    expect(validators).not.toMatch(/\beval\(|new Function\(/);
  });
});
