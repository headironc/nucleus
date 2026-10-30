import { afterEach, expect, test } from "bun:test";
import { spawn } from "node:child_process";
import {
  existsSync,
  readFileSync,
  statSync,
  mkdtempSync,
  writeFileSync,
  rmSync,
  chmodSync,
} from "node:fs";
import { resolve, join } from "node:path";
import { tmpdir } from "node:os";
import { setTimeout as delay } from "node:timers/promises";
import {
  assertContext,
  assertPlatform,
  accountAssociation,
  hash,
  image,
  publish,
  remoteState,
  selected,
  SettledCopyFailure,
  type Inspection,
} from "./publication";
import { withAuth, command } from "./registry";
import { prepareColdCache, pulledReference, pullReceipt } from "./cold";

const root = resolve(import.meta.dir, "../..");
// Original accepted manifest bytes, with only this fixture's final LF removed.
const manifest = Buffer.from(
  readFileSync(
    resolve(import.meta.dir, "fixtures/manifest.json"),
    "utf8",
  ).trimEnd(),
);
const same: Inspection = { status: 0, stdout: manifest, stderr: "" };
const absent: Inspection = {
  status: 1,
  stdout: Buffer.alloc(0),
  stderr: "reading manifest: manifest unknown",
};
const context = {
  GITHUB_EVENT_NAME: "workflow_dispatch",
  GITHUB_REPOSITORY: "headironc/nucleus",
  GITHUB_REPOSITORY_ID: "1432619931",
  GITHUB_REF: "refs/heads/dev",
  GITHUB_WORKFLOW_SHA: "e".repeat(40),
};
function accepted() {
  return {
    repository: {
      id: 1432619931,
      full_name: "headironc/nucleus",
      default_branch: "dev",
    },
    run: {
      id: 40964056245,
      run_attempt: 1,
      event: "push",
      head_branch: "dev",
      head_sha: String(selected.source),
      status: "completed",
      conclusion: "success",
      repository: { id: 1432619931 },
      head_repository: { id: 1432619931 },
      path: ".github/workflows/halo-image.yml",
    },
    jobs: [123763896679, 123771833610].map((id) => ({
      id,
      run_id: 40964056245,
      run_attempt: 1,
      head_sha: selected.source,
      status: "completed",
      conclusion: "success",
    })),
    artifact: {
      id: 12938521777,
      name: "nucleus-ci-40964056245-1",
      expired: false,
      expires_at: "2099-01-01T00:00:00Z",
      digest: String(selected.zip),
      size_in_bytes: 1521551740,
      workflow_run: {
        id: 40964056245,
        repository_id: 1432619931,
        head_repository_id: 1432619931,
        head_branch: "dev",
        head_sha: selected.source,
      },
    },
    pkg: {
      id: 16266672,
      name: "nucleus-ci",
      visibility: "private",
      repository: { id: 1432619931, full_name: "headironc/nucleus" },
    },
    comparison: {
      status: "ahead",
      base_commit: { sha: String(selected.source) },
      merge_base_commit: { sha: String(selected.source) },
    },
  };
}
const check = (a: ReturnType<typeof accepted>) =>
  assertPlatform(a.repository, a.run, a.jobs, a.artifact, a.pkg, a.comparison);
const directories: string[] = [];
afterEach(() => {
  for (const path of directories.splice(0))
    rmSync(path, { recursive: true, force: true });
});

test("cancellation waits for the original command terminal before removing credentials", async () => {
  const directory = mkdtempSync(join(tmpdir(), "nucleus-publish-child-"));
  directories.push(directory);
  const ready = join(directory, "ready"),
    terminal = join(directory, "terminal");
  const script = join(directory, "child.py");
  writeFileSync(
    script,
    `import os,pathlib,signal,time
stopped=False
def stop(*args):
 global stopped
 stopped=True
signal.signal(signal.SIGTERM,stop)
pathlib.Path(${JSON.stringify(ready)}).write_text(str(os.getpid()))
while not stopped: time.sleep(.01)
time.sleep(.05)
pathlib.Path(${JSON.stringify(terminal)}).write_text('settled')
`,
  );
  const workerFile = join(directory, "worker.ts"),
    result = join(directory, "result.json");
  writeFileSync(
    workerFile,
    `
import {withAuth,command} from ${JSON.stringify(resolve(import.meta.dir, "registry.ts"))};
import {existsSync,writeFileSync} from 'node:fs';
delete process.env.PUBLICATION_AUTH_ROOT;
let auth='',outcome='';
try {await withAuth(async path=>{auth=path;return command('python3',[${JSON.stringify(script)}]);},'inactive-test-only','actor');outcome='resolved';}
catch(e){outcome=e.message;}
writeFileSync(${JSON.stringify(result)},JSON.stringify({outcome,authRemoved:!existsSync(auth)}));
`,
  );
  const worker = spawn(process.execPath, [workerFile], { stdio: "ignore" });
  const exited = new Promise<number | null>((done, reject) => {
    worker.once("error", reject);
    worker.once("close", done);
  });
  try {
    const deadline = Date.now() + 5_000;
    while (!existsSync(ready) && Date.now() < deadline) await delay(10);
    expect(existsSync(ready)).toBe(true);
    const pid = Number(readFileSync(ready, "utf8"));
    worker.kill("SIGTERM");
    expect(await exited).toBe(0);
    const report = JSON.parse(readFileSync(result, "utf8"));
    expect(report.outcome).toContain("cancelled");
    expect(report.authRemoved).toBe(true);
    expect(readFileSync(terminal, "utf8")).toBe("settled");
    expect(() => process.kill(pid, 0)).toThrow();
  } finally {
    if (worker.exitCode === null) worker.kill("SIGTERM");
    await exited;
  }
}, 10_000);

test("only the trusted dev dispatch can enter publication", () => {
  expect(() => assertContext(context)).not.toThrow();
  for (const change of [
    { GITHUB_EVENT_NAME: "pull_request" },
    { GITHUB_EVENT_NAME: "workflow_run" },
    { GITHUB_REF: "refs/heads/main" },
    { GITHUB_REPOSITORY_ID: "1" },
    { GITHUB_REPOSITORY: "other/nucleus" },
    { GITHUB_WORKFLOW_SHA: "dev" },
  ])
    expect(() => assertContext({ ...context, ...change })).toThrow();
});
test("accepted platform tuple requires actual success, attempt, artifact ownership and the selected private package", () => {
  expect(() => check(accepted())).not.toThrow();
  const mutations: ((a: ReturnType<typeof accepted>) => void)[] = [
    (a) => {
      a.repository.id = 1;
    },
    (a) => {
      a.repository.default_branch = "main";
    },
    (a) => {
      a.run.event = "pull_request";
    },
    (a) => {
      a.run.run_attempt = 2;
    },
    (a) => {
      a.run.head_sha = "a".repeat(40);
    },
    (a) => {
      a.run.path = ".github/workflows/other.yml";
    },
    (a) => {
      a.run.conclusion = "failure";
    },
    (a) => {
      a.run.status = "in_progress";
    },
    (a) => {
      a.run.head_repository.id = 2;
    },
    (a) => {
      a.jobs.pop();
    },
    (a) => {
      a.jobs.push({ ...a.jobs[0]! });
    },
    (a) => {
      a.jobs[0]!.run_attempt = 2;
    },
    (a) => {
      a.jobs[1]!.conclusion = "cancelled";
    },
    (a) => {
      a.artifact.id = 1;
    },
    (a) => {
      a.artifact.digest = "sha256:" + "a".repeat(64);
    },
    (a) => {
      a.artifact.size_in_bytes = 1;
    },
    (a) => {
      a.artifact.expired = true;
    },
    (a) => {
      a.artifact.expires_at = "2020-01-01T00:00:00Z";
    },
    (a) => {
      a.artifact.expires_at = "invalid";
    },
    (a) => {
      a.artifact.workflow_run.id = 1;
    },
    (a) => {
      a.artifact.workflow_run.head_repository_id = 2;
    },
    (a) => {
      a.pkg.id = 1;
    },
    (a) => {
      a.pkg.visibility = "public";
    },
    (a) => {
      a.pkg.repository.id = 1;
    },
    (a) => {
      a.comparison.status = "diverged";
    },
    (a) => {
      a.comparison.status = "behind";
    },
    (a) => {
      a.comparison.merge_base_commit.sha = "f".repeat(40);
    },
  ];
  for (const mutate of mutations) {
    const a = accepted();
    mutate(a);
    expect(() => check(a)).toThrow();
  }
});
test("an API omission is unknown, not permission or producer acceptance", () => {
  for (const key of ["repository", "run", "jobs", "artifact", "pkg"] as const) {
    const a: any = accepted();
    a[key] = null;
    expect(() => check(a)).toThrow();
  }
  const malformed: any = accepted();
  malformed.artifact.expires_at = ["2099-01-01T00:00:00Z"];
  expect(() => check(malformed)).toThrow();
});
test("unavailable job-token association metadata remains an explicit view rather than verified association", () => {
  const a: any = accepted();
  expect(check(a).packageRepositoryView).toBe("object");
  a.pkg.repository = null;
  expect(check(a).packageRepositoryView).toBe("null");
  delete a.pkg.repository;
  expect(check(a).packageRepositoryView).toBe("missing");
});
test("persisted platform and account receipts exclude synthetic secrets at every API nesting level", () => {
  const secret = "inactive-synthetic-clone-token";
  const a: any = accepted();
  const poison = (value: any) => {
    if (!value || typeof value !== "object") return;
    Object.values(value).forEach(poison);
    if (!Array.isArray(value)) {
      value.temp_clone_token = secret;
      value.unexpected = { nestedCredential: secret };
    }
  };
  poison(a);
  const receipt = check(a);
  const account = accountAssociation((path) => {
    if (path === "user") return { login: "headironc", token: secret };
    if (path === "users/headironc/packages/container/nucleus-ci") return a.pkg;
    throw new Error("Unexpected API read");
  });
  for (const value of [receipt, account]) {
    const json = JSON.stringify(value);
    expect(json).not.toContain(secret);
    expect(json).not.toContain("temp_clone_token");
    expect(json).not.toContain("unexpected");
    expect(value.package).toEqual({
      id: 16266672,
      name: "nucleus-ci",
      visibility: "private",
      repository: { id: 1432619931, full_name: "headironc/nucleus" },
    });
  }
  expect(receipt.jobs.map((job) => job.id)).toEqual([
    123763896679, 123771833610,
  ]);
  expect(receipt.artifact.workflow_run.head_sha).toBe(selected.source);
});
test("malformed API JSON cannot leak its body through a parser diagnostic", async () => {
  const directory = mkdtempSync(join(tmpdir(), "nucleus-safe-api-"));
  directories.push(directory);
  const gh = join(directory, "gh");
  writeFileSync(
    gh,
    "#!/bin/sh\nprintf '%s' '{\"temp_clone_token\":\"inactive-synthetic-api-secret\",'\n",
  );
  chmodSync(gh, 0o700);
  const child = spawn(
    process.execPath,
    [
      "-e",
      `import {api} from ${JSON.stringify(resolve(import.meta.dir, "publication.ts"))};try{api("repos/headironc/nucleus");}catch(error){console.log(error.message);}`,
    ],
    {
      env: {
        ...process.env,
        PATH: `${directory}:${process.env.PATH}`,
        GH_TOKEN: "inactive-fixture-only",
      },
      stdio: ["ignore", "pipe", "pipe"],
    },
  );
  let output = "";
  child.stdout.on("data", (chunk) => (output += chunk));
  child.stderr.on("data", (chunk) => (output += chunk));
  const status = await new Promise<number | null>((done, reject) => {
    child.once("error", reject);
    child.once("close", done);
  });
  expect(status).toBe(0);
  expect(output).toContain(
    "GitHub returned invalid JSON: repos/headironc/nucleus",
  );
  expect(output).not.toContain("inactive-synthetic-api-secret");
});
test("independent account receipts require a visible exact association and the verified owner", () => {
  for (const repository of [
    null,
    undefined,
    { id: 1, full_name: "other/repo" },
  ]) {
    const pkg = { ...accepted().pkg, repository };
    expect(() =>
      accountAssociation((path) =>
        path === "user" ? { login: "headironc" } : pkg,
      ),
    ).toThrow();
  }
  expect(() => accountAssociation(() => ({ login: "another-owner" }))).toThrow(
    "verified owner",
  );
});
test("supplied package associations reject conflicts, incomplete identities and malformed structures", () => {
  for (const association of [
    {},
    { id: 1432619931 },
    { full_name: "headironc/nucleus" },
    { id: 1, full_name: "headironc/nucleus" },
    { id: 1432619931, full_name: "other/nucleus" },
    [],
    "headironc/nucleus",
    false,
    0,
  ]) {
    const a: any = accepted();
    a.pkg.repository = association;
    expect(() => check(a)).toThrow(
      "Existing private package association is not ready: ",
    );
  }
});
test("a rejected job-token package response reports only the identity fields needed for recovery", () => {
  const a: any = accepted();
  a.pkg.repository = null;
  a.pkg.visibility = "public";
  a.pkg.credentials = "inactive-secret-fixture";
  let message = "";
  try {
    check(a);
  } catch (error) {
    message = (error as Error).message;
  }
  expect(message).toStartWith(
    "Existing private package association is not ready: ",
  );
  expect(JSON.parse(message.slice(message.indexOf("{")))).toEqual({
    id: 16266672,
    name: "nucleus-ci",
    visibility: "public",
    repositoryView: "null",
    repositoryId: null,
    repositoryName: null,
  });
  expect(message).not.toContain("inactive-secret-fixture");
  a.pkg.name = { temp_clone_token: "inactive-secret-fixture" };
  expect(() => check(a)).toThrow("association is not ready");
  try {
    check(a);
  } catch (error) {
    expect((error as Error).message).not.toContain("inactive-secret-fixture");
  }
});

function cacheFixture(occupied = false, refused = false) {
  const first = "sha256:" + "a".repeat(64);
  const untagged = "sha256:" + "b".repeat(64);
  const state = {
    rows: [
      { ID: first, Repository: "example/preload", Tag: "one" },
      { ID: first, Repository: "example/preload", Tag: "two" },
      { ID: untagged, Repository: "<none>", Tag: "<none>" },
    ],
    calls: [] as string[][],
    afterRemoval: () => {},
  };
  const docker = (args: string[]) => {
    state.calls.push(args);
    if (args[0] === "context")
      return args[1] === "show" ? "default" : "unix:///var/run/docker.sock";
    if (args[0] === "container") return occupied ? "c".repeat(64) : "";
    if (args[1] === "ls")
      return state.rows.map((r) => JSON.stringify(r)).join("\n");
    const target = args.at(-1)!;
    if (args[1] === "inspect") {
      const row = state.rows.find((r) => `${r.Repository}:${r.Tag}` === target);
      if (!row) throw new Error("Image absent");
      return row.ID;
    }
    if (args[1] === "rm") {
      if (refused) throw new Error("Removal refused");
      state.rows = state.rows.filter(
        (r) => r.ID !== target && `${r.Repository}:${r.Tag}` !== target,
      );
      state.afterRemoval();
      return "";
    }
    throw new Error("Unexpected Docker command");
  };
  const output = mkdtempSync(join(tmpdir(), "nucleus-cold-cache-"));
  directories.push(output);
  const env = {
    ...context,
    GITHUB_JOB: "cold",
    GITHUB_WORKFLOW_REF:
      "headironc/nucleus/.github/workflows/halo-publish.yml@refs/heads/dev",
    GITHUB_RUN_ID: "123",
    GITHUB_RUN_ATTEMPT: "1",
    RUNNER_ENVIRONMENT: "github-hosted",
    RUNNER_OS: "Linux",
  };
  return { state, docker, output, env };
}

test("dedicated hosted preparation removes only enumerated preloads and proves an empty cache", () => {
  const f = cacheFixture();
  (f.state.rows[0] as any).temp_clone_token = "inactive-cache-metadata-secret";
  const receipt = prepareColdCache(f.output, f.docker, f.env);
  expect(JSON.stringify(receipt)).not.toContain(
    "inactive-cache-metadata-secret",
  );
  expect(receipt.state).toBe("prepared");
  expect(receipt.imagesBefore).toHaveLength(3);
  expect(receipt.imagesAfter).toEqual([]);
  expect(receipt.containersAfter).toEqual([]);
  expect(receipt.attempts.map((x) => x.target)).toEqual([
    "example/preload:one",
    "example/preload:two",
    "sha256:" + "b".repeat(64),
  ]);
  for (const args of f.state.calls) {
    expect(args).not.toContain("--force");
    expect(args).not.toContain("prune");
  }
});
test("cold cache preparation refuses containers and preserves all images", () => {
  const f = cacheFixture(true);
  expect(() => prepareColdCache(f.output, f.docker, f.env)).toThrow(
    "has containers",
  );
  expect(f.state.rows).toHaveLength(3);
  expect(f.state.calls.some((a) => a[1] === "rm")).toBe(false);
});
test("cache cleanup refusal preserves failure evidence without retry or force", () => {
  const f = cacheFixture(false, true);
  expect(() => prepareColdCache(f.output, f.docker, f.env)).toThrow(
    "Removal refused",
  );
  expect(f.state.calls.filter((a) => a[1] === "rm")).toHaveLength(1);
  const receipt = JSON.parse(
    readFileSync(join(f.output, "cold-cache.json"), "utf8"),
  );
  expect(receipt.state).toBe("failed");
  expect(receipt.attempts[0].state).toBe("started");
  expect(receipt.imagesAfter).toHaveLength(3);
});
test("cache preparation refuses self-hosted, wrong jobs and remote Docker contexts", () => {
  for (const change of [
    { RUNNER_ENVIRONMENT: "self-hosted" },
    { GITHUB_JOB: "publish" },
    {
      GITHUB_WORKFLOW_REF:
        "headironc/nucleus/.github/workflows/other.yml@refs/heads/dev",
    },
    { DOCKER_HOST: "tcp://elsewhere:2375" },
    { DOCKER_CONTEXT: "remote" },
  ]) {
    const f = cacheFixture();
    expect(() =>
      prepareColdCache(f.output, f.docker, { ...f.env, ...change }),
    ).toThrow();
    expect(f.state.calls).toEqual([]);
  }
  const f = cacheFixture();
  expect(() => prepareColdCache(f.output, () => "remote", f.env)).toThrow(
    "context",
  );
});
test("unexpected new images stop preparation without deleting the new image", () => {
  const f = cacheFixture();
  const id = "sha256:" + "d".repeat(64);
  f.state.afterRemoval = () => {
    if (!f.state.rows.some((r) => r.ID === id))
      f.state.rows.push({ ID: id, Repository: "unexpected/new", Tag: "one" });
  };
  expect(() => prepareColdCache(f.output, f.docker, f.env)).toThrow(
    "unowned image",
  );
  expect(
    f.state.calls.filter((a) => a[1] === "rm").some((a) => a.at(-1) === id),
  ).toBe(false);
});
test("an already empty dedicated cache needs no removal", () => {
  const f = cacheFixture();
  f.state.rows = [];
  expect(prepareColdCache(f.output, f.docker, f.env).state).toBe("prepared");
  expect(f.state.calls.some((a) => a[1] === "rm")).toBe(false);
});
test("pull receipts preserve digest and layer progress without copying arbitrary subprocess output", () => {
  const result = pullReceipt({
    status: 0,
    stdout: Buffer.from(
      `aaaaaaaaaaaa: Pulling fs layer\naaaaaaaaaaaa: Pull complete\nDigest: ${selected.manifest}\ninactive-synthetic-credential\n`,
    ),
  });
  expect(result.digest).toBe(selected.manifest);
  expect(result.progress).toEqual([
    { layer: "aaaaaaaaaaaa", state: "Pulling fs layer" },
    { layer: "aaaaaaaaaaaa", state: "Pull complete" },
  ]);
  expect(JSON.stringify(result)).not.toContain("inactive-synthetic-credential");
});
test("registry bytes match the original manifest and all changed bytes conflict", () => {
  expect(hash(manifest)).toBe(
    "sha256:80ae416d4bc7287532c0974d25390c5bc9b27cd62defe4e64860b501414b4592",
  );
  expect(remoteState(same)).toBe("same");
  expect(() =>
    remoteState({
      ...same,
      stdout: Buffer.concat([manifest, Buffer.from("\n")]),
    }),
  ).toThrow("conflict");
  expect(remoteState(absent)).toBe("absent");
});
test("403, authentication, transport and ambiguous failures never authorize a write", async () => {
  for (const stderr of [
    "denied: 403",
    "unauthorized: 401",
    "connection closed",
    "request timeout",
    "manifest unknown: forbidden",
    "HTTP 429",
    "",
  ]) {
    let copied = 0;
    await expect(
      publish(
        async () => ({ ...absent, stderr }),
        async () => {
          copied++;
        },
      ),
    ).rejects.toThrow("unknown");
    expect(copied).toBe(0);
  }
});
test("same version is reused; a new publication writes once and reads back after original settlement", async () => {
  let copies = 0,
    reads = 0,
    settled = false;
  expect(
    await publish(
      async () => same,
      async () => {
        copies++;
      },
    ),
  ).toEqual({ publication: "reused" });
  expect(copies).toBe(0);
  const result = await publish(
    async () => {
      reads++;
      if (reads === 1) return absent;
      expect(settled).toBe(true);
      return same;
    },
    async () => {
      copies++;
      await Promise.resolve();
      settled = true;
    },
  );
  expect(result).toEqual({ publication: "published" });
  expect(copies).toBe(1);
  expect(reads).toBe(2);
});
test("settled failed writes recover by exact readback without uploading again", async () => {
  let reads = 0,
    copies = 0;
  const result = await publish(
    async () => (++reads === 1 ? absent : same),
    async () => {
      copies++;
      throw new SettledCopyFailure("CLI failed after remote commit");
    },
  );
  expect(result.publication).toBe("recovered-by-readback");
  expect(copies).toBe(1);
});
test("absence after failure, conflicts, failed readback and unresolved original lifetime stop retries", async () => {
  for (const final of [
    absent,
    { ...same, stdout: Buffer.from("another manifest") },
    { ...absent, stderr: "403 denied" },
  ]) {
    let reads = 0,
      copies = 0;
    await expect(
      publish(
        async () => (++reads === 1 ? absent : final),
        async () => {
          copies++;
          throw new SettledCopyFailure("failed");
        },
      ),
    ).rejects.toThrow();
    expect(reads).toBe(2);
    expect(copies).toBe(1);
  }
  let reads = 0;
  await expect(
    publish(
      async () => {
        reads++;
        return absent;
      },
      async () => {
        throw Error("Original container state unknown");
      },
    ),
  ).rejects.toThrow("unknown");
  expect(reads).toBe(1);
});
test("private authfile is scoped, mode 0600 and removed on successful and failed work", async () => {
  const previous = process.env.PUBLICATION_AUTH_ROOT;
  delete process.env.PUBLICATION_AUTH_ROOT;
  try {
    for (const failed of [false, true]) {
      let file = "";
      const work = withAuth(
        async (path) => {
          file = path;
          expect(statSync(path).mode & 0o777).toBe(0o600);
          expect(
            Object.keys(JSON.parse(readFileSync(path, "utf8")).auths),
          ).toEqual(["ghcr.io"]);
          if (failed) throw Error("controlled failure");
          return "done";
        },
        "inactive-test-only",
        "actor",
      );
      if (failed) await expect(work).rejects.toThrow("controlled");
      else expect(await work).toBe("done");
      expect(existsSync(file)).toBe(false);
    }
  } finally {
    if (previous === undefined) delete process.env.PUBLICATION_AUTH_ROOT;
    else process.env.PUBLICATION_AUTH_ROOT = previous;
  }
});
test("pulled daemon reference is separate from config but bound to registry digest and RootFS", () => {
  const local = {
    Id: "sha256:" + "f".repeat(64),
    Os: "linux",
    Architecture: "amd64",
    RepoDigests: [image],
    RootFS: { Layers: ["sha256:" + "b".repeat(64)] },
  };
  expect(pulledReference(local, local.RootFS.Layers)).toBe(local.Id);
  for (const change of [
    { RepoDigests: [] },
    { RepoDigests: [image, { temp_clone_token: "inactive-inspect-secret" }] },
    { Architecture: "arm64" },
    { Id: "latest" },
    { RootFS: { Layers: [] } },
  ])
    expect(() =>
      pulledReference({ ...local, ...change }, local.RootFS.Layers),
    ).toThrow();
});
test("workflow keeps producer code and writable package credentials in separate jobs", () => {
  const workflow: any = Bun.YAML.parse(
    readFileSync(resolve(root, ".github/workflows/halo-publish.yml"), "utf8"),
  );
  expect(Object.keys(workflow.on)).toEqual(["workflow_dispatch"]);
  expect(workflow.concurrency["cancel-in-progress"]).toBe(false);
  const { preflight, publish, cold } = workflow.jobs;
  expect(preflight.permissions.packages).toBe("read");
  expect(publish.permissions.packages).toBe("write");
  expect(cold.permissions.packages).toBe("read");
  expect(publish.needs).toBe("preflight");
  expect(cold.needs).toBe("publish");
  expect(
    [preflight, publish, cold].every(
      (job) => job["runs-on"] === "ubuntu-24.04",
    ),
  ).toBe(true);
  const checkouts = publish.steps.filter((s: any) =>
    s.uses?.startsWith("actions/checkout@"),
  );
  expect(checkouts.length).toBe(1);
  expect(checkouts[0].with.ref).toBe("${{ github.workflow_sha }}");
  expect(checkouts[0].with["sparse-checkout"].trim().split("\n")).toEqual([
    ".github/halo-publish",
    ".tool-versions",
  ]);
  for (const job of [preflight, publish]) {
    const download = job.steps.find((s: any) =>
      s.uses?.startsWith("actions/download-artifact@"),
    );
    expect(download.with["artifact-ids"]).toBe("12938521777");
    expect(download.with["run-id"]).toBe("40964056245");
    expect(download.with["digest-mismatch"]).toBe("error");
  }
  for (const job of [publish, cold])
    expect(
      job.steps.some(
        (s: any) =>
          s.if === "always()" && s.name === "Remove job registry credentials",
      ),
    ).toBe(true);
});
