import { afterEach, expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import {
  appendFileSync,
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
import { basename, join } from "node:path";
import { fileURLToPath } from "node:url";
import { checkDependencies } from "./check-dependencies";

const script = fileURLToPath(
  new URL("./check-dependencies.ts", import.meta.url),
);
const toolchain = readFileSync(
  new URL("../rust-toolchain.toml", import.meta.url),
);
const directories: string[] = [];

afterEach(() => {
  for (const directory of directories.splice(0))
    rmSync(directory, { recursive: true, force: true });
});

function fixture() {
  // Exercise argv handling and the tree's final | separator in every real workspace.
  const directory = mkdtempSync(join(tmpdir(), "nucleus dependencies | "));
  directories.push(directory);
  const workspace = join(directory, "workspace");
  const manifest = join(workspace, "Cargo.toml");
  const target = join(directory, "cargo-output");
  const env = {
    ...process.env,
    CARGO_NET_OFFLINE: "true",
    CARGO_TARGET_DIR: target,
  };
  function packageSource(path: string, name: string, contents = "") {
    mkdirSync(join(path, "src"), { recursive: true });
    writeFileSync(join(path, "src/lib.rs"), "");
    writeFileSync(
      join(path, "Cargo.toml"),
      `[package]\nname = "${name}"\nversion = "0.1.0"\nedition = "2024"\n${contents}`,
    );
  }
  function append(name: string, contents: string) {
    appendFileSync(join(workspace, name, "Cargo.toml"), contents);
  }
  function external(name: string, contents = "") {
    const path = join(directory, name);
    packageSource(path, name, contents);
    return path;
  }
  mkdirSync(workspace);
  writeFileSync(join(workspace, "rust-toolchain.toml"), toolchain);
  writeFileSync(
    manifest,
    '[workspace]\nresolver = "3"\nmembers = ["nucleus", "domain", "platform", "deployment", "fixture"]\n',
  );
  for (const name of ["nucleus", "domain", "platform", "deployment", "fixture"])
    packageSource(join(workspace, name), name);
  append(
    "nucleus",
    '[dependencies]\ndomain = { path = "../domain" }\nplatform = { path = "../platform" }\n',
  );
  append(
    "domain",
    '[dependencies]\nplatform = { path = "../platform" }\n[features]\ntest-support = []\n',
  );
  append(
    "deployment",
    '[dependencies]\ndomain = { path = "../domain" }\nplatform = { path = "../platform" }\n',
  );
  for (const name of ["platform", "domain", "deployment"])
    append(name, '[dev-dependencies]\nfixture = { path = "../fixture" }\n');
  append(
    "nucleus",
    '[dev-dependencies]\nfixture = { path = "../fixture" }\ndomain = { path = "../domain", features = ["test-support"] }\n',
  );

  function inputs() {
    return new Map(
      readdirSync(directory, { recursive: true, encoding: "utf8" })
        .filter((path) => basename(path).startsWith("Cargo."))
        .map((path) => [path, readFileSync(join(directory, path))]),
    );
  }
  function check(
    options: {
      lock?: boolean;
      manifest?: string | null;
      script?: string;
      args?: string[];
      env?: NodeJS.ProcessEnv;
    } = {},
  ) {
    if (options.lock !== false) {
      const locked = spawnSync(
        "cargo",
        ["generate-lockfile", "--manifest-path", manifest, "--offline"],
        { cwd: workspace, env, encoding: "utf8", timeout: 30_000 },
      );
      expect(locked.error).toBeUndefined();
      expect(locked.status, locked.stderr).toBe(0);
    }
    const before = inputs();
    const result = spawnSync(
      process.execPath,
      [
        options.script ?? script,
        ...(options.manifest === null
          ? []
          : ["--manifest-path", options.manifest ?? manifest]),
        ...(options.args ?? []),
      ],
      {
        cwd: directory,
        env: { ...env, ...options.env },
        encoding: "utf8",
        maxBuffer: Infinity,
        timeout: 30_000,
        killSignal: "SIGKILL",
      },
    );
    expect(inputs()).toEqual(before);
    expect(
      readdirSync(directory, { recursive: true, encoding: "utf8" }).filter(
        (path) => basename(path) === "target",
      ),
    ).toEqual([]);
    expect(existsSync(target)).toBe(false);
    expect(result.error).toBeUndefined();
    return result;
  }
  function stub(source: string) {
    const bin = join(directory, "bin");
    mkdirSync(bin, { recursive: true });
    writeFileSync(join(bin, "cargo"), `#!${process.execPath}\n${source}`, {
      mode: 0o700,
    });
    return { PATH: `${bin}:${process.env.PATH}` };
  }
  function metadata() {
    return {
      workspace_root: workspace,
      workspace_members: ["nucleus-id"],
      packages: [{ id: "nucleus-id", name: "nucleus", dependencies: [] }],
    };
  }
  function inspect(data: unknown = metadata(), tree = "nucleus v0.1.0|\n") {
    return stub(`process.stdout.write(process.argv[2] === "metadata"
      ? ${JSON.stringify(JSON.stringify(data))} : ${JSON.stringify(tree)});`);
  }
  return {
    directory,
    workspace,
    manifest,
    append,
    external,
    packageSource,
    check,
    stub,
    metadata,
    inspect,
  };
}

function passed(result: ReturnType<typeof spawnSync>) {
  expect(result.status, String(result.stderr)).toBe(0);
  expect(String(result.stdout)).toContain("Dependency boundaries passed");
}

function rejected(result: ReturnType<typeof spawnSync>, diagnostic: string) {
  expect(result.status, String(result.stderr)).toBe(1);
  expect(String(result.stderr)).toContain(diagnostic);
  expect(String(result.stderr)).toContain(
    "docs/engineering/rust.md#dependency-boundaries",
  );
  expect(String(result.stdout)).not.toContain("Dependency boundaries passed");
}

function incomplete(
  result: ReturnType<typeof spawnSync>,
  diagnostic = "Dependency check could not complete",
) {
  expect(result.status, String(result.stderr)).toBe(2);
  expect(String(result.stderr)).toContain(diagnostic);
  expect(String(result.stdout)).not.toContain("Dependency boundaries passed");
}

test("current directions and development features are allowed", () => {
  const f = fixture();
  f.external("rcgen");
  f.external("refinery");
  f.append("fixture", '[dependencies]\nrcgen = { path = "../../rcgen" }\n');
  f.append("deployment", '[dependencies.refinery]\npath = "../../refinery"\n');
  passed(f.check());
}, 30_000);

test("normal test dependencies are rejected", () => {
  const f = fixture();
  f.append("nucleus", '[dependencies.fixture]\npath = "../fixture"\n');
  rejected(f.check(), "nucleus normal dependency: fixture crosses");
}, 30_000);

test("build test dependencies are rejected", () => {
  const f = fixture();
  f.append(
    "domain",
    '[build-dependencies]\nfixture = { path = "../fixture" }\n',
  );
  rejected(f.check(), "domain build dependency: fixture crosses");
}, 30_000);

test("transitive build dependencies are rejected without executing build scripts", () => {
  const f = fixture();
  f.external("rcgen");
  const helper = f.external(
    "helper",
    '[build-dependencies]\nrcgen = { path = "../rcgen" }\n',
  );
  writeFileSync(
    join(helper, "build.rs"),
    'fn main() { panic!("must never execute"); }\n',
  );
  f.append("nucleus", '[dependencies.helper]\npath = "../../helper"\n');
  rejected(f.check(), "nucleus production graph: rcgen crosses");
}, 30_000);

test("dormant optional test tools are rejected", () => {
  const f = fixture();
  f.external("sqlx");
  f.append(
    "nucleus",
    '[dependencies.sqlx]\npath = "../../sqlx"\noptional = true\n',
  );
  rejected(f.check(), "nucleus normal dependency: sqlx crosses");
}, 30_000);

test("non-host transitive test dependencies are rejected", () => {
  const f = fixture();
  f.external("rcgen");
  f.external(
    "helper",
    '[target.\'cfg(target_os = "none")\'.dependencies]\nrcgen = { path = "../rcgen" }\n',
  );
  f.append("nucleus", '[dependencies.helper]\npath = "../../helper"\n');
  rejected(f.check(), "nucleus production graph: rcgen crosses");
}, 30_000);

test("renamed migration dependencies are rejected", () => {
  const f = fixture();
  f.append(
    "nucleus",
    '[dependencies.maintenance]\npackage = "deployment"\npath = "../deployment"\n',
  );
  rejected(f.check(), "nucleus normal dependency: deployment crosses");
}, 30_000);

test("deployment cannot depend on the online entrypoint", () => {
  const f = fixture();
  f.append("deployment", '[dependencies.nucleus]\npath = "../nucleus"\n');
  rejected(f.check(), "deployment normal dependency: nucleus crosses");
}, 30_000);

test("fixture cannot depend on business code", () => {
  const f = fixture();
  f.append("fixture", '[dependencies]\ndomain = { path = "../domain" }\n');
  rejected(f.check(), "fixture normal dependency: domain crosses");
}, 30_000);

test("direct test-support features are rejected", () => {
  const f = fixture();
  f.append(
    "deployment",
    '[build-dependencies]\ndomain = { path = "../domain", features = ["test-support"] }\n',
  );
  rejected(
    f.check(),
    "deployment build dependency: domain/test-support belongs only in dev-dependencies",
  );
}, 30_000);

test("default feature forwarding is rejected", () => {
  const f = fixture();
  f.append(
    "nucleus",
    '[features]\ndefault = ["testing"]\ntesting = ["domain/test-support"]\n',
  );
  rejected(
    f.check(),
    "nucleus production graph: domain/test-support is enabled",
  );
}, 30_000);

test("domain's own default test feature is rejected", () => {
  const f = fixture();
  const manifest = join(f.workspace, "domain/Cargo.toml");
  writeFileSync(
    manifest,
    readFileSync(manifest, "utf8").replace(
      "test-support = []",
      'test-support = []\ndefault = ["test-support"]',
    ),
  );
  rejected(
    f.check(),
    "domain production graph: domain/test-support is enabled",
  );
}, 30_000);

test("crypto providers are rejected even in development dependencies", () => {
  const f = fixture();
  for (const name of ["aws-lc-rs", "aws-lc-sys"]) {
    f.external(name);
    f.append("nucleus", `[dev-dependencies.${name}]\npath = "../../${name}"\n`);
  }
  const result = f.check();
  for (const name of ["aws-lc-rs", "aws-lc-sys"])
    rejected(result, `Cargo.lock: forbidden crypto provider ${name}`);
}, 30_000);

test.each(["unknown", "__proto__"])(
  "new member %s needs an explicit boundary",
  (name) => {
    const f = fixture();
    f.packageSource(join(f.workspace, name), name);
    writeFileSync(
      f.manifest,
      readFileSync(f.manifest, "utf8").replace(
        '"fixture"]',
        `"fixture", "${name}"]`,
      ),
    );
    rejected(f.check(), `${name}: define its architecture dependency boundary`);
  },
  30_000,
);

test("missing lockfiles fail without creating one", () => {
  const f = fixture();
  incomplete(f.check({ lock: false }));
  expect(existsSync(join(f.workspace, "Cargo.lock"))).toBe(false);
}, 30_000);

test("Cargo failures do not report success", () => {
  const f = fixture();
  passed(f.check());
  f.append("nucleus", '[dependencies.missing]\npath = "../missing"\n');
  incomplete(
    f.check({ lock: false }),
    "Dependency check could not inspect Cargo inputs",
  );
}, 30_000);

test("a manifest symlink uses the real workspace and its toolchain", () => {
  const f = fixture();
  const linked = join(f.directory, "Cargo.toml");
  symlinkSync(f.manifest, linked);
  passed(f.check({ manifest: linked }));
  const env = f.stub(`
    if (process.cwd() !== ${JSON.stringify(f.workspace)}) process.exit(91);
    if (process.argv[process.argv.indexOf("--manifest-path") + 1] !== ${JSON.stringify(f.manifest)}) process.exit(92);
    process.stdout.write(process.argv[2] === "metadata" ? ${JSON.stringify(JSON.stringify(f.metadata()))} : "nucleus v0.1.0|\\n");`);
  passed(f.check({ manifest: linked, env }));
}, 30_000);

test("parent segments are resolved after directory symlinks", () => {
  const f = fixture();
  const linked = join(f.directory, "linked-domain");
  symlinkSync(join(f.workspace, "domain"), linked, "dir");
  passed(f.check({ manifest: `${linked}/../Cargo.toml` }));
}, 30_000);

test.each([false, true])(
  "symlink targets with parent segments preserve filesystem lookup (relative: %j)",
  (relative) => {
    const f = fixture();
    symlinkSync(
      join(f.workspace, "platform"),
      join(f.directory, "jump"),
      "dir",
    );
    symlinkSync("jump/../domain", join(f.directory, "linked-domain"), "dir");
    const manifest = "linked-domain/../Cargo.toml";
    passed(
      f.check({ manifest: relative ? manifest : `${f.directory}/${manifest}` }),
    );
  },
  30_000,
);

test("a manifest symlink can traverse another symlink before a parent segment", () => {
  const f = fixture();
  symlinkSync(join(f.workspace, "domain"), join(f.directory, "jump"), "dir");
  const manifest = join(f.directory, "linked-manifest.toml");
  symlinkSync("jump/../Cargo.toml", manifest);
  passed(f.check({ manifest }));
}, 30_000);

test("missing manifests remain Cargo input failures", () => {
  const f = fixture();
  incomplete(
    f.check({ manifest: "workspace/missing.toml" }),
    "Dependency check could not inspect Cargo inputs",
  );
}, 30_000);

test("missing manifest directories remain startup failures", () => {
  const f = fixture();
  incomplete(f.check({ manifest: "absent/Cargo.toml" }));
}, 30_000);

test("the default manifest follows the physical script when started from another directory", () => {
  const f = fixture();
  const linked = join(f.directory, "linked-check.ts");
  symlinkSync(script, linked);
  const actual = fileURLToPath(new URL("../Cargo.toml", import.meta.url));
  const env = f.stub(`
    if (process.argv[process.argv.indexOf("--manifest-path") + 1] !== ${JSON.stringify(actual)}) process.exit(91);
    process.stdout.write(process.argv[2] === "metadata" ? ${JSON.stringify(JSON.stringify(f.metadata()))} : "nucleus v0.1.0|\\n");`);
  passed(f.check({ manifest: null, script: linked, env }));
}, 30_000);

test("CRLF trees use the last separator and deduplicate sorted diagnostics", () => {
  const f = fixture();
  const tree =
    "nucleus v0.1.0 (/path with | separator)|\r\n" +
    "rcgen v0.1.0|\r\nrcgen v0.1.0| (*)\r\n" +
    "domain v0.1.0|test-support (*)\r\ndomain v0.1.0|test-support\r\n";
  const result = f.check({ env: f.inspect(f.metadata(), tree) });
  rejected(result, "nucleus production graph: domain/test-support is enabled");
  expect(result.stderr.trim().split("\n")).toEqual([
    "nucleus production graph: domain/test-support is enabled",
    "nucleus production graph: rcgen crosses the production boundary",
    "See docs/engineering/rust.md#dependency-boundaries and docs/architecture.md#crate-与模块组织.",
  ]);
}, 30_000);

test.each([
  "\r",
  "\v",
  "\f",
  "\u001c",
  "\u001d",
  "\u001e",
  "\u0085",
  "\u2028",
  "\u2029",
])(
  "Python line separator %j preserves each production node",
  (separator) => {
    const f = fixture();
    const tree = `nucleus v0.1.0|${separator}domain v0.1.0|test-support${separator}`;
    rejected(
      f.check({ env: f.inspect(f.metadata(), tree) }),
      "nucleus production graph: domain/test-support is enabled",
    );
  },
  30_000,
);

test("production graphs larger than Node's default buffer remain inspectable", () => {
  const f = fixture();
  const tree = "nucleus v0.1.0|\n" + "platform v0.1.0|\n".repeat(80_000);
  expect(Buffer.byteLength(tree)).toBeGreaterThan(1024 * 1024);
  passed(f.check({ env: f.inspect(f.metadata(), tree) }));
}, 30_000);

test.each(["", "nucleus v0.1.0\n", "|default\n", "nucleus v0.1.0|\n\n"])(
  "malformed or empty tree %j fails closed",
  (tree) => {
    const f = fixture();
    incomplete(f.check({ env: f.inspect(f.metadata(), tree) }));
  },
  30_000,
);

test.each([
  "[[package]\n",
  "",
  "package = []\n",
  'package = "wrong"\n',
  '[[package]]\nversion = "1"\n',
])(
  "invalid lockfile %j fails closed",
  (lockfile) => {
    const f = fixture();
    writeFileSync(join(f.workspace, "Cargo.lock"), lockfile);
    incomplete(f.check({ lock: false, env: f.inspect() }));
  },
  30_000,
);

test("malformed JSON is an inspection failure", () => {
  const f = fixture();
  incomplete(f.check({ env: f.stub('process.stdout.write("{ broken");') }));
}, 30_000);

test("unknown package diagnostics retain Unicode code-point order", () => {
  const f = fixture();
  const data = {
    workspace_root: f.workspace,
    workspace_members: ["first", "second"],
    packages: [
      { id: "first", name: "𝒜", dependencies: [] },
      { id: "second", name: "界", dependencies: [] },
    ],
  };
  const result = f.check({ env: f.inspect(data) });
  rejected(result, "界: define its architecture dependency boundary");
  expect(result.stderr.trim().split("\n").slice(0, 2)).toEqual([
    "界: define its architecture dependency boundary before adding a workspace member",
    "𝒜: define its architecture dependency boundary before adding a workspace member",
  ]);
}, 30_000);

test.each([
  null,
  { packages: [], workspace_members: [], workspace_root: "/tmp" },
  { packages: [], workspace_members: ["missing"], workspace_root: "/tmp" },
  { packages: [], workspace_members: [null], workspace_root: "/tmp" },
])(
  "incomplete metadata %j fails closed",
  (data) => {
    const f = fixture();
    incomplete(f.check({ env: f.inspect(data) }));
  },
  30_000,
);

test.each([
  "id",
  "name",
  "dependencies",
  "workspace_root",
  "workspace_members",
])(
  "missing metadata field %s fails closed",
  (field) => {
    const f = fixture();
    const data = f.metadata();
    if (field === "workspace_root" || field === "workspace_members")
      Reflect.deleteProperty(data, field);
    else Reflect.deleteProperty(data.packages[0], field);
    incomplete(f.check({ env: f.inspect(data) }));
  },
  30_000,
);

test.each([
  { name: "domain", target: null, features: [] },
  { name: "domain", kind: null, features: [] },
  { name: "domain", kind: null, target: null },
  { name: "domain", kind: null, target: null, features: "test-support" },
  { name: "domain", kind: "dev", target: null, features: [null] },
])(
  "invalid dependency metadata %j fails closed",
  (dependency) => {
    const f = fixture();
    const data = {
      ...f.metadata(),
      packages: [
        { id: "nucleus-id", name: "nucleus", dependencies: [dependency] },
      ],
    };
    incomplete(f.check({ env: f.inspect(data) }));
  },
  30_000,
);

test("missing members cannot turn a partial inspection into success", () => {
  const f = fixture();
  const data = f.metadata();
  data.workspace_members.push("missing-id");
  incomplete(f.check({ env: f.inspect(data) }));
}, 30_000);

test("a Cargo startup failure is distinguished from rejected inputs", () => {
  const f = fixture();
  const empty = join(f.directory, "no-executables");
  mkdirSync(empty);
  incomplete(f.check({ env: { PATH: empty } }));
}, 30_000);

test("invalid UTF-8 from Cargo cannot become a clean inspection", () => {
  const f = fixture();
  incomplete(
    f.check({ env: f.stub("process.stdout.write(Buffer.from([255]));") }),
  );
}, 30_000);

test("Cargo uses a 120-second deadline and its timeout kills and reaps the child", () => {
  const f = fixture();
  let child: ReturnType<typeof spawnSync> | undefined;
  expect(() =>
    checkDependencies(f.manifest, (_args, options) => {
      expect(options.timeout).toBe(120_000);
      expect(options.maxBuffer).toBe(Infinity);
      expect(options.killSignal).toBe("SIGKILL");
      const result = spawnSync(
        process.execPath,
        ["-e", "setInterval(() => {}, 1000);"],
        { ...options, timeout: 500 },
      );
      child = result;
      return result;
    }),
  ).toThrow();
  expect(child?.error).toMatchObject({ code: "ETIMEDOUT" });
  expect(child?.signal).toBe("SIGKILL");
  const pid = child?.pid ?? 0;
  expect(pid).toBeGreaterThan(0);
  expect(() => process.kill(pid, 0)).toThrow();
}, 30_000);

test.each([
  { args: ["--unknown"] },
  { args: ["unexpected"] },
  { args: ["--manifest-path"] },
])(
  "CLI misuse %j returns usage failure without inspecting Cargo",
  ({ args }) => {
    const f = fixture();
    incomplete(f.check({ args: [...args], env: f.stub("process.exit(97);") }));
  },
  30_000,
);

test("help succeeds without inspecting Cargo", () => {
  const f = fixture();
  const result = f.check({
    args: ["--help"],
    env: f.stub("process.exit(97);"),
  });
  expect(result.status).toBe(0);
  expect(result.stdout).toContain("--manifest-path");
  expect(result.stdout).not.toContain("Dependency boundaries passed");
}, 30_000);
