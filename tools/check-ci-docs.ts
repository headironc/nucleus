import { spawnSync } from "node:child_process";
import { lstatSync, readFileSync, readdirSync, realpathSync } from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";
import * as prettier from "prettier";
import { lint } from "markdownlint/promise";

const policy = realpathSync(path.resolve(import.meta.dirname, ".."));
const markdown = [
  "README.md",
  "AGENTS.md",
  "halo/README.md",
  "docs/**/*.md",
  "design/**/*.md",
];

function files(root: string, patterns: readonly string[]): string[] {
  const found = new Set<string>();
  for (const pattern of patterns)
    for (const file of new Bun.Glob(pattern).scanSync({
      cwd: root,
      onlyFiles: true,
      followSymlinks: false,
    })) {
      const location = path.join(root, file);
      if (!lstatSync(location).isFile() || realpathSync(location) !== location)
        throw new Error(
          "Docs input must be a regular file inside the candidate",
        );
      found.add(location);
    }
  return [...found].sort();
}

export async function checkDocs(
  mode: string,
  candidate: string,
): Promise<void> {
  const root = realpathSync(candidate);
  if (root === policy || realpathSync(process.cwd()) !== policy)
    throw new Error(
      "Docs checks must start in the frozen policy with a separate candidate",
    );
  // Globs do not descend into symlinked directories, even when inputs match.
  const inspect = (directory: string): void => {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const location = path.join(directory, entry.name);
      if (entry.isSymbolicLink())
        throw new Error(
          "Docs input must be a regular file inside the candidate",
        );
      if (entry.isDirectory()) inspect(location);
    }
  };
  for (const entry of readdirSync(root, { withFileTypes: true })) {
    if (!["docs", "design"].includes(entry.name)) continue;
    if (entry.isSymbolicLink())
      throw new Error("Docs input must be a regular file inside the candidate");
    if (entry.isDirectory()) inspect(path.join(root, entry.name));
  }
  const inputs = files(root, markdown);
  for (const file of ["README.md", "AGENTS.md", "halo/README.md"])
    if (!inputs.includes(path.join(root, file)))
      throw new Error(`Missing required Docs input: ${file}`);
  if (mode === "format") {
    const config = JSON.parse(
      readFileSync(path.join(policy, ".prettierrc.json"), "utf8"),
    );
    for (const file of inputs)
      if (
        !(await prettier.check(readFileSync(file, "utf8"), {
          ...config,
          filepath: file,
        }))
      )
        throw new Error(`Unformatted document: ${path.relative(root, file)}`);
  } else if (mode === "markdown") {
    const config = JSON.parse(
      readFileSync(path.join(policy, ".markdownlint.jsonc"), "utf8"),
    );
    const result = await lint({ files: inputs, config, noInlineConfig: true });
    if (Object.values(result).some((errors) => errors.length))
      throw new Error(
        Object.entries(result)
          .flatMap(([file, errors]) =>
            errors.map(
              (error) =>
                `${path.relative(root, file)}:${error.lineNumber}: ${error.ruleNames.join("/")}: ${error.ruleDescription}`,
            ),
          )
          .join("\n"),
      );
  } else if (mode === "links") {
    const result = spawnSync(
      "lychee",
      [
        "--config",
        "/dev/null",
        "--offline",
        "--no-progress",
        "--include-fragments=anchor-only",
        ...inputs,
        ...files(root, ["design/**/*.html"]).filter((file) =>
          file.endsWith(".html"),
        ),
      ],
      {
        cwd: policy,
        env: process.env,
        encoding: "utf8",
        timeout: 180_000,
        maxBuffer: 8 * 1024 * 1024,
      },
    );
    if (result.error || result.status !== 0 || result.signal)
      throw new Error(
        `Docs links failed: ${result.error?.message ?? (result.stderr + result.stdout).slice(-8_000)}`,
      );
  } else throw new Error("Unknown Docs check");
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href
) {
  try {
    const [mode, candidate] = process.argv.slice(2);
    if (process.argv.length !== 4 || !mode || !candidate)
      throw new Error("Expected Docs check and candidate");
    await checkDocs(mode, candidate);
  } catch (error) {
    console.error(error instanceof Error ? error.message : "Docs check failed");
    process.exitCode = 1;
  }
}
