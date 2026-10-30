import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { bunVersionProblems, type Manifest, pinnedBun } from "./check-bun";

const tools = "bun 1.4.2\n";
const pinned = {
  packageManager: "bun@1.4.2",
  devDependencies: { "@types/bun": "1.4.2" },
};

test("a matching runtime, package manager and type package pass", () => {
  expect(bunVersionProblems(tools, pinned, "1.4.2")).toEqual([]);
});

test("a different running Bun is reported with both install routes", () => {
  const [problem] = bunVersionProblems(tools, pinned, "1.4.3");
  expect(problem).toContain("Bun 1.4.3 is running");
  expect(problem).toContain('"mise install"');
  expect(problem).toContain('bash -s "bun-v1.4.2"');
});

test("packageManager and type definitions must follow .tool-versions", () => {
  expect(
    bunVersionProblems(
      tools,
      {
        packageManager: "bun@1.4.3",
        devDependencies: { "@types/bun": "1.4.3" },
      },
      "1.4.2",
    ),
  ).toEqual([
    'package.json packageManager is "bun@1.4.3" but .tool-versions pins Bun 1.4.2; set it to "bun@1.4.2"',
    '@types/bun is "1.4.3" but .tool-versions pins Bun 1.4.2; update them together',
  ]);
  for (const packageManager of [undefined, "bun", "bun@^1.4.2", 1])
    expect(
      bunVersionProblems(
        tools,
        { ...pinned, packageManager } as Manifest,
        "1.4.2",
      ),
    ).toHaveLength(1);
});

test.each([
  "",
  "nodejs 24.0.0\n",
  "bun\n",
  "bun 1.4\n",
  "bun ^1.4.2\n",
  "bun latest\n",
  "bun 1.4.2 1.4.3\n",
  "bun 1.4.2\nbun 1.4.3\n",
  "  bun 1.4.2\n",
  "bun 1.4.2 \n",
  "bun 1.4.2\r\n",
  // setup-bun would read the first line starting with "bun".
  "bunyan 1.0.0\nbun 1.4.2\n",
])(".tool-versions must pin exactly one exact Bun version: %p", (content) => {
  expect(pinnedBun(content)).toBeUndefined();
  expect(bunVersionProblems(content, pinned, "1.4.2")).toEqual([
    '.tool-versions must contain exactly one "bun x.y.z" line',
  ]);
});

test("other tools do not affect the Bun pin", () => {
  expect(pinnedBun("nodejs 24.0.0\nbun 1.4.2\nrust 1.90.0")).toBe("1.4.2");
  expect(pinnedBun("bun 1.4.2")).toBe("1.4.2");
});

test("the repository pins one Bun version for mise, CI, runtime and types", () => {
  const read = (path: string) =>
    readFileSync(new URL(`../${path}`, import.meta.url), "utf8");
  const toolVersions = read(".tool-versions");
  const version = pinnedBun(toolVersions);
  expect(version).toBeString();
  expect(
    bunVersionProblems(
      toolVersions,
      JSON.parse(read("package.json")) as Manifest,
      version!,
    ),
  ).toEqual([]);
  // Every setup-bun step reads the same file.
  for (const path of [
    ".github/actions/rust-setup/action.yml",
    ".github/workflows/docs.yml",
    "tools/fixtures/retired-workflows/docs.yml",
  ]) {
    const source = read(path);
    expect(source).toContain("oven-sh/setup-bun@");
    expect(source.match(/bun-version-file: (\S+)/g)).toEqual([
      "bun-version-file: .tool-versions",
    ]);
    expect(source).not.toContain("bun-version:");
  }
});
