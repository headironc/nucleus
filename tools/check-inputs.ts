import { spawnSync } from "node:child_process";
import { appendFileSync, realpathSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// Input ownership is defined in docs/engineering/verification.md.
type Tier = "skip" | "tools" | "checks" | "full";
export type CheckSelection = {
  tier: Tier;
  halo: "skip" | "run";
  packages: "all" | string[];
};
const tiers: Tier[] = ["skip", "tools", "checks", "full"];
const root = realpathSync(
  resolve(dirname(fileURLToPath(import.meta.url)), ".."),
);
const environment = {
  ...Object.fromEntries(
    Object.entries(process.env).filter(([key]) => !key.startsWith("GIT_")),
  ),
  GIT_CONFIG_NOSYSTEM: "1",
  GIT_CONFIG_GLOBAL: "/dev/null",
  GIT_NO_REPLACE_OBJECTS: "1",
};

function git(args: string[]): Buffer {
  const result = spawnSync(
    "git",
    ["--no-optional-locks", "-c", "core.fsmonitor=false", ...args],
    {
      cwd: root,
      env: environment,
      timeout: 10_000,
      maxBuffer: 64 * 1024 * 1024,
    },
  );
  if (result.error || result.status !== 0) {
    throw new Error(`Git ${args[0]} failed`);
  }
  return result.stdout;
}

function text(args: string[]) {
  return new TextDecoder("utf-8", { fatal: true, ignoreBOM: true })
    .decode(git(args))
    .trim();
}

function paths(args: string[]) {
  const bytes = git(args);
  if (bytes.length === 0) return [];
  if (bytes.at(-1) !== 0) throw new Error("Incomplete Git path output");
  return new TextDecoder("utf-8", { fatal: true, ignoreBOM: true })
    .decode(bytes.subarray(0, -1))
    .split("\0")
    .map((path) => {
      if (!path || path.startsWith("/") || path.split("/").includes("..")) {
        throw new Error("Invalid Git path output");
      }
      return path;
    });
}

function commit(ref: string) {
  return text(["rev-parse", "--verify", "--end-of-options", `${ref}^{commit}`]);
}

function ciPaths() {
  const {
    BASE_SHA: base,
    HEAD_SHA: head,
    QUEUE_SHA: queue,
    MERGE_SHA: merge,
    EVENT_NAME: event,
  } = process.env;
  if (!merge || commit("HEAD") !== merge) {
    throw new Error("Event candidate is unverified");
  }
  const parents = text(["show", "-s", "--format=%P", "HEAD"]).split(" ");
  if (event === "pull_request") {
    if (
      !head ||
      parents.length !== 2 ||
      parents[1] !== head ||
      commit(head) !== head ||
      commit(parents[0]) !== parents[0]
    ) {
      throw new Error("Pull request parents do not match the event");
    }
  } else if (event === "merge_group") {
    if (
      !base ||
      commit(base) !== base ||
      queue !== merge ||
      parents.length !== 1 ||
      parents[0] !== base
    ) {
      throw new Error("Queue head or sole parent does not match the event");
    }
  } else {
    throw new Error("Unknown event");
  }
  return paths([
    "diff",
    "--no-ext-diff",
    "--name-only",
    "--no-renames",
    "-z",
    parents[0],
    merge,
    "--",
  ]);
}

function localPaths(base: string) {
  const ancestors = text([
    "merge-base",
    "--all",
    commit(base),
    commit("HEAD"),
  ]).split("\n");
  if (ancestors.length !== 1 || !ancestors[0])
    throw new Error("Local comparison requires one merge base");
  const ancestor = ancestors[0];
  return [
    ...paths([
      "diff",
      "--no-ext-diff",
      "--name-only",
      "--no-renames",
      "-z",
      "--cached",
      ancestor,
      "--",
    ]),
    ...paths([
      "diff",
      "--no-ext-diff",
      "--name-only",
      "--no-renames",
      "-z",
      "--",
    ]),
    ...paths(["ls-files", "--others", "--exclude-standard", "-z", "--"]),
  ];
}

function documentation(path: string) {
  return (
    [
      "AGENTS.md",
      "README.md",
      ".prettierrc.json",
      ".markdownlint.jsonc",
    ].includes(path) ||
    (path.startsWith("docs/") && path.endsWith(".md"))
  );
}

function design(path: string) {
  return (
    path.startsWith("design/") &&
    [
      "html",
      "css",
      "js",
      "md",
      "txt",
      "png",
      "jpg",
      "jpeg",
      "webp",
      "avif",
      "gif",
      "svg",
    ].some((suffix) => path.endsWith(`.${suffix}`))
  );
}

function rustTier(path: string): Tier {
  if (
    documentation(path) ||
    path === ".gitignore" ||
    design(path) ||
    path.startsWith("halo/") ||
    path === ".github/workflows/halo.yml"
  )
    return "skip";
  if (path === "tools/check-packages.ts" || path === "tools/check-inputs.ts")
    return "full";
  if (
    path.startsWith("tools/") ||
    [
      ".tool-versions",
      "package.json",
      "bun.lock",
      "tsconfig.json",
      ".github/workflows/docs.yml",
      ".github/workflows/review-gate.yml",
      ".github/workflows/review-context.yml",
      ".github/workflows/review-activity.yml",
    ].includes(path)
  )
    return "tools";
  if (path === "rustfmt.toml") return "checks";
  return "full";
}

function rustPackages(path: string): string[] | undefined {
  if (
    /(^|\/)(Cargo\.toml|Cargo\.lock|build\.rs)$/.test(path) ||
    path === "rust-toolchain.toml" ||
    path.startsWith(".cargo/")
  )
    return;
  if (path.startsWith("src/") || path.startsWith("tests/")) return ["nucleus"];
  for (const name of ["domain", "platform", "deployment", "fixture"]) {
    if (path.startsWith(`${name}/src/`)) return [name];
  }
  // Domain's database fixtures embed deployment SQL outside its Cargo graph.
  if (
    path === "deployment/roles.sql" ||
    path === "deployment/prepare.sql" ||
    (path.startsWith("deployment/migrations/") && path.endsWith(".sql"))
  )
    return ["domain", "deployment"];
  if (path === "fixture/postgresql.ts" || path === "fixture/postgresql.version")
    return ["fixture"];
  if (
    /^assets\/avatars\/(0-cat|1-dog|2-rabbit|3-panda|4-fox|5-penguin|6-pig|7-chick|8-bear|9-koala)\.webp$/.test(
      path,
    )
  )
    return ["domain"];
  // Unknown resources and validation inputs retain every workspace package.
}

// These existing tools and workflows have no Halo validation consumer.
const haloUnrelated = new Set([
  "tools/check-bun.test.ts",
  "tools/check-dependencies.ts",
  "tools/check-dependencies.test.ts",
  "tools/check-packages.ts",
  "tools/check-packages.test.ts",
  "tools/cache.test.ts",
  "tools/rust-workflow.test.ts",
  "tools/preview-design.ts",
  "tools/preview-design.test.ts",
  "tools/review-fixtures.ts",
  "tools/review-gate.ts",
  "tools/review-gate.test.ts",
  "tools/review-json.ts",
  "tools/review-json.test.ts",
  "tools/review-queue.ts",
  "tools/review-queue.test.ts",
  ".github/workflows/docs.yml",
  ".github/workflows/cache.yml",
  ".github/workflows/rust.yml",
  ".github/workflows/review-activity.yml",
  ".github/workflows/review-context.yml",
  ".github/workflows/review-gate.yml",
  "rustfmt.toml",
]);
const sharedDesign = new Set([
  "design/assets/tokens.css",
  "design/assets/logo.png",
  "design/assets/halo-botanical.svg",
]);

function haloInput(path: string) {
  if (sharedDesign.has(path)) return true;
  return !(documentation(path) || design(path) || haloUnrelated.has(path));
}

export function selectChecks(
  changed: readonly string[] | null,
): CheckSelection {
  if (changed === null) return { tier: "full", halo: "run", packages: "all" };
  let tier: Tier = "skip";
  let halo: "skip" | "run" = "skip";
  let packages: "all" | Set<string> = new Set();
  for (const path of changed) {
    if (
      !path ||
      path.startsWith("/") ||
      path.includes("\0") ||
      path.split("/").some((part) => part === "." || part === ".." || !part)
    )
      throw new Error("Invalid changed source path");
    const input = rustTier(path);
    if (tiers.indexOf(input) > tiers.indexOf(tier)) tier = input;
    if (input === "full" && packages !== "all") {
      const affected = rustPackages(path);
      if (affected === undefined) packages = "all";
      else for (const name of affected) packages.add(name);
    }
    if (haloInput(path)) halo = "run";
  }
  return {
    tier,
    halo,
    packages: packages === "all" ? "all" : [...packages].sort(),
  };
}

function main() {
  const args = process.argv.slice(2);
  const ci = args.length === 1 && args[0] === "--ci";
  const local = args.length === 2 && args[0] === "--base" && Boolean(args[1]);
  if (!ci && !local)
    throw new Error("Usage: bun tools/check-inputs.ts --ci | --base REF");
  if (ci && !process.env.GITHUB_OUTPUT)
    throw new Error("--ci requires GITHUB_OUTPUT");
  let selected = selectChecks(null);
  try {
    if (
      realpathSync(process.cwd()) !== root ||
      realpathSync(text(["rev-parse", "--show-toplevel"])) !== root
    ) {
      throw new Error("Run the classifier from its repository root");
    }
    const changed = ci ? ciPaths() : localPaths(args[1]);
    selected = selectChecks(changed);
  } catch (error) {
    selected = selectChecks(null);
    console.error(
      `Unverified comparison; running all validation: ${error instanceof Error ? error.message : "unknown failure"}`,
    );
  }
  const { tier, halo, packages } = selected;
  const selection = packages === "all" ? "all" : JSON.stringify(packages);
  console.log(`Rust validation tier: ${tier}\nHalo validation: ${halo}`);
  console.log(`Rust affected package roots: ${selection}`);
  if (!ci && tier === "full")
    console.log(
      `Package checks: bun tools/check-packages.ts --affected '${selection}'`,
    );
  if (ci)
    appendFileSync(
      process.env.GITHUB_OUTPUT!,
      `tier=${tier}\nhalo=${halo}\npackages=${selection}\n`,
    );
}

if (import.meta.main) {
  try {
    main();
  } catch (error) {
    console.error(
      error instanceof Error ? error.message : "Input classification failed",
    );
    process.exitCode = 1;
  }
}
