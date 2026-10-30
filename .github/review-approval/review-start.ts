import {
  approvalWorkflow,
  codex,
  ownerId,
  repository,
  repositoryId,
  requireThat,
  sha,
  type Binding,
} from "./identity.ts";
import type { Api } from "./github.ts";
import {
  codexRequests,
  summaryState,
  type Record as Data,
} from "./providers.ts";
import { codexSummary } from "./provider-text.ts";
import { uneditedOwner } from "./quota.ts";

type Lane = "code" | "security";
export type StartSelection = { code?: number; security?: number };
type Requests = NonNullable<ReturnType<typeof codexRequests>>;
const lanes = ["code", "security"] as const;
const verified = Symbol("verified-review-start");
export type ReviewStart = {
  readonly [verified]: true;
  artifact: {
    id: number;
    name: string;
    digest: string;
    run: number;
    attempt: number;
  };
  proof: StartProof;
  start: string;
};
export type ReviewStarts = { code?: ReviewStart; security?: ReviewStart };
export type StartProof = {
  version: 1;
  repositoryId: number;
  pr: number;
  head: string;
  base: string;
  producer: { run: number; attempt: number; revision: string };
  capturedAt: string;
  requests: { code: Data; security: Data };
  summary: Data;
  starts: { code?: string; security?: string };
};
const prefix = `/repos/${repository}`;
const positive = (id: unknown) => Number.isSafeInteger(id) && Number(id) > 0;
function time(value: unknown) {
  requireThat(
    typeof value === "string" &&
      /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,9})?Z$/.test(value),
    "Missing review-start timestamp",
  );
  const result = Date.parse(value);
  requireThat(
    Number.isFinite(result) &&
      new Date(result).toISOString().slice(0, 19) === value.slice(0, 19),
    "Invalid review-start timestamp",
  );
  return result;
}
function requestStamp(request: Data) {
  requireThat(
    positive(request.id) &&
      typeof request.node_id === "string" &&
      uneditedOwner(request),
    "Review-start request is not an unedited owner request",
  );
  time(request.created_at);
  return {
    id: request.id,
    node_id: request.node_id,
    user: { id: ownerId, type: "User" },
    body: request.body,
    created_at: request.created_at,
    updated_at: request.updated_at,
    lastEditedAt: null,
  };
}
function summaryStamp(summary: Data) {
  requireThat(
    positive(summary.id) &&
      typeof summary.node_id === "string" &&
      summary.user?.id === codex.bot &&
      summary.user.type === "Bot" &&
      summary.performed_via_github_app?.id === codex.app &&
      typeof summary.body === "string",
    "Untrusted review-start summary",
  );
  requireThat(
    time(summary.created_at) <= time(summary.updated_at),
    "Review-start summary timestamps contradict",
  );
  return {
    id: summary.id,
    node_id: summary.node_id,
    user: { id: codex.bot, type: "Bot" },
    performed_via_github_app: { id: codex.app },
    body: summary.body,
    created_at: summary.created_at,
    updated_at: summary.updated_at,
  };
}
function running(
  summary: Data,
  b: Binding,
  requests: Requests,
  capturedAt: string,
) {
  const stamp = summaryStamp(summary);
  const { state, security } = summaryState(stamp.body);
  requireThat(
    security.repository === repository &&
      security.pullRequestNumber === b.pr &&
      security.headSha === b.head &&
      security.status === state.security_review.status,
    "Review-start PR/head association differs",
  );
  const layout = codexSummary(stamp.body, state, security, true);
  requireThat(layout, "Unknown review-start summary layout or report surface");
  requireThat(
    time(stamp.updated_at) <= time(capturedAt),
    "Review-start observation predates provider response",
  );
  const starts: StartProof["starts"] = {};
  for (const lane of lanes) {
    const value = state[`${lane}_review`];
    if (value.status !== "running") continue;
    requireThat(value.commit_sha === b.head, "Running review has another head");
    const start = lane === "code" ? layout.codeAt : layout.securityAt;
    requireThat(
      time(start) > time(requests[lane].created_at) &&
        time(start) <= time(stamp.updated_at),
      "Review did not start after its bound request",
    );
    starts[lane] = start;
  }
  return { stamp, starts };
}
export function startSelection(inputs: Data): StartSelection {
  const selection: StartSelection = {};
  for (const lane of lanes) {
    const value = inputs[`${lane}_start_artifact`];
    if (value === undefined || value === "") continue;
    requireThat(
      typeof value === "string" &&
        /^[1-9][0-9]*$/.test(value) &&
        positive(Number(value)),
      "Invalid selected review-start artifact ID",
    );
    selection[lane] = Number(value);
  }
  return selection;
}
export async function captureReviewStart(
  b: Binding,
  comments: Data[],
  env: NodeJS.ProcessEnv,
  reread: () => Promise<{ b: Binding; comments: Data[] }>,
  now = Date.now(),
): Promise<StartProof | undefined> {
  requireThat(
    env.GITHUB_EVENT_NAME === "workflow_dispatch" &&
      env.GITHUB_REF === "refs/heads/dev" &&
      env.GITHUB_REPOSITORY === repository &&
      Number(env.GITHUB_REPOSITORY_ID) === repositoryId &&
      env.GITHUB_WORKFLOW_REF ===
        `${repository}/${approvalWorkflow}@refs/heads/dev` &&
      env.GITHUB_WORKFLOW_SHA === b.base &&
      Number(env.GITHUB_ACTOR_ID) === ownerId &&
      Number(env.CLADIRON_TRIGGERING_ACTOR_ID) === ownerId &&
      positive(Number(env.GITHUB_RUN_ID)) &&
      positive(Number(env.GITHUB_RUN_ATTEMPT)),
    "Review-start producer is not the owner trusted dev prepare",
  );
  const requests = codexRequests(comments, b);
  if (!requests) return undefined;
  const summaries = comments.filter(
    (c) =>
      c.user?.id === codex.bot &&
      c.body?.includes("<!-- codex-review-state:v1 "),
  );
  if (!summaries.length) return undefined;
  requireThat(summaries.length === 1, "Ambiguous review-start summary");
  const state = summaryState(summaries[0].body).state;
  if (!lanes.some((lane) => state[`${lane}_review`].status === "running"))
    return undefined;
  const capturedAt = new Date(now).toISOString();
  const { stamp, starts } = running(summaries[0], b, requests, capturedAt);
  requireThat(
    Object.keys(starts).length > 0,
    "No trustworthy running review lane",
  );
  const requestStamps = {
    code: requestStamp(requests.code),
    security: requestStamp(requests.security),
  };
  const after = await reread();
  const afterRequests = codexRequests(after.comments, after.b);
  requireThat(
    after.b.pr === b.pr &&
      after.b.head === b.head &&
      after.b.base === b.base &&
      afterRequests &&
      lanes.every(
        (lane) =>
          JSON.stringify(requestStamp(afterRequests[lane])) ===
          JSON.stringify(requestStamps[lane]),
      ),
    "Candidate/request changed during review-start capture",
  );
  // Keep the single complete provider response observed above, never splice later edits into it.
  const proof: StartProof = {
    version: 1,
    repositoryId,
    pr: b.pr,
    head: b.head,
    base: b.base,
    producer: {
      run: Number(env.GITHUB_RUN_ID),
      attempt: Number(env.GITHUB_RUN_ATTEMPT),
      revision: b.base,
    },
    capturedAt,
    requests: requestStamps,
    summary: stamp,
    starts,
  };
  requireThat(
    Buffer.byteLength(JSON.stringify(proof)) < 16384,
    "Review-start evidence exceeds bounded artifact member",
  );
  return proof;
}
export async function readReviewStarts(
  api: Api,
  b: Binding,
  comments: Data[],
  selection: StartSelection,
  now = Date.now(),
): Promise<ReviewStarts> {
  const selected = lanes.filter((lane) => selection[lane] !== undefined);
  if (!selected.length) return {};
  const requests = codexRequests(comments, b);
  requireThat(requests, "Selected review-start has no current requests");
  const cache = new Map<
    number,
    { artifact: ReviewStart["artifact"]; proof: StartProof }
  >();
  const result: ReviewStarts = {};
  for (const lane of selected) {
    const id = selection[lane]!;
    requireThat(positive(id), "Invalid review-start artifact selection");
    let loaded = cache.get(id);
    if (!loaded) {
      const artifact = await api.get(`${prefix}/actions/artifacts/${id}`);
      const match = artifact.name?.match(
        /^cladiron-review-start-([1-9][0-9]*)-([1-9][0-9]*)$/,
      );
      requireThat(
        artifact.id === id &&
          artifact.expired === false &&
          match &&
          positive(Number(match[1])) &&
          positive(Number(match[2])) &&
          /^sha256:[a-f0-9]{64}$/.test(artifact.digest ?? "") &&
          artifact.workflow_run?.id === Number(match[1]) &&
          artifact.workflow_run?.repository_id === repositoryId &&
          artifact.workflow_run?.head_repository_id === repositoryId &&
          artifact.workflow_run?.head_sha === b.base,
        "Unbound review-start artifact",
      );
      const run = await api.get(`${prefix}/actions/runs/${match[1]}`);
      requireThat(
        run.id === Number(match[1]) &&
          run.run_attempt === Number(match[2]) &&
          run.path === approvalWorkflow &&
          run.event === "workflow_dispatch" &&
          run.head_branch === "dev" &&
          run.head_sha === b.base &&
          run.actor?.id === ownerId &&
          run.triggering_actor?.id === ownerId &&
          run.repository?.id === repositoryId &&
          run.head_repository?.id === repositoryId &&
          run.status === "completed" &&
          run.conclusion === "success",
        "Unknown or unsuccessful review-start producer",
      );
      const artifacts = await api.pages(
        `${prefix}/actions/runs/${run.id}/artifacts`,
        "artifacts",
      );
      const named = artifacts.filter((a) => a.name === artifact.name);
      requireThat(
        named.length === 1 &&
          named[0].id === id &&
          named[0].digest === artifact.digest,
        "Review-start artifact is missing or ambiguous in producer run",
      );
      const jobs = await api.pages(
        `${prefix}/actions/runs/${run.id}/attempts/${run.run_attempt}/jobs`,
        "jobs",
      );
      const capture = jobs.filter(
        (j) => j.name === "approval" && j.run_id === run.id,
      );
      requireThat(
        capture.length === 1 &&
          capture[0].status === "completed" &&
          capture[0].conclusion === "success" &&
          ["Prepare review request text", "Upload review start evidence"].every(
            (name) => {
              const steps = capture[0].steps?.filter(
                (s: Data) => s.name === name,
              );
              return (
                steps?.length === 1 &&
                steps[0].status === "completed" &&
                steps[0].conclusion === "success"
              );
            },
          ),
        "Review-start capture/upload did not succeed",
      );
      const checkId = Number(capture[0].check_run_url?.split("/").at(-1));
      requireThat(positive(checkId), "Missing review-start Actions check");
      const check = await api.get(`${prefix}/check-runs/${checkId}`);
      requireThat(
        check.id === checkId &&
          check.app?.id === 15368 &&
          check.check_suite?.id === run.check_suite_id &&
          check.conclusion === "success",
        "Untrusted review-start Actions check",
      );
      const proof: StartProof = await api.receipt(
        artifact,
        "review-start.json",
      );
      requireThat(
        proof.version === 1 &&
          proof.repositoryId === repositoryId &&
          proof.pr === b.pr &&
          proof.head === b.head &&
          proof.base === b.base &&
          sha(proof.head) &&
          sha(proof.base) &&
          proof.producer?.run === run.id &&
          proof.producer.attempt === run.run_attempt &&
          proof.producer.revision === b.base &&
          time(proof.capturedAt) <= now,
        "Review-start proof identity/time differs",
      );
      loaded = {
        artifact: {
          id,
          name: artifact.name,
          digest: artifact.digest,
          run: run.id,
          attempt: run.run_attempt,
        },
        proof,
      };
      cache.set(id, loaded);
    }
    const { proof } = loaded;
    requireThat(
      proof.requests &&
        JSON.stringify(requestStamp(proof.requests[lane])) ===
          JSON.stringify(requestStamp(requests[lane])),
      "Review-start request changed",
    );
    // Reconstruct the provider observation; serialized derived lane fields confer no trust.
    const capturedRequests = {
      code: proof.requests.code,
      security: proof.requests.security,
    };
    const parsed = running(
      proof.summary,
      b,
      capturedRequests,
      proof.capturedAt,
    );
    requireThat(
      JSON.stringify(parsed.starts) === JSON.stringify(proof.starts) &&
        parsed.starts[lane],
      "Review-start artifact lacks selected lane",
    );
    result[lane] = { [verified]: true, ...loaded, start: parsed.starts[lane]! };
  }
  return result;
}
export function verifyReviewCompletion(
  start: ReviewStart,
  summary: Data,
  completedAt: string,
) {
  requireThat(
    start[verified] === true,
    "Review-start evidence was not verified",
  );
  const before = summaryStamp(start.proof.summary),
    after = summaryStamp(summary);
  requireThat(
    after.id === before.id &&
      after.node_id === before.node_id &&
      after.created_at === before.created_at &&
      time(completedAt) > time(start.start) &&
      time(completedAt) >= time(start.proof.capturedAt) &&
      time(after.updated_at) >= time(completedAt) &&
      time(after.updated_at) >= time(before.updated_at),
    "Completed review does not follow the captured summary lane",
  );
}
