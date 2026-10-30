import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { binding, snapshot, threads } from "./evidence.ts";
import { b, providerFixture } from "./fixtures.ts";
import { stableWorkflows } from "./ci.ts";
import type { Api } from "./github.ts";
import type { Record } from "./providers.ts";
import { trustedCandidate } from "./main.ts";

export class EvidenceApi implements Api {
  f = providerFixture();
  state: any = {
    number: b.pr,
    state: "open",
    draft: false,
    merged: false,
    mergeable: true,
    merge_commit_sha: b.merge,
    changed_files: 1,
    base: { sha: b.base, ref: "dev", repo: { id: 1432619931 } },
    head: { sha: b.head, ref: "codex/fixture", repo: { id: 1432619931 } },
  };
  tree = "e".repeat(40);
  files: any[] = [{ filename: "docs/example.md" }];
  missingReceipt = false;
  failRun = false;
  changedProducer = false;
  unresolved = false;
  outdated = false;
  afterPage = false;
  nestedPage = false;
  calls: string[] = [];
  trees: { [sha: string]: any[] } = {
    ["b".repeat(40)]: [
      { path: ".github", type: "tree", mode: "040000", sha: "d".repeat(40) },
      { path: "tools", type: "tree", mode: "040000", sha: "f".repeat(40) },
      ...["package.json", "bun.lock", "tsconfig.json", ".tool-versions"].map(
        (path) => ({ path, type: "blob", mode: "100644", sha: "0".repeat(40) }),
      ),
    ],
    ["d".repeat(40)]: [
      {
        path: "review-approval",
        type: "tree",
        mode: "040000",
        sha: "c".repeat(40),
      },
      { path: "workflows", type: "tree", mode: "040000", sha: "a".repeat(40) },
    ],
    ["a".repeat(40)]: [
      {
        path: "review-approval.yml",
        type: "blob",
        mode: "100644",
        sha: "0".repeat(40),
      },
    ],
    ["f".repeat(40)]: [
      {
        path: "check-bun.ts",
        type: "blob",
        mode: "100644",
        sha: "0".repeat(40),
      },
    ],
  };
  runs = stableWorkflows.map((f, i) => ({
    id: 10 + i,
    path: `.github/workflows/${f}`,
    head_sha: b.head,
    event: "pull_request",
    run_number: 20,
    run_attempt: 1,
    check_suite_id: 100 + i,
    repository: { id: 1432619931 },
    head_repository: { id: 1432619931 },
    status: "completed",
    conclusion: "success",
  }));
  async get(path: string): Promise<any> {
    this.calls.push(path);
    if (path.endsWith(`/pulls/${b.pr}`)) return this.state;
    if (path.includes("/git/ref/heads/dev")) return { object: { sha: b.base } };
    if (path.includes("/compare/"))
      return { status: "ahead", merge_base_commit: { sha: b.base } };
    if (path.endsWith(`/git/commits/${b.base}`))
      return { sha: b.base, tree: { sha: "b".repeat(40) } };
    if (path.includes("/git/commits/"))
      return {
        sha: b.merge,
        parents: [{ sha: b.base }, { sha: b.head }],
        tree: { sha: this.tree },
      };
    if (path.includes("/git/trees/")) {
      const sha = path.split("/").at(-1)!;
      let entries = this.trees[sha];
      if (sha === this.tree)
        entries = this.trees[this.tree] ?? this.trees["b".repeat(40)];
      if (this.changedProducer && sha === this.tree) {
        entries = structuredClone(entries);
        entries.find((e: any) => e.path === ".github").sha = "8".repeat(40);
        this.trees["8".repeat(40)] = this.trees["d".repeat(40)].map((e) =>
          e.path === "review-approval" ? { ...e, sha: "9".repeat(40) } : e,
        );
      }
      return { sha, truncated: false, tree: entries };
    }
    if (path.includes("/contents/.github/workflows?"))
      return stableWorkflows
        .map((name) => ({
          name,
          path: `.github/workflows/${name}`,
          type: "file",
        }))
        .concat([
          {
            name: "halo-image.yml",
            path: ".github/workflows/halo-image.yml",
            type: "file",
          },
        ]);
    if (path.includes("/contents/")) {
      const file = path.split("/contents/")[1].split("?")[0];
      return {
        type: "file",
        encoding: "base64",
        sha:
          this.changedProducer && path.includes(`ref=${b.merge}`)
            ? "different"
            : "same",
        content: Buffer.from(
          readFileSync(new URL(`../../${file}`, import.meta.url), "utf8"),
        ).toString("base64"),
      };
    }
    if (path.includes("/actions/runs/"))
      return {
        ...this.runs.find((r) => path.endsWith(`/${r.id}`)),
        ...(this.failRun ? { conclusion: "failure" } : {}),
      };
    throw new Error(`Unexpected get ${path}`);
  }
  async pages(path: string, key?: string): Promise<any[]> {
    this.calls.push(path);
    if (path.includes("/issues/")) return this.f.comments;
    if (path.endsWith("/reviews")) return this.f.reviews;
    if (path.endsWith("/comments")) return this.afterPage ? [{ id: 999 }] : [];
    if (path.endsWith("/files")) return this.files;
    if (path.includes("/commits/")) return this.f.checks;
    if (key === "workflow_runs") return this.runs;
    const index =
      Number(path.match(/\/(?:runs|check-suites)\/(\d+)/)?.[1]) -
      (key === "check_runs" ? 100 : 10);
    if (key === "jobs") {
      const workflow: any = Bun.YAML.parse(
        readFileSync(
          new URL(`../workflows/${stableWorkflows[index]}`, import.meta.url),
          "utf8",
        ),
      );
      const name = Object.values(workflow.jobs)
        .map((j: any) => j.name)
        .find((n: any) => n?.startsWith("Nucleus "));
      return [
        {
          id: 1000 + index,
          name,
          status: "completed",
          conclusion: "success",
          run_id: 10 + index,
          check_run_url: `https://api.github.com/repos/headironc/nucleus/check-runs/${200 + index}`,
        },
      ];
    }
    if (key === "check_runs")
      return [
        {
          id: 200 + index,
          app: { id: 15368 },
          check_suite: { id: 100 + index },
          conclusion: "success",
        },
      ];
    if (key === "artifacts")
      return this.missingReceipt
        ? []
        : [
            {
              id: 500 + index,
              name: `cladiron-ci-${10 + index}-1`,
              expired: false,
              workflow_run: {
                id: 10 + index,
                repository_id: 1432619931,
                head_repository_id: 1432619931,
                head_sha: b.head,
              },
            },
          ];
    throw new Error(`Unexpected pages ${path}`);
  }
  async graphql(_query: string, variables: Record): Promise<any> {
    if (variables.ids)
      return {
        nodes: variables.ids.map((id: string) => {
          const c = this.f.comments.find((c) => c.node_id === id);
          return {
            id,
            fullDatabaseId: String(c.id),
            createdAt: c.created_at,
            updatedAt: c.updated_at,
            lastEditedAt: c.lastEditedAt,
          };
        }),
      };
    if (variables.id)
      return {
        node: {
          comments: {
            nodes: [{ fullDatabaseId: "1000" }],
            pageInfo: { hasNextPage: false, endCursor: null },
          },
        },
      };
    if (variables.cursor === "after")
      return {
        repository: {
          pullRequest: {
            reviewThreads: {
              nodes: [
                {
                  id: "thread2",
                  isResolved: !this.unresolved,
                  isOutdated: this.outdated,
                  comments: {
                    nodes: [{ fullDatabaseId: "999" }],
                    pageInfo: { hasNextPage: false, endCursor: null },
                  },
                },
              ],
              pageInfo: { hasNextPage: false, endCursor: null },
            },
          },
        },
      };
    return {
      repository: {
        pullRequest: {
          reviewThreads: {
            nodes: this.nestedPage
              ? [
                  {
                    id: "nested",
                    isResolved: true,
                    isOutdated: false,
                    comments: {
                      nodes: [{ fullDatabaseId: "1" }],
                      pageInfo: {
                        hasNextPage: true,
                        endCursor: "nested-after",
                      },
                    },
                  },
                ]
              : [],
            pageInfo: {
              hasNextPage: this.afterPage,
              endCursor: this.afterPage ? "after" : null,
            },
          },
        },
      },
    };
  }
  async receipt(artifact: Record): Promise<any> {
    const index = artifact.id - 500;
    return {
      version: 1,
      repository: "headironc/nucleus",
      repositoryId: 1432619931,
      pr: b.pr,
      target: "dev",
      head: b.head,
      merge: b.merge,
      parents: [b.base, b.head],
      tree: this.tree,
      workflow: `.github/workflows/${stableWorkflows[index]}`,
      run: 10 + index,
      attempt: 1,
    };
  }
  async post() {
    throw new Error("No writes allowed in evidence fixture");
  }
  async put() {
    throw new Error("No writes allowed in evidence fixture");
  }
}
test("complete snapshot consumes all four actual workflow definitions and exact merge receipts", async () => {
  const api = new EvidenceApi();
  const result = await snapshot(api, b.pr);
  expect(result.b).toEqual(b);
  expect(result.evidence.ci).toHaveLength(4);
});
function candidateExecutor(api: EvidenceApi) {
  const root = structuredClone(api.trees["b".repeat(40)]);
  api.trees[api.tree] = root;
  const github = structuredClone(api.trees["d".repeat(40)]);
  root.find((e) => e.path === ".github").sha = "8".repeat(40);
  api.trees["8".repeat(40)] = github;
  const workflows = structuredClone(api.trees["a".repeat(40)]);
  github.find((e) => e.path === "workflows").sha = "7".repeat(40);
  api.trees["7".repeat(40)] = workflows;
  const tools = structuredClone(api.trees["f".repeat(40)]);
  root.find((e) => e.path === "tools").sha = "6".repeat(40);
  api.trees["6".repeat(40)] = tools;
  return { root, github, workflows, tools };
}
const executable = (path: string) => ({
  path,
  type: "blob",
  mode: "100644",
  sha: "9".repeat(40),
});
for (const [label, change] of Object.entries({
  "capability code or membership change": (
    c: ReturnType<typeof candidateExecutor>,
  ) => {
    c.github.find((e) => e.path === "review-approval").sha = "9".repeat(40);
  },
  "capability deletion": (c: ReturnType<typeof candidateExecutor>) => {
    c.github.splice(
      c.github.findIndex((e) => e.path === "review-approval"),
      1,
    );
  },
  "capability symlink": (c: ReturnType<typeof candidateExecutor>) => {
    Object.assign(
      c.github.find((e) => e.path === "review-approval"),
      { type: "blob", mode: "120000" },
    );
  },
  "dispatch workflow": (c: ReturnType<typeof candidateExecutor>) => {
    c.workflows[0].sha = "9".repeat(40);
  },
  "check-bun executable": (c: ReturnType<typeof candidateExecutor>) => {
    c.tools[0].sha = "9".repeat(40);
  },
  "check-bun file mode": (c: ReturnType<typeof candidateExecutor>) => {
    c.tools[0].mode = "100755";
  },
  "root preload configuration": (c: ReturnType<typeof candidateExecutor>) => {
    c.root.push(executable("bunfig.toml"));
  },
  "root automatic environment": (c: ReturnType<typeof candidateExecutor>) => {
    c.root.push(executable(".env.local"));
  },
  "ancestor package configuration": (
    c: ReturnType<typeof candidateExecutor>,
  ) => {
    c.github.push(executable("package.json"));
  },
  "tools runtime configuration": (c: ReturnType<typeof candidateExecutor>) => {
    c.tools.push(executable(".bunfig.toml"));
  },
  "path mapping configuration": (c: ReturnType<typeof candidateExecutor>) => {
    c.root.find((e) => e.path === "tsconfig.json").sha = "9".repeat(40);
  },
  "alternative lockfile": (c: ReturnType<typeof candidateExecutor>) => {
    c.root.push(executable("bun.lockb"));
  },
}))
  test(`executor refuses ${label} before any credential or approval write`, async () => {
    const api = new EvidenceApi();
    change(candidateExecutor(api));
    let external = 0,
      writes = 0;
    const originalFetch = globalThis.fetch;
    globalThis.fetch = Object.assign(
      async () => {
        external++;
        throw new Error("No credential network allowed");
      },
      { preconnect: originalFetch.preconnect },
    );
    api.post = async () => {
      writes++;
      throw new Error("No approval write allowed");
    };
    try {
      await expect(trustedCandidate(api, b.pr, b.base)).rejects.toThrow(
        "executor",
      );
      await expect(snapshot(api, b.pr)).rejects.toThrow("executor");
      expect(external).toBe(0);
      expect(writes).toBe(0);
    } finally {
      globalThis.fetch = originalFetch;
    }
  });
test("truncated executor trees fail closed; unrelated business changes remain eligible", async () => {
  const api = new EvidenceApi();
  candidateExecutor(api).root.push(executable("unrelated-business.rs"));
  expect((await snapshot(api, b.pr)).b).toEqual(b);
  expect(await trustedCandidate(api, b.pr, b.base)).toEqual(b);
  await expect(trustedCandidate(api, b.pr, "0".repeat(40))).rejects.toThrow(
    "candidate's baseline",
  );
  const get = api.get.bind(api);
  api.get = async (path) => {
    const value = await get(path);
    return path.endsWith(`/git/trees/${api.tree}`)
      ? { ...value, truncated: true }
      : value;
  };
  await expect(snapshot(api, b.pr)).rejects.toThrow(
    "Incomplete executor Git tree",
  );
});
for (const [label, mutate] of Object.entries({
  fork: (a: EvidenceApi) => {
    a.state.head.repo.id = 1;
  },
  promotion: (a: EvidenceApi) => {
    a.state.base.ref = "main";
  },
  draft: (a: EvidenceApi) => {
    a.state.draft = true;
  },
  "stacked target": (a: EvidenceApi) => {
    a.state.base.ref = "codex/other";
  },
  "queued head": (a: EvidenceApi) => {
    a.state.head.ref = "gh-readonly-queue/dev/pr-1";
  },
  "latest failure": (a: EvidenceApi) => {
    a.failRun = true;
  },
  "receipt missing": (a: EvidenceApi) => {
    a.missingReceipt = true;
  },
  "CI producer changed": (a: EvidenceApi) => {
    a.changedProducer = true;
  },
  "unresolved thread on later page": (a: EvidenceApi) => {
    a.afterPage = true;
    a.unresolved = true;
  },
  "outdated unresolved thread": (a: EvidenceApi) => {
    a.afterPage = true;
    a.unresolved = true;
    a.outdated = true;
  },
  "applicable dedicated CI missing": (a: EvidenceApi) => {
    a.files = [{ filename: "tools/example.ts" }];
  },
}))
  test(label, async () => {
    const api = new EvidenceApi();
    mutate(api);
    await expect(snapshot(api, b.pr)).rejects.toThrow();
  });
test("resolved later-page and nested comments remain acceptable", async () => {
  const api = new EvidenceApi();
  api.afterPage = true;
  api.nestedPage = true;
  const result = await snapshot(api, b.pr);
  expect(result.evidence.threads).toHaveLength(2);
  expect(result.evidence.threads[0].comments).toEqual([1, 1000]);
});
test("head containing baseline is required even when PR is mergeable", async () => {
  const api = new EvidenceApi();
  const get = api.get.bind(api);
  api.get = async (path) =>
    path.includes("/compare/")
      ? { status: "diverged", merge_base_commit: { sha: "0".repeat(40) } }
      : get(path);
  await expect(binding(api, b.pr)).rejects.toThrow("contain baseline");
});
test("partial GraphQL data and repeated cursors are rejected", async () => {
  const api = new EvidenceApi();
  api.graphql = async () => ({ repository: { pullRequest: null } });
  await expect(threads(api, b.pr)).rejects.toThrow("Missing thread page");
});
for (const trigger of [
  "on: pull_request",
  "on: [push, pull_request]",
  "on:\n  pull_request:",
]) {
  for (const inventory of ["trusted", "candidate"])
    test(`${inventory} inventory refuses unfiltered PR event ${trigger}`, async () => {
      const api = new EvidenceApi();
      const get = api.get.bind(api);
      api.get = async (path) => {
        if (path.includes("/contents/.github/workflows?")) {
          const list = await get(path);
          if (inventory === "candidate" || path.includes(`ref=${b.base}`))
            return [
              ...list,
              {
                name: "extra.yml",
                path: ".github/workflows/extra.yml",
                type: "file",
              },
            ];
          return list;
        }
        if (path.includes("/contents/.github/workflows/extra.yml?"))
          return {
            type: "file",
            encoding: "base64",
            sha: "same",
            content: Buffer.from(
              `name: Extra\n${trigger}\njobs: {}\n`,
            ).toString("base64"),
          };
        return get(path);
      };
      await expect(snapshot(api, b.pr)).rejects.toThrow(
        "Unsupported PR workflow selector",
      );
    });
}
