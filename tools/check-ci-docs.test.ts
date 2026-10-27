import { afterEach, expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import {
  mkdirSync,
  mkdtempSync,
  rmSync,
  writeFileSync,
  existsSync,
  symlinkSync,
} from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { checkDocs } from "./check-ci-docs";
import { selectChecks } from "./check-inputs";

const directories: string[] = [];
afterEach(() => {
  for (const directory of directories.splice(0))
    rmSync(directory, { recursive: true, force: true });
});

function candidate() {
  const root = mkdtempSync(path.join(tmpdir(), "nucleus docs candidate "));
  directories.push(root);
  const write = (file: string, content: string) => {
    mkdirSync(path.dirname(path.join(root, file)), { recursive: true });
    writeFileSync(path.join(root, file), content);
  };
  for (const file of [
    "README.md",
    "AGENTS.md",
    "halo/README.md",
    "docs/guide.md",
  ])
    write(file, "# Guide\n\nA document.\n");
  write("design/page.html", "<!doctype html><title>Preview</title>\n");
  return { root, write };
}

test("Docs selection preserves backend, website and unknown-input requirements", () => {
  expect(selectChecks(["docs/guide.md", "design/page.html"])).toEqual({
    tier: "skip",
    halo: "skip",
    packages: [],
  });
  expect(selectChecks(["docs/guide.md", "src/lib.rs"])).toEqual({
    tier: "full",
    halo: "run",
    packages: ["nucleus"],
  });
  expect(selectChecks(["design/assets/tokens.css"])).toMatchObject({
    tier: "skip",
    halo: "run",
  });
  expect(selectChecks(["tools/check-inputs.ts"])).toEqual({
    tier: "full",
    halo: "run",
    packages: "all",
  });
  expect(selectChecks(null)).toEqual({
    tier: "full",
    halo: "run",
    packages: "all",
  });
  expect(selectChecks(["unrecognized.input"])).toEqual({
    tier: "full",
    halo: "run",
    packages: "all",
  });
  expect(() => selectChecks(["docs/../src/lib.rs"])).toThrow(
    "Invalid changed source path",
  );
});

test("fixed format and Markdown checks validate the entire candidate document scope", async () => {
  const fixture = candidate();
  await checkDocs("format", fixture.root);
  await checkDocs("markdown", fixture.root);
  fixture.write("docs/unrelated.md", "#    Unformatted\n");
  await expect(checkDocs("format", fixture.root)).rejects.toThrow(
    "Unformatted document: docs/unrelated.md",
  );
  fixture.write("docs/unrelated.md", "# First\n\n# Second\n");
  await expect(checkDocs("markdown", fixture.root)).rejects.toThrow("MD025");
});

test("candidate config and inline directives cannot disable frozen minimum checks", async () => {
  const fixture = candidate();
  const sentinel = path.join(fixture.root, "candidate-config-ran");
  fixture.write(
    ".prettierrc.json",
    JSON.stringify({ requirePragma: true, plugins: ["./plugin.mjs"] }),
  );
  fixture.write(
    "plugin.mjs",
    `import {writeFileSync} from 'node:fs';writeFileSync(${JSON.stringify(sentinel)},'ran');export default {};`,
  );
  fixture.write(
    "docs/.markdownlint-cli2.mjs",
    `import '../plugin.mjs';export default {config:{default:false}};`,
  );
  fixture.write("docs/.markdownlint.json", '{"default":false}');
  fixture.write("docs/guide.md", "#    Unformatted\n");
  await expect(checkDocs("format", fixture.root)).rejects.toThrow(
    "Unformatted document",
  );
  fixture.write(
    "docs/guide.md",
    "<!-- markdownlint-disable -->\n\n# First\n\n# Second\n",
  );
  await expect(checkDocs("markdown", fixture.root)).rejects.toThrow("MD025");
  expect(existsSync(sentinel)).toBe(false);
});

test("missing mandatory document inputs fail instead of reducing scope", async () => {
  const fixture = candidate();
  rmSync(path.join(fixture.root, "halo/README.md"));
  await expect(checkDocs("format", fixture.root)).rejects.toThrow(
    "Missing required Docs input: halo/README.md",
  );
});

test("Docs scans reject symlinked directories instead of silently omitting inputs", async () => {
  for (const file of [
    "docs/hidden.md",
    "docs/nested",
    "design/hidden.html",
    "design/assets",
    "docs",
  ]) {
    const fixture = candidate();
    const target = path.join(fixture.root, "outside");
    mkdirSync(target);
    writeFileSync(path.join(target, "invalid.md"), "#    Unformatted\n");
    rmSync(path.join(fixture.root, file), { recursive: true, force: true });
    symlinkSync(target, path.join(fixture.root, file));
    for (const mode of ["format", "markdown", "links"])
      await expect(checkDocs(mode, fixture.root)).rejects.toThrow(
        "Docs input must be a regular file inside the candidate",
      );
  }
});

test("the real offline link checker detects a broken candidate link", async () => {
  const fixture = candidate();
  await checkDocs("links", fixture.root);
  fixture.write("lychee.toml", 'exclude = [".*"]\n');
  fixture.write(".ignore", "**\n");
  fixture.write("docs/guide.md", "# Guide\n\n[Missing](./does-not-exist.md)\n");
  await expect(checkDocs("links", fixture.root)).rejects.toThrow(
    "Docs links failed",
  );
});

test("frozen preview regression reads the candidate design instead of the policy design", () => {
  const fixture = candidate();
  const run = () =>
    spawnSync(
      process.execPath,
      [
        "test",
        "--test-name-pattern",
        "^every current design file",
        path.join(import.meta.dirname, "preview-design.test.ts"),
      ],
      {
        cwd: path.resolve(import.meta.dirname, ".."),
        env: { ...process.env, NUCLEUS_DESIGN_CANDIDATE: fixture.root },
        encoding: "utf8",
        timeout: 20_000,
      },
    );
  const passed = run();
  expect(passed.status, passed.stderr).toBe(0);
  rmSync(path.join(fixture.root, "design/page.html"));
  const failed = run();
  expect(failed.status).not.toBe(0);
  expect(failed.stderr).toContain("toBeGreaterThan");
}, 30_000);
