import {
  cp,
  mkdtemp,
  readFile,
  rm,
  symlink,
  writeFile,
} from "node:fs/promises";
import { spawnSync } from "node:child_process";
import { join, relative } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { expect, it } from "vitest";

it("changes the release identity when TypeScript compiler options change", async () => {
  const source = fileURLToPath(new URL("../", import.meta.url));
  const temporary = await mkdtemp(join(tmpdir(), "halo-build-"));
  const root = join(temporary, "halo");
  try {
    await cp(source, root, {
      recursive: true,
      filter: (path) =>
        !["node_modules", "build", ".react-router", "test-results"].includes(
          relative(source, path).split("/")[0],
        ),
    });
    for (const file of ["tokens.css", "logo.png", "halo-botanical.svg"]) {
      await cp(
        join(source, "../design/assets", file),
        join(temporary, "design/assets", file),
      );
    }
    await cp(join(source, "../client/protocol"), join(temporary, "client/protocol"), { recursive: true });
    await symlink(
      join(source, "node_modules"),
      join(root, "node_modules"),
      "dir",
    );
    async function build() {
      const result = spawnSync(process.execPath, ["tools/build.ts"], {
        cwd: root,
        encoding: "utf8",
        timeout: 30_000,
      });
      expect(result.error, result.stderr).toBeUndefined();
      expect(result.status, result.stdout + result.stderr).toBe(0);
      return JSON.parse(
        await readFile(join(root, "build/release.json"), "utf8"),
      );
    }
    const first = await build();
    const configuration = JSON.parse(
      await readFile(join(root, "tsconfig.json"), "utf8"),
    );
    configuration.compilerOptions.useDefineForClassFields = false;
    await writeFile(join(root, "tsconfig.json"), JSON.stringify(configuration));
    const second = await build();
    expect(second.buildId).not.toBe(first.buildId);
    expect(second.resources).not.toEqual(first.resources);
    const shared = join(temporary, "client/protocol/response.ts");
    await writeFile(shared, `${await readFile(shared, "utf8")}\n// Shared input changed.\n`);
    const third = await build();
    expect(third.buildId).not.toBe(second.buildId);
  } finally {
    await rm(temporary, { recursive: true, force: true });
  }
}, 90_000);
