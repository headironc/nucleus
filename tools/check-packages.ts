import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { realpathSync } from "node:fs";
import { dirname, isAbsolute, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

type Package = {
  id: string;
  name: string;
  directory: string;
  targets: {
    name: string;
    kind: string[];
    test: boolean;
    doctest: boolean;
    "required-features"?: string[];
  }[];
  dependencies: { path?: string | null }[];
};

function record(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function workspacePackages(metadata: unknown): Package[] {
  if (
    !record(metadata) ||
    !Array.isArray(metadata.packages) ||
    !Array.isArray(metadata.workspace_members) ||
    !metadata.workspace_members.every(
      (member) => typeof member === "string" && member.length > 0,
    )
  )
    throw new Error("Cargo metadata must identify every workspace member");
  const members = new Set<string>(metadata.workspace_members);
  const packages: Package[] = [];
  for (const entry of metadata.packages) {
    if (!record(entry) || typeof entry.id !== "string")
      throw new Error("Cargo metadata contains an invalid package");
    if (!members.has(entry.id)) continue;
    if (
      typeof entry.name !== "string" ||
      !entry.name ||
      typeof entry.manifest_path !== "string" ||
      !isAbsolute(entry.manifest_path) ||
      !Array.isArray(entry.targets) ||
      entry.targets.length === 0 ||
      !entry.targets.every(
        (target) =>
          record(target) &&
          Array.isArray(target.kind) &&
          target.kind.length > 0 &&
          target.kind.every((kind: unknown) => typeof kind === "string") &&
          typeof target.name === "string" &&
          target.name.length > 0 &&
          typeof target.test === "boolean" &&
          typeof target.doctest === "boolean" &&
          (target["required-features"] === undefined ||
            (Array.isArray(target["required-features"]) &&
              target["required-features"].every(
                (feature: unknown) => typeof feature === "string",
              ))),
      ) ||
      !Array.isArray(entry.dependencies) ||
      !entry.dependencies.every(
        (dependency) =>
          record(dependency) &&
          typeof dependency.name === "string" &&
          dependency.name.length > 0 &&
          (dependency.kind === null ||
            ["normal", "dev", "build"].some(
              (kind) => kind === dependency.kind,
            )) &&
          typeof dependency.optional === "boolean" &&
          (dependency.source === null ||
            typeof dependency.source === "string") &&
          (dependency.source !== null || typeof dependency.path === "string") &&
          (dependency.target === null ||
            typeof dependency.target === "string") &&
          (dependency.path === undefined ||
            dependency.path === null ||
            (typeof dependency.path === "string" &&
              isAbsolute(dependency.path))),
      )
    )
      throw new Error(`Cargo metadata is incomplete for ${entry.id}`);
    packages.push({
      id: entry.id,
      name: entry.name,
      directory: realpathSync(dirname(entry.manifest_path)),
      targets: entry.targets,
      dependencies: entry.dependencies,
    });
  }
  if (
    packages.length === 0 ||
    members.size !== metadata.workspace_members.length ||
    packages.length !== members.size ||
    new Set(packages.map((entry) => entry.id)).size !== packages.length ||
    new Set(packages.map((entry) => entry.name)).size !== packages.length ||
    new Set(packages.map((entry) => entry.directory)).size !== packages.length
  )
    throw new Error("Cargo metadata must identify every workspace member");
  return packages;
}

function requireLibraryTests(entry: Package) {
  // --lib plus --doc must cover every test of a sharded package.
  const targets = entry.targets.filter(
    (target) => !target.kind.includes("custom-build"),
  );
  if (
    targets.length !== 1 ||
    targets[0].kind.join() !== "lib" ||
    !targets[0].test
  )
    throw new Error(
      "--shard requires a library-only package with library tests enabled",
    );
}

function nucleusGroupPlan(entry: Package): {
  rest: string[];
  doctest: boolean;
} {
  const rest: string[] = [];
  const seen = new Set<string>();
  let http = false;
  let doctest = false;
  for (const target of entry.targets) {
    const kind = target.kind.join();
    const identity = `${kind}:${target.name}`;
    // Explicit selectors preserve default test flags; feature-gated targets need
    // a new plan. Examples also compile by default, so reject unsupported shapes.
    if (
      !["lib", "bin", "test", "custom-build"].includes(kind) ||
      !/^[A-Za-z0-9_-]+$/.test(target.name) ||
      (target["required-features"]?.length ?? 0) > 0 ||
      seen.has(identity)
    )
      throw new Error(
        "Nucleus groups require distinct lib, bin or test targets without required features",
      );
    seen.add(identity);
    if (kind === "custom-build") continue;
    if (kind === "lib") doctest = target.doctest;
    if (!target.test) continue;
    if (kind === "test" && target.name === "http") http = true;
    else
      rest.push(...(kind === "lib" ? ["--lib"] : [`--${kind}`, target.name]));
  }
  if (
    !http ||
    rest.length === 0 ||
    entry.targets.filter((target) => target.kind.join() === "lib").length !== 1
  )
    throw new Error(
      "Nucleus groups require one library, an enabled http integration target and nonempty remaining tests",
    );
  return { rest, doctest };
}

function testNames(output: string): string[] {
  const lines = output.trim().split(/\r?\n/).filter(Boolean);
  const summary = lines.pop()?.match(/^(\d+) tests?, 0 benchmarks?$/);
  const names = lines.map((line) => {
    const match = line.match(/^(.+): test$/);
    if (!match)
      throw new Error(
        `Test listing contains an unexpected line: ${JSON.stringify(line)}`,
      );
    return match[1];
  });
  if (
    !summary ||
    Number(summary[1]) !== names.length ||
    new Set(names).size !== names.length
  ) {
    throw new Error(
      "Test listing must contain unique names and a matching test count",
    );
  }
  return names.sort();
}

function check() {
  const { values } = parseArgs({
    options: {
      package: { type: "string", short: "p", multiple: true },
      affected: { type: "string" },
      "manifest-path": { type: "string" },
      "test-threads": { type: "string" },
      shard: { type: "string" },
      group: { type: "string" },
      list: { type: "boolean" },
      matrix: { type: "boolean" },
      help: { type: "boolean", short: "h" },
    },
    strict: true,
    allowPositionals: false,
  });
  if (values.help) {
    console.log(
      "Usage: bun tools/check-packages.ts [--package NAME ... | --affected SELECTION] [--manifest-path PATH] [--test-threads N] [--shard INDEX/TOTAL | --group http|rest | --list | --matrix]\n" +
        "Build, test and document each selected workspace package with --locked.\n" +
        "Defaults to every workspace member in this repository, including the root package.\n" +
        "--affected accepts all or a nonempty JSON array of package roots, including their Cargo consumers.\n" +
        "--test-threads sets test harness concurrency without changing Cargo build jobs.\n" +
        "--shard runs one deterministic library-test shard of one explicit library-only package.\n" +
        "Run every shard; shard 1 also builds, runs doctests and generates rustdoc.\n" +
        "--group splits one explicit nucleus package: http runs ordinary HTTP tests; rest retains all other checks. Run both groups.\n" +
        "--list prints the validated package names as JSON without building them.\n" +
        "--matrix prints CI entries, using four domain shards and two nucleus groups, without building them.",
    );
    return;
  }

  if (values.affected !== undefined && values.package !== undefined)
    throw new Error("--affected cannot be combined with --package");
  if (values.matrix && (values.list || values.shard !== undefined))
    throw new Error("--matrix cannot be combined with --list or --shard");
  const group = values.group;
  if (
    group !== undefined &&
    (!["http", "rest"].includes(group) ||
      values.affected !== undefined ||
      values.list ||
      values.matrix ||
      values.shard !== undefined ||
      new Set(values.package).size !== 1 || values.package?.[0] !== "nucleus")
  )
    throw new Error(
      "--group requires http or rest and one explicit nucleus package; discovery and shards cannot be combined",
    );
  let affected: string[] | undefined;
  if (values.affected !== undefined && values.affected !== "all") {
    const input: unknown = JSON.parse(values.affected);
    if (
      !Array.isArray(input) ||
      input.length === 0 ||
      !input.every((name) => typeof name === "string" && name.length > 0)
    )
      throw new Error(
        "--affected requires all or a nonempty JSON package array",
      );
    affected = input;
  }

  const threads = values["test-threads"];
  if (
    threads !== undefined &&
    (!/^[1-9][0-9]*$/.test(threads) || !Number.isSafeInteger(Number(threads)))
  ) {
    throw new Error("--test-threads must be a positive safe integer");
  }

  let shard: { index: number; total: number } | undefined;
  if (values.shard !== undefined) {
    const match = values.shard.match(/^([1-9][0-9]*)\/([1-9][0-9]*)$/);
    if (
      !match ||
      !match.slice(1).every((part) => Number.isSafeInteger(Number(part))) ||
      Number(match[1]) > Number(match[2])
    ) {
      throw new Error(
        "--shard must be INDEX/TOTAL with positive safe integers and INDEX <= TOTAL",
      );
    }
    if (
      values.list ||
      values.affected !== undefined ||
      new Set(values.package).size !== 1
    ) {
      throw new Error(
        "--shard requires one explicit package and cannot be combined with --list",
      );
    }
    shard = { index: Number(match[1]), total: Number(match[2]) };
  }

  const manifest = resolve(
    values["manifest-path"] ??
      fileURLToPath(new URL("../Cargo.toml", import.meta.url)),
  );
  const cwd = dirname(manifest);
  function cargo(
    command: string,
    args: string[],
    capture = false,
    env: NodeJS.ProcessEnv = process.env,
  ): string {
    const result = spawnSync(
      "cargo",
      [command, "--manifest-path", manifest, "--locked", ...args],
      {
        cwd,
        env,
        encoding: "utf8",
        stdio: capture ? ["inherit", "pipe", "inherit"] : "inherit",
      },
    );
    if (result.error) throw result.error;
    if (result.status !== 0) {
      throw new Error(
        `cargo ${command} ${args.join(" ")} failed (${result.signal ?? result.status})`,
      );
    }
    return result.stdout ?? "";
  }

  const packages = workspacePackages(
    JSON.parse(cargo("metadata", ["--format-version", "1", "--no-deps"], true)),
  );
  const names = [
    ...new Set(
      affected ?? values.package ?? packages.map((entry) => entry.name),
    ),
  ];
  let selected = names.map((name) => {
    const entry = packages.find((entry) => entry.name === name);
    if (!entry)
      throw new Error(`Unknown workspace package: ${JSON.stringify(name)}`);
    return entry;
  });

  if (affected) {
    const owners = new Map(packages.map((entry) => [entry.directory, entry]));
    const consumers = new Map(
      packages.map((entry) => [entry.id, new Set<string>()]),
    );
    let complete = true;
    for (const consumer of packages) {
      // Include inactive optional, target-qualified and development declarations.
      for (const dependency of consumer.dependencies) {
        if (!dependency.path) continue;
        const owner = owners.get(realpathSync(dependency.path));
        if (owner) consumers.get(owner.id)!.add(consumer.id);
        else complete = false;
      }
    }
    // --no-deps cannot reveal paths through excluded packages back to workspace members.
    const included = new Set(
      (complete ? selected : packages).map((entry) => entry.id),
    );
    for (const id of included)
      for (const consumer of consumers.get(id)!) included.add(consumer);
    selected = packages.filter((entry) => included.has(entry.id));
  }

  if (shard) requireLibraryTests(selected[0]);
  const groupPlan = group === undefined ? undefined : nucleusGroupPlan(selected[0]);

  if (values.list) {
    console.log(JSON.stringify(selected.map((entry) => entry.name)));
    return;
  }
  if (values.matrix) {
    const include = selected.flatMap((entry) => {
      if (entry.name === "nucleus") {
        nucleusGroupPlan(entry);
        return ["rest", "http"].map((group) => ({
          package: entry.name,
          group,
        }));
      }
      if (entry.name !== "domain") return [{ package: entry.name }];
      requireLibraryTests(entry);
      return Array.from({ length: 4 }, (_, index) => ({
        package: entry.name,
        shard: `${index + 1}/4`,
      }));
    });
    console.log(JSON.stringify({ include }));
    return;
  }

  console.log(`Packages: ${selected.map((entry) => entry.name).join(", ")}`);
  if (threads !== undefined) console.log(`Test threads: ${threads}`);
  for (const entry of selected) {
    let steps = ["build", "test", "doc"];
    if (groupPlan)
      steps = group === "http"
        ? ["test"]
        : ["build", "test", ...(groupPlan.doctest ? ["doctest"] : []), "doc"];
    else if (shard)
      steps = shard.index === 1
        ? ["build", "test", "doctest", "doc"]
        : ["test"];
    for (const step of steps) {
      console.log(`[${entry.name}] ${step}`);
      const args = ["--package", entry.id];
      const harness =
        threads === undefined ? [] : [`--test-threads=${threads}`];
      if (step === "test" && groupPlan) {
        cargo("test", [
          ...args,
          ...(group === "http" ? ["--test", "http"] : groupPlan.rest),
          "--",
          ...harness,
        ]);
        continue;
      }
      if (step === "test" && shard) {
        const library = [...args, "--lib", "--"];
        const listing = ["--list", "--format=pretty"];
        const all = testNames(cargo("test", [...library, ...listing], true));
        if (shard.total > all.length)
          throw new Error("Shard count exceeds the library test count");
        const partitions: string[][] = Array.from(
          { length: shard.total },
          () => [],
        );
        for (const name of all) {
          const hash = createHash("sha256")
            .update(name)
            .digest()
            .readUInt32BE(0);
          partitions[hash % shard.total].push(name);
        }
        const union = partitions.flat().sort();
        if (
          new Set(union).size !== all.length ||
          JSON.stringify(union) !== JSON.stringify(all)
        ) {
          throw new Error(
            "Test shards must cover every library test exactly once",
          );
        }
        const tests = partitions[shard.index - 1];
        console.log(
          `[${entry.name}] shard ${shard.index}/${shard.total}: ${tests.length}/${all.length} library tests; partition counts: ${partitions.map((part) => part.length).join(" + ")}`,
        );
        console.log(
          `[${entry.name}] test list SHA-256: ${createHash("sha256").update(JSON.stringify(all)).digest("hex")}`,
        );
        // An empty filter would execute the whole suite, so never invoke it.
        if (tests.length === 0) continue;
        const filtered = [...library, "--exact", ...tests];
        const actual = testNames(
          cargo("test", [...filtered, ...listing], true),
        );
        if (JSON.stringify(actual) !== JSON.stringify(tests)) {
          throw new Error("Test harness selection does not match the shard");
        }
        cargo("test", [...filtered, ...harness]);
        continue;
      }
      if (step === "doctest") {
        cargo("test", [...args, "--doc", "--", ...harness]);
        continue;
      }
      if (step === "test" && harness.length > 0) {
        args.push("--", ...harness);
      }
      if (step === "doc") {
        // Encoded flags take precedence over RUSTDOCFLAGS; keep warnings fatal here.
        cargo(step, [...args, "--no-deps"], false, {
          ...process.env,
          RUSTDOCFLAGS: "-D warnings",
          CARGO_ENCODED_RUSTDOCFLAGS: undefined,
        });
      } else {
        cargo(step, args);
      }
    }
  }
  console.log(
    group
      ? `Package group checks passed (${group}).`
      : shard
      ? `Package shard checks passed (${shard.index}/${shard.total}).`
      : `Package checks passed (${selected.length} packages).`,
  );
}

try {
  check();
} catch (error) {
  console.error(error instanceof Error ? error.message : error);
  process.exitCode = 1;
}
