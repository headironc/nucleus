import { expect, test } from "bun:test";
import { data, Evaluator, Lexer, Parser } from "@actions/expressions";
import { truthy } from "@actions/expressions/result";
import {
  readFileSync,
  mkdirSync,
  mkdtempSync,
  writeFileSync,
  rmSync,
  renameSync,
  chmodSync,
  symlinkSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import { EvidenceApi } from "./evidence.test.ts";
import { b } from "./fixtures.ts";
import { snapshot } from "./evidence.ts";
import { approvalBody } from "./approve.ts";
import {
  approveCandidate,
  invalidateCandidate,
  prepareIntent,
} from "./main.ts";
import type { Intent } from "./intent.ts";

const bot = { id: 300000001, type: "Bot", login: "cladiron[bot]" };
class WriterApi extends EvidenceApi {
  puts: number[] = [];
  posts = 0;
  failDismiss = false;
  override async pages(path: string, key?: string) {
    if (path.includes("/actions/workflows/review-approval.yml/")) return [];
    return super.pages(path, key);
  }
  override async get(path: string): Promise<any> {
    if (path === "/users/cladiron%5Bbot%5D") return bot;
    if (/\/reviews\/\d+$/.test(path))
      return this.f.reviews.find((r) => path.endsWith(`/${r.id}`));
    return super.get(path);
  }
  override async put(path = "") {
    const id = Number(path.split("/").at(-2));
    this.puts.push(id);
    if (this.failDismiss) throw new Error("Unknown dismissal");
    this.f.reviews.find((r) => r.id === id)!.state = "DISMISSED";
  }
  override async post() {
    this.posts++;
    throw new Error("Unexpected approval POST");
  }
}
const workflow: any = Bun.YAML.parse(
  readFileSync(
    new URL("../workflows/review-approval.yml", import.meta.url),
    "utf8",
  ),
);
function fallback(
  writer: boolean,
  intent = false,
  failed = true,
  mode = "approve",
) {
  const step = workflow.jobs.approval.steps.find(
    (s: any) =>
      s.name === "Withdraw this App's prior approvals after failed evidence",
  );
  const failure = {
    name: "failure",
    minArgs: 0,
    maxArgs: 0,
    call: () => new data.BooleanData(failed),
  };
  const parsed = new Parser(
    new Lexer(step.if.slice(3, -2)).lex().tokens,
    ["steps", "inputs"],
    [failure],
  ).parse();
  const context = JSON.parse(
    JSON.stringify({
      inputs: { mode },
      steps: {
        intent: { outputs: { invalidate: String(intent) } },
        writer: { outputs: { invalidate: String(writer) } },
      },
    }),
    data.reviver,
  );
  return truthy(
    new Evaluator(parsed, context, new Map([["failure", failure]])).evaluate(),
  );
}
async function fixture() {
  const api = new WriterApi();
  const current = await snapshot(api, b.pr);
  const planned: Intent = {
    version: 1,
    repositoryId: 1432619931,
    run: 700,
    attempt: 1,
    b,
    digest: current.digest,
  };
  api.f.reviews.push({
    id: 901,
    user: bot,
    performed_via_github_app: { id: 3104501 },
    state: "APPROVED",
    commit_id: b.head,
    body: approvalBody(current, "run-699-attempt-1"),
    submitted_at: "2026-10-30T14:00:00Z",
  });
  api.f.reviews.push({
    id: 902,
    user: { id: 1, type: "User", login: "other" },
    state: "APPROVED",
    commit_id: b.head,
    body: "Human approval",
    submitted_at: "2026-10-30T14:00:01Z",
  });
  let invalidation = false,
    minted = 0,
    revoked = 0,
    persisted = 0;
  const controls = {
    invalidate: () => {
      invalidation = true;
    },
    persist: async () => {
      persisted++;
    },
    mint: async () => {
      minted++;
      return {
        client: api,
        revoke: async () => {
          revoked++;
        },
      };
    },
  };
  return {
    api,
    planned,
    controls,
    counts: () => ({ invalidation, minted, revoked, persisted }),
  };
}
for (const changed of ["thread", "CI", "provider"] as const)
  test(`successful intent then changed ${changed} delegates only trusted withdrawal before mint`, async () => {
    const f = await fixture();
    if (changed === "thread") {
      f.api.afterPage = true;
      f.api.unresolved = true;
    }
    if (changed === "CI") f.api.failRun = true;
    if (changed === "provider") f.api.f.comments[2].body += "\nUnknown finding";
    await expect(
      approveCandidate(f.api, f.planned, b.base, {}, f.controls),
    ).rejects.toThrow();
    expect(f.counts()).toEqual({
      invalidation: true,
      minted: 0,
      revoked: 0,
      persisted: 1,
    });
    expect(f.api.posts).toBe(0);
    expect(fallback(f.counts().invalidation)).toBe(true);
    expect(
      await invalidateCandidate(f.api, b.pr, b.base, f.controls.mint),
    ).toEqual([901]);
    expect(f.api.puts).toEqual([901]);
    expect(f.api.f.reviews.find((r) => r.id === 902)!.state).toBe("APPROVED");
    expect(f.counts().minted).toBe(1);
    expect(f.counts().revoked).toBe(1);
  });
test("executor changes block both writer and dedicated withdrawal before token creation", async () => {
  const f = await fixture();
  f.api.changedProducer = true;
  await expect(
    approveCandidate(f.api, f.planned, b.base, {}, f.controls),
  ).rejects.toThrow();
  await expect(
    invalidateCandidate(f.api, b.pr, b.base, f.controls.mint),
  ).rejects.toThrow();
  expect(f.counts().invalidation).toBe(false);
  expect(f.counts().minted).toBe(0);
  expect(f.api.posts).toBe(0);
  expect(f.api.puts).toEqual([]);
});
for (const operation of ["prepare", "approve"])
  test(`null security with a trusted abbreviated receipt blocks ${operation} before mint`, async () => {
    const f = await fixture();
    const summary = f.api.f.comments.find((c) => c.id === 201);
    summary.body = summary.body.replace(
      /"security_review":\{"commit_sha":"[a-f0-9]{40}","has_any_findings":false,"has_unresolved_findings":false/,
      `"security_review":{"commit_sha":"${b.head}","has_any_findings":null,"has_unresolved_findings":null`,
    );
    const result =
      operation === "prepare"
        ? prepareIntent(f.api, b.pr, b.base, {}, 700, 1, f.controls.invalidate)
        : approveCandidate(f.api, f.planned, b.base, {}, f.controls);
    await expect(result).rejects.toThrow("unknown for the full head");
    expect(f.counts()).toEqual({
      invalidation: true,
      minted: 0,
      revoked: 0,
      persisted: operation === "approve" ? 1 : 0,
    });
    expect(f.api.posts).toBe(0);
    expect(f.api.puts).toEqual([]);
  });
for (const field of ["head", "merge"])
  test(`trusted ${field} drift invalidates evidence before mint and withdraws only prior App approval`, async () => {
    const f = await fixture();
    const changed = "3".repeat(40);
    if (field === "head") f.api.state.head.sha = changed;
    else {
      f.api.state.merge_commit_sha = changed;
      const get = f.api.get.bind(f.api);
      f.api.get = async (path) =>
        path.endsWith(`/git/commits/${changed}`)
          ? {
              ...(await get(`/repos/headironc/nucleus/git/commits/${b.merge}`)),
              sha: changed,
            }
          : get(path);
    }
    await expect(
      approveCandidate(f.api, f.planned, b.base, {}, f.controls),
    ).rejects.toThrow("Candidate changed");
    expect(f.counts()).toEqual({
      invalidation: true,
      minted: 0,
      revoked: 0,
      persisted: 1,
    });
    expect(fallback(f.counts().invalidation)).toBe(true);
    expect(
      await invalidateCandidate(f.api, b.pr, b.base, f.controls.mint),
    ).toEqual([901]);
    expect(f.api.puts).toEqual([901]);
    expect(f.api.posts).toBe(0);
    expect(f.counts().revoked).toBe(1);
  });
test("changed baseline or failed trust reread never delegates cleanup", async () => {
  for (const cause of ["base", "reread"] as const) {
    const f = await fixture();
    if (cause === "base") f.api.state.base.sha = "3".repeat(40);
    else {
      f.api.state.head.sha = "3".repeat(40);
      const get = f.api.get.bind(f.api);
      let reads = 0;
      f.api.get = async (path) => {
        if (path.endsWith(`/pulls/${b.pr}`) && ++reads === 2)
          throw new Error("Trust read unavailable");
        return get(path);
      };
    }
    await expect(
      approveCandidate(f.api, f.planned, b.base, {}, f.controls),
    ).rejects.toThrow();
    expect(f.counts().invalidation).toBe(false);
    expect(f.counts().minted).toBe(0);
    expect(f.api.puts).toEqual([]);
  }
});
test("intent identity failure is outside evidence invalidation, while evidence failure signals trusted cleanup", async () => {
  const f = await fixture();
  f.api.f.comments[2].body += "\nUnknown provider finding";
  await expect(
    prepareIntent(f.api, b.pr, b.base, {}, 0, 1, f.controls.invalidate),
  ).rejects.toThrow("Invalid operation intent run");
  expect(f.counts().invalidation).toBe(false);
  await expect(
    prepareIntent(f.api, b.pr, b.base, {}, 700, 1, f.controls.invalidate),
  ).rejects.toThrow();
  expect(f.counts().invalidation).toBe(true);
  expect(f.counts().minted).toBe(0);
  expect(f.api.posts).toBe(0);
});
test("unknown historical executions block a new approval without an evidence-invalidation signal", async () => {
  for (const cause of ["unavailable", "unterminated"] as const) {
    const f = await fixture();
    const pages = f.api.pages.bind(f.api);
    f.api.pages = async (path, key) => {
      if (path.includes("/actions/workflows/review-approval.yml/")) {
        if (cause === "unavailable")
          throw new Error("Historical audit unavailable");
        return [
          {
            id: 699,
            path: ".github/workflows/review-approval.yml",
            repository: { id: 1432619931 },
            status: "in_progress",
            run_attempt: 1,
          },
        ];
      }
      return pages(path, key);
    };
    await expect(
      approveCandidate(f.api, f.planned, b.base, {}, f.controls),
    ).rejects.toThrow();
    expect(f.counts()).toEqual({
      invalidation: false,
      minted: 0,
      revoked: 0,
      persisted: 1,
    });
    expect(f.api.puts).toEqual([]);
  }
});
for (const mutation of [
  "push workflow",
  "changed non-PR workflow",
  "deleted workflow",
  "renamed workflow",
  "mode",
  "symlink",
  "missing",
])
  test(`all entrypoints reject ${mutation} tree changes before any credential or review write`, async () => {
    const f = await fixture();
    const directory = mkdtempSync(join(tmpdir(), "workflow-tree-"));
    try {
      const git = (...args: string[]) => {
        const result = spawnSync("git", ["--no-replace-objects", ...args], {
          cwd: directory,
          encoding: "utf8",
          timeout: 10000,
          env: {
            PATH: process.env.PATH,
            GIT_CONFIG_NOSYSTEM: "1",
            GIT_CONFIG_GLOBAL: "/dev/null",
          },
        });
        expect(result.status, result.stderr).toBe(0);
        return result.stdout.trim();
      };
      for (const path of [
        ".github/review-approval",
        ".github/workflows",
        "tools",
      ])
        mkdirSync(join(directory, path), { recursive: true });
      for (const path of [
        ".github/review-approval/main.ts",
        "tools/check-bun.ts",
      ])
        writeFileSync(join(directory, path), "// Synthetic trusted executor\n");
      const workflows = join(directory, ".github/workflows");
      const existing = join(workflows, "lock-pr.yml");
      writeFileSync(existing, "on: workflow_dispatch\n");
      writeFileSync(join(workflows, "publish.yml"), "on: push\n");
      git("init", "-q");
      git("add", "--all");
      const baseTree = git("write-tree");
      const original = f.api.get.bind(f.api);
      let candidateTree = baseTree;
      f.api.get = async (path) => {
        if (path.endsWith(`/git/commits/${b.base}`))
          return { sha: b.base, tree: { sha: baseTree } };
        if (path.endsWith(`/git/commits/${b.merge}`))
          return { sha: b.merge, tree: { sha: candidateTree } };
        if (path.includes("/git/trees/")) {
          const sha = path.split("/").at(-1)!;
          const tree = git("ls-tree", "-z", sha)
            .split("\0")
            .filter(Boolean)
            .map((line) => {
              const [, mode, type, oid, path] = line.match(
                /^(\d+) (\w+) ([a-f0-9]+)\t(.+)$/,
              )!;
              return { path, mode, type, sha: oid };
            });
          return { sha, truncated: false, tree };
        }
        return original(path);
      };
      const { trustedCandidate } = await import("./main.ts");
      expect(await trustedCandidate(f.api, b.pr, b.base)).toEqual(b);
      const injected =
        "on:\n  push:\n    branches: [dev]\njobs:\n  expose:\n    environment: dev-admission\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo synthetic\n        env:\n          APP_KEY: ${{ secrets.NUCLEUS_DEV_PRIVATE_KEY }}\n";
      if (mutation === "push workflow")
        writeFileSync(join(workflows, "push.yml"), injected);
      if (mutation === "changed non-PR workflow")
        writeFileSync(existing, injected);
      if (mutation === "deleted workflow") rmSync(existing);
      if (mutation === "renamed workflow")
        renameSync(existing, join(workflows, "renamed.yml"));
      if (mutation === "mode") chmodSync(existing, 0o755);
      if (mutation === "missing" || mutation === "symlink")
        rmSync(workflows, { recursive: true });
      if (mutation === "symlink") symlinkSync("synthetic-target", workflows);
      git("add", "--all");
      candidateTree = git("write-tree");
      expect(candidateTree).not.toBe(baseTree);
      // Intent and both write entrypoints consume actual Git tree identities, not YAML scans.
      await expect(trustedCandidate(f.api, b.pr, b.base)).rejects.toThrow();
      await expect(
        approveCandidate(f.api, f.planned, b.base, {}, f.controls),
      ).rejects.toThrow();
      await expect(
        invalidateCandidate(f.api, b.pr, b.base, f.controls.mint),
      ).rejects.toThrow();
      expect(f.counts().invalidation).toBe(false);
      expect(f.counts().minted).toBe(0);
      expect(f.api.posts).toBe(0);
      expect(f.api.puts).toEqual([]);
    } finally {
      rmSync(directory, { recursive: true, force: true });
    }
  });
for (const malformed of ["missing", "symlink", "mode"])
  test(`an equally malformed trusted and candidate workflow ${malformed} is never accepted`, async () => {
    const f = await fixture();
    const github = f.api.trees["d".repeat(40)];
    const workflows = github.find((e) => e.path === "workflows");
    if (malformed === "missing") github.splice(github.indexOf(workflows), 1);
    if (malformed === "symlink")
      Object.assign(workflows, { type: "blob", mode: "120000" });
    if (malformed === "mode") workflows.mode = "100644";
    await expect(
      approveCandidate(f.api, f.planned, b.base, {}, f.controls),
    ).rejects.toThrow();
    await expect(
      invalidateCandidate(f.api, b.pr, b.base, f.controls.mint),
    ).rejects.toThrow();
    expect(f.counts().minted).toBe(0);
    expect(f.api.puts).toEqual([]);
  });
test("execution trust is rechecked after a failed preflight and before its cleanup signal", async () => {
  const f = await fixture();
  const original = f.api.graphql.bind(f.api);
  f.api.graphql = async (...args) => {
    f.api.changedProducer = true;
    return original(...args);
  };
  f.api.afterPage = true;
  f.api.unresolved = true;
  await expect(
    approveCandidate(f.api, f.planned, b.base, {}, f.controls),
  ).rejects.toThrow();
  expect(f.counts().invalidation).toBe(false);
  expect(f.counts().minted).toBe(0);
  expect(f.api.puts).toEqual([]);
});
for (const phase of ["mint", "approve", "persist", "revoke"] as const)
  test(`${phase} failure after preflight does not enable a second generic withdrawal`, async () => {
    const f = await fixture();
    const mint = f.controls.mint;
    f.controls.mint = async () => {
      if (phase === "mint") throw new Error("Mint failed");
      const credential = await mint();
      if (phase === "approve") {
        f.api.failRun = true;
        f.api.failDismiss = true;
      }
      if (phase === "revoke")
        credential.revoke = async () => {
          throw new Error("Revoke failed");
        };
      return credential;
    };
    if (phase === "persist")
      f.controls.persist = async () => {
        throw new Error("Persist failed");
      };
    await expect(
      approveCandidate(f.api, f.planned, b.base, {}, f.controls),
    ).rejects.toThrow();
    expect(f.counts().invalidation).toBe(false);
    expect(fallback(f.counts().invalidation)).toBe(false);
    expect(f.api.posts).toBe(0);
    expect(f.api.puts).toEqual(phase === "approve" ? [901] : []);
    if (phase === "approve" || phase === "persist")
      expect(f.counts().revoked).toBe(1);
  });
test("actual workflow fallback requires a failing approve path with a scoped intent or writer signal", () => {
  const writer = workflow.jobs.approval.steps.find(
    (s: any) => s.name === "Verify evidence and perform selected operation",
  );
  expect(writer.id).toBe("writer");
  expect(fallback(false, true)).toBe(true);
  expect(fallback(true)).toBe(true);
  expect(fallback(false)).toBe(false);
  expect(fallback(true, false, false)).toBe(false);
  expect(fallback(true, false, true, "prepare")).toBe(false);
});
