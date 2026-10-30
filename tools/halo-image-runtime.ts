// Runs inside the restored image. Its finally owns the private PostgreSQL
// cluster and every foreground child, including signal cancellation.
import { spawn, spawnSync } from "node:child_process";
import {
  chmodSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { imageInputs } from "./halo-ci-image";

export function prepareHaloDependencies(repository: string, directory: string) {
  mkdirSync(directory);
  for (const file of ["package.json", "bun.lock"])
    copyFileSync(join(repository, "halo", file), join(directory, file));
  const result = spawnSync(
    process.execPath,
    ["install", "--frozen-lockfile", "--ignore-scripts", "--no-env-file"],
    { cwd: directory, stdio: "inherit", timeout: 120_000 },
  );
  if (result.error || result.status !== 0)
    throw new Error("Private Halo dependency preparation failed");
  return join(directory, "node_modules");
}

export async function acceptRuntime(
  evidence: string,
  fault?: "pg" | "browser" | "consumer" | "cancel",
) {
  if (process.getuid?.() === 0)
    throw new Error("Artifact acceptance requires non-root");
  const inputs = imageInputs();
  const directory = mkdtempSync(join(tmpdir(), "np-"));
  chmodSync(directory, 0o700);
  const data = join(directory, "d");
  const socket = join(directory, "s");
  mkdirSync(socket, { mode: 0o700 });
  const bin = `/opt/nucleus/target/postgresql/${inputs.postgresql}/bin`;
  let child: ReturnType<typeof spawn> | null = null;
  let cancelled = false;
  let postmaster: number | null = null;
  const steps: string[] = [];
  const stop = () => {
    cancelled = true;
    if (child?.pid) {
      try {
        process.kill(-child.pid, "SIGTERM");
      } catch {
        /* Already exited. */
      }
    }
  };
  process.on("SIGINT", stop);
  process.on("SIGTERM", stop);
  const run = async (args: string[], cwd = process.cwd()) => {
    if (cancelled) throw new Error("Artifact runtime cancelled");
    console.log(`[image-runtime] ${args[0]} ${args.slice(1).join(" ")}`);
    const current = spawn(args[0]!, args.slice(1), {
      cwd,
      stdio: "inherit",
      detached: true,
    });
    child = current;
    const code = await new Promise<number | null>((done, reject) => {
      current.once("error", reject);
      current.once("exit", done);
    });
    child = null;
    if (cancelled || code !== 0)
      throw new Error(
        `Runtime child failed: ${args[0]} (${code}, cancelled=${cancelled})`,
      );
  };
  let failure: unknown;
  const cleanup: string[] = [];
  try {
    await run(["bun", "tools/halo-ci-image.ts", "verify"]);
    steps.push("versions-and-inputs-non-root");
    await run([
      join(bin, "initdb"),
      "-D",
      data,
      "--no-locale",
      "--encoding=UTF8",
      "--auth=trust",
    ]);
    await run([
      join(bin, "pg_ctl"),
      "-D",
      data,
      "-l",
      join(directory, "pg.log"),
      "-o",
      `-F -c listen_addresses='' -c unix_socket_directories='${socket}'`,
      "-w",
      "start",
    ]);
    postmaster = Number(
      readFileSync(join(data, "postmaster.pid"), "utf8").split("\n")[0],
    );
    if (!Number.isSafeInteger(postmaster) || postmaster <= 1)
      throw new Error("Invalid private postmaster PID");
    const query = spawnSync(
      join(bin, "psql"),
      [
        "-h",
        socket,
        "-d",
        "postgres",
        "-At",
        "-v",
        "ON_ERROR_STOP=1",
        "-c",
        "SELECT 6 * 7, current_setting('listen_addresses'), current_setting('server_version_num')",
      ],
      { encoding: "utf8" },
    );
    // This expectation is intentionally independent of the query expression.
    if (
      query.error ||
      query.status !== 0 ||
      query.stdout.trim() !== "42||180004" ||
      fault === "pg"
    )
      throw new Error("Private PostgreSQL query mismatch");
    steps.push("postgresql-private-socket-query");
    writeFileSync(
      join(evidence, "pg-ready.json"),
      JSON.stringify({ directory, postmaster, uid: process.getuid?.() }),
    );
    if (fault === "cancel") {
      console.log("[image-runtime] cancellation-ready");
      while (!cancelled) await delay(50);
      throw new Error("Controlled cancellation");
    }
    const playwright = JSON.parse(
      readFileSync(
        resolve("halo/node_modules/@playwright/test/package.json"),
        "utf8",
      ),
    ).version;
    if (playwright !== inputs.playwright)
      throw new Error("Prepared Playwright version mismatch");
    await run(["bun", "tools/browser-smoke.ts"], resolve("halo"));
    if (fault === "browser")
      throw new Error("Controlled browser failure after real browser cleanup");
    steps.push("chromium-headless-shell-and-chromium");
    await run(["bun", "test", "tools/check-inputs.test.ts"]);
    if (fault === "consumer")
      throw new Error("Controlled consumer failure after real consumer");
    steps.push("thin-consumer");
    const packages = spawnSync(
      "dpkg-query",
      ["-W", "-f=${binary:Package}\t${Version}\t${Architecture}\t${Status}\n"],
      { encoding: "utf8" },
    );
    if (packages.error || packages.status !== 0)
      throw new Error("Cannot inventory system packages");
    writeFileSync(join(evidence, "packages.tsv"), packages.stdout);
    const copyrights = [
      ...new Bun.Glob("**/copyright").scanSync({
        cwd: "/usr/share/doc",
        onlyFiles: true,
        followSymlinks: false,
      }),
    ];
    if (!copyrights.length)
      throw new Error("System copyright materials missing");
    const licenses = spawnSync(
      "tar",
      [
        "-czf",
        join(evidence, "licenses.tar.gz"),
        "-C",
        "/usr/share/doc",
        "--null",
        "-T",
        "-",
      ],
      { input: copyrights.join("\0") + "\0", encoding: "utf8" },
    );
    if (licenses.error || licenses.status !== 0)
      throw new Error("Cannot retain license source materials");
    writeFileSync(
      join(evidence, "licenses.json"),
      JSON.stringify(
        {
          files: copyrights,
          scope:
            "Installed dpkg copyright materials; not legal approval. Other tool licenses remain in layer inventory.",
        },
        null,
        2,
      ),
    );
  } catch (error) {
    failure = error;
  } finally {
    if (existsSync(join(data, "postmaster.pid"))) {
      const result = spawnSync(
        join(bin, "pg_ctl"),
        ["-D", data, "-m", "fast", "-w", "stop"],
        { encoding: "utf8", timeout: 60_000 },
      );
      if (result.error || result.status !== 0)
        cleanup.push("Private PostgreSQL fast-stop failed");
    }
    if (postmaster !== null) {
      // A stopped postmaster can briefly be an init-owned zombie before reap.
      const deadline = Date.now() + 10_000;
      while (existsSync(`/proc/${postmaster}`) && Date.now() < deadline)
        await delay(20);
      if (existsSync(`/proc/${postmaster}`))
        cleanup.push("Private postmaster still exists");
    }
    if (!cleanup.length) rmSync(directory, { recursive: true });
    if (existsSync(directory))
      cleanup.push(`Private PostgreSQL directory retained: ${directory}`);
    process.off("SIGINT", stop);
    process.off("SIGTERM", stop);
    writeFileSync(
      join(evidence, "runtime.json"),
      JSON.stringify(
        {
          success: !failure && !cancelled && !cleanup.length,
          steps,
          cancelled,
          cleanup,
          pgDirectoryRemoved: !existsSync(directory),
          postmasterGone:
            postmaster === null || !existsSync(`/proc/${postmaster}`),
        },
        null,
        2,
      ),
    );
  }
  if (failure || cleanup.length || cancelled)
    throw new Error(
      `Artifact runtime failed: ${failure instanceof Error ? failure.message : String(failure ?? "cancelled")}; ${cleanup.join("; ")}`,
    );
}

if (import.meta.main) {
  const [evidence, ...extra] = process.argv.slice(2);
  if (!evidence || extra.length)
    throw new Error("Usage: bun tools/halo-image-runtime.ts EVIDENCE");
  await acceptRuntime(evidence);
}
