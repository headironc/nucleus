import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import {
  admission,
  candidate,
  evaluate,
  request,
  reviewSnapshot,
  targets,
} from "./dev-admission";
import { ci, preflight, protection } from "./merge-admission";
import { type Api } from "./merge-credentials";
import {
  appId,
  devWorkflow,
  ownerId,
  repository,
  repositoryId,
  type Protection,
} from "./merge-deployment";
import { Queue, contract } from "./review-queue";
import { headChecks } from "./review-gate";
import {
  BASE,
  HEAD,
  OLD,
  TREE,
  CONTEXT,
  MAIN_RULES,
  VIEW,
  STACK,
  approval,
  run,
  summary,
} from "./review-fixtures";
import {
  array,
  encode,
  field,
  object,
  type Json,
  type Record,
} from "./review-json";

const config: Protection = { updateRule: 7n, rules: [7n, 8n] };
const wanted = { pr: 42n, head: HEAD };
const rules: Json[] = [
  { type: "update", ruleset_id: 7n },
  ...MAIN_RULES.filter((rule) => field(rule, "type") !== "workflows").map(
    (rule) => {
      const row = structuredClone(object(rule));
      if (row.type === "pull_request")
        Object.assign(object(row.parameters), {
          allowed_merge_methods: ["squash"],
          require_code_owner_review: true,
        });
      return { ...row, ruleset_id: 8n };
    },
  ),
];

function service(mutate: (row: Record, path: string) => void = () => {}) {
  const calls: string[] = [];
  const read: Api = async (path, method) => {
    expect(method ?? "GET").toBe("GET");
    calls.push(path);
    let row: Record;
    if (path.includes("/rules/branches/dev")) return structuredClone(rules);
    if (path.includes("/rulesets/")) {
      const id = BigInt(path.split("/").at(-1)!);
      row = {
        id,
        source_type: "Repository",
        source: repository,
        target: "branch",
        enforcement: "active",
        rules: id === 7n ? [{ type: "update" }] : [],
        bypass_actors:
          id === 7n
            ? [
                {
                  actor_id: appId,
                  actor_type: "Integration",
                  bypass_mode: "pull_request",
                },
              ]
            : [],
      };
    } else if (path.endsWith("/pulls/42"))
      row = {
        number: 42n,
        node_id: "pull",
        url: `https://api.github.com/repos/${repository}/pulls/42`,
        head: { sha: HEAD, ref: "feature/one", repo: { id: repositoryId } },
        base: { sha: BASE, ref: "dev", repo: { id: repositoryId } },
        state: "open",
        draft: false,
        merged: false,
      };
    else if (path.endsWith("/git/ref/heads/dev"))
      row = { ref: "refs/heads/dev", object: { sha: BASE } };
    else if (path.includes("/compare/"))
      row = { status: "ahead", merge_base_commit: { sha: BASE } };
    else if (path.endsWith(`/commits/${HEAD}`))
      row = {
        sha: HEAD,
        commit: { tree: { sha: TREE }, verification: { verified: true } },
      };
    else if (path.includes("/actions/workflows/"))
      row = {
        total_count: 1n,
        workflow_runs: [
          {
            id: 1n,
            run_attempt: 1n,
            head_sha: HEAD,
            event: "pull_request",
            path: `.github/workflows/${path.split("/workflows/")[1]!.split("/")[0]}`,
            repository: { id: repositoryId },
            pull_requests: [
              { number: 42n, head: { sha: HEAD }, base: { ref: "dev" } },
            ],
            status: "completed",
            conclusion: "success",
          },
        ],
      };
    else throw new Error(`Unexpected route ${path}`);
    mutate(row, path);
    return row;
  };
  return { read, calls };
}

function gateFixture() {
  const reviews: Record[] = [],
    checks: Json[] = [];
  const state = {
    base: BASE,
    relation: "ahead",
    view: structuredClone(VIEW),
    contextBase: BASE,
  };
  const gate = admission(
    { installation: 17n, tokens: { review: "fixture" } },
    () => {
      throw new Error("Unexpected actions call");
    },
    rules,
    wanted,
    BASE,
    (args) => {
      if (args.includes("POST")) {
        reviews.push({
          ...approval(),
          id: 8n,
          body: args.find((arg) => arg.startsWith("body="))!.slice(5),
        });
        return "{}";
      }
      if (args.includes("PUT")) {
        reviews[0]!.state = "DISMISSED";
        return "{}";
      }
      if (args.at(-1)!.includes("/reviews/")) return encode(reviews[0]);
      throw new Error(`Unexpected review route ${args}`);
    },
  );
  gate.prView = () => structuredClone(state.view);
  gate.targetHead = () => state.base;
  gate.relationship = () => state.relation;
  gate.contexts = () => [
    {
      ...CONTEXT,
      repository: { full_name: repository },
      display_title: `context:42:${HEAD}:${state.contextBase}:dev`,
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
  const queue = new Queue(gate);
  queue.shaPrs = () => ({ heads: [42n], entries: [] });
  queue.entry = () => {
    throw new Error("Personal dev must never enter a queue");
  };
  queue.setCheck = (_repo, _head, _identity, conclusion) => {
    checks.push(conclusion);
    return 19n;
  };
  return { queue, gate, reviews, checks, state };
}

test("an exact signed independent dev PR contains the current baseline; drift, fork and unsigned candidates fail", async () => {
  expect(request({ pr: "42", head: HEAD })).toEqual(wanted);
  for (const input of [
    { pr: "0", head: HEAD },
    { pr: "42", head: "dev" },
  ])
    expect(() => request(input)).toThrow();
  expect(await candidate(wanted, BASE, service().read)).toEqual({
    id: "pull",
    base: BASE,
    tree: TREE,
    headRef: "feature/one",
  });
  for (const change of [
    (row: Record, path: string) => {
      if (path.endsWith("/pulls/42")) row.draft = true;
    },
    (row: Record, path: string) => {
      if (path.endsWith("/pulls/42")) object(row.base).ref = "main";
    },
    (row: Record, path: string) => {
      if (path.endsWith("/pulls/42")) object(row.head).repo = { id: 1n };
    },
    (row: Record, path: string) => {
      if (path.endsWith("/heads/dev")) object(row.object).sha = OLD;
    },
    (row: Record, path: string) => {
      if (path.includes("/compare/")) row.status = "diverged";
    },
    (row: Record, path: string) => {
      if (path.includes("/compare/")) object(row.merge_base_commit).sha = OLD;
    },
    (row: Record, path: string) => {
      if (path.includes("/commits/"))
        object(field(row.commit, "verification")).verified = false;
    },
  ])
    await expect(
      candidate(wanted, BASE, service(change).read),
    ).rejects.toThrow();
});

test("dev enforces squash, exact rules, strict checks and no queue", async () => {
  expect(await protection(service().read, config, "dev")).toEqual(rules);
  const { gate } = gateFixture();
  contract(gate, rules, "dev");
  expect(() =>
    contract(gate, [...rules, { type: "merge_queue", parameters: {} }], "dev"),
  ).toThrow();
  const changed: Api = async (path, ...args) => {
    const result = await service().read(path, ...args);
    if (path.includes("/rules/branches/dev"))
      for (const row of array(result))
        if (field(row, "type") === "pull_request")
          object(field(row, "parameters")).allowed_merge_methods = ["merge"];
    return result;
  };
  await expect(protection(changed, config, "dev")).rejects.toThrow(
    "merge method",
  );
});

test("real Gate and Queue admit a synchronized dev candidate without a queue or dual-baseline reuse", async () => {
  const { queue, reviews, checks } = gateFixture(),
    api = service();
  const result = await evaluate(
    wanted,
    BASE,
    api.read,
    api.read,
    queue,
    config,
  );
  expect(field(field(result, "candidate"), "tree")).toBe(TREE);
  expect(reviews).toHaveLength(1);
  expect(reviews[0]!.state).toBe("APPROVED");
  expect(checks.at(-1)).toBe("success");
  expect(reviewSnapshot(queue, wanted, BASE).reasons).toEqual([]);
});

test("advanced dev, old context, diverged ancestry and stacks withdraw approval and never publish success", async () => {
  for (const change of [
    (state: ReturnType<typeof gateFixture>["state"]) => {
      state.base = OLD;
    },
    (state: ReturnType<typeof gateFixture>["state"]) => {
      state.contextBase = OLD;
    },
    (state: ReturnType<typeof gateFixture>["state"]) => {
      state.relation = "diverged";
    },
    (state: ReturnType<typeof gateFixture>["state"]) => {
      state.view.stack = STACK;
    },
  ]) {
    const fixture = gateFixture(),
      api = service();
    await evaluate(wanted, BASE, api.read, api.read, fixture.queue, config);
    change(fixture.state);
    fixture.checks.length = 0;
    await expect(
      evaluate(wanted, BASE, api.read, api.read, fixture.queue, config),
    ).rejects.toThrow();
    expect(fixture.reviews[0]!.state).toBe("DISMISSED");
    expect(fixture.checks).not.toContain("success");
    expect(fixture.checks.at(-1)).toBe("failure");
  }
});

test("eligibility drift after approval invalidates its check and withdraws approval", async () => {
  const fixture = gateFixture();
  let reads = 0;
  const api = service((row, path) => {
    if (path.endsWith("/heads/dev") && ++reads > 1)
      object(row.object).sha = OLD;
  });
  await expect(
    evaluate(wanted, BASE, api.read, api.read, fixture.queue, config),
  ).rejects.toThrow("advanced");
  expect(fixture.checks).toContain("success");
  expect(fixture.checks.at(-1)).toBe("failure");
  expect(fixture.reviews[0]!.state).toBe("DISMISSED");
});

test("a different head observed by the gate cannot inherit the earlier eligibility check", async () => {
  const fixture = gateFixture(),
    api = service();
  let reads = 0;
  fixture.gate.prView = () => ({
    ...VIEW,
    headRefOid: ++reads === 1 ? HEAD : OLD,
  });
  await expect(
    evaluate(wanted, BASE, api.read, api.read, fixture.queue, config),
  ).rejects.toThrow();
  expect(fixture.reviews).toHaveLength(0);
  expect(fixture.checks).not.toContain("success");
  expect(fixture.checks.at(-1)).toBe("failure");
});

test("new failed CI attempt revokes admission and never becomes merge authority", async () => {
  const fixture = gateFixture(),
    good = service();
  await evaluate(wanted, BASE, good.read, good.read, fixture.queue, config);
  const failed = service((row, path) => {
    if (path.includes("/actions/workflows/")) {
      const run = object(array(row.workflow_runs)[0]);
      run.run_attempt = 2n;
      run.conclusion = "failure";
    }
  });
  await expect(
    evaluate(wanted, BASE, good.read, failed.read, fixture.queue, config),
  ).rejects.toThrow("Latest");
  expect(fixture.reviews[0]!.state).toBe("DISMISSED");
  expect(fixture.checks.at(-1)).toBe("failure");
  await expect(ci(good.read, wanted, "main")).rejects.toThrow();
});

test("a failed check invalidation still attempts approval withdrawal", async () => {
  const fixture = gateFixture(),
    good = service();
  await evaluate(wanted, BASE, good.read, good.read, fixture.queue, config);
  fixture.queue.setCheck = () => {
    throw new Error("check write unavailable");
  };
  expect(() => fixture.queue.reject(repository, 42n, VIEW)).toThrow(
    "check write unavailable",
  );
  expect(fixture.reviews[0]!.state).toBe("DISMISSED");
});

test("trusted automated events may evaluate but manual writes require the owner and exact current dev source", async () => {
  const env = {
    GITHUB_REPOSITORY: repository,
    GITHUB_REPOSITORY_ID: String(repositoryId),
    GITHUB_REPOSITORY_OWNER_ID: String(ownerId),
    GITHUB_EVENT_NAME: "pull_request_target",
    GITHUB_REF: "refs/heads/dev",
    GITHUB_WORKFLOW_REF: `${repository}/${devWorkflow}@refs/heads/dev`,
    GITHUB_ACTOR_ID: "1",
    GITHUB_SHA: BASE,
    GITHUB_WORKFLOW_SHA: BASE,
    GITHUB_RUN_ID: "12",
    GITHUB_RUN_ATTEMPT: "1",
  };
  const read: Api = async (path): Promise<Json> => {
    if (path.includes("/heads/dev"))
      return { ref: "refs/heads/dev", object: { sha: BASE } };
    if (path.includes("/actions/runs/"))
      return {
        id: 12n,
        run_attempt: 1n,
        head_sha: HEAD,
        head_branch: "feature/one",
        path: devWorkflow,
        event: "pull_request_target",
        status: "in_progress",
        repository: { id: repositoryId },
        actor: { id: 1n },
        triggering_actor: { id: 1n },
      };
    return {
      id: repositoryId,
      full_name: repository,
      private: true,
      default_branch: "dev",
      owner: { id: ownerId, login: "headironc", type: "User" },
    };
  };
  expect((await preflight(read, env, "dev")).source).toBe(BASE);
  for (const change of [
    { GITHUB_EVENT_NAME: "pull_request" },
    { GITHUB_REF: "refs/pull/42/merge" },
    { GITHUB_WORKFLOW_SHA: HEAD },
    { GITHUB_EVENT_NAME: "workflow_dispatch" },
  ])
    await expect(
      preflight(read, { ...env, ...change }, "dev"),
    ).rejects.toThrow();
});

test("notifications select related PRs; unassociated completions do not trigger a full sweep", async () => {
  const paths: string[] = [];
  const read: Api = async (path) => {
    paths.push(path);
    return [
      { number: 42n, head: { ref: "feature/one", repo: { id: repositoryId } } },
      { number: 43n, head: { ref: "other", repo: { id: repositoryId } } },
    ];
  };
  expect(
    await targets(
      { pull_request: { number: 42n } },
      "pull_request_target",
      read,
    ),
  ).toEqual([42n]);
  expect(
    await targets({ issue: { number: 42n } }, "issue_comment", read),
  ).toEqual([]);
  const workflow = {
    repository: { id: repositoryId },
    pull_requests: [],
    event: "pull_request",
    path: ".github/workflows/rust.yml",
    display_title: "context:43:fake",
    head_branch: "feature/one",
    head_repository: { id: repositoryId },
  };
  expect(
    await targets({ workflow_run: workflow }, "workflow_run", read),
  ).toEqual([42n]);
  expect(
    await targets(
      { workflow_run: { ...workflow, head_branch: "missing" } },
      "workflow_run",
      read,
    ),
  ).toEqual([]);
  expect(await targets({}, "push", read)).toEqual([42n, 43n]);
  expect(paths.every((path) => path.includes("state=open&base=dev"))).toBe(
    true,
  );
});

test("dev workflow isolates secrets to the trusted source and serializes evaluation with both merge targets", () => {
  const yaml = Bun.YAML.parse(
    readFileSync(
      new URL("./fixtures/retired-workflows/dev-admission.yml", import.meta.url),
      "utf8",
    ),
  ) as any;
  const job = yaml.jobs.admit;
  expect(job.if).toContain("github.repository == 'headironc/nucleus'");
  expect(job.if).toContain("github.ref == 'refs/heads/dev'");
  expect(job.permissions).toEqual({ contents: "read", actions: "read" });
  expect(job.concurrency).toEqual({
    group: "review-gate",
    "cancel-in-progress": false,
    queue: "max",
  });
  expect(job.steps[0].with.ref).toBe("${{ github.workflow_sha }}");
  expect(job.steps[0].with["persist-credentials"]).toBe(false);
  expect(
    job.steps.filter((step: any) => step.env?.NUCLEUS_DEV_PRIVATE_KEY),
  ).toHaveLength(1);
  expect(
    job.steps
      .filter((step: any) => step.run)
      .every((step: any) => !step.run.includes("${{")),
  ).toBe(true);
  expect(yaml.on.workflow_dispatch.inputs.mode.default).toBe("evaluate");
  expect(yaml.on.merge_group).toBeUndefined();
});
