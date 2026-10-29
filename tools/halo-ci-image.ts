import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  realpathSync,
  rmSync,
  symlinkSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { pinnedBun } from "./check-bun";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const read = (path: string) => readFileSync(resolve(root, path), "utf8");

export function imageInputs(readInput: (path: string) => string = read) {
  const read = readInput;
  const rust = Bun.TOML.parse(read("rust-toolchain.toml")) as {
    toolchain: { channel: string; components: string[]; profile: string };
  };
  const toolVersions = read(".tool-versions");
  const bun = pinnedBun(toolVersions);
  const nodeLines = toolVersions
    .split("\n")
    .filter((line) => /^\s*nodejs/.test(line));
  const node =
    nodeLines.length === 1
      ? nodeLines[0]!.match(/^nodejs (\d+\.\d+\.\d+)$/)?.[1]
      : undefined;
  const postgresql = read("fixture/postgresql.version").trim();
  const manifest = JSON.parse(read("halo/package.json"));
  const playwright = manifest.devDependencies["@playwright/test"] as string;
  for (const version of [rust.toolchain.channel, bun, node, playwright]) {
    if (!version || !/^\d+\.\d+\.\d+$/.test(version)) {
      throw new Error(
        "Halo CI requires exact Rust, Bun, Node and Playwright versions",
      );
    }
  }
  if (manifest.packageManager !== `bun@${bun}`) {
    throw new Error("Halo packageManager must match the pinned Bun version");
  }
  if (
    typeof manifest.engines?.node !== "string" ||
    !Bun.semver.satisfies(node!, manifest.engines.node)
  ) {
    throw new Error("The pinned Node version must satisfy Halo engines.node");
  }
  if (!/^\d+\.\d+$/.test(postgresql) || rust.toolchain.profile !== "minimal") {
    throw new Error("Unsupported Halo CI PostgreSQL version or Rust profile");
  }
  const dockerfile = read(".github/halo-ci/Dockerfile");
  if (
    !dockerfile.includes(
      `FROM mcr.microsoft.com/playwright:v${playwright}-noble@sha256:`,
    )
  ) {
    throw new Error(
      "Update the pinned Playwright base image digests before rebuilding Halo CI",
    );
  }
  const hash = createHash("sha256");
  for (const path of [
    ".github/halo-ci/Dockerfile",
    ".github/halo-ci/Dockerfile.dockerignore",
    ".tool-versions",
    "rust-toolchain.toml",
    "fixture/postgresql.ts",
    "fixture/postgresql.version",
    "tools/halo-ci-image.ts",
    "tools/check-bun.ts",
  ]) {
    hash.update(path).update("\0").update(read(path)).update("\0");
  }
  hash.update(playwright).update("\0").update(manifest.engines.node);
  return {
    bun,
    node,
    playwright,
    postgresql,
    rust: rust.toolchain,
    digest: hash.digest("hex"),
  };
}

function run(command: string, args: string[]) {
  const result = spawnSync(command, args, { cwd: root, stdio: "inherit" });
  if (result.error) throw result.error;
  if (result.status !== 0)
    throw new Error(`${command} failed (${result.signal ?? result.status})`);
}

function verifyImage() {
  const inputs = imageInputs();
  if (process.getuid?.() === 0)
    throw new Error("Halo CI must run as a non-root user for initdb");
  if (
    readFileSync("/etc/nucleus-halo-ci-inputs", "utf8").trim() !== inputs.digest
  ) {
    throw new Error(
      "Halo CI image inputs changed; rebuild, publish and pin the new digest",
    );
  }
  if (Bun.version !== inputs.bun)
    throw new Error("Halo CI Bun version mismatch");
  const node = spawnSync("node", ["--version"], { encoding: "utf8" });
  if (
    node.error ||
    node.status !== 0 ||
    node.stdout.trim() !== `v${inputs.node}`
  ) {
    throw new Error(`Halo CI requires Node ${inputs.node}`);
  }
  run("rustup", ["show", "active-toolchain"]);
  const compiler = spawnSync("rustc", ["--version"], { encoding: "utf8" });
  if (
    compiler.error ||
    compiler.status !== 0 ||
    !compiler.stdout.startsWith(`rustc ${inputs.rust.channel} `)
  ) {
    throw new Error("Halo CI Rust version mismatch");
  }
  const components = spawnSync("rustup", ["component", "list", "--installed"], {
    encoding: "utf8",
  });
  if (
    components.error ||
    components.status !== 0 ||
    inputs.rust.components.some(
      (name) =>
        !components.stdout
          .split("\n")
          .some((line) => line.startsWith(`${name}-`)),
    )
  ) {
    throw new Error("Halo CI Rust components are missing");
  }
  run("nginx", ["-V"]);
  const installation = `/opt/nucleus/target/postgresql/${inputs.postgresql}`;
  const destination = resolve(root, "target/postgresql", inputs.postgresql);
  mkdirSync(dirname(destination), { recursive: true });
  if (!existsSync(destination)) symlinkSync(installation, destination, "dir");
  if (realpathSync(destination) !== installation) {
    throw new Error(
      "Halo CI must use the preinstalled PostgreSQL installation",
    );
  }
  run("bun", ["fixture/postgresql.ts"]);
}

if (import.meta.main) {
  const [command, tag, ...extra] = process.argv.slice(2);
  if (
    extra.length ||
    !["build", "verify"].includes(command ?? "") ||
    (command === "build" ? !tag : tag)
  ) {
    throw new Error("Usage: bun tools/halo-ci-image.ts build IMAGE | verify");
  }
  if (command === "verify") verifyImage();
  else {
    const inputs = imageInputs();
    const context = mkdtempSync(resolve(tmpdir(), "nucleus-halo-image-"));
    try {
      for (const path of [
        ".github/halo-ci/Dockerfile",
        ".github/halo-ci/Dockerfile.dockerignore",
        "fixture/postgresql.ts",
        "fixture/postgresql.version",
      ]) {
        const destination = resolve(context, path);
        mkdirSync(dirname(destination), { recursive: true });
        copyFileSync(resolve(root, path), destination);
      }
      run("docker", [
        "build",
        "--platform",
        "linux/amd64",
        "--file",
        resolve(context, ".github/halo-ci/Dockerfile"),
        "--build-arg",
        `BUN_VERSION=${inputs.bun}`,
        "--build-arg",
        `NODE_VERSION=${inputs.node}`,
        "--build-arg",
        `RUST_VERSION=${inputs.rust.channel}`,
        "--build-arg",
        `RUST_COMPONENTS=${inputs.rust.components.join(",")}`,
        "--build-arg",
        `INPUT_DIGEST=${inputs.digest}`,
        "--tag",
        tag!,
        context,
      ]);
    } finally {
      rmSync(context, { recursive: true, force: true });
    }
  }
}
