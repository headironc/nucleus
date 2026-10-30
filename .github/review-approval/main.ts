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
import { appendFileSync } from "node:fs";

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
      await trustedCandidate(api, pr, process.env.GITHUB_WORKFLOW_SHA);
      const credential = await installationToken();
      try {
        console.log(
          JSON.stringify({
            withdrawn: await withdraw(
              new Github(process.env.GITHUB_TOKEN!, credential.token),
              pr,
            ),
          }),
        );
      } finally {
        await credential.revoke();
      }
      process.exit(0);
    }
    if (process.argv[2] === "intent") {
      requireThat(
        event.inputs?.mode === "approve",
        "Intent requires approve mode",
      );
      await trustedCandidate(api, pr, process.env.GITHUB_WORKFLOW_SHA);
      invalidate = true;
      const s = await snapshot(api, pr);
      requireThat(
        s.b.base === process.env.GITHUB_WORKFLOW_SHA,
        "Dev moved during intent read",
      );
      const intent: Intent = {
        version: 1,
        repositoryId,
        run: Number(process.env.GITHUB_RUN_ID),
        attempt: Number(process.env.GITHUB_RUN_ATTEMPT),
        b: s.b,
        digest: s.digest,
        ...(s.evidence.providers.quota
          ? { quota: s.evidence.providers.quota }
          : {}),
      };
      requireThat(
        Number.isSafeInteger(intent.run) &&
          Number.isSafeInteger(intent.attempt),
        "Invalid operation intent run",
      );
      invalidate = false;
      await Bun.write(
        process.env.APPROVAL_INTENT_PATH!,
        JSON.stringify(intent) + "\n",
      );
    } else if (event.inputs?.mode === "prepare") {
      const b = await binding(api, pr);
      const quotaCheck = event.inputs?.cubic_quota_check;
      const quota = quotaCheck
        ? await prepareQuota(api, b, Number(quotaCheck))
        : undefined;
      const comments = await requestComments(api, b);
      const existingCode = latestCodeRequest(comments, b);
      const existingCubic = comments.find(
        (c) => uneditedOwner(c) && c.body === requestBodies(b)[0],
      );
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
          planned.b.pr === pr,
        "Approval lacks this run's persisted intent",
      );
      const outcome: OperationOutcome = {
        attempted: false,
        resolved: false,
        reviews: [],
      };
      let credential: Awaited<ReturnType<typeof installationToken>> | undefined;
      try {
        const uncertain = await uncertainIntents(
          api,
          pr,
          planned.run,
          planned.attempt,
        );
        const candidate = await trustedCandidate(
          api,
          pr,
          process.env.GITHUB_WORKFLOW_SHA,
        );
        requireThat(
          JSON.stringify(candidate) === JSON.stringify(planned.b),
          "Candidate changed before credential creation",
        );
        credential = await installationToken();
        const load = async (client: Api, number: number) => {
          const s = await snapshot(client, number);
          requireThat(
            s.b.base === process.env.GITHUB_WORKFLOW_SHA &&
              s.digest === planned.digest &&
              JSON.stringify(s.b) === JSON.stringify(planned.b),
            "Evidence differs from persisted operation intent",
          );
          return s;
        };
        console.log(
          JSON.stringify(
            await approve(
              new Github(process.env.GITHUB_TOKEN!, credential.token),
              pr,
              `run-${planned.run}-attempt-${planned.attempt}`,
              load,
              uncertain,
              outcome,
            ),
          ),
        );
      } finally {
        try {
          await Bun.write(
            process.env.APPROVAL_INTENT_PATH!,
            JSON.stringify({ ...planned, outcome }) + "\n",
          );
        } finally {
          if (credential) await credential.revoke();
        }
      }
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
