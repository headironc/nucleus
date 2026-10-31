import { test, expect } from "bun:test";
import { spawnSync } from "node:child_process";
import { readFileSync, existsSync } from "node:fs";
import { join } from "node:path";

const revision = process.env.NATIVE_SELFTEST_CLIENT_REVISION;
if (!revision || !/^[0-9a-f]{40}$/.test(revision))
  throw new Error(
    "NATIVE_SELFTEST_CLIENT_REVISION must bind an official Git commit",
  );

for (const mode of [
  "control",
  "disconnect",
  "startup",
  "sigterm",
  "sigint",
  "deadline",
  "cleanup_failure",
]) {
  test(`finite native fixture: ${mode}`, () => {
    const child = spawnSync(
      process.execPath,
      [
        "--no-env-file",
        "tests/http/native/runner.ts",
        "--owner",
        process.env.CODEX_THREAD_ID ?? "native-fixture-self-proof",
        "--client-revision",
        revision,
        "--mode",
        mode,
      ],
      {
        cwd: process.cwd(),
        env: process.env,
        encoding: "utf8",
        timeout: 60_000,
        maxBuffer: 1024 * 1024,
      },
    );
    const root = child.stdout.match(/^private native run: (.+)$/m)?.[1];
    expect(root, child.stderr).toBeDefined();
    expect(child.signal, child.stderr).toBeNull();
    expect(child.status, child.stderr).toBe(mode === "cleanup_failure" ? 1 : 0);
    const report = JSON.parse(readFileSync(join(root!, "runner.json"), "utf8"));
    expect(report.forcedTermination ?? false).toBe(false);
    expect(report.independentReadback.remaining).toBe(0);
    if (mode === "cleanup_failure") {
      expect(report.status).toBe("failed");
      expect(report.independentReadback.forced).toBe(true);
      const original = JSON.parse(
        readFileSync(join(root!, "evidence.json"), "utf8"),
      );
      expect(original.status).toBe("failed");
      expect(original.cleanup.postgresql).toBe(false);
      expect(existsSync(join(root!, "tmp"))).toBe(true);
    } else {
      expect(report.status).toBe("passed");
      expect(report.independentReadback.clean).toBe(true);
      expect(report.independentReadback.forced).toBe(false);
    }
  }, 60_000);
}
