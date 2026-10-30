import {
  repository,
  repositoryId,
  ownerId,
  requireThat,
  requestBodies,
} from "./identity.ts";
import { Github } from "./github.ts";
import type { Api } from "./github.ts";
import {
  binding,
  snapshot,
  requestComments,
  verifyExecutor,
} from "./evidence.ts";
import { approve, withdraw, type OperationOutcome } from "./approve.ts";
import { installationToken } from "./credentials.ts";
import { uncertainIntents, type Intent } from "./intent.ts";
import {
  prepareQuota,
  quotaRequest,
  quotaWithdrawal,
  latestCodeRequest,
  quotaDeclaration,
  uneditedOwner,
} from "./quota.ts";
import { appendFileSync, writeFileSync } from "node:fs";
import {
  captureReviewStart,
  startSelection,
  type StartSelection,
} from "./review-start.ts";

export function context(env = process.env) {
  requireThat(
    env.GITHUB_EVENT_NAME === "workflow_dispatch" &&
      env.GITHUB_REPOSITORY === repository &&
      Number(env.GITHUB_REPOSITORY_ID) === repositoryId &&
      env.GITHUB_REF === "refs/heads/dev" &&
      Number(env.GITHUB_ACTOR_ID) === ownerId &&
      Number(env.CLADIRON_TRIGGERING_ACTOR_ID) === ownerId,
    "Only the owner-dispatched trusted dev workflow is accepted",
  );
}
export async function trustedCandidate(
  api: Api,
  pr: number,
  revision: string | undefined,
) {
  const candidate = await binding(api, pr);
  requireThat(
    candidate.base === revision,
    "Trusted workflow is not this candidate's baseline",
  );
  await verifyExecutor(api, candidate);
  return candidate;
}
type Credential = { client: Api; revoke: () => Promise<void> };
async function boundEvidence(
  api: Api,
  pr: number,
  revision: string | undefined,
  startArtifacts: StartSelection,
  planned?: Intent,
) {
  const s = await snapshot(api, pr, Date.now(), startArtifacts);
  requireThat(s.b.base === revision, "Dev moved during evidence read");
  if (planned)
    requireThat(
      s.digest === planned.digest &&
        JSON.stringify(s.b) === JSON.stringify(planned.b),
      "Evidence differs from persisted operation intent",
    );
  return s;
}
async function candidateEvidence(
  api: Api,
  pr: number,
  revision: string | undefined,
  startArtifacts: StartSelection,
  invalidate: () => void,
  planned?: Intent,
) {
  const candidate = await trustedCandidate(api, pr, revision);
  try {
    if (planned)
      requireThat(
        JSON.stringify(candidate) === JSON.stringify(planned.b),
        "Candidate changed before credential creation",
      );
    return await boundEvidence(api, pr, revision, startArtifacts, planned);
  } catch (error) {
    // Evidence invalidation starts only after trust and ends before credentials.
    await trustedCandidate(api, pr, revision);
    invalidate();
    throw error;
  }
}
export async function prepareIntent(
  api: Api,
  pr: number,
  revision: string | undefined,
  startArtifacts: StartSelection,
  run: number,
  attempt: number,
  invalidate: () => void,
): Promise<Intent> {
  requireThat(
    Number.isSafeInteger(run) &&
      run > 0 &&
      Number.isSafeInteger(attempt) &&
      attempt > 0,
    "Invalid operation intent run",
  );
  const s = await candidateEvidence(
    api,
    pr,
    revision,
    startArtifacts,
    invalidate,
  );
  return {
    version: 1,
    repositoryId,
    run,
    attempt,
    b: s.b,
    digest: s.digest,
    startArtifacts,
    ...(s.evidence.reviewStarts
      ? { reviewStarts: s.evidence.reviewStarts }
      : {}),
    ...(s.evidence.providers.quota
      ? { quota: s.evidence.providers.quota }
      : {}),
  };
}
export async function invalidateCandidate(
  api: Api,
  pr: number,
  revision: string | undefined,
  mint: () => Promise<Credential>,
) {
  await trustedCandidate(api, pr, revision);
  const credential = await mint();
  try {
    return await withdraw(credential.client, pr);
  } finally {
    await credential.revoke();
  }
}
export async function approveCandidate(
  api: Api,
  planned: Intent,
  revision: string | undefined,
  startArtifacts: StartSelection,
  controls: {
    invalidate: () => void;
    persist: (outcome: OperationOutcome) => Promise<unknown>;
    mint: () => Promise<Credential>;
  },
) {
  const pr = planned.b.pr;
  const outcome: OperationOutcome = {
    attempted: false,
    resolved: false,
    reviews: [],
  };
  let credential: Awaited<ReturnType<typeof controls.mint>> | undefined;
  try {
    const uncertain = await uncertainIntents(
      api,
      pr,
      planned.run,
      planned.attempt,
    );
    await candidateEvidence(
      api,
      pr,
      revision,
      startArtifacts,
      controls.invalidate,
      planned,
    );
    const load = (client: Api, number: number) =>
      boundEvidence(client, number, revision, startArtifacts, planned);
    credential = await controls.mint();
    return await approve(
      credential.client,
      pr,
      `run-${planned.run}-attempt-${planned.attempt}`,
      load,
      uncertain,
      outcome,
    );
  } finally {
    try {
      await controls.persist(outcome);
    } finally {
      if (credential) await credential.revoke();
    }
  }
}
if (import.meta.main) {
  let invalidate = false;
  try {
    context();
    const event = await Bun.file(process.env.GITHUB_EVENT_PATH!).json();
    const pr = Number(event.inputs?.pr);
    requireThat(
      Number.isSafeInteger(pr) && pr > 0,
      "PR must be a positive integer",
    );
    const api = new Github(process.env.GITHUB_TOKEN!);
    const mint = async () => {
      const credential = await installationToken();
      return {
        client: new Github(process.env.GITHUB_TOKEN!, credential.token),
        revoke: credential.revoke,
      };
    };
    const startArtifacts = startSelection(event.inputs ?? {});
    const current = await api.get(`/repos/${repository}/git/ref/heads/dev`);
    requireThat(
      process.env.GITHUB_WORKFLOW_SHA === current.object?.sha,
      "Trusted workflow revision is no longer current dev",
    );
    if (process.argv[2] === "invalidate") {
      requireThat(
        event.inputs?.mode === "approve" && Number.isSafeInteger(pr) && pr > 0,
        "Invalid withdrawal context",
      );
      console.log(
        JSON.stringify({
          withdrawn: await invalidateCandidate(
            api,
            pr,
            process.env.GITHUB_WORKFLOW_SHA,
            mint,
          ),
        }),
      );
      process.exit(0);
    }
    if (process.argv[2] === "intent") {
      requireThat(
        event.inputs?.mode === "approve",
        "Intent requires approve mode",
      );
      const intent = await prepareIntent(
        api,
        pr,
        process.env.GITHUB_WORKFLOW_SHA,
        startArtifacts,
        Number(process.env.GITHUB_RUN_ID),
        Number(process.env.GITHUB_RUN_ATTEMPT),
        () => {
          invalidate = true;
        },
      );
      await Bun.write(
        process.env.APPROVAL_INTENT_PATH!,
        JSON.stringify(intent) + "\n",
      );
    } else if (event.inputs?.mode === "prepare") {
      requireThat(
        Object.keys(startArtifacts).length === 0,
        "Review-start selection is approve-only",
      );
      const b = await trustedCandidate(
        api,
        pr,
        process.env.GITHUB_WORKFLOW_SHA,
      );
      const quotaCheck = event.inputs?.cubic_quota_check;
      const quota = quotaCheck
        ? await prepareQuota(api, b, Number(quotaCheck))
        : undefined;
      const comments = await requestComments(api, b);
      const existingCode = latestCodeRequest(comments, b);
      const existingCubic = comments.find(
        (c) => uneditedOwner(c) && c.body === requestBodies(b)[0],
      );
      const reviewStart = await captureReviewStart(
        b,
        comments,
        process.env,
        async () => {
          const current = await trustedCandidate(
            api,
            pr,
            process.env.GITHUB_WORKFLOW_SHA,
          );
          return { b: current, comments: await requestComments(api, current) };
        },
      );
      if (reviewStart) {
        requireThat(
          process.env.REVIEW_START_PATH && process.env.GITHUB_OUTPUT,
          "Review-start output paths unavailable",
        );
        writeFileSync(
          process.env.REVIEW_START_PATH,
          JSON.stringify(reviewStart) + "\n",
          { flag: "wx" },
        );
        appendFileSync(
          process.env.GITHUB_OUTPUT,
          "review_start_captured=true\n",
        );
      }
      console.log(
        JSON.stringify(
          {
            pr,
            head: b.head,
            base: b.base,
            cubic: quota || existingCubic ? null : requestBodies(b)[0],
            codex: quota
              ? existingCode
                ? quotaDeclaration(b, quota, existingCode.id)
                : quotaRequest(b, quota)
              : existingCode
                ? null
                : requestBodies(b)[1],
            existingCodexRequest: existingCode?.id,
            reviewStartRecordedLocally: Boolean(reviewStart),
            reviewStartArtifact: reviewStart
              ? `cladiron-review-start-${reviewStart.producer.run}-${reviewStart.producer.attempt}`
              : null,
            securityOnlyIfMissing: requestBodies(b)[2],
            ...(quota
              ? {
                  quotaSelection: quota,
                  withdrawQuotaSelection: quotaWithdrawal(b),
                  operatorResponsibility:
                    "Before posting, verify applicable pool/cycle and available recovery surfaces; confirm no known recovery or conflicting later selection.",
                }
              : {}),
          },
          null,
          2,
        ),
      );
    } else {
      requireThat(event.inputs?.mode === "approve", "Unknown approval mode");
      requireThat(
        !event.inputs?.cubic_quota_check,
        "Quota source input is prepare-only; approve consumes the posted unedited selection",
      );
      const planned: Intent = await Bun.file(
        process.env.APPROVAL_INTENT_PATH!,
      ).json();
      requireThat(
        planned.run === Number(process.env.GITHUB_RUN_ID) &&
          planned.attempt === Number(process.env.GITHUB_RUN_ATTEMPT) &&
          planned.repositoryId === repositoryId &&
          planned.b.pr === pr &&
          JSON.stringify(planned.startArtifacts ?? {}) ===
            JSON.stringify(startArtifacts),
        "Approval lacks this run's persisted intent",
      );
      console.log(
        JSON.stringify(
          await approveCandidate(
            api,
            planned,
            process.env.GITHUB_WORKFLOW_SHA,
            startArtifacts,
            {
              invalidate: () => {
                invalidate = true;
              },
              persist: (outcome) =>
                Bun.write(
                  process.env.APPROVAL_INTENT_PATH!,
                  JSON.stringify({ ...planned, outcome }) + "\n",
                ),
              mint,
            },
          ),
        ),
      );
    }
  } catch (error) {
    if (invalidate && process.env.GITHUB_OUTPUT)
      appendFileSync(process.env.GITHUB_OUTPUT, "invalidate=true\n");
    // Do not emit raw HTTP bodies, credential values, or stack traces.
    console.error(
      error instanceof Error ? error.message : "Approval operation failed",
    );
    process.exitCode = 1;
  }
}
