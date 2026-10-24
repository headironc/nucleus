// Cargo resolves manifests and features; this gate only inspects its outputs.
// No build scripts, binaries or services are run.
import {
  spawnSync,
  type SpawnSyncOptionsWithBufferEncoding,
  type SpawnSyncReturns,
} from "node:child_process";
import { readFileSync, realpathSync } from "node:fs";
import { dirname, isAbsolute, join, parse, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

// The production directions in docs/architecture.md#crate-与模块组织.
const dependencies = new Map([
  ["nucleus", new Set(["domain", "platform"])],
  ["domain", new Set(["platform"])],
  ["platform", new Set<string>()],
  ["deployment", new Set(["domain", "platform"])],
  ["fixture", new Set<string>()],
]);
const testTools = new Set([
  "fixture",
  "postgresql_embedded",
  "postgresql_archive",
  "postgresql_commands",
  "rcgen",
  "sqlx",
]);
const migrationTools = new Set(["deployment", "refinery", "refinery-core"]);
const cryptoProviders = new Set(["aws-lc-rs", "aws-lc-sys"]);

type Cargo = (
  args: string[],
  options: SpawnSyncOptionsWithBufferEncoding,
) => SpawnSyncReturns<Buffer>;

class CargoFailure extends Error {}

function record(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function strings(value: unknown): value is string[] {
  return (
    Array.isArray(value) && value.every((item) => typeof item === "string")
  );
}

function metadata(output: string) {
  const workspace: unknown = JSON.parse(output);
  if (
    !record(workspace) ||
    !Array.isArray(workspace.packages) ||
    !strings(workspace.workspace_members) ||
    typeof workspace.workspace_root !== "string" ||
    !workspace.workspace_root
  ) {
    throw new Error("Cargo reported invalid workspace metadata");
  }
  const packages = workspace.packages.map((entry: unknown) => {
    if (
      !record(entry) ||
      typeof entry.id !== "string" ||
      !entry.id ||
      typeof entry.name !== "string" ||
      !entry.name ||
      !Array.isArray(entry.dependencies)
    ) {
      throw new Error("Cargo reported invalid package metadata");
    }
    const declarations = entry.dependencies.map((dependency: unknown) => {
      if (
        !record(dependency) ||
        typeof dependency.name !== "string" ||
        !dependency.name ||
        !(
          dependency.kind === null ||
          dependency.kind === "normal" ||
          dependency.kind === "build" ||
          dependency.kind === "dev"
        ) ||
        !(
          dependency.target === null || typeof dependency.target === "string"
        ) ||
        !strings(dependency.features)
      ) {
        throw new Error(
          `Cargo reported invalid dependencies for ${entry.name}`,
        );
      }
      return {
        name: dependency.name,
        kind: dependency.kind,
        target: dependency.target,
        features: dependency.features,
      };
    });
    return { id: entry.id, name: entry.name, dependencies: declarations };
  });
  const ids = new Set(workspace.workspace_members);
  const members = packages.filter((entry) => ids.has(entry.id));
  if (!members.length) throw new Error("Cargo reported no workspace members");
  if (
    ids.size !== workspace.workspace_members.length ||
    members.length !== ids.size ||
    new Set(members.map((entry) => entry.id)).size !== ids.size
  ) {
    throw new Error("Cargo metadata must identify every workspace member");
  }
  return { root: workspace.workspace_root, members };
}

function forbidden(owner: string, dependency: string): boolean {
  return (
    (dependencies.has(dependency) &&
      !dependencies.get(owner)?.has(dependency)) ||
    (owner !== "fixture" && testTools.has(dependency)) ||
    (owner !== "deployment" && migrationTools.has(dependency))
  );
}

export function checkDependencies(
  manifest: string,
  run: Cargo = (args, options) => spawnSync("cargo", args, options),
): string[] {
  const absolute = isAbsolute(manifest)
    ? manifest
    : `${process.cwd()}${sep}${manifest}`;
  manifest = parse(absolute).root;
  // Bun's realpath normalizes .. before symlinks. Resolve each prefix first so
  // the selected workspace and its Cargo toolchain match Python Path.resolve.
  for (const part of absolute.slice(manifest.length).split(sep)) {
    if (!part || part === ".") continue;
    if (part === "..") {
      manifest = dirname(manifest);
      continue;
    }
    manifest = join(manifest, part);
    try {
      manifest = realpathSync(manifest);
    } catch (error) {
      // Keep absent components so Cargo can diagnose a missing manifest.
      if (!record(error) || error.code !== "ENOENT") throw error;
    }
  }
  function cargo(...args: string[]): string {
    const result = run(
      [...args, "--manifest-path", manifest, "--locked", "--color", "never"],
      {
        cwd: dirname(manifest),
        encoding: "buffer",
        stdio: ["inherit", "pipe", "pipe"],
        // Python's previous subprocess capture had no size cap and killed on timeout.
        maxBuffer: Infinity,
        timeout: 120_000,
        killSignal: "SIGKILL",
      },
    );
    if (result.error) throw result.error;
    const decode = (bytes: Buffer | null) =>
      new TextDecoder("utf-8", { fatal: true }).decode(bytes ?? undefined);
    if (result.status !== 0)
      throw new CargoFailure(decode(result.stderr).trim());
    return decode(result.stdout);
  }

  const workspace = metadata(
    cargo("metadata", "--format-version", "1", "--no-deps"),
  );
  const locked = Bun.TOML.parse(
    readFileSync(join(workspace.root, "Cargo.lock")),
  );
  if (
    !record(locked) ||
    !Array.isArray(locked.package) ||
    !locked.package.length
  )
    throw new Error("Cargo.lock must contain a package array");
  const names = new Set(
    locked.package.map((entry: unknown) => {
      if (!record(entry) || typeof entry.name !== "string" || !entry.name)
        throw new Error("Cargo.lock contains a package without a name");
      return entry.name;
    }),
  );
  const failures = new Set<string>();
  for (const name of names) {
    if (cryptoProviders.has(name))
      failures.add(
        `Cargo.lock: forbidden crypto provider ${name}; keep the graph ring-only`,
      );
  }
  for (const member of workspace.members) {
    const owner = member.name;
    if (!dependencies.has(owner)) {
      failures.add(
        `${owner}: define its architecture dependency boundary before adding a workspace member`,
      );
      continue;
    }
    // Metadata includes dormant optional/target declarations and actual renamed packages.
    for (const dependency of member.dependencies) {
      if (dependency.kind === "dev") continue;
      let location = `${owner} ${dependency.kind || "normal"} dependency`;
      if (dependency.target) location += ` (${dependency.target})`;
      if (forbidden(owner, dependency.name))
        failures.add(
          `${location}: ${dependency.name} crosses the production boundary`,
        );
      if (
        dependency.name === "domain" &&
        dependency.features.includes("test-support")
      )
        failures.add(
          `${location}: domain/test-support belongs only in dev-dependencies`,
        );
    }
    const graph = cargo(
      "tree",
      "--package",
      member.id,
      "--edges",
      "normal,build",
      "--target",
      "all",
      "--prefix",
      "none",
      "--format",
      "{p}|{f}",
    );
    if (!graph.trim())
      throw new Error(`Cargo reported an empty production graph for ${owner}`);
    // Match Python str.splitlines, including lone CR and Unicode line separators.
    const lines = graph.split(/\r\n|[\n\r\v\f\u001c-\u001e\u0085\u2028\u2029]/);
    if (lines.at(-1) === "") lines.pop();
    for (const line of lines) {
      const separator = line.lastIndexOf("|");
      const packageText = line.slice(0, separator).trim();
      if (separator === -1 || !packageText)
        throw new Error(
          `Cargo reported an unrecognized tree entry for ${owner}`,
        );
      const name = packageText.split(/\s+/)[0];
      if (name !== owner && forbidden(owner, name))
        failures.add(
          `${owner} production graph: ${name} crosses the production boundary`,
        );
      // The root's features and cargo tree's repeated-node suffix matter too.
      const enabled = line
        .slice(separator + 1)
        .replace(/ \(\*\)$/, "")
        .trim()
        .split(",");
      if (name === "domain" && enabled.includes("test-support"))
        failures.add(
          `${owner} production graph: domain/test-support is enabled`,
        );
    }
  }
  // UTF-8 order matches Python's Unicode code-point order, including package names.
  return [...failures].sort((left, right) =>
    Buffer.compare(Buffer.from(left), Buffer.from(right)),
  );
}

function main(): number {
  try {
    const { values } = parseArgs({
      options: {
        "manifest-path": { type: "string" },
        help: { type: "boolean", short: "h" },
      },
      strict: true,
      allowPositionals: false,
    });
    if (values.help) {
      console.log(
        "Usage: bun tools/check-dependencies.ts [--manifest-path PATH]\nCheck the dependency boundaries owned by docs/engineering/rust.md.",
      );
      return 0;
    }
    const manifest =
      values["manifest-path"] ??
      resolve(
        dirname(realpathSync(fileURLToPath(import.meta.url))),
        "../Cargo.toml",
      );
    const failures = checkDependencies(manifest);
    if (failures.length) {
      for (const failure of failures) console.error(failure);
      console.error(
        "See docs/engineering/rust.md#dependency-boundaries and docs/architecture.md#crate-与模块组织.",
      );
      return 1;
    }
    console.log(
      "Dependency boundaries passed (declarations, locked crypto providers, production graphs and features).",
    );
    return 0;
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    console.error(
      error instanceof CargoFailure
        ? `Dependency check could not inspect Cargo inputs:\n${message}`
        : `Dependency check could not complete: ${message}`,
    );
    return 2;
  }
}

if (import.meta.main) process.exitCode = main();
