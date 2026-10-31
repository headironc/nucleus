import { afterEach, expect, test } from "bun:test";
import { link, mkdir, mkdtemp, readFile, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { packageOutput } from "../tools/package-output";

const fixtures: string[] = [];
afterEach(async () => { for (const path of fixtures.splice(0)) await rm(path, { recursive: true, force: true }); });
async function fixture() {
  const root = await mkdtemp(join(tmpdir(), "orbit-output-"));
  fixtures.push(root);
  await mkdir(join(root, "build"));
  await mkdir(join(root, "src"));
  await writeFile(join(root, "build/app.js"), "frozen package");
  await writeFile(join(root, "src/app.ts"), "source");
  return root;
}

test("output selection rejects source, absolute and traversal paths before writing", async () => {
  const root = await fixture();
  for (const name of ["src", "../build", "/build", "build/../src", "build-review/", "build-review/child"]) {
    await expect(packageOutput(name, root)).rejects.toThrow("Expected build");
  }
  expect(await readFile(join(root, "build/app.js"), "utf8")).toBe("frozen package");
  expect(await packageOutput("build-review", root)).toEqual({ name: "build-review", directory: join(root, "build-review") });
});

test.each(["build", "src"])("a named output cannot alias the existing %s directory", async target => {
  const root = await fixture();
  await symlink(join(root, target), join(root, "build-review"));
  await expect(packageOutput("build-review", root)).rejects.toThrow("link aliases");
  expect(await readFile(join(root, "build/app.js"), "utf8")).toBe("frozen package");
  expect(await readFile(join(root, "src/app.ts"), "utf8")).toBe("source");
});

test.each(["symbolic", "hard"])("existing output children cannot contain %s aliases into a frozen package", async kind => {
  const root = await fixture();
  await mkdir(join(root, "build-review"));
  const previous = join(root, "build/app.js"), destination = join(root, "build-review/app.js");
  if (kind === "symbolic") await symlink(previous, destination);
  else await link(previous, destination);
  await expect(packageOutput("build-review", root)).rejects.toThrow("link aliases");
  expect(await readFile(previous, "utf8")).toBe("frozen package");
});

test("a regular file cannot be selected as an output directory", async () => {
  const root = await fixture();
  await writeFile(join(root, "build-review"), "not a directory");
  await expect(packageOutput("build-review", root)).rejects.toThrow("must be a directory");
});
