import { afterEach, expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
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

test("invalid selections and options fail before any build starts", () => {
  const fixture = workspace();
  for (const args of [
    ["--package", "worker", "--package", "missing"],
    ["--package", ""],
    ["--package", "*"],
    ["--package"],
    ["--workspace"],
  ]) {
    const result = fixture.check(args);
    expect(result.error).toBeUndefined();
    expect(result.status).toBe(1);
    expect(result.stdout).not.toContain("[worker] build");
    expect(existsSync(fixture.target)).toBe(false);
  }
}, 30_000);

test("independent build exposes features hidden by workspace and test builds", () => {
  const fixture = workspace();
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
    ) +
      `
[dependencies]
helper = { path = "helper" }
[dev-dependencies]
helper = { path = "helper", features = ["enabled"] }
`,
  );
  fixture.write(
    "worker/Cargo.toml",
    readFileSync(join(fixture.directory, "worker/Cargo.toml"), "utf8") +
      '\n[dependencies]\nhelper = { path = "../helper", features = ["enabled"] }\n',
  );
  fixture.write("src/lib.rs", "pub fn value() -> u8 { helper::value() }\n");
  passed(fixture.cargo(["generate-lockfile", "--offline"]));
  passed(fixture.cargo(["build", "--workspace", "--locked"]));
  passed(fixture.cargo(["test", "--package", "application", "--locked"]));
  failed(
    fixture.check(["-p", "application", "-p", "worker"]),
    "cannot find function `value`",
  );
  expect(existsSync(join(fixture.directory, "worker/tested"))).toBe(false);
  expect(existsSync(join(fixture.target, "doc/application"))).toBe(false);
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
  fixture.write("Cargo.toml", '[workspace]\nresolver = "3"\nmembers = []\n');
  failed(
    fixture.check(),
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
