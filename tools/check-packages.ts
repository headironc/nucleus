import { spawnSync } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

function check() {
  const { values } = parseArgs({
    options: {
      package: { type: "string", short: "p", multiple: true },
      "manifest-path": { type: "string" },
      help: { type: "boolean", short: "h" },
    },
    strict: true,
    allowPositionals: false,
  });
  if (values.help) {
    console.log(
      "Usage: bun tools/check-packages.ts [--package NAME ...] [--manifest-path PATH]\n" +
        "Build, test and document each selected workspace package with --locked.\n" +
        "Defaults to every workspace member in this repository, including the root package.",
    );
    return;
  }

  const manifest = resolve(
    values["manifest-path"] ??
      fileURLToPath(new URL("../Cargo.toml", import.meta.url)),
  );
  const cwd = dirname(manifest);
  function cargo(
    args: string[],
    capture = false,
    env: NodeJS.ProcessEnv = process.env,
  ): string {
    const result = spawnSync(
      "cargo",
      [...args, "--manifest-path", manifest, "--locked"],
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
        `cargo ${args.join(" ")} failed (${result.signal ?? result.status})`,
      );
    }
    return result.stdout ?? "";
  }

  const metadata = JSON.parse(
    cargo(["metadata", "--format-version", "1", "--no-deps"], true),
  );
  const members = new Set<string>(metadata.workspace_members);
  const packages: { id: string; name: string }[] = metadata.packages.filter(
    (entry: { id: string }) => members.has(entry.id),
  );
  if (packages.length === 0 || packages.length !== members.size) {
    throw new Error("Cargo metadata must identify every workspace member");
  }
  const names = [
    ...new Set(values.package ?? packages.map((entry) => entry.name)),
  ];
  const selected = names.map((name) => {
    const entry = packages.find((entry) => entry.name === name);
    if (!entry)
      throw new Error(`Unknown workspace package: ${JSON.stringify(name)}`);
    return entry;
  });

  console.log(`Packages: ${selected.map((entry) => entry.name).join(", ")}`);
  for (const entry of selected) {
    for (const step of ["build", "test", "doc"]) {
      console.log(`[${entry.name}] ${step}`);
      const args = [step, "--package", entry.id];
      if (step === "doc") {
        // Encoded flags take precedence over RUSTDOCFLAGS; keep warnings fatal here.
        cargo([...args, "--no-deps"], false, {
          ...process.env,
          RUSTDOCFLAGS: "-D warnings",
          CARGO_ENCODED_RUSTDOCFLAGS: undefined,
        });
      } else {
        cargo(args);
      }
    }
  }
  console.log(`Package checks passed (${selected.length} packages).`);
}

try {
  check();
} catch (error) {
  console.error(error instanceof Error ? error.message : error);
  process.exitCode = 1;
}
