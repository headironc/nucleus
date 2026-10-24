import { afterEach, expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const script = fileURLToPath(new URL("./check-packages.ts", import.meta.url));
const toolchain = readFileSync(
  new URL("../rust-toolchain.toml", import.meta.url),
);
const directories: string[] = [];

afterEach(() => {
  for (const directory of directories.splice(0)) {
    rmSync(directory, { recursive: true, force: true });
  }
});

function workspace() {
  const directory = mkdtempSync(join(tmpdir(), "nucleus package checks "));
  directories.push(directory);
  const target = join(directory, "target");
  const manifest = join(directory, "Cargo.toml");
  const env = {
    ...process.env,
    CARGO_NET_OFFLINE: "true",
    CARGO_TARGET_DIR: target,
  };
  function write(path: string, source: string | Buffer) {
    writeFileSync(join(directory, path), source);
  }
  function cargo(args: string[]) {
    return spawnSync("cargo", args, {
      cwd: directory,
      env,
      encoding: "utf8",
      timeout: 30_000,
    });
  }
  function check(args: string[] = [], extra: NodeJS.ProcessEnv = {}) {
    return spawnSync(
      process.execPath,
      [script, "--manifest-path", manifest, ...args],
      {
        // Selection and the toolchain must come from the manifest, not this directory.
        cwd: tmpdir(),
        env: { ...env, ...extra },
        encoding: "utf8",
        timeout: 30_000,
      },
    );
  }
  function packageSource(name: string) {
    return `/// A package whose examples run during validation.
/// \`\`\`
/// std::fs::write(concat!(env!("CARGO_MANIFEST_DIR"), "/doctested"), "${name}").expect("marker");
/// \`\`\`
pub fn value() -> u8 { 1 }

#[test]
fn records_test_execution() {
    std::fs::write(concat!(env!("CARGO_MANIFEST_DIR"), "/tested"), "${name}").expect("marker");
}
`;
  }
  mkdirSync(join(directory, "src"));
  mkdirSync(join(directory, "worker/src"), { recursive: true });
  write("rust-toolchain.toml", toolchain);
  write(
    "Cargo.toml",
    `[package]
name = "application"
version = "0.1.0"
edition = "2024"
[workspace]
resolver = "3"
members = ["worker"]
default-members = ["."]
`,
  );
  write("src/lib.rs", packageSource("application"));
  write("src/main.rs", "fn main() { assert_eq!(application::value(), 1); }\n");
  write(
    "worker/Cargo.toml",
    '[package]\nname = "worker"\nversion = "0.1.0"\nedition = "2024"\n',
  );
  write("worker/src/lib.rs", packageSource("worker"));
  const locked = cargo(["generate-lockfile", "--offline"]);
  expect(locked.error).toBeUndefined();
  expect(locked.status).toBe(0);
  return { directory, target, manifest, write, cargo, check };
}

function passed(result: ReturnType<typeof spawnSync>) {
  expect(result.error).toBeUndefined();
  expect(result.status, `${result.stdout}\n${result.stderr}`).toBe(0);
}

function failed(result: ReturnType<typeof spawnSync>, diagnostic: string) {
  expect(result.error).toBeUndefined();
  expect(result.status).toBe(1);
  expect(String(result.stderr)).toContain(diagnostic);
  expect(String(result.stdout)).not.toContain("Package checks passed");
}

function nucleusWorkspace() {
  const fixture = workspace();
  fixture.write("Cargo.toml", readFileSync(fixture.manifest, "utf8").replace('name = "application"', 'name = "nucleus"'));
  fixture.write("src/lib.rs", `/// Records a checked documentation example.
/// \`\`\`
/// nucleus::record("doctest");
/// \`\`\`
pub fn record(name: &str) {
    use std::io::Write;
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("ran-{name}"));
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
    writeln!(file, "once").unwrap();
}
#[test] fn library() { record("lib"); }
`);
  fixture.write("src/main.rs", 'fn main() {}\n#[test] fn binary() { nucleus::record("bin"); }\n');
  mkdirSync(join(fixture.directory, "tests"));
  fixture.write("tests/http.rs", '#[test] fn http() { nucleus::record("http"); }\n#[test] #[ignore] fn browser() { panic!("ordinary checks must ignore this test"); }\n');
  fixture.write("tests/cli.rs", '#[test] fn cli() { nucleus::record("cli"); }\n');
  passed(fixture.cargo(["generate-lockfile", "--offline"]));
  return fixture;
}

test("nucleus groups execute the complete default target union exactly once, including future targets", () => {
  const fixture = nucleusWorkspace();
  fixture.write("tests/new.rs", '#[test] fn future() { nucleus::record("new"); }\n');
  mkdirSync(join(fixture.directory, "src/bin"));
  fixture.write("src/bin/new.rs", 'fn main() {}\n#[test] fn future_binary() { nucleus::record("new-bin"); }\n');
  fixture.write("tests/custom.rs", 'fn main() { assert!(std::env::args().any(|arg| arg == "--test-threads=4")); nucleus::record("custom"); }\n');
  fixture.write("tests/disabled.rs", 'compile_error!("disabled tests must not be selected");\n');
  fixture.write("Cargo.toml", readFileSync(fixture.manifest, "utf8") + '\n[[test]]\nname = "custom"\nharness = false\n[[test]]\nname = "disabled"\ntest = false\n');
  const matrix = fixture.check(["--matrix", "--affected", '["nucleus"]']);
  passed(matrix);
  expect(JSON.parse(String(matrix.stdout))).toEqual({ include: [
    { package: "nucleus", group: "rest" }, { package: "nucleus", group: "http" },
  ] });
  expect(existsSync(fixture.target)).toBe(false);
  const http = fixture.check(["-p", "nucleus", "--group", "http", "--test-threads", "4"]);
  passed(http);
  expect(http.stdout).toContain("1 passed; 0 failed; 1 ignored");
  expect(http.stdout).not.toContain("[nucleus] build");
  expect(readdirSync(fixture.directory).filter(name => name.startsWith("ran-"))).toEqual(["ran-http"]);
  expect(existsSync(join(fixture.target, "doc/nucleus/index.html"))).toBe(false);
  const rest = fixture.check(["-p", "nucleus", "--group", "rest", "--test-threads", "4"]);
  passed(rest);
  for (const step of ["build", "test", "doctest", "doc"]) expect(rest.stdout).toContain(`[nucleus] ${step}\n`);
  expect(existsSync(join(fixture.target, "doc/nucleus/index.html"))).toBe(true);
  const expected = ["bin", "cli", "custom", "doctest", "http", "lib", "new", "new-bin"].map(name => `ran-${name}`).sort();
  expect(readdirSync(fixture.directory).filter(name => name.startsWith("ran-")).sort()).toEqual(expected);
  for (const name of expected) {
    expect(readFileSync(join(fixture.directory, name), "utf8")).toBe("once\n");
    rmSync(join(fixture.directory, name));
  }
  // Cargo's ungrouped default is an independent reference for the same target set.
  passed(fixture.check(["-p", "nucleus", "--test-threads", "4"]));
  expect(readdirSync(fixture.directory).filter(name => name.startsWith("ran-")).sort()).toEqual(expected);
  for (const name of expected) expect(readFileSync(join(fixture.directory, name), "utf8")).toBe("once\n");
}, 30_000);

test.each(["example", "bench", "features", "missing-http", "disabled-http", "empty-rest"])(
  "unsupported nucleus topology is refused before discovery or execution builds: %s", shape => {
    const fixture = nucleusWorkspace();
    if (shape === "example" || shape === "bench") {
      const directory = shape === "example" ? "examples" : "benches";
      mkdirSync(join(fixture.directory, directory));
      fixture.write(`${directory}/future.rs`, "fn main() {}\n");
    } else if (shape === "features") {
      fixture.write("Cargo.toml", readFileSync(fixture.manifest, "utf8") + '\n[features]\noptional = []\n[[test]]\nname = "http"\nrequired-features = ["optional"]\n');
    } else if (shape === "missing-http") rmSync(join(fixture.directory, "tests/http.rs"));
    else if (shape === "disabled-http") fixture.write("Cargo.toml", readFileSync(fixture.manifest, "utf8") + '\n[[test]]\nname = "http"\ntest = false\n');
    else {
      fixture.write("Cargo.toml", readFileSync(fixture.manifest, "utf8") + '\n[lib]\ntest = false\n[[bin]]\nname = "nucleus"\ntest = false\n[[test]]\nname = "cli"\ntest = false\n');
    }
    for (const args of [["--matrix", "--affected", '["nucleus"]'], ["-p", "nucleus", "--group", "http"], ["-p", "nucleus", "--group", "rest"]]) {
      failed(fixture.check(args), "Nucleus groups require");
    }
    expect(existsSync(fixture.target)).toBe(false);
  }, 30_000,
);

test("nucleus groups reject incompatible options and preserve disabled doctests", () => {
  const fixture = nucleusWorkspace();
  for (const args of [[], ["-p", "worker"], ["-p", "nucleus", "-p", "worker"], ["-p", "nucleus", "--shard", "1/4"], ["--affected", "all"], ["-p", "nucleus", "--matrix"], ["-p", "nucleus", "--list"]]) {
    failed(fixture.check([...args, "--group", "http"]), "--group requires");
  }
  failed(fixture.check(["-p", "nucleus", "--group", "unknown"]), "--group requires");
  expect(existsSync(fixture.target)).toBe(false);
  fixture.write("Cargo.toml", readFileSync(fixture.manifest, "utf8") + '\n[lib]\ndoctest = false\n');
  passed(fixture.check(["-p", "nucleus", "--group", "rest"]));
  expect(existsSync(join(fixture.directory, "ran-doctest"))).toBe(false);
  expect(existsSync(join(fixture.target, "doc/nucleus/index.html"))).toBe(true);
}, 30_000);

test.each(["http", "rest"])("a failing nucleus test fails its group: %s", group => {
  const fixture = nucleusWorkspace();
  fixture.write(group === "http" ? "tests/http.rs" : "tests/cli.rs", '#[test] fn fails() { panic!("group failure"); }\n');
  failed(fixture.check(["-p", "nucleus", "--group", group]), "cargo test");
  expect(existsSync(join(fixture.directory, "ran-doctest"))).toBe(false);
  expect(existsSync(join(fixture.target, "doc/nucleus/index.html"))).toBe(false);
}, 30_000);

test.each([
  ['/// ```\n/// panic!("doctest rejected");\n/// ```\npub fn record(_: &str) {}', "doctest failed"],
  ['/// [MissingDocumentationTarget]\npub fn record(_: &str) {}', "unresolved link"],
])("nucleus rest retains doctest and fatal rustdoc failures: %s", (source, diagnostic) => {
  const fixture = nucleusWorkspace();
  fixture.write("src/lib.rs", source + '\n#[test] fn library() {}\n');
  failed(fixture.check(["-p", "nucleus", "--group", "rest"], { RUSTDOCFLAGS: "-A warnings", CARGO_ENCODED_RUSTDOCFLAGS: "-A\x1fwarnings" }), diagnostic);
}, 30_000);

test("default selection checks every member, including the root and non-default members", () => {
  const fixture = workspace();
  const manifest = readFileSync(fixture.manifest);
  const lock = readFileSync(join(fixture.directory, "Cargo.lock"));
  passed(fixture.check());
  for (const [name, path] of [
    ["application", ""],
    ["worker", "worker"],
  ]) {
    expect(readFileSync(join(fixture.directory, path, "tested"), "utf8")).toBe(
      name,
    );
    expect(
      readFileSync(join(fixture.directory, path, "doctested"), "utf8"),
    ).toBe(name);
    expect(existsSync(join(fixture.target, "doc", name, "index.html"))).toBe(
      true,
    );
  }
  expect(existsSync(join(fixture.target, "debug/application"))).toBe(true);
  expect(readFileSync(fixture.manifest)).toEqual(manifest);
  expect(readFileSync(join(fixture.directory, "Cargo.lock"))).toEqual(lock);
}, 30_000);

test("explicit selection runs only the named member and coalesces duplicates", () => {
  const fixture = workspace();
  fixture.write("src/lib.rs", 'compile_error!("unselected package");\n');
  const result = fixture.check(["--package", "worker", "-p", "worker"]);
  passed(result);
  expect(result.stdout).toContain("Package checks passed (1 packages)");
  expect(readFileSync(join(fixture.directory, "worker/tested"), "utf8")).toBe(
    "worker",
  );
  expect(existsSync(join(fixture.target, "doc/worker/index.html"))).toBe(true);
  expect(existsSync(join(fixture.target, "doc/application"))).toBe(false);
}, 30_000);

test("JSON discovery includes new members without compiling or modifying the workspace", () => {
  const fixture = workspace();
  mkdirSync(join(fixture.directory, "newcomer/src"), { recursive: true });
  fixture.write(
    "newcomer/Cargo.toml",
    '[package]\nname = "newcomer"\nversion = "0.1.0"\nedition = "2024"\n',
  );
  fixture.write(
    "newcomer/src/lib.rs",
    'compile_error!("discovery must not compile");\n',
  );
  fixture.write(
    "Cargo.toml",
    readFileSync(fixture.manifest, "utf8").replace(
      'members = ["worker"]',
      'members = ["worker", "newcomer"]',
    ),
  );
  passed(fixture.cargo(["generate-lockfile", "--offline"]));
  const manifest = readFileSync(fixture.manifest);
  const lock = readFileSync(join(fixture.directory, "Cargo.lock"));
  const result = fixture.check(["--list"]);
  passed(result);
  expect(JSON.parse(String(result.stdout)).sort()).toEqual([
    "application",
    "newcomer",
    "worker",
  ]);
  const all = fixture.check(["--matrix", "--affected", "all"]);
  passed(all);
  expect(
    JSON.parse(String(all.stdout))
      .include.map((entry: { package: string }) => entry.package)
      .sort(),
  ).toEqual(["application", "newcomer", "worker"]);
  expect(existsSync(fixture.target)).toBe(false);
  expect(readFileSync(fixture.manifest)).toEqual(manifest);
  expect(readFileSync(join(fixture.directory, "Cargo.lock"))).toEqual(lock);
  const selected = fixture.check(["--list", "-p", "worker", "-p", "worker"]);
  passed(selected);
  expect(JSON.parse(String(selected.stdout))).toEqual(["worker"]);
  failed(
    fixture.check(["--list", "-p", "missing"]),
    "Unknown workspace package",
  );
}, 30_000);

test("affected roots include every transitive Cargo consumer regardless of dependency kind, feature or target", () => {
  const fixture = workspace();
  const declarations = {
    optional:
      '[dependencies]\nrenamed = { package = "worker", path = "../worker", optional = true }\n',
    target: `[target.'cfg(target_os = "none")'.dependencies]\nworker = { path = "../worker" }\n`,
    builder: '[build-dependencies]\nworker = { path = "../worker" }\n',
    transitive: '[dev-dependencies]\noptional = { path = "../optional" }\n',
    unrelated: "",
  };
  for (const [name, dependencies] of Object.entries(declarations)) {
    mkdirSync(join(fixture.directory, name, "src"), { recursive: true });
    fixture.write(
      `${name}/Cargo.toml`,
      `[package]\nname = "${name}"\nversion = "0.1.0"\nedition = "2024"\n${dependencies}`,
    );
    fixture.write(
      `${name}/src/lib.rs`,
      'compile_error!("discovery must not build");\n',
    );
  }
  fixture.write(
    "Cargo.toml",
    readFileSync(fixture.manifest, "utf8").replace(
      'members = ["worker"]',
      `members = ["worker", ${Object.keys(declarations)
        .map((name) => JSON.stringify(name))
        .join(", ")}]`,
    ) + '\n[dev-dependencies]\nworker = { path = "worker" }\n',
  );
  passed(fixture.cargo(["generate-lockfile", "--offline"]));
  const result = fixture.check(["--list", "--affected", '["worker","worker"]']);
  passed(result);
  expect(JSON.parse(String(result.stdout)).sort()).toEqual([
    "application",
    "builder",
    "optional",
    "target",
    "transitive",
    "worker",
  ]);
  const consumer = fixture.check(["--matrix", "--affected", '["optional"]']);
  passed(consumer);
  expect(
    JSON.parse(String(consumer.stdout))
      .include.map((entry: { package: string }) => entry.package)
      .sort(),
  ).toEqual(["optional", "transitive"]);
  const explicit = fixture.check(["--list", "--package", "worker"]);
  passed(explicit);
  expect(JSON.parse(String(explicit.stdout))).toEqual(["worker"]);
  // Cargo may cache rustc target information while inspecting target-qualified declarations.
  expect(existsSync(join(fixture.target, "debug"))).toBe(false);
  expect(existsSync(join(fixture.target, "doc"))).toBe(false);
}, 30_000);

test("excluded path intermediaries conservatively retain every workspace member for affected checks", () => {
  const fixture = workspace();
  for (const name of ["intermediary", "independent"]) {
    mkdirSync(join(fixture.directory, name, "src"), { recursive: true });
    fixture.write(
      `${name}/Cargo.toml`,
      `[package]\nname = "${name}"\nversion = "0.1.0"\nedition = "2024"\n` +
        (name === "intermediary"
          ? '\n[dependencies]\nworker = { path = "../worker" }\n'
          : ""),
    );
    fixture.write(
      `${name}/src/lib.rs`,
      'compile_error!("discovery must not build");\n',
    );
  }
  fixture.write(
    "Cargo.toml",
    readFileSync(fixture.manifest, "utf8").replace(
      'members = ["worker"]',
      'members = ["worker", "independent"]\nexclude = ["intermediary"]',
    ) + '\n[dependencies]\nintermediary = { path = "intermediary" }\n',
  );
  passed(fixture.cargo(["generate-lockfile", "--offline"]));
  const result = fixture.cargo([
    "metadata",
    "--format-version",
    "1",
    "--no-deps",
    "--locked",
  ]);
  passed(result);
  const metadata = JSON.parse(String(result.stdout));
  expect(
    metadata.packages.map((entry: { name: string }) => entry.name).sort(),
  ).toEqual(["application", "independent", "worker"]);
  const application = metadata.packages.find(
    (entry: { name: string }) => entry.name === "application",
  );
  expect(
    application.dependencies.some(
      (dependency: { name: string; path: string }) =>
        dependency.name === "intermediary" &&
        dependency.path === join(fixture.directory, "intermediary"),
    ),
  ).toBe(true);
  const complete = fixture.cargo([
    "metadata",
    "--format-version",
    "1",
    "--locked",
  ]);
  passed(complete);
  const intermediary = JSON.parse(String(complete.stdout)).packages.find(
    (entry: { name: string }) => entry.name === "intermediary",
  );
  expect(
    intermediary.dependencies.some(
      (dependency: { name: string; path: string }) =>
        dependency.name === "worker" &&
        dependency.path === join(fixture.directory, "worker"),
    ),
  ).toBe(true);
  for (const affected of ['["worker"]', '["independent"]', "all"]) {
    const matrix = fixture.check(["--matrix", "--affected", affected]);
    passed(matrix);
    expect(
      JSON.parse(String(matrix.stdout))
        .include.map((entry: { package: string }) => entry.package)
        .sort(),
    ).toEqual(["application", "independent", "worker"]);
  }
  const explicit = fixture.check(["--list", "--package", "worker"]);
  passed(explicit);
  expect(JSON.parse(String(explicit.stdout))).toEqual(["worker"]);
  expect(existsSync(join(fixture.target, "debug"))).toBe(false);
}, 30_000);

test("consumer-aware execution keeps independent build, tests, doctests and rustdoc for each selected member", () => {
  const fixture = workspace();
  fixture.write(
    "Cargo.toml",
    readFileSync(fixture.manifest, "utf8") +
      '\n[dev-dependencies]\nworker = { path = "worker" }\n',
  );
  passed(fixture.cargo(["generate-lockfile", "--offline"]));
  const result = fixture.check(["--affected", '["worker"]']);
  passed(result);
  for (const [name, directory] of [
    ["application", ""],
    ["worker", "worker"],
  ]) {
    for (const step of ["build", "test", "doc"])
      expect(result.stdout).toContain(`[${name}] ${step}\n`);
    expect(
      readFileSync(join(fixture.directory, directory, "tested"), "utf8"),
    ).toBe(name);
    expect(
      readFileSync(join(fixture.directory, directory, "doctested"), "utf8"),
    ).toBe(name);
    expect(existsSync(join(fixture.target, "doc", name, "index.html"))).toBe(
      true,
    );
  }
  expect(existsSync(join(fixture.target, "debug/application"))).toBe(true);
}, 30_000);

test("matrix discovery expands domain only when selected and validates its full test target coverage", () => {
  const fixture = workspace();
  fixture.write(
    "worker/Cargo.toml",
    readFileSync(join(fixture.directory, "worker/Cargo.toml"), "utf8").replace(
      'name = "worker"',
      'name = "domain"',
    ),
  );
  passed(fixture.cargo(["generate-lockfile", "--offline"]));
  const application = fixture.check([
    "--matrix",
    "--affected",
    '["application"]',
  ]);
  passed(application);
  expect(JSON.parse(String(application.stdout))).toEqual({
    include: [{ package: "application" }],
  });
  const domain = fixture.check(["--matrix", "--affected", '["domain"]']);
  passed(domain);
  const shards = ["1/4", "2/4", "3/4", "4/4"].map((shard) => ({
    package: "domain",
    shard,
  }));
  expect(JSON.parse(String(domain.stdout))).toEqual({ include: shards });
  const all = fixture.check(["--matrix", "--affected", "all"]);
  passed(all);
  expect(JSON.parse(String(all.stdout)).include).toHaveLength(5);
  mkdirSync(join(fixture.directory, "worker/tests"));
  fixture.write("worker/tests/new.rs", "#[test] fn acceptance() {}\n");
  failed(
    fixture.check(["--matrix", "--affected", '["domain"]']),
    "library-only package",
  );
  expect(existsSync(fixture.target)).toBe(false);
}, 30_000);

test("invalid affected selections and incompatible matrix options never succeed empty", () => {
  const fixture = workspace();
  for (const selection of [
    "",
    "[]",
    "{}",
    "null",
    '"worker"',
    "[1]",
    '[""]',
    '["missing"]',
    "ALL",
    '["worker",null]',
  ]) {
    for (const mode of [[], ["--list"], ["--matrix"]]) {
      const result = fixture.check([...mode, "--affected", selection]);
      expect(result.error).toBeUndefined();
      expect(result.status).toBe(1);
      expect(result.stdout).toBe("");
    }
  }
  for (const args of [
    ["--affected", "all", "--package", "worker"],
    ["--affected", '["worker"]', "--shard", "1/4"],
    ["--matrix", "--list"],
    ["--matrix", "--shard", "1/4", "--package", "worker"],
  ]) {
    const result = fixture.check(args);
    expect(result.error).toBeUndefined();
    expect(result.status).toBe(1);
    expect(result.stdout).toBe("");
  }
  expect(existsSync(fixture.target)).toBe(false);
}, 30_000);

test("path identity selects real workspace owners and registry name collisions add no edge", () => {
  const fixture = workspace();
  fixture.write(
    "worker-manifest.toml",
    readFileSync(join(fixture.directory, "worker/Cargo.toml")),
  );
  rmSync(join(fixture.directory, "worker/Cargo.toml"));
  symlinkSync(
    "../worker-manifest.toml",
    join(fixture.directory, "worker/Cargo.toml"),
  );
  const result = fixture.cargo([
    "metadata",
    "--format-version",
    "1",
    "--no-deps",
    "--locked",
  ]);
  passed(result);
  const metadata = JSON.parse(String(result.stdout));
  const application = metadata.packages.find(
    (entry: { name: string }) => entry.name === "application",
  );
  symlinkSync("worker", join(fixture.directory, "worker-link"));
  const commands = join(fixture.directory, "commands");
  mkdirSync(commands);
  writeFileSync(
    join(commands, "cargo"),
    `#!${process.execPath}\nprocess.stdout.write(process.env.TEST_METADATA);\n`,
  );
  chmodSync(join(commands, "cargo"), 0o700);
  const dependency = {
    name: "worker",
    kind: null,
    optional: false,
    target: null,
    source: "registry+https://example.invalid/index",
  };
  application.dependencies = [dependency];
  const registry = fixture.check(["--list", "--affected", '["worker"]'], {
    PATH: `${commands}:${process.env.PATH}`,
    TEST_METADATA: JSON.stringify(metadata),
  });
  passed(registry);
  expect(JSON.parse(String(registry.stdout))).toEqual(["worker"]);
  application.dependencies = [
    {
      ...dependency,
      name: "renamed",
      source: null,
      path: join(fixture.directory, "worker-link"),
    },
  ];
  const linked = fixture.check(["--list", "--affected", '["worker"]'], {
    PATH: `${commands}:${process.env.PATH}`,
    TEST_METADATA: JSON.stringify(metadata),
  });
  passed(linked);
  expect(JSON.parse(String(linked.stdout)).sort()).toEqual([
    "application",
    "worker",
  ]);
  expect(existsSync(fixture.target)).toBe(false);
}, 30_000);

test("missing or inconsistent Cargo metadata cannot publish a successful matrix", () => {
  const fixture = workspace();
  const result = fixture.cargo([
    "metadata",
    "--format-version",
    "1",
    "--no-deps",
    "--locked",
  ]);
  passed(result);
  const metadata = JSON.parse(String(result.stdout));
  const commands = join(fixture.directory, "commands");
  mkdirSync(commands);
  writeFileSync(
    join(commands, "cargo"),
    `#!${process.execPath}\nprocess.stdout.write(process.env.TEST_METADATA);\n`,
  );
  chmodSync(join(commands, "cargo"), 0o700);
  const invalid = [
    {},
    { ...metadata, workspace_members: [] },
    {
      ...metadata,
      workspace_members: [
        ...metadata.workspace_members,
        metadata.workspace_members[0],
      ],
    },
    { ...metadata, packages: metadata.packages.slice(1) },
    ...[
      { dependencies: undefined },
      { manifest_path: "relative/Cargo.toml" },
      { targets: [] },
      {
        dependencies: [
          {
            name: "worker",
            kind: null,
            optional: false,
            target: null,
            source: null,
          },
        ],
      },
      {
        dependencies: [
          {
            name: "worker",
            path: "relative",
            kind: null,
            optional: false,
            target: null,
            source: null,
          },
        ],
      },
    ].map((change) => ({
      ...metadata,
      packages: [
        { ...metadata.packages[0], ...change },
        ...metadata.packages.slice(1),
      ],
    })),
  ];
  for (const input of invalid) {
    const failure = fixture.check(["--matrix", "--affected", "all"], {
      PATH: `${commands}:${process.env.PATH}`,
      TEST_METADATA: JSON.stringify(input),
    });
    failed(failure, "Cargo metadata");
    expect(failure.stdout).toBe("");
  }
  expect(existsSync(fixture.target)).toBe(false);
}, 30_000);

test("invalid selections and options fail before any build starts", () => {
  const fixture = workspace();
  for (const args of [
    ["--package", "worker", "--package", "missing"],
    ["--package", ""],
    ["--package", "*"],
    ["--package"],
    ["--workspace"],
    ["--test-threads"],
    ...["", "0", "-1", "1.5", "2e1", " 2", "9007199254740992"].map((value) => [
      "--test-threads",
      value,
    ]),
  ]) {
    const result = fixture.check(args);
    expect(result.error).toBeUndefined();
    expect(result.status).toBe(1);
    expect(result.stdout).not.toContain("[worker] build");
    expect(existsSync(fixture.target)).toBe(false);
  }
}, 30_000);

test("test threads control actual harness concurrency while retaining doctests", () => {
  const fixture = workspace();
  fixture.write(
    "worker/src/lib.rs",
    `/// A package whose examples still execute with explicit test concurrency.
/// \`\`\`
/// std::fs::write(concat!(env!("CARGO_MANIFEST_DIR"), "/doctested"), "worker").expect("marker");
/// \`\`\`
pub fn value() {}

#[cfg(test)]
mod tests {
    use std::sync::{Condvar, Mutex};
    use std::time::Duration;

    static ARRIVED: Mutex<usize> = Mutex::new(0);
    static READY: Condvar = Condvar::new();

    fn rendezvous() {
        let mut arrived = ARRIVED.lock().expect("arrival counter");
        *arrived += 1;
        READY.notify_all();
        let (arrived, _) = READY.wait_timeout_while(arrived, Duration::from_secs(2), |count| *count < 2).expect("bounded rendezvous");
        let count = *arrived;
        drop(arrived);
        assert_eq!(count, 2, "both test workers must be active");
    }

    #[test]
    fn first() { rendezvous(); }

    #[test]
    fn second() { rendezvous(); }
}
`,
  );
  const sequential = fixture.check(["-p", "worker", "--test-threads", "1"], {
    RUST_TEST_THREADS: "2",
  });
  failed(sequential, "cargo test");
  expect(sequential.stdout).toContain("both test workers must be active");
  expect(existsSync(join(fixture.directory, "worker/doctested"))).toBe(false);
  const result = fixture.check(["-p", "worker", "--test-threads", "2"], {
    RUST_TEST_THREADS: "1",
  });
  passed(result);
  expect(result.stdout).toContain("2 passed; 0 failed");
  expect(
    readFileSync(join(fixture.directory, "worker/doctested"), "utf8"),
  ).toBe("worker");
  expect(existsSync(join(fixture.target, "doc/worker/index.html"))).toBe(true);
}, 30_000);

test.each([false, true])("independent build exposes features hidden by workspace and test builds (grouped=%s)", grouped => {
  const fixture = workspace();
  const name = grouped ? "nucleus" : "application";
  mkdirSync(join(fixture.directory, "helper/src"), { recursive: true });
  fixture.write(
    "helper/Cargo.toml",
    `[package]
name = "helper"
version = "0.1.0"
edition = "2024"
[features]
enabled = []
[workspace]
`,
  );
  fixture.write(
    "helper/src/lib.rs",
    '#[cfg(feature = "enabled")]\npub fn value() -> u8 { 1 }\n',
  );
  fixture.write(
    "Cargo.toml",
    readFileSync(fixture.manifest, "utf8").replace(
      'default-members = ["."]',
      'default-members = ["."]\nexclude = ["helper"]',
    ).replace('name = "application"', `name = "${name}"`) +
      `
[dependencies]
helper = { path = "helper" }
[dev-dependencies]
helper = { path = "helper", features = ["enabled"] }
`,
  );
  if (grouped) {
    fixture.write("src/main.rs", "fn main() {}\n");
    mkdirSync(join(fixture.directory, "tests"));
    fixture.write("tests/http.rs", "#[test] fn http() {}\n");
  }
  fixture.write(
    "worker/Cargo.toml",
    readFileSync(join(fixture.directory, "worker/Cargo.toml"), "utf8") +
      '\n[dependencies]\nhelper = { path = "../helper", features = ["enabled"] }\n',
  );
  fixture.write("src/lib.rs", "pub fn value() -> u8 { helper::value() }\n");
  passed(fixture.cargo(["generate-lockfile", "--offline"]));
  passed(fixture.cargo(["build", "--workspace", "--locked"]));
  passed(fixture.cargo(["test", "--package", name, "--locked"]));
  failed(
    fixture.check(grouped ? ["-p", "nucleus", "--group", "rest"] : ["-p", "application", "-p", "worker"]),
    "cannot find function `value`",
  );
  expect(existsSync(join(fixture.directory, "worker/tested"))).toBe(false);
  expect(existsSync(join(fixture.target, "doc", name))).toBe(false);
}, 30_000);

test("test failures stop documentation and later packages", () => {
  const fixture = workspace();
  fixture.write(
    "src/lib.rs",
    'pub fn value() -> u8 { 1 }\n#[test]\nfn fails() { panic!("intentional test failure"); }\n',
  );
  failed(fixture.check(["-p", "application", "-p", "worker"]), "cargo test");
  expect(existsSync(join(fixture.target, "doc/application"))).toBe(false);
  expect(existsSync(join(fixture.directory, "worker/tested"))).toBe(false);
}, 30_000);

test("rustdoc warnings fail even with inherited flags that allow warnings", () => {
  const fixture = workspace();
  fixture.write(
    "src/lib.rs",
    "/// [MissingDocumentationTarget]\n" +
      readFileSync(join(fixture.directory, "src/lib.rs"), "utf8"),
  );
  failed(
    fixture.check(["-p", "application", "-p", "worker"], {
      RUSTDOCFLAGS: "-A warnings",
      CARGO_ENCODED_RUSTDOCFLAGS: "-A\x1fwarnings",
    }),
    "unresolved link",
  );
  expect(existsSync(join(fixture.directory, "tested"))).toBe(true);
  expect(existsSync(join(fixture.directory, "worker/tested"))).toBe(false);
}, 30_000);

test("metadata errors and an empty workspace cannot report success", () => {
  const fixture = workspace();
  fixture.write("Cargo.toml", "invalid manifest");
  failed(fixture.check(), "cargo metadata");
  failed(fixture.check(["--list"]), "cargo metadata");
  fixture.write("Cargo.toml", '[workspace]\nresolver = "3"\nmembers = []\n');
  failed(
    fixture.check(),
    "Cargo metadata must identify every workspace member",
  );
  failed(
    fixture.check(["--list"]),
    "Cargo metadata must identify every workspace member",
  );
  expect(existsSync(fixture.target)).toBe(false);
}, 30_000);

test("a stale lockfile is rejected without updating it", () => {
  const fixture = workspace();
  fixture.write(
    "worker/Cargo.toml",
    readFileSync(join(fixture.directory, "worker/Cargo.toml"), "utf8").replace(
      'version = "0.1.0"',
      'version = "0.2.0"',
    ),
  );
  const lock = readFileSync(join(fixture.directory, "Cargo.lock"));
  failed(fixture.check(), "--locked");
  expect(readFileSync(join(fixture.directory, "Cargo.lock"))).toEqual(lock);
}, 30_000);

test("shards execute a disjoint union of the complete library list and keep doctests and rustdoc in shard one", () => {
  const fixture = workspace();
  const names = Array.from({ length: 12 }, (_, index) => `test_${index}`);
  fixture.write(
    "worker/src/lib.rs",
    `
/// A doctest that fails if executed more than once.
/// \`\`\`
/// std::fs::File::create_new(concat!(env!("CARGO_MANIFEST_DIR"), "/doctested")).unwrap();
/// \`\`\`
pub fn value() {}
#[cfg(test)]
fn record(name: &str) {
    std::fs::File::create_new(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(name)).unwrap();
}
${names.map((name) => `#[test] fn ${name}() { record("${name}"); }`).join("\n")}
mod nested { #[test] fn test_1() { super::record("nested"); } }
#[test] #[ignore] fn ignored() { panic!("must remain ignored"); }
#[test] #[should_panic] fn expected_panic() { panic!("expected"); }
`,
  );
  const listed = fixture.cargo([
    "test",
    "-p",
    "worker",
    "--locked",
    "--lib",
    "--",
    "--list",
  ]);
  passed(listed);
  const all = [...listed.stdout.matchAll(/^(.+): test$/gm)]
    .map((match) => match[1])
    .sort();
  expect(all).toEqual(
    [...names, "nested::test_1", "ignored", "expected_panic"].sort(),
  );
  const executed: string[] = [];
  const counts: number[] = [];
  const fingerprints = new Set<string>();
  // Non-owning shards run first so they cannot borrow a build or doc result.
  for (const index of [3, 2, 1]) {
    const result = fixture.check([
      "-p",
      "worker",
      "--shard",
      `${index}/3`,
      "--test-threads",
      "2",
    ]);
    passed(result);
    const tests = [
      ...result.stdout.matchAll(
        /^test (\S+)(?: - should panic)? \.\.\. (?:ok|ignored.*)$/gm,
      ),
    ].map((match) => match[1]);
    executed.push(...tests);
    counts.push(tests.length);
    expect(result.stdout).toContain(
      `shard ${index}/3: ${tests.length}/${all.length} library tests`,
    );
    fingerprints.add(
      result.stdout.match(/test list SHA-256: ([a-f0-9]{64})/)![1],
    );
    for (const step of ["build", "doctest", "doc"]) {
      expect(result.stdout.includes(`[worker] ${step}\n`)).toBe(index === 1);
    }
    expect(existsSync(join(fixture.directory, "worker/doctested"))).toBe(
      index === 1,
    );
    expect(existsSync(join(fixture.target, "doc/worker/index.html"))).toBe(
      index === 1,
    );
  }
  expect(executed.sort()).toEqual(all);
  expect(new Set(executed).size).toBe(all.length);
  expect(counts.reduce((sum, count) => sum + count, 0)).toBe(all.length);
  expect(fingerprints.size).toBe(1);
  for (const name of [...names, "nested"]) {
    expect(existsSync(join(fixture.directory, "worker", name))).toBe(true);
  }
  expect(existsSync(join(fixture.directory, "tested"))).toBe(false);
}, 30_000);

test("invalid shard options and unsupported targets fail before building", () => {
  const fixture = workspace();
  for (const value of [
    "",
    "0/3",
    "1/0",
    "4/3",
    "-1/3",
    "1.5/3",
    "1/2e1",
    "01/3",
    "1/9007199254740992",
    "1",
    "1/2/3",
  ]) {
    failed(fixture.check(["-p", "worker", "--shard", value]), "--shard");
  }
  for (const args of [
    [],
    ["-p", "worker", "-p", "application"],
    ["-p", "worker", "--list"],
  ]) {
    failed(fixture.check([...args, "--shard", "1/3"]), "one explicit package");
  }
  failed(
    fixture.check(["-p", "application", "--shard", "1/3"]),
    "library-only package",
  );
  mkdirSync(join(fixture.directory, "worker/tests"));
  fixture.write("worker/tests/new.rs", "#[test] fn newly_added() {}\n");
  failed(
    fixture.check(["-p", "worker", "--shard", "1/3"]),
    "library-only package",
  );
  rmSync(join(fixture.directory, "worker/tests"), { recursive: true });
  fixture.write(
    "worker/Cargo.toml",
    readFileSync(join(fixture.directory, "worker/Cargo.toml"), "utf8") +
      "\n[lib]\ntest = false\n",
  );
  failed(
    fixture.check(["-p", "worker", "--shard", "1/3"]),
    "library-only package",
  );
  expect(existsSync(fixture.target)).toBe(false);
}, 30_000);

test("empty hash buckets do not run an unfiltered suite and excessive shard counts fail", () => {
  const fixture = workspace();
  // Both names hash to the first of two buckets.
  fixture.write(
    "worker/src/lib.rs",
    '#[test] fn first() { panic!("unselected"); }\n#[test] fn second() { panic!("unselected"); }\n',
  );
  const result = fixture.check(["-p", "worker", "--shard", "2/2"]);
  passed(result);
  expect(result.stdout).toContain("shard 2/2: 0/2 library tests");
  expect(result.stdout).not.toContain("running");
  failed(
    fixture.check(["-p", "worker", "--shard", "2/3"]),
    "Shard count exceeds",
  );
}, 30_000);

test("a failing test fails its shard and prevents that shard's later checks", () => {
  const fixture = workspace();
  fixture.write(
    "worker/src/lib.rs",
    '#[test] fn fails() { panic!("shard failure"); }\n#[test] fn passes() {}\n',
  );
  const failure = fixture.check(["-p", "worker", "--shard", "1/2"]);
  failed(failure, "cargo test");
  expect(failure.stdout).toContain("shard failure");
  expect(failure.stdout).not.toContain("[worker] doctest");
  expect(existsSync(join(fixture.target, "doc/worker"))).toBe(false);
  const success = fixture.check(["-p", "worker", "--shard", "2/2"]);
  passed(success);
  expect(success.stdout).toContain("1 passed; 0 failed");
}, 30_000);

test.each([
  [
    '#[cfg(not(test))] compile_error!("delivery rejected");\n#[test] fn works() {}',
    "delivery rejected",
  ],
  [
    '/// ```\n/// panic!("doctest rejected");\n/// ```\npub fn value() {}\n#[test] fn works() {}',
    "doctest failed",
  ],
  [
    "/// [MissingDocumentationTarget]\npub fn value() {}\n#[test] fn works() {}",
    "unresolved link",
  ],
])(
  "shard one retains build, doctest and fatal rustdoc checks: %s",
  (source, diagnostic) => {
    const fixture = workspace();
    fixture.write("worker/src/lib.rs", source);
    failed(
      fixture.check(["-p", "worker", "--shard", "1/1"], {
        RUSTDOCFLAGS: "-A warnings",
        CARGO_ENCODED_RUSTDOCFLAGS: "-A\x1fwarnings",
      }),
      diagnostic,
    );
  },
  30_000,
);

test.each([
  "first: test\nfirst: test\n\n2 tests, 0 benchmarks",
  "first: test\n\n2 tests, 0 benchmarks",
  "first: test\nsecond: benchmark\n\n1 test, 1 benchmark",
  "unknown output",
])(
  "unrecognized or inconsistent test listings fail closed: %s",
  (listing) => {
    const fixture = workspace();
    fixture.write(
      "worker/Cargo.toml",
      readFileSync(join(fixture.directory, "worker/Cargo.toml"), "utf8") +
        "\n[lib]\nharness = false\n",
    );
    fixture.write(
      "worker/src/lib.rs",
      `fn main() { println!("{}", ${JSON.stringify(listing)}); }\n`,
    );
    const result = fixture.check(["-p", "worker", "--shard", "2/2"]);
    failed(result, "Test listing");
    expect(result.stdout).not.toContain("Package shard checks passed");
  },
  30_000,
);
