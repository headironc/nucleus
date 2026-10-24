import { readFileSync } from "node:fs";

export type Manifest = {
  packageManager?: unknown;
  devDependencies?: Record<string, unknown>;
};

// The root `.tool-versions` is the single source of the Bun version: mise
// installs and activates it locally, and CI installs it through setup-bun's
// `bun-version-file`. package.json `packageManager` and `@types/bun` must name
// the same version.
//
// setup-bun takes the first line matching /^bun\s*(.*?)$/m, keeping trailing
// whitespace and skipping indented lines, so the pin must be the only line
// starting with "bun" and contain nothing but the exact version.
export function pinnedBun(toolVersions: string): string | undefined {
  const lines = toolVersions.split("\n").filter((line) => /^\s*bun/.test(line));
  if (lines.length !== 1) return;
  return lines[0].match(/^bun (\d+\.\d+\.\d+)$/)?.[1];
}

export function bunVersionProblems(
  toolVersions: string,
  manifest: Manifest,
  running: string,
): string[] {
  const pinned = pinnedBun(toolVersions);
  if (!pinned)
    return ['.tool-versions must contain exactly one "bun x.y.z" line'];
  const problems: string[] = [];
  if (manifest.packageManager !== `bun@${pinned}`)
    problems.push(
      `package.json packageManager is ${JSON.stringify(manifest.packageManager)} but .tool-versions pins Bun ${pinned}; set it to "bun@${pinned}"`,
    );
  const types = manifest.devDependencies?.["@types/bun"];
  if (types !== pinned)
    problems.push(
      `@types/bun is ${JSON.stringify(types)} but .tool-versions pins Bun ${pinned}; update them together`,
    );
  if (running !== pinned)
    problems.push(
      `Bun ${running} is running but .tool-versions pins ${pinned}; run "mise install" in the repository, or install it with: curl -fsSL https://bun.sh/install | bash -s "bun-v${pinned}"`,
    );
  return problems;
}

if (import.meta.main) {
  const read = (path: string) =>
    readFileSync(new URL(`../${path}`, import.meta.url), "utf8");
  const problems = bunVersionProblems(
    read(".tool-versions"),
    JSON.parse(read("package.json")) as Manifest,
    Bun.version,
  );
  for (const problem of problems) console.error(problem);
  if (problems.length) process.exit(1);
}
