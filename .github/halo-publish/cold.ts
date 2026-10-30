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
    !Array.isArray(local.RepoDigests) ||
    !local.RepoDigests.every((digest: unknown) => typeof digest === "string") ||
    !local.RepoDigests.includes(image) ||
    JSON.stringify(local.RootFS?.Layers) !== JSON.stringify(diffIds)
  )
    throw new Error("Pulled image/config/platform mismatch");
  return local.Id as string;
}

export function pullReceipt(result: { status: number | null; stdout: Buffer }) {
  const text = result.stdout.toString();
  return {
    image,
    status: result.status,
    digest: text.match(/^Digest: (sha256:[0-9a-f]{64})\s*$/m)?.[1] ?? null,
    progress: text.split(/\r?\n/).flatMap((line) => {
      const match = line.match(
        /^([0-9a-f]{12}): (Pulling fs layer|Waiting|Verifying Checksum|Download complete|Pull complete|Already exists)$/,
      );
      return match ? [{ layer: match[1]!, state: match[2]! }] : [];
    }),
  };
}

export function prepareColdCache(
  output: string,
  docker = capture,
  env = process.env,
) {
  assertContext(env);
  const socket = "unix:///var/run/docker.sock";
  if (
    env.RUNNER_ENVIRONMENT !== "github-hosted" ||
    env.RUNNER_OS !== "Linux" ||
    env.GITHUB_JOB !== "cold" ||
    env.GITHUB_WORKFLOW_REF !==
      "headironc/nucleus/.github/workflows/halo-publish.yml@refs/heads/dev" ||
    !/^\d+$/.test(env.GITHUB_RUN_ID ?? "") ||
    !/^\d+$/.test(env.GITHUB_RUN_ATTEMPT ?? "") ||
    (env.DOCKER_HOST && env.DOCKER_HOST !== socket) ||
    (env.DOCKER_CONTEXT && env.DOCKER_CONTEXT !== "default")
  )
    throw new Error(
      "Cache preparation requires this dedicated hosted cold job",
    );
  if (
    docker(["context", "show"]) !== "default" ||
    docker([
      "context",
      "inspect",
      "default",
      "--format",
      "{{.Endpoints.docker.Host}}",
    ]) !== socket
  )
    throw new Error(
      "Cold cache Docker context is not the local default daemon",
    );
  const images = () => {
    const text = docker([
      "image",
      "ls",
      "--all",
      "--no-trunc",
      "--format",
      "{{json .}}",
    ]);
    return text
      ? text.split("\n").map((line) => {
          const row = JSON.parse(line);
          if (
            typeof row.ID !== "string" ||
            !/^sha256:[0-9a-f]{64}$/.test(row.ID) ||
            typeof row.Repository !== "string" ||
            typeof row.Tag !== "string"
          )
            throw new Error("Unverifiable initial Docker image identity");
          return {
            id: row.ID as string,
            reference:
              row.Repository === "<none>" || row.Tag === "<none>"
                ? null
                : `${row.Repository}:${row.Tag}`,
          };
        })
      : [];
  };
  const containers = () => {
    const text = docker(["container", "ls", "--all", "--quiet", "--no-trunc"]);
    const ids = text ? text.split("\n") : [];
    if (ids.some((id) => !/^[0-9a-f]{64}$/.test(id)))
      throw new Error("Unverifiable initial Docker container identity");
    return ids;
  };
  const before = images();
  const receipt = {
    run: env.GITHUB_RUN_ID,
    attempt: env.GITHUB_RUN_ATTEMPT,
    workflowRevision: env.GITHUB_WORKFLOW_SHA,
    runnerEnvironment: "github-hosted",
    context: { name: "default", host: socket },
    imagesBefore: before,
    containersBefore: containers(),
    attempts: [] as { id: string; target: string; state: string }[],
    imagesAfter: null as ReturnType<typeof images> | null,
    containersAfter: null as string[] | null,
    state: "observed",
  };
  const save = () =>
    writeFileSync(
      join(output, "cold-cache.json"),
      JSON.stringify(receipt, null, 2),
    );
  const remove = (id: string, target: string) => {
    const attempt = { id, target, state: "started" };
    receipt.attempts.push(attempt);
    save();
    // Never force removal or prune images outside the recorded initial set.
    docker(["image", "rm", "--no-prune", "--", target]);
    attempt.state = "completed";
    save();
  };
  save();
  try {
    if (receipt.containersBefore.length)
      throw new Error(
        "Cold runner has containers; initial images are retained",
      );
    const initial = new Set(before.map((row) => row.id));
    const references = new Set<string>();
    for (const row of before) {
      if (row.reference && !references.has(row.reference)) {
        references.add(row.reference);
        if (
          docker([
            "image",
            "inspect",
            "--format",
            "{{.Id}}",
            "--",
            row.reference,
          ]) !== row.id
        )
          throw new Error("Initial image reference changed; cache retained");
        remove(row.id, row.reference);
      }
    }
    for (const id of new Set(images().map((row) => row.id))) {
      if (!initial.has(id))
        throw new Error(
          "New unowned image appeared; cache preparation stopped",
        );
      remove(id, id);
    }
    receipt.imagesAfter = images();
    receipt.containersAfter = containers();
    if (receipt.imagesAfter.length || receipt.containersAfter.length)
      throw new Error("Cold runner cache did not become empty");
    receipt.state = "prepared";
    save();
    return receipt;
  } catch (error) {
    receipt.state = "failed";
    try {
      receipt.imagesAfter = images();
      receipt.containersAfter = containers();
    } catch {
      receipt.imagesAfter = null;
      receipt.containersAfter = null;
    }
    save();
    throw error;
  }
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
  const cache = prepareColdCache(output);
  writeFileSync(
    join(output, "cold-before.json"),
    JSON.stringify({
      cache: cache.imagesAfter,
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
      const pull = pullReceipt(result);
      writeFileSync(
        join(output, "pull-transfer.json"),
        JSON.stringify(pull, null, 2),
      );
      if (result.status !== 0)
        throw new Error(
          "Private job-token pull failed; no anonymous or PAT fallback",
        );
      pulled = true;
      if (
        pull.progress.some((entry) => entry.state === "Already exists") ||
        (pull.digest !== null && pull.digest !== selected.manifest)
      )
        throw new Error("Cold pull cache reuse or digest conflict detected");
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
    if (
      !Number.isSafeInteger(defaultUser.uid) ||
      defaultUser.uid <= 0 ||
      !Number.isSafeInteger(defaultUser.gid) ||
      defaultUser.gid < 0
    )
      throw new Error(
        "Default user receipt must identify a non-root numeric user",
      );
    writeFileSync(
      join(output, "default-user.json"),
      JSON.stringify({ uid: defaultUser.uid, gid: defaultUser.gid }),
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
