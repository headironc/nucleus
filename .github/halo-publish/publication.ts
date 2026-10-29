import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { createReadStream, readFileSync, readdirSync } from "node:fs";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

// This first publication deliberately selects one independently accepted artifact.
export const selected = Object.freeze({
  repository: "headironc/nucleus",
  repositoryId: 1432619931,
  packageId: 16266672,
  source: "55506adfd5c4ba8e34632d2cc2eadcfd189c75e8",
  before: "ecb3be02ebbf1a6ade93cfe751231bdd38133eab",
  run: 40964056245,
  attempt: 1,
  jobs: [123763896679, 123771833610],
  artifact: 12938521777,
  zipSize: 1521551740,
  zip: "sha256:00559733fbfaa9a17472db479eb789386b90296b9f9c253f2b287f1506ceb702",
  tar: "sha256:b3e4b27ee705a3ecf09c795d8f4b17e9613dbcc53086cb95b9ab1de54aefbe61",
  tarSize: 1502102528,
  manifest:
    "sha256:80ae416d4bc7287532c0974d25390c5bc9b27cd62defe4e64860b501414b4592",
  config:
    "sha256:29297e0c0cd2a0f0cf79ed631bb12b194433f8876f6eb826cace4b6b61a7ebd6",
  image: "ghcr.io/headironc/nucleus-ci",
  skopeo:
    "quay.io/skopeo/stable@sha256:e946ec32d5b6243a3e8df383bad72b4e11bd0dfcf4690eed09b6378b4deba122",
});
export const tag = `source-${selected.source}-run-${selected.run}-attempt-${selected.attempt}`;
export const image = `${selected.image}@${selected.manifest}`;
export const hash = (bytes: Uint8Array | string) =>
  "sha256:" + createHash("sha256").update(bytes).digest("hex");
export async function fileHash(file: string) {
  const h = createHash("sha256");
  for await (const chunk of createReadStream(file)) h.update(chunk);
  return "sha256:" + h.digest("hex");
}

export function assertContext(env = process.env) {
  if (
    env.GITHUB_EVENT_NAME !== "workflow_dispatch" ||
    env.GITHUB_REPOSITORY !== selected.repository ||
    env.GITHUB_REPOSITORY_ID !== String(selected.repositoryId) ||
    env.GITHUB_REF !== "refs/heads/dev" ||
    !/^[0-9a-f]{40}$/.test(env.GITHUB_WORKFLOW_SHA ?? "")
  )
    throw new Error("Publication requires the trusted dev workflow_dispatch");
}

export function assertPlatform(
  repository: any,
  run: any,
  jobs: any[],
  artifact: any,
  pkg: any,
  comparison: any,
  now = Date.now(),
) {
  if (
    repository.id !== selected.repositoryId ||
    repository.full_name !== selected.repository ||
    repository.default_branch !== "dev"
  )
    throw new Error("Publication repository mismatch");
  if (
    run.id !== selected.run ||
    run.run_attempt !== selected.attempt ||
    run.event !== "push" ||
    run.head_branch !== "dev" ||
    run.head_sha !== selected.source ||
    run.status !== "completed" ||
    run.conclusion !== "success" ||
    run.repository?.id !== selected.repositoryId ||
    run.head_repository?.id !== selected.repositoryId ||
    run.path !== ".github/workflows/halo-image.yml"
  )
    throw new Error("Producer run/attempt/source/workflow is not accepted");
  for (const id of selected.jobs) {
    const matching = jobs.filter((job) => job.id === id);
    if (
      matching.length !== 1 ||
      matching[0].run_id !== selected.run ||
      matching[0].run_attempt !== selected.attempt ||
      matching[0].head_sha !== selected.source ||
      matching[0].status !== "completed" ||
      matching[0].conclusion !== "success"
    )
      throw new Error("Producer attempt jobs incomplete");
  }
  if (
    artifact.id !== selected.artifact ||
    artifact.name !== `nucleus-ci-${selected.run}-1` ||
    artifact.expired !== false ||
    !Number.isFinite(Date.parse(artifact.expires_at)) ||
    Date.parse(artifact.expires_at) <= now ||
    artifact.digest !== selected.zip ||
    artifact.size_in_bytes !== selected.zipSize ||
    artifact.workflow_run?.id !== selected.run ||
    artifact.workflow_run?.repository_id !== selected.repositoryId ||
    artifact.workflow_run?.head_repository_id !== selected.repositoryId ||
    artifact.workflow_run?.head_branch !== "dev" ||
    artifact.workflow_run?.head_sha !== selected.source
  )
    throw new Error("Artifact ownership, bytes or expiration mismatch");
  if (
    pkg.id !== selected.packageId ||
    pkg.name !== "nucleus-ci" ||
    pkg.visibility !== "private" ||
    pkg.repository?.id !== selected.repositoryId ||
    pkg.repository?.full_name !== selected.repository
  )
    throw new Error(
      "Existing private package association is not ready: " +
        JSON.stringify({
          id: pkg.id ?? null,
          name: pkg.name ?? null,
          visibility: pkg.visibility ?? null,
          repositoryId: pkg.repository?.id ?? null,
          repositoryName: pkg.repository?.full_name ?? null,
        }),
    );
  if (
    !comparison ||
    !["ahead", "identical"].includes(comparison.status) ||
    comparison.base_commit?.sha !== selected.source ||
    comparison.merge_base_commit?.sha !== selected.source
  )
    throw new Error("Producer source no longer belongs to dev history");
}

export function api(path: string, pages = false): any {
  const args = [
    "api",
    "--hostname",
    "github.com",
    path,
    ...(pages ? ["--paginate", "--slurp"] : []),
  ];
  const result = spawnSync("gh", args, { encoding: "utf8", timeout: 60_000 });
  if (result.error || result.status !== 0)
    throw new Error(
      `GitHub read failed: ${path}; status=${result.status}; HTTP=${result.stderr.match(/HTTP (\d{3})/)?.[1] ?? "unknown"}`,
    );
  return JSON.parse(result.stdout);
}
export function platform() {
  const base = `repos/${selected.repository}`;
  const repository = api(base);
  const run = api(`${base}/actions/runs/${selected.run}/attempts/1`);
  const jobPages = api(
    `${base}/actions/runs/${selected.run}/attempts/1/jobs?per_page=100`,
    true,
  );
  if (
    !Array.isArray(jobPages) ||
    !jobPages.every((page) => Array.isArray(page.jobs))
  )
    throw new Error("Incomplete job pagination");
  const artifact = api(`${base}/actions/artifacts/${selected.artifact}`);
  const pkg = api("users/headironc/packages/container/nucleus-ci");
  const comparison = api(`${base}/compare/${selected.source}...dev`);
  assertPlatform(
    repository,
    run,
    jobPages.flatMap((page) => page.jobs),
    artifact,
    pkg,
    comparison,
  );
  return {
    repository,
    run,
    jobs: jobPages.flatMap((page) => page.jobs),
    artifact,
    package: pkg,
    comparison,
  };
}

export async function frozen(directory: string) {
  const read = (name: string) =>
    JSON.parse(readFileSync(join(directory, name), "utf8"));
  const p = read("provenance.json");
  if (
    p.protocol !== 2 ||
    p.repository.id !== selected.repositoryId ||
    p.repository.name !== selected.repository ||
    JSON.stringify(p.source) !==
      JSON.stringify({
        event: "push",
        source: selected.source,
        before: selected.before,
      }) ||
    p.workflow.run !== String(selected.run) ||
    p.workflow.attempt !== "1" ||
    p.workflow.job !== "build" ||
    p.workflow.revision !== selected.source ||
    p.workflow.path !== ".github/workflows/halo-image.yml" ||
    p.archive.format !== "oci-image-layout-1.0.0" ||
    p.archive.sha256 !== selected.tar ||
    p.archive.size !== selected.tarSize ||
    p.oci.manifest.digest !== selected.manifest ||
    p.oci.config.digest !== selected.config ||
    p.oci.layers.length !== 14 ||
    p.build.configDigest !== selected.config ||
    p.restored.configDigest !== selected.config
  )
    throw new Error("Frozen provenance mismatch");
  if ((await fileHash(join(directory, "image.oci.tar"))) !== selected.tar)
    throw new Error("Frozen archive bytes changed");
  if (!p.materials || typeof p.materials !== "object")
    throw new Error("Artifact materials missing");
  for (const [name, digest] of Object.entries(p.materials)) {
    if (
      !/^[a-z][a-z0-9.-]*$/.test(name) ||
      typeof digest !== "string" ||
      (await fileHash(join(directory, name))) !== digest
    )
      throw new Error("Artifact material digest mismatch");
  }
  if (
    JSON.stringify(readdirSync(directory).sort()) !==
    JSON.stringify(
      ["image.oci.tar", "provenance.json", ...Object.keys(p.materials)].sort(),
    )
  )
    throw new Error("Unexpected artifact members");
  const v = read("validation.json");
  const c = read("cleanup.json");
  if (
    v.success !== true ||
    v.runtime.success !== true ||
    v.runtime.pgDirectoryRemoved !== true ||
    v.runtime.postmasterGone !== true ||
    v.cleanup.success !== true ||
    v.scanFindings !== 0 ||
    c.success !== true ||
    c.scratchRemoved !== true ||
    c.cancelled !== false ||
    !Array.isArray(c.errors) ||
    c.errors.length
  )
    throw new Error("Artifact acceptance incomplete");
  return p;
}

export async function preflight(directory: string, producer: string) {
  const receipt = platform();
  const actual = spawnSync("git", ["-C", producer, "rev-parse", "HEAD"], {
    encoding: "utf8",
  });
  const ancestor = spawnSync("git", [
    "-C",
    producer,
    "merge-base",
    "--is-ancestor",
    selected.source,
    "origin/dev",
  ]);
  if (
    actual.status !== 0 ||
    actual.stdout.trim() !== selected.source ||
    ancestor.status !== 0
  )
    throw new Error("Selected source checkout/ancestry mismatch");
  const { verifyArtifact } = await import(
    pathToFileURL(join(resolve(producer), "tools/halo-image-artifact.ts")).href
  );
  await verifyArtifact(directory, {
    source: selected.source,
    run: String(selected.run),
    attempt: "1",
    tar: selected.tar,
    manifest: selected.manifest,
    config: selected.config,
    binding: {
      event: "push",
      source: selected.source,
      before: selected.before,
    },
    workflowRevision: selected.source,
  });
  await frozen(directory);
  return receipt;
}

export type Inspection = {
  status: number | null;
  stdout: Buffer;
  stderr: string;
};
export class SettledCopyFailure extends Error {}
export function remoteState(result: Inspection): "absent" | "same" {
  if (result.status === 0) {
    if (hash(result.stdout) !== selected.manifest)
      throw new Error("Immutable publication tag conflict");
    const manifest = JSON.parse(result.stdout.toString());
    if (
      manifest.schemaVersion !== 2 ||
      manifest.mediaType !==
        "application/vnd.docker.distribution.manifest.v2+json" ||
      manifest.config?.digest !== selected.config ||
      manifest.layers?.length !== 14
    )
      throw new Error("Remote manifest identity mismatch");
    return "same";
  }
  // Only the registry's explicit MANIFEST_UNKNOWN response means absence.
  if (
    result.status !== null &&
    /manifest unknown/i.test(result.stderr) &&
    !/unauthorized|denied|forbidden|401|403|429|timeout|connection/i.test(
      result.stderr,
    )
  )
    return "absent";
  throw new Error("Registry state unknown; no write or retry permitted");
}

export async function publish(
  inspect: () => Promise<Inspection>,
  copy: () => Promise<void>,
) {
  if (remoteState(await inspect()) === "same") return { publication: "reused" };
  let failed = false;
  try {
    await copy();
  } catch (error) {
    if (!(error instanceof SettledCopyFailure)) throw error;
    failed = true;
  }
  // copy() must settle its original CLI/container before this readback.
  const state = remoteState(await inspect());
  if (state !== "same")
    throw new Error(
      `Publication ${failed ? "failed" : "returned success"} but manifest absent; no automatic retry`,
    );
  return { publication: failed ? "recovered-by-readback" : "published" };
}
