import {
  codex,
  cubic,
  ownerId,
  requestBodies,
  requireThat,
  sha,
  type Binding,
} from "./identity.ts";
import {
  codexReceipt,
  cubicReport,
  codexSummary,
  cubicOutput,
  cubicCompletionSummary,
  cubicCheckReport,
} from "./provider-text.ts";
import {
  latestCodeRequest,
  uneditedOwner,
  type QuotaEvidence,
} from "./quota.ts";
import { verifyReviewCompletion, type ReviewStarts } from "./review-start.ts";
import { reviewPublication } from "./reviews.ts";

export type Record = { [key: string]: any };
export type ProviderEvidence = {
  cubic: "passed" | "quota-unavailable";
  quota?: QuotaEvidence;
  requests: { id: number; createdAt: string }[];
  results: number[];
  findings: number[];
  checkReports: Record[];
  reviewReports: ReturnType<typeof reviewPublication>[];
};
function time(value: unknown) {
  requireThat(
    typeof value === "string" && Number.isFinite(Date.parse(value)),
    "Missing provider timestamp",
  );
  return Date.parse(value);
}
function bot(
  value: Record,
  identity: { app: number; bot: number },
  review = false,
) {
  return (
    value.user?.id === identity.bot &&
    value.user?.type === "Bot" &&
    (value.performed_via_github_app?.id === identity.app ||
      (review &&
        identity === cubic &&
        value.performed_via_github_app === undefined))
  );
}
function latest(
  values: Record[],
  label: string,
  timestamp = (r: Record) => r.updated_at ?? r.submitted_at,
) {
  requireThat(values.length > 0, `Missing ${label}`);
  const ordered = [...values].sort(
    (a, b) => time(timestamp(b)) - time(timestamp(a)),
  );
  requireThat(
    ordered.length === 1 ||
      time(timestamp(ordered[0])) !== time(timestamp(ordered[1])),
    `Ambiguous ${label}`,
  );
  return ordered[0];
}
function requests(comments: Record[], b: Binding, index: number) {
  const command = requestBodies(b)[index];
  return comments.filter((c) => c.body === command && uneditedOwner(c));
}
function jsonMarker(body: unknown, name: string) {
  requireThat(typeof body === "string", "Missing provider body");
  const matches = [
    ...body.matchAll(new RegExp(`<!-- ${name}:v1 (\\{[^\\n]*\\}) -->`, "g")),
  ];
  requireThat(matches.length === 1, `Unknown or ambiguous ${name} shape`);
  try {
    return JSON.parse(matches[0][1]);
  } catch {
    throw new Error(`Invalid ${name} JSON`);
  }
}
export function summaryState(body: unknown) {
  const state = jsonMarker(body, "codex-review-state"),
    security = jsonMarker(body, "codex-security-review");
  const keys = (value: any, names: string[]) =>
    value !== null &&
    typeof value === "object" &&
    !Array.isArray(value) &&
    Object.keys(value).sort().join(",") === names.sort().join(",");
  requireThat(
    keys(state, ["code_review", "security_review"]),
    "Unknown Codex summary state fields",
  );
  for (const kind of ["code_review", "security_review"])
    requireThat(
      keys(state[kind], [
        "commit_sha",
        "has_any_findings",
        "has_unresolved_findings",
        "status",
      ]) && sha(state[kind].commit_sha),
      "Unknown Codex review state fields",
    );
  for (const kind of ["code_review", "security_review"])
    requireThat(
      typeof state[kind].status === "string" &&
        [
          state[kind].has_any_findings,
          state[kind].has_unresolved_findings,
        ].every((v) => v === null || typeof v === "boolean"),
      "Unknown Codex review state values",
    );
  requireThat(
    keys(security, [
      "blockingSeverityThreshold",
      "headSha",
      "mergeGateEnabled",
      "pullRequestNumber",
      "repository",
      "status",
    ]) &&
      typeof security.blockingSeverityThreshold === "string" &&
      typeof security.mergeGateEnabled === "boolean",
    "Unknown security summary fields",
  );
  return { state, security };
}
function summaryParts(body: unknown) {
  const { state, security } = summaryState(body);
  const layout = codexSummary(body, state, security);
  requireThat(
    layout,
    "Unknown Codex complete summary layout or additional findings",
  );
  return { state, security, layout };
}
export function codexRequests(comments: Record[], b: Binding) {
  const code = latestCodeRequest(comments, b);
  if (!code) return undefined;
  const candidates = comments.filter(
    (c) =>
      c.user?.id === ownerId &&
      c.user?.type === "User" &&
      c.body?.startsWith(requestBodies(b)[2]),
  );
  const security = candidates.length
    ? latest(candidates, "Codex security request")
    : code;
  requireThat(
    uneditedOwner(security) &&
      (security === code || security.body === requestBodies(b)[2]),
    "Latest security request is edited or unknown",
  );
  return { code, security };
}

export function verifyProviders(
  b: Binding,
  comments: Record[],
  reviews: Record[],
  checks: Record[],
  quota?: QuotaEvidence,
  starts: ReviewStarts = {},
): ProviderEvidence {
  const cubicRequest = quota
    ? undefined
    : latest(
        requests(comments, b, 0),
        "unedited owner Cubic request for current H/B",
      );
  const boundRequests = codexRequests(comments, b);
  const codeRequest = boundRequests?.code;
  requireThat(
    codeRequest,
    "Missing unedited owner Codex request for current H/B",
  );
  const securityRequest = boundRequests!.security;
  for (const review of reviews.filter((r) => r.user?.id === cubic.bot))
    requireThat(
      reviewPublication(review).pr === b.pr,
      "Cubic review belongs to another PR",
    );
  const cubicChecks = checks.filter(
    (c) => c.app?.id === cubic.app || c.name === "cubic · AI code reviewer",
  );
  for (const check of cubicChecks) {
    requireThat(
      check.app?.id === cubic.app &&
        check.name === "cubic · AI code reviewer" &&
        check.head_sha === b.head &&
        Number.isSafeInteger(check.id) &&
        check.id > 0,
      "Cubic check identity/head unknown",
    );
    const report = cubicCheckReport(check.output);
    requireThat(
      report !== null,
      `Unknown Cubic check ${check.id}: unparsed report; disposition required`,
    );
    requireThat(
      check.status === "completed" &&
        check.conclusion === (report === "quota" ? "neutral" : "success") &&
        time(check.started_at) <= time(check.completed_at),
      `Cubic check ${check.id} report completion unknown`,
    );
    requireThat(
      check.output.annotations_url == null ||
        check.output.annotations_url ===
          `https://api.github.com/repos/headironc/nucleus/check-runs/${check.id}/annotations`,
      "Cubic annotation surface identity changed",
    );
  }
  let cubicIds: number[] = [],
    cubicDisposition: Record | undefined;
  const disposedByCubic = (report: Record) =>
    cubicDisposition !== undefined &&
    (report === cubicDisposition ||
      time(reviewPublication(report).at) <
        time(reviewPublication(cubicDisposition).at));
  if (!quota) {
    requireThat(cubicRequest, "Missing Cubic request");
    const check = latest(
      cubicChecks.map((c) => ({
        ...c,
        updated_at: c.completed_at ?? c.started_at,
      })),
      "Cubic execution",
    );
    requireThat(
      check.status === "completed" &&
        check.conclusion === "success" &&
        time(check.started_at) > time(cubicRequest.created_at),
      "Cubic did not complete after bound request",
    );
    requireThat(
      cubicCompletionSummary(check.output?.summary),
      "Cubic skipped, limited, unresolved, or unknown completion",
    );
    requireThat(
      (check.output?.text == null || check.output.text === "") &&
        cubicOutput(check.output),
      "Cubic has an additional unparsed report surface",
    );
    const cubicResults = reviews.filter(
      (r) =>
        bot(r, cubic, true) &&
        r.commit_id === b.head &&
        time(r.submitted_at) >= time(check.started_at) &&
        time(r.submitted_at) <= time(check.completed_at) &&
        time(reviewPublication(r).at) <= time(check.completed_at),
    );
    requireThat(
      cubicResults.length === 1,
      "Cubic execution does not identify one unique head report",
    );
    const review = latest(
      cubicResults,
      "Cubic head review",
      (r) => reviewPublication(r).at,
    );
    requireThat(
      review.state === "COMMENTED" && typeof review.body === "string",
      "Cubic review is not a completed report",
    );
    requireThat(
      cubicReport(review.body, b),
      "Unknown Cubic complete report layout or extra findings",
    );
    requireThat(
      new RegExp(
        `<!-- cubic:review-post:ai_pr_review_github_delivery_[a-f0-9]+:${b.head}:[a-f0-9-]+ -->`,
      ).test(review.body),
      "Missing Cubic delivery/head association",
    );
    requireThat(
      /<!-- cubic:review-summary:start -->\s*(?:\*\*No issues found\*\* across \d+ files|\*\*All reported issues addressed\*\*)\s*<!-- cubic:review-summary:end -->/.test(
        review.body,
      ),
      "Cubic report does not explicitly dispose findings",
    );
    const checkFiles = check.output.summary.match(
      /No issues found in (\d+) files\./,
    )?.[1];
    const reviewFiles = review.body.match(
      /\*\*No issues found\*\* across (\d+) files/,
    )?.[1];
    requireThat(
      checkFiles === reviewFiles,
      "Cubic completion reports disagree about reviewed scope",
    );
    const priorCubicFindings = reviews.filter(
      (r) =>
        bot(r, cubic, true) &&
        r.id !== review.id &&
        typeof r.body === "string" &&
        !(
          cubicReport(r.body, { ...b, head: r.commit_id }) &&
          /\*\*No issues found\*\* across \d+ files/.test(r.body)
        ),
    );
    if (review.body.includes("**All reported issues addressed**"))
      cubicDisposition = review;
    requireThat(
      priorCubicFindings.length === 0 ||
        priorCubicFindings.every(disposedByCubic),
      "Cubic findings lack a later explicit disposition",
    );

    cubicIds = [check.id, review.id];
  } else {
    requireThat(
      quota.request === codeRequest.id &&
        quota.head === b.head &&
        quota.base === b.base,
      "Quota selection is not this request/H/B",
    );
    const reports = reviews.filter((r) => bot(r, cubic, true));
    const priorFindings = reports.some(
      (r) =>
        !cubicReport(r.body, { ...b, head: r.commit_id }) ||
        !/\*\*No issues found\*\* across \d+ files/.test(r.body),
    );
    if (priorFindings) {
      const last = latest(
        reports,
        "Cubic finding disposition",
        (r) => reviewPublication(r).at,
      );
      requireThat(
        cubicReport(last.body, { ...b, head: last.commit_id }) &&
          last.body.includes("**All reported issues addressed**"),
        "Quota fallback cannot erase unresolved/unknown Cubic findings",
      );
      cubicDisposition = last;
    }
  }
  for (const report of reviews.filter((r) => r.user?.id === cubic.bot)) {
    requireThat(bot(report, cubic, true), "Cubic report identity changed");
    reviewPublication(report);
    const clean =
      report.state === "COMMENTED" &&
      cubicReport(report.body, { ...b, head: report.commit_id }) &&
      /\*\*No issues found\*\* across \d+ files/.test(report.body);
    requireThat(
      clean || disposedByCubic(report),
      "Cubic report lacks a later explicit disposition",
    );
  }
  const summaries = comments.filter(
    (c) =>
      bot(c, codex) &&
      typeof c.body === "string" &&
      c.body.includes("<!-- codex-review-state:v1 ") &&
      (time(c.created_at) > time(codeRequest.created_at) ||
        [starts.code, starts.security].some(
          (s) => s?.proof.summary.id === c.id,
        )),
  );
  requireThat(
    summaries.length === 1,
    "Codex request does not identify one unique completion summary",
  );
  const summary = latest(summaries, "Codex summary after request");
  const { state, security, layout } = summaryParts(summary.body);
  for (const kind of ["code_review", "security_review"]) {
    requireThat(
      state[kind]?.commit_sha === b.head && state[kind]?.status === "completed",
      `Codex ${kind} missing full-head completion`,
    );
  }
  requireThat(
    state.code_review.has_unresolved_findings === false &&
      typeof state.code_review.has_any_findings === "boolean",
    "Codex code finding disposition unknown",
  );
  for (const [lane, request, completed] of [
    ["code", codeRequest, layout.codeAt],
    ["security", securityRequest, layout.securityAt],
  ] as const) {
    const start = starts[lane];
    if (start) verifyReviewCompletion(start, summary, completed);
    else
      requireThat(
        time(summary.created_at) > time(request.created_at),
        `Codex ${lane} start/completion is not after its bound request`,
      );
  }
  requireThat(
    time(layout.codeAt) > time(codeRequest.created_at) &&
      time(layout.securityAt) > time(securityRequest.created_at) &&
      time(layout.codeAt) >= time(summary.created_at) &&
      time(layout.securityAt) >= time(summary.created_at) &&
      time(layout.codeAt) <= time(summary.updated_at) &&
      time(layout.securityAt) <= time(summary.updated_at),
    "Codex completion timestamps do not bind to the requests/summary",
  );
  requireThat(
    security.headSha === b.head &&
      security.repository === "headironc/nucleus" &&
      security.pullRequestNumber === b.pr &&
      security.status === "completed",
    "Codex security association missing",
  );
  // A provider's configured P0 threshold is not evidence that lower findings were disposed.
  const securityState = state.security_review;
  const previous = comments
    .filter(
      (c) =>
        bot(c, codex) &&
        c.id !== summary.id &&
        c.body?.includes("<!-- codex-review-state:v1 "),
    )
    .map((report) => ({ report, ...summaryParts(report.body) }));
  for (const prior of previous) {
    const published = time(prior.report.updated_at);
    requireThat(
      time(prior.report.created_at) <= published,
      "Historical Codex summary timestamps contradict",
    );
    for (const [kind, completed] of [
      ["code_review", "codeAt"],
      ["security_review", "securityAt"],
    ] as const) {
      const old = prior.state[kind];
      requireThat(
        time(prior.layout[completed]) >= time(prior.report.created_at) &&
          time(prior.layout[completed]) <= published,
        "Historical Codex completion is outside its report boundary",
      );
      if (
        old.has_unresolved_findings === false &&
        typeof old.has_any_findings === "boolean"
      )
        continue;
      requireThat(
        state[kind].has_unresolved_findings === false &&
          typeof state[kind].has_any_findings === "boolean" &&
          time(layout[completed]) > published,
        `Codex ${kind} report lacks a later explicit disposition`,
      );
    }
  }
  // Observed standalone receipts expose only a 10-hex prefix. Neither their
  // timestamp nor a separate full-head summary binds that receipt to this H.
  requireThat(
    typeof securityState.has_any_findings === "boolean" &&
      securityState.has_unresolved_findings === false,
    "Codex security findings unresolved or unknown for the full head",
  );
  for (const c of comments.filter(
    (c) => c.user?.id === cubic.bot || c.user?.id === codex.bot,
  )) {
    requireThat(c.user.type === "Bot", "Provider account type changed");
    const known =
      c.user.id === codex.bot &&
      bot(c, codex) &&
      ((c.body?.includes("<!-- codex-review-state:v1 ") &&
        Boolean(summaryParts(c.body))) ||
        codexReceipt(c.body) !== null);
    requireThat(
      known,
      `Unknown provider/threadless report ${c.id}; disposition required`,
    );
  }
  // No REST Codex review body has an accepted provider contract yet. An issue
  // summary cannot establish disposition of an unparsed report on this surface.
  const unknownReview = reviews.find((r) => r.user?.id === codex.bot);
  requireThat(
    !unknownReview,
    `Unknown Codex formal-review report ${unknownReview?.id}; verified report/disposition required`,
  );
  const decisions = new Map<number, Record>();
  const disposed = new Set<number>();
  if (
    state.code_review.has_unresolved_findings === false &&
    securityState.has_unresolved_findings === false
  )
    disposed.add(codex.bot);
  const formal = reviews
    .filter(
      (r) =>
        (r.state === "CHANGES_REQUESTED" || r.state === "APPROVED") &&
        !disposed.has(r.user?.id) &&
        !(r.user?.id === cubic.bot && disposedByCubic(r)),
    )
    .sort((a, b) => time(a.submitted_at) - time(b.submitted_at));
  for (const review of formal) {
    requireThat(
      Number.isSafeInteger(review.user?.id) && review.user.id > 0,
      "Formal review author identity unknown",
    );
    decisions.set(review.user.id, review);
  }
  const outstanding = [...decisions.values()].filter(
    (r) => r.state === "CHANGES_REQUESTED",
  );
  requireThat(
    outstanding.length === 0,
    "Unresolved changes-requested review remains; advancing head does not dispose it",
  );
  return {
    cubic: quota ? "quota-unavailable" : "passed",
    ...(quota ? { quota } : {}),
    requests: [cubicRequest, codeRequest, securityRequest].flatMap((c) =>
      c ? [{ id: c.id, createdAt: c.created_at }] : [],
    ),
    results: [...cubicIds, summary.id],
    findings: [...reviews, ...comments]
      .filter((c) => c.user?.id === cubic.bot || c.user?.id === codex.bot)
      .map((c) => c.id),
    checkReports: [...cubicChecks]
      .sort((a, b) => a.id - b.id)
      .map((c) => ({
        id: c.id,
        app: c.app.id,
        name: c.name,
        head: c.head_sha,
        status: c.status,
        conclusion: c.conclusion,
        started: c.started_at,
        completed: c.completed_at,
        output: Object.fromEntries(
          Object.keys(c.output)
            .sort()
            .map((key) => [key, c.output[key]]),
        ),
      })),
    reviewReports: reviews
      .filter((r) => r.user?.id === cubic.bot)
      .sort((a, b) => a.id - b.id)
      .map(reviewPublication),
  };
}
