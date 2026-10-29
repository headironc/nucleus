import { spawn, spawnSync } from "node:child_process";
import { randomBytes } from "node:crypto";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  rmdirSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import {
  assertContext,
  frozen,
  hash,
  image,
  platform,
  preflight,
  publish,
  remoteState,
  selected,
  SettledCopyFailure,
  tag,
  type Inspection,
} from "./publication";

export async function command(
  executable: string,
  args: string[],
): Promise<Inspection> {
  const child = spawn(executable, args, { stdio: ["ignore", "pipe", "pipe"] });
  const stdout: Buffer[] = [],
    stderr: Buffer[] = [];
  let cancelled = false;
  const stop = () => {
    cancelled = true;
    child.kill("SIGTERM");
  };
  process.once("SIGINT", stop);
  process.once("SIGTERM", stop);
  child.stdout.on("data", (chunk) => stdout.push(chunk));
  child.stderr.on("data", (chunk) => stderr.push(chunk));
  try {
    return await new Promise((done, reject) => {
      child.once("error", reject);
      child.once("close", (status) =>
        cancelled
          ? reject(
              new Error(
                "Publication command cancelled; original write outcome requires recovery",
              ),
            )
          : done({
              status,
              stdout: Buffer.concat(stdout),
              stderr: Buffer.concat(stderr).toString(),
            }),
      );
    });
  } finally {
    process.off("SIGINT", stop);
    process.off("SIGTERM", stop);
  }
}

export function capture(args: string[]) {
  const result = spawnSync("docker", args, {
    encoding: "utf8",
    timeout: 120_000,
  });
  if (result.error || result.status !== 0)
    throw new Error(
      `Docker operation failed: ${args[0]} ${args[1]}; status=${result.status}`,
    );
  return result.stdout.trim();
}

export function removeContainer(name: string) {
  const inspect = spawnSync(
    "docker",
    ["container", "inspect", "--format", "{{json .State}}", name],
    { encoding: "utf8" },
  );
  if (inspect.status === 0) {
    if (JSON.parse(inspect.stdout).Running) {
      capture(["stop", "--time", "60", name]);
      capture(["wait", name]);
    }
    const state = JSON.parse(
      capture(["container", "inspect", "--format", "{{json .State}}", name]),
    );
    if (state.Running) throw new Error("Owned container still running");
    capture(["rm", name]);
  } else if (!/no such (container|object)/i.test(inspect.stderr))
    throw new Error("Owned container state unknown");
  const absent = spawnSync("docker", ["container", "inspect", name], {
    encoding: "utf8",
  });
  if (absent.status === 0 || !/no such (container|object)/i.test(absent.stderr))
    throw new Error("Container removal unverified");
}

export async function withAuth<T>(
  work: (file: string) => Promise<T>,
  token = process.env.GH_TOKEN,
  actor = process.env.GITHUB_ACTOR,
) {
  if (!token || !actor) throw new Error("Short-lived job credential missing");
  const scratch = process.env.PUBLICATION_AUTH_ROOT
    ? resolve(process.env.PUBLICATION_AUTH_ROOT)
    : mkdtempSync(join(tmpdir(), "nucleus-publish-auth-"));
  if (process.env.PUBLICATION_AUTH_ROOT) mkdirSync(scratch, { mode: 0o700 });
  const file = join(scratch, "config.json");
  try {
    writeFileSync(
      file,
      JSON.stringify({
        auths: {
          "ghcr.io": {
            auth: Buffer.from(`${actor}:${token}`).toString("base64"),
          },
        },
      }),
      { mode: 0o600, flag: "wx" },
    );
    return await work(file);
  } finally {
    rmSync(file, { force: true });
    rmdirSync(scratch);
    if (existsSync(file) || existsSync(scratch))
      throw new Error("Registry authfile cleanup failed");
    console.log("Registry authfile removed and absence verified");
  }
}

async function skopeo(auth: string, args: string[], archive?: string) {
  const name = `nucleus-publish-tool-${randomBytes(12).toString("hex")}`;
  const receipt = join(
    resolve(process.env.PUBLICATION_EVIDENCE!),
    `${name}.json`,
  );
  writeFileSync(
    receipt,
    JSON.stringify({ name, operation: args[0], state: "starting" }),
  );
  try {
    return await command("docker", [
      "run",
      "--name",
      name,
      "--read-only",
      "--tmpfs",
      "/tmp:rw,nosuid,nodev,mode=1777",
      "--user",
      `${process.getuid!()}:${process.getgid!()}`,
      "-v",
      `${auth}:/auth.json:ro`,
      ...(archive ? ["-v", `${archive}:/image.oci.tar:ro`] : []),
      selected.skopeo,
      "--command-timeout",
      "15m",
      "--tmpdir",
      "/tmp",
      ...args,
    ]);
  } finally {
    try {
      removeContainer(name);
      writeFileSync(
        receipt,
        JSON.stringify({ name, operation: args[0], state: "absent" }),
      );
    } catch {
      writeFileSync(
        receipt,
        JSON.stringify({ name, operation: args[0], state: "cleanup-unknown" }),
      );
      throw new Error(
        `Original tool container outcome unresolved: ${name}; preserve receipt before recovery`,
      );
    }
  }
}

export async function registryIdentity(auth: string) {
  const manifest = await skopeo(auth, [
    "inspect",
    "--authfile",
    "/auth.json",
    "--raw",
    `docker://${image}`,
  ]);
  if (remoteState(manifest) !== "same")
    throw new Error("Published digest missing");
  const config = await skopeo(auth, [
    "inspect",
    "--authfile",
    "/auth.json",
    "--raw",
    "--config",
    `docker://${image}`,
  ]);
  if (config.status !== 0 || hash(config.stdout) !== selected.config)
    throw new Error("Registry config raw bytes mismatch");
  const parsed = JSON.parse(config.stdout.toString());
  if (
    parsed.os !== "linux" ||
    parsed.architecture !== "amd64" ||
    !parsed.config?.User ||
    ["0", "root"].includes(parsed.config.User.split(":")[0])
  )
    throw new Error("Registry platform/default user mismatch");
  return {
    manifest: selected.manifest,
    config: selected.config,
    layers: JSON.parse(manifest.stdout.toString()).layers,
    diffIds: parsed.rootfs.diff_ids as string[],
    platform: { os: "linux", architecture: "amd64" },
  };
}

export async function publishArtifact(directory: string) {
  const before = platform();
  await frozen(directory);
  return withAuth(async (auth) => {
    const outcome = await publish(
      () =>
        skopeo(auth, [
          "inspect",
          "--authfile",
          "/auth.json",
          "--raw",
          `docker://${selected.image}:${tag}`,
        ]),
      async () => {
        const journal = join(
          resolve(process.env.PUBLICATION_EVIDENCE!),
          "publication-attempt.json",
        );
        writeFileSync(
          journal,
          JSON.stringify({
            state: "started",
            image,
            tag,
            run: process.env.GITHUB_RUN_ID,
            job: process.env.GITHUB_JOB,
          }),
        );
        const result = await skopeo(
          auth,
          [
            "copy",
            "--authfile",
            "/auth.json",
            "--preserve-digests",
            "oci-archive:/image.oci.tar",
            `docker://${selected.image}:${tag}`,
          ],
          join(directory, "image.oci.tar"),
        );
        writeFileSync(
          journal,
          JSON.stringify({
            state: "settled",
            status: result.status,
            image,
            tag,
            run: process.env.GITHUB_RUN_ID,
            job: process.env.GITHUB_JOB,
          }),
        );
        if (result.status !== 0)
          throw new SettledCopyFailure(
            "Original registry copy finished unsuccessfully; readback required",
          );
      },
    );
    const identity = await registryIdentity(auth);
    const after = platform();
    return {
      ...outcome,
      image,
      tag,
      identity,
      packageBefore: before.package,
      packageAfter: after.package,
      producer: {
        run: selected.run,
        attempt: 1,
        artifact: selected.artifact,
        source: selected.source,
      },
    };
  });
}

if (import.meta.main) {
  assertContext();
  const [mode, directory, producer, ...extra] = process.argv.slice(2);
  if (!directory || extra.length)
    throw new Error(
      "Usage: registry.ts preflight ARTIFACT PRODUCER | publish ARTIFACT",
    );
  const output = resolve(process.env.PUBLICATION_EVIDENCE!);
  mkdirSync(output, { recursive: true });
  if (mode === "preflight" && producer) {
    const result = await preflight(resolve(directory), resolve(producer));
    writeFileSync(
      join(output, "preflight.json"),
      JSON.stringify(result, null, 2),
    );
  } else if (mode === "publish" && !producer) {
    const result = await publishArtifact(resolve(directory));
    writeFileSync(
      join(output, "publication.json"),
      JSON.stringify(result, null, 2),
    );
    console.log(JSON.stringify(result));
  } else throw new Error("Invalid publication command");
}
