import { spawnSync } from "node:child_process";
import { randomBytes } from "node:crypto";
import {
  existsSync,
  constants,
  openSync,
  closeSync,
  fstatSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import {
  currentIdentity,
  createDocker,
  prepareAccounts,
} from "../../tools/halo-local";
import { prepareHaloDependencies } from "../../tools/halo-image-runtime";
import { importRuntimeEvidence } from "../../tools/halo-image-artifact";
import { assertContext, image, platform, selected } from "./publication";
import {
  capture,
  command,
  registryIdentity,
  removeContainer,
  withAuth,
} from "./registry";

export function pulledReference(local: any, diffIds: string[]) {
  if (
    !/^sha256:[0-9a-f]{64}$/.test(local.Id) ||
    local.Os !== "linux" ||
    local.Architecture !== "amd64" ||
    !local.RepoDigests?.includes(image) ||
    JSON.stringify(local.RootFS?.Layers) !== JSON.stringify(diffIds)
  )
    throw new Error("Pulled image/config/platform mismatch");
  return local.Id as string;
}

async function cold(producer: string, output: string) {
  process.env.PUBLICATION_EVIDENCE = output;
  const identity = currentIdentity();
  const head = spawnSync("git", ["-C", producer, "rev-parse", "HEAD"], {
    encoding: "utf8",
  });
  if (head.status !== 0 || head.stdout.trim() !== selected.source)
    throw new Error("Cold consumer source mismatch");
  const before = platform();
  const cache = capture(["image", "ls", "--quiet", "--no-trunc"]);
  if (cache)
    throw new Error(
      "Cold acceptance requires a new runner with an empty image cache",
    );
  writeFileSync(
    join(output, "cold-before.json"),
    JSON.stringify({
      cache,
      image,
      source: selected.source,
      package: before.package,
    }),
  );
  const scratch = mkdtempSync(join(tmpdir(), "nucleus-cold-runtime-"));
  const names: string[] = [];
  const states: unknown[] = [];
  const errors: string[] = [];
  let pulled = false;
  let failure: unknown;
  const nonce = randomBytes(12).toString("hex");
  const run = async (name: string, args: string[]) => {
    names.push(name);
    const result = await command("docker", ["run", "--name", name, ...args]);
    // These containers never receive job credentials or the authentication file.
    console.log(result.stdout.toString());
    console.error(result.stderr);
    if (result.status !== 0)
      throw new Error(`Cold consumer failed: ${name}; status=${result.status}`);
    states.push({
      name,
      state: JSON.parse(
        capture(["container", "inspect", "--format", "{{json .State}}", name]),
      ),
    });
  };
  try {
    const registry = await withAuth(async (auth) => {
      const result = await command("docker", [
        "--config",
        resolve(auth, ".."),
        "pull",
        image,
      ]);
      if (result.status !== 0)
        throw new Error(
          "Private job-token pull failed; no anonymous or PAT fallback",
        );
      pulled = true;
      const remote = await registryIdentity(auth);
      writeFileSync(
        join(output, "registry.json"),
        JSON.stringify(remote, null, 2),
      );
      return remote;
    });
    const local = JSON.parse(
      capture(["image", "inspect", "--format", "{{json .}}", image]),
    );
    const reference = pulledReference(local, registry.diffIds);
    writeFileSync(
      join(output, "pull.json"),
      JSON.stringify(
        {
          image,
          config: selected.config,
          runtimeReference: reference,
          repoDigests: local.RepoDigests,
          credentialsRemoved: true,
        },
        null,
        2,
      ),
    );
    const defaultName = `nucleus-cold-${nonce}-default`;
    await run(defaultName, [
      "--pull=never",
      "--read-only",
      "--network=none",
      "--entrypoint",
      "bun",
      reference,
      "-e",
      'const uid=process.getuid(); if (!Number.isSafeInteger(uid)||uid<=0) throw Error("Default user must be non-root"); console.log(JSON.stringify({uid,gid:process.getgid()}));',
    ]);
    const defaultUser = JSON.parse(capture(["logs", defaultName]));
    writeFileSync(
      join(output, "default-user.json"),
      JSON.stringify(defaultUser),
    );
    for (const path of ["target", "root-modules", "home", "runtime-evidence"])
      mkdirSync(join(scratch, path));
    const modules = prepareHaloDependencies(
      producer,
      join(scratch, "dependencies"),
    );
    const accounts = prepareAccounts(
      createDocker(producer, identity).accounts(reference),
      identity,
      "/home/acceptance",
      scratch,
    );
    for (const path of ["target", "node_modules", "halo/node_modules"])
      mkdirSync(join(producer, path), { recursive: true });
    const runtime = `nucleus-cold-${nonce}-runtime`;
    await run(runtime, [
      "--init",
      "--pull=never",
      "--read-only",
      "--network=none",
      "--ipc=private",
      "--shm-size=1g",
      "--cpus=2",
      "--user",
      `${identity.uid}:${identity.gid}`,
      "--workdir",
      "/workspace",
      "--tmpfs",
      "/tmp:rw,nosuid,nodev,mode=1777",
      "-e",
      "HOME=/home/acceptance",
      "-e",
      "BUN_INSTALL_CACHE_DIR=/home/acceptance/bun-cache",
      "-v",
      `${producer}:/workspace:ro`,
      "-v",
      `${join(scratch, "target")}:/workspace/target`,
      "-v",
      `${modules}:/workspace/halo/node_modules:ro`,
      "-v",
      `${join(scratch, "root-modules")}:/workspace/node_modules`,
      "-v",
      `${join(scratch, "home")}:/home/acceptance`,
      ...accounts.flatMap((m) => ["-v", `${m.source}:${m.target}:ro`]),
      "-v",
      `${join(scratch, "runtime-evidence")}:/evidence`,
      reference,
      "bun",
      "tools/halo-image-runtime.ts",
      "/evidence",
    ]);
    const actual = capture([
      "container",
      "inspect",
      "--format",
      "{{.Image}}",
      runtime,
    ]);
    const requested = capture([
      "container",
      "inspect",
      "--format",
      "{{.Config.Image}}",
      runtime,
    ]);
    if (actual !== reference || requested !== reference)
      throw new Error("Consumer used another image");
    importRuntimeEvidence(join(scratch, "runtime-evidence"), output);
    const evidence = JSON.parse(
      readFileSync(join(output, "runtime.json"), "utf8"),
    );
    if (
      !evidence.success ||
      !evidence.postmasterGone ||
      !evidence.pgDirectoryRemoved
    )
      throw new Error("Cold consumer cleanup incomplete");
  } catch (error) {
    failure = error;
  } finally {
    for (const name of names)
      try {
        removeContainer(name);
      } catch {
        errors.push(`Container cleanup failed: ${name}`);
      }
    const failedRuntime = join(scratch, "runtime-evidence", "runtime.json");
    if (failure && !errors.length && existsSync(failedRuntime)) {
      let fd: number | undefined;
      try {
        // The container has stopped; its parent directory was never mounted.
        fd = openSync(
          failedRuntime,
          constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK,
        );
        const stat = fstatSync(fd);
        if (!stat.isFile() || stat.nlink !== 1) throw Error();
        writeFileSync(join(output, "runtime-failure.json"), readFileSync(fd), {
          flag: "wx",
          mode: 0o600,
        });
      } catch {
        errors.push("Runtime failure evidence preservation failed");
      } finally {
        if (fd !== undefined) closeSync(fd);
      }
    }
    // A failed pull can have installed data: inspect our exact digest before cleanup.
    const present = spawnSync("docker", ["image", "inspect", image], {
      encoding: "utf8",
    });
    if (present.status === 0) {
      try {
        capture(["image", "rm", image]);
        const absent = spawnSync("docker", ["image", "inspect", image], {
          encoding: "utf8",
        });
        if (
          absent.status === 0 ||
          !/no such (image|object)/i.test(absent.stderr)
        )
          throw Error();
      } catch {
        errors.push("Pulled image cleanup failed");
      }
    } else if (!/no such (image|object)/i.test(present.stderr))
      errors.push("Pulled image state unknown");
    if (!errors.length) rmSync(scratch, { recursive: true });
    if (existsSync(scratch)) errors.push("Cold private directory retained");
    writeFileSync(
      join(output, "cleanup.json"),
      JSON.stringify(
        {
          success: !errors.length,
          containers: states,
          errors,
          scratchRemoved: !existsSync(scratch),
        },
        null,
        2,
      ),
    );
  }
  if (failure || errors.length || !pulled)
    throw new Error(
      `Cold acceptance failed: ${failure instanceof Error ? failure.message : String(failure)}; ${errors.join("; ")}`,
    );
  writeFileSync(
    join(output, "accepted.json"),
    JSON.stringify(
      {
        success: true,
        image,
        source: selected.source,
        run: process.env.GITHUB_RUN_ID,
        job: process.env.GITHUB_JOB,
        packageId: selected.packageId,
      },
      null,
      2,
    ),
  );
}

if (import.meta.main) {
  assertContext();
  const [producer, output, ...extra] = process.argv.slice(2);
  if (!producer || !output || extra.length)
    throw new Error("Usage: cold.ts PRODUCER OUTPUT");
  mkdirSync(output, { recursive: true });
  await cold(resolve(producer), resolve(output));
}
