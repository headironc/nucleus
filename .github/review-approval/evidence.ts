import {
  repository,
  repositoryId,
  ownerId,
  requestBodies,
  numericId,
  requireThat,
  sha,
  type Binding,
} from "./identity.ts";
import { verifyProviders, type Record } from "./providers.ts";
import { stableWorkflows, selectRun, verifyJobs, verifyReceipt } from "./ci.ts";
import type { Api } from "./github.ts";
import {
  codeRequest,
  readQuota,
  quotaWithdrawal,
  quotaStatement,
} from "./quota.ts";
import { readReviewStarts, type StartSelection } from "./review-start.ts";
import { readCubicReviews } from "./reviews.ts";

const prefix = `/repos/${repository}`;
export async function binding(api: Api, pr: number): Promise<Binding> {
  requireThat(
    Number.isSafeInteger(pr) && pr > 0,
    "PR must be a positive integer",
  );
  const p = await api.get(`${prefix}/pulls/${pr}`);
  requireThat(
    p.number === pr &&
      p.state === "open" &&
      p.draft === false &&
      p.merged === false &&
      p.base?.ref === "dev" &&
      p.base?.repo?.id === repositoryId &&
      p.head?.repo?.id === repositoryId &&
      p.head?.ref !== "dev" &&
      !p.head?.ref?.startsWith("gh-readonly-queue/"),
    "Only open non-draft same-repository ordinary dev PRs are eligible",
  );
  requireThat(
    sha(p.head.sha) &&
      sha(p.base.sha) &&
      sha(p.merge_commit_sha) &&
      p.mergeable === true,
    "PR candidate/mergeability unknown",
  );
  const base = await api.get(`${prefix}/git/ref/heads/dev`);
  requireThat(
    base.object?.sha === p.base.sha,
    "PR baseline differs from current dev",
  );
  const comparison = await api.get(
    `${prefix}/compare/${p.base.sha}...${p.head.sha}`,
  );
  requireThat(
    ["ahead", "identical"].includes(comparison.status) &&
      comparison.merge_base_commit?.sha === p.base.sha,
    "Head must contain baseline; stacked or stale PR denied",
  );
  return { pr, head: p.head.sha, base: p.base.sha, merge: p.merge_commit_sha };
}

async function source(api: Api, path: string, ref: string) {
  const encoded = path.split("/").map(encodeURIComponent).join("/");
  const value = await api.get(
    `${prefix}/contents/${encoded}?ref=${encodeURIComponent(ref)}`,
  );
  requireThat(
    value.type === "file" &&
      value.encoding === "base64" &&
      typeof value.content === "string",
    `Cannot read trusted ${path}`,
  );
  return {
    sha: value.sha,
    text: Buffer.from(value.content, "base64").toString("utf8"),
  };
}

export async function threads(api: Api, pr: number) {
  const all: Record[] = [];
  let cursor: string | null = null;
  do {
    const result: any = await api.graphql(
      `query($pr:Int!,$cursor:String){repository(owner:"headironc",name:"nucleus"){pullRequest(number:$pr){reviewThreads(first:100,after:$cursor){nodes{id isResolved isOutdated comments(first:100){nodes{fullDatabaseId} pageInfo{hasNextPage endCursor}}} pageInfo{hasNextPage endCursor}}}}}`,
      { pr, cursor },
    );
    const page = result.repository?.pullRequest?.reviewThreads;
    requireThat(
      Array.isArray(page?.nodes) &&
        typeof page.pageInfo?.hasNextPage === "boolean",
      "Missing thread page",
    );
    for (const thread of page.nodes) {
      requireThat(
        typeof thread.id === "string" &&
          typeof thread.isResolved === "boolean" &&
          typeof thread.isOutdated === "boolean" &&
          Array.isArray(thread.comments?.nodes) &&
          typeof thread.comments.pageInfo?.hasNextPage === "boolean",
        "Incomplete thread",
      );
      let comments = thread.comments;
      const ids = comments.nodes.map((c: Record) =>
        numericId(c.fullDatabaseId),
      );
      while (comments.pageInfo.hasNextPage) {
        requireThat(
          typeof comments.pageInfo.endCursor === "string",
          "Missing nested comment cursor",
        );
        const next = await api.graphql(
          `query($id:ID!,$cursor:String!){node(id:$id){... on PullRequestReviewThread{comments(first:100,after:$cursor){nodes{fullDatabaseId} pageInfo{hasNextPage endCursor}}}}}`,
          { id: thread.id, cursor: comments.pageInfo.endCursor },
        );
        const previous = comments.pageInfo.endCursor;
        comments = next.node?.comments;
        requireThat(
          Array.isArray(comments?.nodes) &&
            typeof comments.pageInfo?.hasNextPage === "boolean" &&
            (!comments.pageInfo.hasNextPage ||
              comments.pageInfo.endCursor !== previous),
          "Incomplete nested comments",
        );
        ids.push(
          ...comments.nodes.map((c: Record) => numericId(c.fullDatabaseId)),
        );
      }
      requireThat(
        ids.every((id: any) => Number.isSafeInteger(id) && id > 0) &&
          new Set(ids).size === ids.length,
        "Missing inline identity",
      );
      all.push({
        id: thread.id,
        resolved: thread.isResolved,
        outdated: thread.isOutdated,
        comments: ids,
      });
    }
    const previous: string | null = cursor;
    cursor = page.pageInfo.hasNextPage ? page.pageInfo.endCursor : null;
    requireThat(
      !page.pageInfo.hasNextPage ||
        (typeof cursor === "string" && cursor !== previous),
      "Missing thread cursor",
    );
  } while (cursor);
  requireThat(
    new Set(all.map((t) => t.id)).size === all.length,
    "Duplicate thread page",
  );
  return all;
}

function pathsMatch(patterns: string[], files: string[]) {
  requireThat(
    patterns.every((p) => typeof p === "string"),
    "Unsupported workflow path filter",
  );
  return files.some((file) => {
    let matches = false;
    for (const pattern of patterns) {
      const negative = pattern.startsWith("!");
      if (new Bun.Glob(negative ? pattern.slice(1) : pattern).match(file))
        matches = !negative;
    }
    return matches;
  });
}

export function prTrigger(on: unknown): {
  present: boolean;
  selector: unknown;
} {
  if (typeof on === "string")
    return { present: on === "pull_request", selector: null };
  if (Array.isArray(on)) {
    requireThat(
      on.every((event) => typeof event === "string"),
      "Unknown workflow event array",
    );
    return { present: on.includes("pull_request"), selector: null };
  }
  requireThat(
    on !== null && typeof on === "object",
    "Missing workflow event declaration",
  );
  return {
    present: Object.hasOwn(on, "pull_request"),
    selector: (on as Record).pull_request,
  };
}

export async function verifyExecutor(api: Api, b: Binding) {
  const tree = async (id: string) => {
    const result = await api.get(`${prefix}/git/trees/${id}`);
    requireThat(
      result.sha === id &&
        result.truncated === false &&
        Array.isArray(result.tree) &&
        result.tree.every(
          (entry: Record) =>
            typeof entry.path === "string" &&
            !entry.path.includes("/") &&
            sha(entry.sha) &&
            typeof entry.mode === "string" &&
            typeof entry.type === "string",
        ) &&
        new Set(result.tree.map((e: Record) => e.path)).size ===
          result.tree.length,
      "Incomplete executor Git tree",
    );
    return result.tree as Record[];
  };
  const roots = await Promise.all(
    [b.base, b.merge].map(async (commit) => {
      const value = await api.get(`${prefix}/git/commits/${commit}`);
      requireThat(
        value.sha === commit && sha(value.tree?.sha),
        "Executor commit/tree identity unavailable",
      );
      return tree(value.tree.sha);
    }),
  );
  const config = (name: string) =>
    [
      "package.json",
      "tsconfig.json",
      "jsconfig.json",
      "bunfig.toml",
      ".bunfig.toml",
    ].includes(name) ||
    name === ".env" ||
    name.startsWith(".env.");
  const identity = (entry: Record) => ({
    path: entry.path,
    sha: entry.sha,
    type: entry.type,
    mode: entry.mode,
  });
  const freeze = (
    before: Record[],
    after: Record[],
    select: (n: string) => boolean,
  ) => {
    const selected = (entries: Record[]) =>
      entries
        .filter((entry) => select(entry.path))
        .map(identity)
        .sort((a, b) => a.path.localeCompare(b.path));
    requireThat(
      JSON.stringify(selected(before)) === JSON.stringify(selected(after)),
      "Approval executor/configuration changed; trusted bootstrap required",
    );
  };
  const child = async (entries: Record[], name: string) => {
    const entry = entries.find((e) => e.path === name);
    requireThat(
      entry?.type === "tree" && entry.mode === "040000",
      `Missing executor directory ${name}`,
    );
    return tree(entry.sha);
  };
  freeze(
    roots[0],
    roots[1],
    (name) =>
      config(name) ||
      ["bun.lock", "bun.lockb", ".tool-versions"].includes(name),
  );
  const github = await Promise.all(roots.map((root) => child(root, ".github")));
  freeze(
    github[0],
    github[1],
    (name) =>
      config(name) || name === "review-approval" || name === "workflows",
  );
  for (const entries of github) {
    for (const name of ["review-approval", "workflows"]) {
      const entry = entries.find((e) => e.path === name);
      requireThat(
        entry?.type === "tree" && entry.mode === "040000",
        `Missing approval executor tree ${name}`,
      );
    }
  }
  const tools = await Promise.all(roots.map((root) => child(root, "tools")));
  freeze(tools[0], tools[1], (name) => config(name) || name === "check-bun.ts");
}

async function verifyCI(api: Api, b: Binding, checks: Record[]) {
  await verifyExecutor(api, b);
  const files = await api.pages(`${prefix}/pulls/${b.pr}/files`);
  const p = await api.get(`${prefix}/pulls/${b.pr}`);
  requireThat(
    files.length === p.changed_files && files.length < 3000,
    "PR file list is truncated",
  );
  const changed = files.flatMap((f) => [
    f.filename,
    ...(f.previous_filename ? [f.previous_filename] : []),
  ]);
  const directory = await api.get(
    `${prefix}/contents/.github/workflows?ref=${b.base}`,
  );
  requireThat(Array.isArray(directory), "Missing trusted workflow inventory");
  const candidateDirectory = await api.get(
    `${prefix}/contents/.github/workflows?ref=${b.merge}`,
  );
  requireThat(
    Array.isArray(candidateDirectory),
    "Missing candidate workflow inventory",
  );
  for (const file of candidateDirectory) {
    requireThat(
      file.type === "file" && /\.ya?ml$/.test(file.name),
      "Unknown candidate workflow member",
    );
    const candidate = await source(api, file.path, b.merge);
    const definition: any = Bun.YAML.parse(candidate.text);
    const declaration = prTrigger(definition.on);
    if (!declaration.present) continue;
    requireThat(
      declaration.selector !== null &&
        typeof declaration.selector === "object" &&
        !Array.isArray(declaration.selector),
      "Unsupported PR workflow selector",
    );
    requireThat(
      directory.some((f: Record) => f.path === file.path),
      `New PR CI needs protocol acceptance: ${file.path}`,
    );
    requireThat(
      candidate.sha === (await source(api, file.path, b.base)).sha,
      `PR CI protocol changed: ${file.path}`,
    );
  }
  const required: { path: string; jobs: string[] }[] = [];
  for (const file of directory) {
    requireThat(
      file.type === "file" && /\.ya?ml$/.test(file.name),
      "Unsupported workflow inventory member",
    );
    const trusted = await source(api, file.path, b.base);
    const workflow: any = Bun.YAML.parse(trusted.text);
    const declaration = prTrigger(workflow.on);
    if (!declaration.present) continue;
    const trigger: any = declaration.selector;
    requireThat(
      trigger !== null &&
        typeof trigger === "object" &&
        !Array.isArray(trigger),
      "Unsupported PR workflow selector",
    );
    requireThat(
      Array.isArray(trigger.branches) &&
        !trigger["branches-ignore"] &&
        !trigger["paths-ignore"] &&
        trigger.branches.every((s: string) => ["dev", "main"].includes(s)),
      "Unknown PR workflow applicability",
    );
    if (!trigger.branches.includes("dev")) continue;
    if (trigger.paths && !pathsMatch(trigger.paths, changed)) continue;
    const current = await source(api, file.path, b.merge);
    requireThat(
      current.sha === trusted.sha,
      `CI protocol changed in candidate: ${file.path}`,
    );
    const jobs = Object.values(workflow.jobs)
      .map((j: any) => j.name)
      .filter(
        (name: any) => typeof name === "string" && name.startsWith("Nucleus "),
      ) as string[];
    required.push({ path: file.path, jobs });
  }
  for (const filename of stableWorkflows)
    requireThat(
      required.some((w) => w.path === `.github/workflows/${filename}`),
      `Missing stable workflow ${filename}`,
    );
  const merge = await api.get(`${prefix}/git/commits/${b.merge}`);
  requireThat(
    merge.sha === b.merge &&
      merge.parents?.length === 2 &&
      merge.parents[0].sha === b.base &&
      merge.parents[1].sha === b.head &&
      sha(merge.tree?.sha),
    "Current merge candidate parents/tree mismatch",
  );
  const runs = await api.pages(
    `${prefix}/actions/runs?head_sha=${b.head}&event=pull_request`,
    "workflow_runs",
  );
  const evidence = [];
  for (const workflow of required) {
    const loaded = new Map<string, Awaited<ReturnType<typeof readRun>>>();
    const read = async (run: Record) => {
      const key = `${run.id}/${run.run_attempt}`;
      const cached = loaded.get(key);
      if (cached) return cached;
      const value = await readRun(api, run, workflow, b.head);
      loaded.set(key, value);
      return value;
    };
    const run = await selectRun(
      runs,
      workflow.path,
      b,
      async (r) => (await read(r)).receipt,
    );
    const result = await read(run);
    verifyReceipt(result.receipt, run, b, merge.tree.sha, workflow.path);
    evidence.push(result);
  }
  return evidence;
}

async function readRun(
  api: Api,
  run: Record,
  workflow: { path: string; jobs: string[] },
  head: string,
) {
  const detail = await api.get(`${prefix}/actions/runs/${run.id}`);
  requireThat(
    detail.run_attempt === run.run_attempt &&
      detail.head_sha === head &&
      detail.path === workflow.path &&
      detail.conclusion === "success" &&
      detail.status === "completed",
    "CI run changed during read",
  );
  const jobs = await api.pages(
    `${prefix}/actions/runs/${run.id}/attempts/${run.run_attempt}/jobs`,
    "jobs",
  );
  const allowedConclusions =
    workflow.path === ".github/workflows/packages.yml"
      ? ["success", "skipped"]
      : ["success"];
  requireThat(
    jobs.length > 0 &&
      jobs.every(
        (j) =>
          j.status === "completed" && allowedConclusions.includes(j.conclusion),
      ) &&
      jobs.some((j) => j.conclusion === "success"),
    `CI jobs incomplete: ${workflow.path}`,
  );
  const suiteChecks = await api.pages(
    `${prefix}/check-suites/${run.check_suite_id}/check-runs`,
    "check_runs",
  );
  verifyJobs(jobs, suiteChecks, run, [
    ...new Set([
      ...workflow.jobs,
      ...jobs.filter((j) => j.conclusion === "success").map((j) => j.name),
    ]),
  ]);
  requireThat(
    suiteChecks.length > 0 && suiteChecks.every((c) => c.app?.id === 15368),
    "CI check suite has unexpected App identity",
  );
  const artifacts = await api.pages(
    `${prefix}/actions/runs/${run.id}/artifacts`,
    "artifacts",
  );
  const matches = artifacts.filter(
    (a) => a.name === `cladiron-ci-${run.id}-${run.run_attempt}`,
  );
  requireThat(
    matches.length === 1 &&
      matches[0].expired === false &&
      matches[0].workflow_run?.id === run.id &&
      matches[0].workflow_run?.repository_id === repositoryId &&
      matches[0].workflow_run?.head_repository_id === repositoryId &&
      matches[0].workflow_run?.head_sha === head,
    `Missing or unbound CI receipt: ${workflow.path}`,
  );
  const receipt = await api.receipt(matches[0]);
  return {
    run: run.id,
    attempt: run.run_attempt,
    artifact: matches[0].id,
    receipt,
    jobs: jobs.map((j) => [j.id, j.name, j.conclusion]),
  };
}

export async function requestComments(api: Api, b: Binding) {
  const pr = b.pr;
  const comments = await api.pages(`${prefix}/issues/${pr}/comments`);
  const requested = comments.filter(
    (c) =>
      c.user?.id === ownerId &&
      (requestBodies(b).includes(c.body) ||
        codeRequest(c.body, b) !== null ||
        quotaStatement(c.body, b) !== null ||
        c.body === quotaWithdrawal(b)),
  );
  for (let offset = 0; offset < requested.length; offset += 100) {
    const batch = requested.slice(offset, offset + 100);
    requireThat(
      batch.every((c) => typeof c.node_id === "string"),
      "Missing request node identity",
    );
    const result = await api.graphql(
      `query($ids:[ID!]!){nodes(ids:$ids){... on IssueComment{id fullDatabaseId createdAt updatedAt lastEditedAt}}}`,
      { ids: batch.map((c) => c.node_id) },
    );
    requireThat(
      Array.isArray(result.nodes) && result.nodes.length === batch.length,
      "Incomplete request edit metadata",
    );
    for (const comment of batch) {
      const matches = result.nodes.filter(
        (n: Record) =>
          n?.id === comment.node_id &&
          numericId(n.fullDatabaseId) === comment.id,
      );
      requireThat(
        matches.length === 1 &&
          matches[0].createdAt === comment.created_at &&
          matches[0].updatedAt === comment.updated_at &&
          Object.hasOwn(matches[0], "lastEditedAt"),
        "Request metadata changed or is unavailable",
      );
      comment.lastEditedAt = matches[0].lastEditedAt;
    }
  }
  return comments;
}
export async function snapshot(
  api: Api,
  pr: number,
  now = Date.now(),
  startArtifacts: StartSelection = {},
) {
  const b = await binding(api, pr);
  const comments = await requestComments(api, b);
  const reviews = await readCubicReviews(api, pr, now);
  const inline = await api.pages(`${prefix}/pulls/${pr}/comments`);
  const allThreads = await threads(api, pr);
  requireThat(
    allThreads.every((t) => t.resolved === true),
    "Unresolved inline thread (outdated is not resolved)",
  );
  const ids = new Set(allThreads.flatMap((t) => t.comments));
  requireThat(
    inline.every((c) => ids.has(c.id)),
    "Inline comment has no fully read thread",
  );
  const checks = await api.pages(
    `${prefix}/commits/${b.head}/check-runs?filter=all`,
    "check_runs",
  );
  const quota = await readQuota(api, b, comments, checks, now, reviews);
  const reviewStarts = await readReviewStarts(
    api,
    b,
    comments,
    startArtifacts,
    now,
  );
  const providers = verifyProviders(
    b,
    comments,
    reviews,
    checks,
    quota,
    reviewStarts,
  );
  const ci = await verifyCI(api, b, checks);
  requireThat(
    JSON.stringify(await binding(api, pr)) === JSON.stringify(b),
    "Candidate drift during evidence read",
  );
  const evidence = {
    b,
    providers,
    threads: allThreads,
    ci,
    ...(Object.keys(reviewStarts).length ? { reviewStarts } : {}),
    // Preserve old findings and dispositions; new no-findings never deletes their evidence.
    reports: [...comments, ...reviews, ...inline]
      .filter((c) => providers.findings.includes(c.id) || inline.includes(c))
      .map((c) => [c.id, c.body, c.state, c.updated_at, c.commit_id]),
  };
  const digest = new Bun.CryptoHasher("sha256")
    .update(JSON.stringify(evidence))
    .digest("hex");
  return { b, digest, evidence, reviews };
}
