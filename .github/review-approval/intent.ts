import {
  repository,
  repositoryId,
  requireThat,
  sha,
  type Binding,
} from "./identity.ts";
import type { Api } from "./github.ts";
import type { QuotaEvidence } from "./quota.ts";
export type Intent = {
  version: 1;
  repositoryId: number;
  run: number;
  attempt: number;
  b: Binding;
  digest: string;
  quota?: QuotaEvidence;
};
export async function uncertainIntents(
  api: Api,
  pr: number,
  currentRun: number,
  currentAttempt: number,
): Promise<Intent[]> {
  const prefix = `/repos/${repository}`;
  const runs = await api.pages(
    `${prefix}/actions/workflows/review-approval.yml/runs?event=workflow_dispatch&branch=dev`,
    "workflow_runs",
  );
  const found: Intent[] = [];
  for (const run of runs) {
    const current = run.id === currentRun;
    requireThat(
      run.path === ".github/workflows/review-approval.yml" &&
        run.repository?.id === repositoryId,
      "Unexpected previous approval executor",
    );
    requireThat(
      current || run.status === "completed",
      `Previous approval executor ${run.id} has not terminated`,
    );
    if (!current && run.conclusion === "success" && run.run_attempt === 1)
      continue;
    // Inspect all attempts: a successful rerun cannot erase an earlier unknown POST.
    const artifacts = await api.pages(
      `${prefix}/actions/runs/${run.id}/artifacts`,
      "artifacts",
    );
    requireThat(
      Number.isSafeInteger(run.run_attempt) &&
        run.run_attempt > 0 &&
        (!current || run.run_attempt === currentAttempt),
      "Approval run attempt identity unavailable",
    );
    const intents = [];
    const lastAttempt = current ? currentAttempt - 1 : run.run_attempt;
    for (let attempt = 1; attempt <= lastAttempt; attempt++) {
      const jobs = await api.pages(
        `${prefix}/actions/runs/${run.id}/attempts/${attempt}/jobs`,
        "jobs",
      );
      requireThat(
        jobs.every((j) => Array.isArray(j.steps)),
        `Missing approval job steps for ${run.id}/${attempt}`,
      );
      const writers = jobs
        .flatMap((j) => j.steps)
        .filter(
          (s: any) =>
            s.name === "Verify evidence and perform selected operation",
        );
      requireThat(
        writers.length <= 1,
        `Multiple writers for ${run.id}/${attempt}`,
      );
      if (
        !writers.length ||
        writers[0].status === "queued" ||
        writers[0].conclusion === "skipped" ||
        writers[0].conclusion === "success"
      )
        continue;
      const matching = artifacts.filter(
        (a) => a.name === `cladiron-approval-intent-${run.id}-${attempt}`,
      );
      requireThat(
        matching.length === 1,
        `Previous approval ${run.id}/${attempt} lost its intent; outcome requires owner investigation`,
      );
      intents.push(matching[0]);
    }
    for (const artifact of intents) {
      requireThat(
        !artifact.expired &&
          artifact.workflow_run?.id === run.id &&
          artifact.workflow_run?.repository_id === repositoryId,
        `Previous approval intent ${artifact.id} unavailable`,
      );
      const intent: Intent = await api.receipt(artifact, "operation.json");
      requireThat(
        intent.version === 1 &&
          intent.repositoryId === repositoryId &&
          intent.run === run.id &&
          artifact.name ===
            `cladiron-approval-intent-${intent.run}-${intent.attempt}` &&
          Number.isSafeInteger(intent.b?.pr) &&
          sha(intent.b?.head) &&
          sha(intent.b?.base) &&
          sha(intent.b?.merge) &&
          /^[a-f0-9]{64}$/.test(intent.digest),
        "Previous intent identity invalid",
      );
      const results = artifacts.filter(
        (a) =>
          a.name === `cladiron-approval-result-${intent.run}-${intent.attempt}`,
      );
      requireThat(results.length <= 1, "Ambiguous prior operation result");
      if (results.length) {
        const resultArtifact = results[0];
        requireThat(
          !resultArtifact.expired &&
            resultArtifact.workflow_run?.id === run.id &&
            resultArtifact.workflow_run?.repository_id === repositoryId,
          "Prior operation result is unavailable or unbound",
        );
        const result = await api.receipt(resultArtifact, "operation.json");
        requireThat(
          result.version === intent.version &&
            result.repositoryId === intent.repositoryId &&
            result.run === intent.run &&
            result.attempt === intent.attempt &&
            result.digest === intent.digest &&
            JSON.stringify(result.b) === JSON.stringify(intent.b),
          "Prior operation result binding differs from intent",
        );
        if (result.outcome) {
          requireThat(
            typeof result.outcome.attempted === "boolean" &&
              typeof result.outcome.resolved === "boolean" &&
              Array.isArray(result.outcome.reviews) &&
              result.outcome.reviews.every(
                (id: unknown) => Number.isSafeInteger(id) && Number(id) > 0,
              ),
            "Prior operation outcome shape unknown",
          );
          if (
            !result.outcome.attempted ||
            (result.outcome.resolved && result.outcome.reviews.length > 0)
          )
            continue;
        }
      }
      if (intent.b.pr === pr) found.push(intent);
    }
  }
  return found;
}
