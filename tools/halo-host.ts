// Run Halo acceptance directly on a prepared Linux host, without installing tools.
import { spawn, spawnSync } from "node:child_process";
import { mkdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { waitForGroupGone } from "../halo/tools/process";
import { bunVersionProblems } from "./check-bun";

const root = dirname(
  fileURLToPath(new URL("../package.json", import.meta.url)),
);
const halo = join(root, "halo");
const read = (path: string) => readFileSync(join(root, path), "utf8");

function output(command: string, args: string[]): string {
  const result = spawnSync(command, args, {
    cwd: root,
    encoding: "utf8",
    timeout: 10_000,
  });
  if (result.error || result.status !== 0)
    throw new Error(`Host prerequisite failed: ${command} ${args.join(" ")}`);
  return `${result.stdout}${result.stderr}`.trim();
}

function prerequisites(): string {
  if (process.platform !== "linux" || process.getuid?.() === 0)
    throw new Error("Halo host acceptance requires a non-root Linux user");
  const versions = read(".tool-versions");
  const problems = bunVersionProblems(
    versions,
    JSON.parse(read("package.json")),
    Bun.version,
  );
  if (problems.length) throw new Error(problems.join("\n"));
  const node = versions.match(/^nodejs (\d+\.\d+\.\d+)$/m)?.[1];
  if (!node || output("node", ["--version"]) !== `v${node}`)
    throw new Error(`Node ${node ?? "pin"} must be available on PATH`);
  if (output("bun", ["--version"]) !== Bun.version)
    throw new Error("The Bun on PATH must match the running pinned Bun");
  const rust = Bun.TOML.parse(read("rust-toolchain.toml")) as {
    toolchain: { channel: string };
  };
  const installed = output("rustup", ["toolchain", "list"]);
  if (
    !installed
      .split("\n")
      .some((line) => line.startsWith(`${rust.toolchain.channel}-`))
  )
    throw new Error(
      `Install Rust ${rust.toolchain.channel} before host checks`,
    );
  if (
    !output("rustup", [
      "run",
      rust.toolchain.channel,
      "rustc",
      "--version",
    ]).startsWith(`rustc ${rust.toolchain.channel} `)
  )
    throw new Error(`Rust ${rust.toolchain.channel} is required`);
  output("rustup", ["run", rust.toolchain.channel, "cargo", "--version"]);
  output(process.env.HALO_NGINX ?? "/usr/sbin/nginx", ["-v"]);
  output("openssl", ["version"]);
  const postgres = read("fixture/postgresql.version").trim();
  for (const program of ["postgres", "initdb", "pg_ctl", "psql"])
    if (
      output(join(root, "target/postgresql", postgres, "bin", program), [
        "--version",
      ]) !== `${program} (PostgreSQL) ${postgres}`
    )
      throw new Error(`Fixture ${program} must be PostgreSQL ${postgres}`);
  const expected = JSON.parse(read("halo/package.json")).devDependencies[
    "@playwright/test"
  ];
  // Resolve only the installed file: Bun's package resolver can auto-install
  // a missing package, which is forbidden in a prerequisite-only entrypoint.
  if (
    JSON.parse(read("halo/node_modules/@playwright/test/package.json"))
      .version !== expected
  )
    throw new Error(
      `Install the locked Halo dependencies; Playwright ${expected} is required`,
    );
  return rust.toolchain.channel;
}

async function main(): Promise<void> {
  const args = process.argv.slice(2);
  if (
    args.length > 1 ||
    (args.length === 1 && !["--prepare", "--browser"].includes(args[0]))
  )
    throw new Error("Usage: bun tools/halo-host.ts [--prepare | --browser]");
  const rust = prerequisites();
  if (args[0] === "--prepare") {
    console.log(
      "[halo-host] Host prerequisites are ready; no acceptance was run",
    );
    return;
  }
  const config = join(root, "target/halo-host/xdg-config");
  const cache = join(root, "target/halo-host/xdg-cache");
  mkdirSync(config, { recursive: true });
  mkdirSync(cache, { recursive: true });
  const steps: [string, string[]][] = [
    [halo, ["run", "typecheck"]],
    [halo, ["run", "build"]],
    [root, ["tools/check-halo-test.ts", "snapshot"]],
    ...(args[0] === "--browser"
      ? ([
          [halo, ["tools/browser-smoke.ts"]],
          [root, ["tools/check-halo-test.ts", "browser"]],
        ] as [string, string[]][])
      : []),
  ];
  for (const [cwd, command] of steps) {
    console.log(`[halo-host] bun ${command.join(" ")}`);
    const child = spawn(process.execPath, ["--no-install", ...command], {
      cwd,
      stdio: "inherit",
      // Keep build wrappers and their children in one observable process group.
      // The HTTP harness retains ownership of its own detached Cargo group.
      detached: true,
      env: {
        ...process.env,
        RUSTUP_TOOLCHAIN: rust,
        HALO_NGINX: process.env.HALO_NGINX ?? "/usr/sbin/nginx",
        XDG_CONFIG_HOME: config,
        XDG_CACHE_HOME: cache,
      },
    });
    let requested: NodeJS.Signals | undefined;
    const forward = (signal: NodeJS.Signals) => {
      if (requested) return;
      requested = signal;
      // Bun run and the HTTP harness forward cancellation to their own children.
      // Signalling their whole group here would deliver it twice.
      child.kill(signal);
    };
    const interrupt = () => forward("SIGINT");
    const terminate = () => forward("SIGTERM");
    process.on("SIGINT", interrupt);
    process.on("SIGTERM", terminate);
    try {
      const status = await new Promise<number>((resolve, reject) => {
        child.once("error", reject);
        child.once("exit", (code, signal) =>
          resolve(code ?? (signal === "SIGINT" ? 130 : 143)),
        );
      });
      if (child.pid && !(await waitForGroupGone(child.pid, 60_000)))
        throw new Error(
          `Host step process group ${child.pid} has not terminated`,
        );
      if (status !== 0 || requested) {
        process.exitCode = status || (requested === "SIGINT" ? 130 : 143);
        return;
      }
    } finally {
      process.removeListener("SIGINT", interrupt);
      process.removeListener("SIGTERM", terminate);
    }
  }
}

if (import.meta.main)
  main().catch((error: unknown) => {
    console.error(error instanceof Error ? error.message : error);
    process.exitCode = 1;
  });
