import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { admission, request, reviewSnapshot } from "./main-promotion";
import { ci, preflight, protection } from "./merge-admission";
import { type Api, type Credentials } from "./merge-credentials";
import { bindPull } from "./merge-operation";
import {
  appId,
  ownerId,
  repository,
  repositoryId,
  workflow,
  type Protection,
  type Ledger,
} from "./merge-deployment";
import { Queue, contract } from "./review-queue";
import {
  BASE,
  HEAD,
  CONTEXT,
  MAIN_RULES,
  approval,
  run,
  summary,
  viewResponse,
} from "./review-fixtures";
import { headChecks } from "./review-gate";
import {
  array,
  encode,
  field,
  object,
  parse,
  type Json,
  type Record,
} from "./review-json";

const config: Protection & Ledger = {
  bot: 99n,
  issue: 1n,
  issueId: "node",
  updateRule: 7n,
  rules: [7n, 8n],
};
function personalRules(): Json[] {
  return [
    { type: "update", ruleset_id: 7n },
    ...MAIN_RULES.filter((rule) => field(rule, "type") !== "workflows").map(
      (rule) => {
        const value = structuredClone(object(rule));
        if (value.type === "pull_request")
          Object.assign(object(value.parameters), {
            allowed_merge_methods: ["merge"],
            require_code_owner_review: true,
          });
        return { ...value, ruleset_id: 8n };
      },
    ),
  ];
}
function rulesApi(
  options: {
    update?: Json;
    bypass?: Json;
    rules?: Json[];
    mutate?: (row: Record) => void;
  } = {},
): Api {
  return async (path): Promise<Json> => {
    if (path.includes("/rules/branches/main"))
      return options.rules ?? personalRules();
    const id = BigInt(path.split("/").at(-1)!);
    const rule: Record = {
      id,
      source_type: "Repository",
      source: repository,
      target: "branch",
      enforcement: "active",
      rules:
        id === config.updateRule
          ? [
              options.update ?? {
                type: "update",
                parameters: { update_allows_fetch_and_merge: false },
              },
            ]
          : [],
      bypass_actors:
        id === config.updateRule
          ? (options.bypass ?? [
              {
                actor_id: appId,
                actor_type: "Integration",
                bypass_mode: "pull_request",
              },
            ])
          : [],
    };
    options.mutate?.(rule);
    return rule;
  };
}
const wanted = { pr: 42n, dev: HEAD, base: BASE, head: HEAD };
const env = {
  GITHUB_REPOSITORY: repository,
  GITHUB_REPOSITORY_ID: String(repositoryId),
  GITHUB_REPOSITORY_OWNER_ID: String(ownerId),
  GITHUB_EVENT_NAME: "workflow_dispatch",
  GITHUB_REF: "refs/heads/dev",
  GITHUB_WORKFLOW_REF: `${repository}/${workflow}@refs/heads/dev`,
  GITHUB_ACTOR_ID: String(ownerId),
  GITHUB_SHA: HEAD,
  GITHUB_WORKFLOW_SHA: HEAD,
  GITHUB_RUN_ID: "12",
  GITHUB_RUN_ATTEMPT: "1",
};
const invocation: Api = async (path): Promise<Json> => {
  if (path.endsWith("/git/ref/heads/dev"))
    return { ref: "refs/heads/dev", object: { sha: HEAD } };
  if (path.includes("/actions/runs/"))
    return {
      id: 12n,
      run_attempt: 1n,
      head_sha: HEAD,
      head_branch: "dev",
      path: workflow,
      event: "workflow_dispatch",
      status: "in_progress",
      repository: { id: repositoryId },
      actor: { id: ownerId },
      triggering_actor: { id: ownerId },
    };
  return {
    id: repositoryId,
    full_name: repository,
    private: true,
    default_branch: "dev",
    owner: { id: ownerId, login: "headironc", type: "User" },
  };
};

test("merge requests require an exact accepted dev declaration", () => {
  expect(
    request({
      pr: "42",
      dev: HEAD,
      base: BASE,
      head: HEAD,
      accepted_dev: "true",
    }),
  ).toEqual(wanted);
  for (const input of [
    { accepted_dev: "false" },
    { accepted_dev: true },
    { pr: "0" },
    { dev: HEAD.slice(0, 7) },
  ])
    expect(() =>
      request({
        pr: "42",
        dev: HEAD,
        base: BASE,
        head: HEAD,
        accepted_dev: "true",
        ...input,
      }),
    ).toThrow();
});

test("trusted invocation rejects another account, ref, event, source, run and retriggering actor", async () => {
  expect(await preflight(invocation, env)).toEqual({
    source: HEAD,
    run: 12n,
    attempt: 1n,
  });
  for (const change of [
    { GITHUB_ACTOR_ID: "1" },
    { GITHUB_REF: "refs/heads/main" },
    { GITHUB_EVENT_NAME: "pull_request" },
    { GITHUB_WORKFLOW_SHA: BASE },
    { GITHUB_REPOSITORY: "attacker/nucleus" },
    { GITHUB_RUN_ATTEMPT: "0" },
  ])
    await expect(
      preflight(invocation, { ...env, ...change }),
    ).rejects.toThrow();
  for (const change of [
    { head_sha: BASE },
    { path: "untrusted.yml" },
    { triggering_actor: { id: 1n } },
    { run_attempt: 2n },
  ] as Record[]) {
    const read: Api = async (path): Promise<Json> => {
      const value = await invocation(path);
      return path.includes("/actions/runs/")
        ? { ...object(value), ...change }
        : value;
    };
    await expect(preflight(read, env)).rejects.toThrow();
  }
});

test("personal main protection accepts optional false update parameters and omits no privilege requirements", async () => {
  for (const update of [
    { type: "update" },
    { type: "update", parameters: { update_allows_fetch_and_merge: false } },
  ] as Json[])
    expect(await protection(rulesApi({ update }), config)).toEqual(
      personalRules(),
    );
  // Omitted bypass data is explicitly a deployment prerequisite; never demand
  // administration permission or claim that omission proves an empty list.
  expect(
    await protection(
      rulesApi({
        mutate: (row) => {
          delete row.bypass_actors;
        },
      }),
      config,
    ),
  ).toEqual(personalRules());
  for (const update of [
    { type: "update", parameters: { update_allows_fetch_and_merge: true } },
    { type: "update", parameters: null },
    { type: "update", parameters: {} },
    { type: "deletion" },
  ] as Json[])
    await expect(protection(rulesApi({ update }), config)).rejects.toThrow();
  for (const bypass of [
    [],
    [{ actor_id: 1n, actor_type: "Integration", bypass_mode: "pull_request" }],
    [{ actor_id: appId, actor_type: "Integration", bypass_mode: "always" }],
  ])
    await expect(protection(rulesApi({ bypass }), config)).rejects.toThrow();
  for (const mutate of [
    (row: Record) => {
      row.enforcement = "disabled";
    },
    (row: Record) => {
      row.source = "other/repo";
    },
    (row: Record) => {
      row.bypass_actors = null;
    },
    (row: Record) => {
      if (row.id === 8n) row.bypass_actors = [{ actor_id: 1n }];
    },
  ])
    await expect(protection(rulesApi({ mutate }), config)).rejects.toThrow();
  const noSignatures = personalRules().filter(
    (rule) => field(rule, "type") !== "required_signatures",
  );
  await expect(
    protection(rulesApi({ rules: noSignatures }), config),
  ).rejects.toThrow();
  await expect(
    protection(rulesApi(), { ...config, rules: [7n] }),
  ).rejects.toThrow();
});

test("release PR binds the same repository, exact SHA, main target and release branch", () => {
  const pull: Record = {
    number: 42n,
    node_id: "node",
    url: `https://api.github.com/repos/${repository}/pulls/42`,
    base: { repo: { id: repositoryId }, ref: "main" },
    head: { repo: { id: repositoryId }, sha: HEAD, ref: "release/one" },
  };
  expect(bindPull(pull, wanted, "main")).toBe("node");
  for (const mutate of [
    (row: Record) => {
      object(row.base).ref = "dev";
    },
    (row: Record) => {
      object(row.head).sha = BASE;
    },
    (row: Record) => {
      object(row.head).repo = { id: 1n };
    },
    (row: Record) => {
      object(row.head).ref = "feature/one";
    },
    (row: Record) => {
      row.number = 43n;
    },
  ]) {
    const row = structuredClone(pull);
    mutate(row);
    expect(() => bindPull(row, wanted, "main")).toThrow();
  }
});

function ciApi(change: Record = {}, count = 1n): Api {
  return async (path) => ({
    total_count: count,
    workflow_runs: [
      {
        id: 1n,
        run_attempt: 1n,
        head_sha: HEAD,
        event: "pull_request",
        path: `.github/workflows/${path.split("/workflows/")[1].split("/")[0]}`,
        repository: { id: repositoryId },
        pull_requests: [
          { number: wanted.pr, head: { sha: HEAD }, base: { ref: "main" } },
        ],
        status: "completed",
        conclusion: "success",
        ...change,
      },
    ],
  });
}
test("CI requires all three exact workflow runs, latest successful attempts and complete pagination", async () => {
  expect(await ci(ciApi(), wanted)).toHaveLength(3);
  for (const change of [
    { conclusion: "failure" },
    { status: "in_progress" },
    { event: "push" },
    { repository: { id: 1n } },
    { run_attempt: 0n },
    { head_sha: BASE },
    { pull_requests: [] },
    { path: ".github/workflows/other.yml" },
  ] as Record[])
    await expect(ci(ciApi(change), wanted)).rejects.toThrow();
  await expect(ci(ciApi({}, 2n), wanted)).rejects.toThrow("incomplete");
  const newerFailure: Api = async (path) => {
    const result = object(await ciApi()(path));
    const old = object(array(result.workflow_runs)[0]);
    return {
      total_count: 2n,
      workflow_runs: [old, { ...old, id: 2n, conclusion: "failure" }],
    };
  };
  await expect(ci(newerFailure, wanted)).rejects.toThrow("Latest");
});

test("personal adapter uses the real Gate approval and Queue admission with exact main rules and no organization workflow", () => {
  const rules = personalRules(),
    reviews: Record[] = [];
  const credentials: Credentials = {
    installation: 17n,
    tokens: { review: "fixture", record: "fixture", merge: "fixture" },
  };
  const gate = admission(
    credentials,
    () => {
      throw new Error("Unexpected Actions read");
    },
    rules,
    (args) => {
      if (args.includes("graphql")) {
        const response = viewResponse(view);
        object(field(response, "data")).repository = {
          ...object(field(field(response, "data"), "repository")),
          nameWithOwner: repository,
        };
        return encode(response);
      }
      const path = args.at(-1)!;
      if (args.includes("POST")) {
        reviews.push({
          ...approval(),
          id: 8n,
          body: args.find((arg) => arg.startsWith("body="))!.slice(5),
        });
        return "{}";
      }
      if (path.includes("/reviews/")) return encode(reviews[0]);
      throw new Error(`Unexpected route ${path}`);
    },
  );
  const view = {
    headRefOid: HEAD,
    baseRefOid: BASE,
    baseRefName: "main",
    state: "OPEN",
    isDraft: false,
    lifecycle: null,
  };
  expect(gate.prView(repository, wanted.pr)).toEqual(view);
  gate.targetHead = () => BASE;
  gate.contexts = () => [
    {
      ...CONTEXT,
      repository: { full_name: repository },
      display_title: `context:42:${HEAD}:${BASE}:main`,
    },
  ];
  gate.evidence = () => ({
    comments: [summary()],
    inline: [],
    reviews: [],
    unresolved: 0n,
    events: [],
    rules,
    checks: headChecks([run("docs"), run("rust"), run("website")], []),
  });
  gate.pages = () => structuredClone(reviews);
  expect(gate.main(42n)).toBe(0);
  expect(reviews).toHaveLength(1);
  const queue = new Queue(gate);
  queue.shaPrs = () => ({ heads: [42n], entries: [] });
  expect(field(reviewSnapshot(queue, wanted), "reasons")).toEqual([]);
  expect(() => reviewSnapshot(queue, { ...wanted, head: BASE })).toThrow(
    "another candidate",
  );
  expect(() =>
    contract(gate, [...rules, { type: "merge_queue", parameters: {} }], "main"),
  ).toThrow();
  const changed = structuredClone(rules);
  object(changed[0]).ruleset_id = 10n;
  expect(() => contract(gate, changed, "main")).toThrow(
    "changed during review",
  );
});

test("workflow keeps secrets on the trusted source, shares the writer lock and exposes no candidate shell input", () => {
  const yaml = Bun.YAML.parse(
    readFileSync(
      new URL("../.github/workflows/main-promotion.yml", import.meta.url),
      "utf8",
    ),
  ) as any;
  const job = yaml.jobs.promote;
  expect(job.if).toContain("github.ref == 'refs/heads/dev'");
  expect(job.concurrency.group).toBe("review-gate");
  expect(job.concurrency["cancel-in-progress"]).toBe(false);
  expect(job.permissions).toEqual({ contents: "read", actions: "read" });
  expect(job.steps[0].with.ref).toBe("${{ github.workflow_sha }}");
  expect(job.steps[0].with["persist-credentials"]).toBe(false);
  expect(
    job.steps.filter((step: any) => step.env?.NUCLEUS_PROMOTION_PRIVATE_KEY),
  ).toHaveLength(1);
  expect(
    job.steps
      .filter((step: any) => step.run)
      .every((step: any) => !step.run.includes("${{")),
  ).toBe(true);
});
