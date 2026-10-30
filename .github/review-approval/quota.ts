import {
  cubic,
  ownerId,
  repository,
  requestBodies,
  requireThat,
  sha,
  type Binding,
} from "./identity.ts";
import type { Api } from "./github.ts";
import type { Record } from "./providers.ts";
import {
  cubicReport,
  cubicOutput,
  cubicCompletionSummary,
  cubicQuotaNotice,
  cubicCheckReport,
} from "./provider-text.ts";
import { requestMarker } from "./identity.ts";

export type QuotaSelection = {
  repository: string;
  check: number;
  head: string;
  digest: string;
  pool: "headironc-personal";
  periodEnd: string | null;
  recoveryChecked: true;
  noKnownRecovery: true;
  requestId?: number;
};
const verified = Symbol("verified-quota-selection");
export type QuotaEvidence = {
  readonly [verified]: true;
  statement: number;
  createdAt: string;
  request: number;
  head: string;
  base: string;
  selection: QuotaSelection;
};
export const uneditedOwner = (c: Record) =>
  c.user?.id === ownerId &&
  c.user?.type === "User" &&
  c.created_at === c.updated_at &&
  c.lastEditedAt === null;
export function quotaRequest(b: Binding, selection: QuotaSelection) {
  return `${requestBodies(b)[1]}\n\n<!-- cladiron-cubic-quota:v1 ${JSON.stringify(selection)} -->`;
}
export function quotaDeclaration(
  b: Binding,
  selection: QuotaSelection,
  requestId: number,
) {
  requireThat(
    Number.isSafeInteger(requestId) && requestId > 0,
    "Missing existing Code request identity",
  );
  return `${requestMarker(b)}\n\n<!-- cladiron-cubic-quota:v1 ${JSON.stringify({ ...selection, requestId })} -->`;
}
export const quotaWithdrawal = (b: Binding) =>
  `${requestMarker(b)}\n\n<!-- cladiron-cubic-quota-withdraw:v1 -->`;
function selectionBody(body: unknown, prefix: string): QuotaSelection | null {
  if (
    typeof body !== "string" ||
    !body.startsWith(`${prefix}\n\n<!-- cladiron-cubic-quota:v1 `) ||
    !body.endsWith(" -->")
  )
    return null;
  try {
    const value = JSON.parse(
      body.slice(`${prefix}\n\n<!-- cladiron-cubic-quota:v1 `.length, -4),
    );
    const selection: QuotaSelection = {
      repository: value.repository,
      check: value.check,
      head: value.head,
      digest: value.digest,
      pool: value.pool,
      periodEnd: value.periodEnd,
      recoveryChecked: value.recoveryChecked,
      noKnownRecovery: value.noKnownRecovery,
      ...(Object.hasOwn(value, "requestId")
        ? { requestId: value.requestId }
        : {}),
    };
    if (
      selection.repository !== repository ||
      !Number.isSafeInteger(selection.check) ||
      selection.check <= 0 ||
      !sha(selection.head) ||
      !/^[a-f0-9]{64}$/.test(selection.digest) ||
      selection.pool !== "headironc-personal" ||
      !(
        selection.periodEnd === null ||
        /^\d{4}-\d{2}-\d{2}$/.test(selection.periodEnd)
      ) ||
      selection.recoveryChecked !== true ||
      selection.noKnownRecovery !== true ||
      (selection.requestId !== undefined &&
        (!Number.isSafeInteger(selection.requestId) ||
          selection.requestId <= 0)) ||
      `${prefix}\n\n<!-- cladiron-cubic-quota:v1 ${JSON.stringify(selection)} -->` !==
        body
    )
      return null;
    return selection;
  } catch {
    return null;
  }
}
export const quotaStatement = (body: unknown, b: Binding) =>
  selectionBody(body, requestMarker(b));
export function codeRequest(
  body: unknown,
  b: Binding,
): { selection?: QuotaSelection } | null {
  if (body === requestBodies(b)[1]) return {};
  const selection = selectionBody(body, requestBodies(b)[1]);
  return selection && selection.requestId === undefined ? { selection } : null;
}
export function latestCodeRequest(
  comments: Record[],
  b: Binding,
): Record | undefined {
  const candidates = comments.filter(
    (c) =>
      c.user?.id === ownerId &&
      c.user?.type === "User" &&
      typeof c.body === "string" &&
      c.body.startsWith(requestBodies(b)[1]),
  );
  candidates.forEach((c) =>
    requireThat(
      Number.isFinite(Date.parse(c.created_at)),
      "Missing code request timestamp",
    ),
  );
  candidates.sort(
    (a, b) => Date.parse(b.created_at) - Date.parse(a.created_at),
  );
  requireThat(
    candidates.length < 2 ||
      candidates[0].created_at !== candidates[1].created_at,
    "Ambiguous owner Code request",
  );
  requireThat(
    !candidates.length || uneditedOwner(candidates[0]),
    "Latest Code request was edited; older selection cannot replace it",
  );
  requireThat(
    !candidates.length || codeRequest(candidates[0].body, b) !== null,
    "Latest Code request has an unknown selection; older request cannot replace it",
  );
  return candidates[0];
}

const months = [
  "January",
  "February",
  "March",
  "April",
  "May",
  "June",
  "July",
  "August",
  "September",
  "October",
  "November",
  "December",
];
function source(check: Record, now: number, b: Binding): QuotaSelection {
  requireThat(
    check.app?.id === cubic.app &&
      check.name === "cubic · AI code reviewer" &&
      check.status === "completed" &&
      check.conclusion === "neutral" &&
      sha(check.head_sha) &&
      Number.isSafeInteger(check.id),
    "Quota source is not a completed trusted Cubic refusal",
  );
  requireThat(
    cubicOutput(check.output),
    "Quota source has unparsed output or annotations",
  );
  const texts = [check.output?.summary, check.output?.text].filter(
    (v) => typeof v === "string" && v.trim().length > 0,
  );
  requireThat(
    texts.length === 1,
    "Quota source has unknown additional report surfaces",
  );
  const text = texts[0].replace(/\s+/g, " ").trim();
  const match = cubicQuotaNotice(text);
  requireThat(
    match,
    "Source does not explicitly confirm monthly quota exhaustion",
  );
  requireThat(
    cubicCheckReport(check.output) === "quota",
    "Quota source has an unparsed title or report",
  );
  requireThat(
    [match[1], match[2]].every((value) =>
      /^(?:[0-9]+|[1-9][0-9]{0,2}(?:,[0-9]{3})+)$/.test(value),
    ),
    "Quota counter format unknown",
  );
  const used = Number(match[1].replaceAll(",", "")),
    limit = Number(match[2].replaceAll(",", ""));
  requireThat(
    Number.isSafeInteger(used) &&
      Number.isSafeInteger(limit) &&
      limit > 0 &&
      used >= limit,
    "Quota counters do not establish exhaustion",
  );
  const completed = Date.parse(check.completed_at);
  requireThat(
    Number.isFinite(completed) && completed <= now,
    "Quota completion time unknown",
  );
  let periodEnd: string | null = null;
  if (match[3]) {
    const day = Number(match[3]),
      month = months.indexOf(match[4]),
      year = Number(match[5]);
    requireThat(month >= 0, "Quota period month unknown");
    const reset = Date.UTC(year, month, day);
    const date = new Date(reset);
    requireThat(
      date.getUTCFullYear() === year &&
        date.getUTCMonth() === month &&
        date.getUTCDate() === day &&
        completed < reset &&
        completed >= Date.UTC(year, month - 1, day),
      "Quota source period invalid",
    );
    // A date without a zone cannot authorize entering the possibly-reset day.
    // Stop at the previous UTC midnight; expiry is never evidence of recovery.
    requireThat(
      now < reset - 24 * 60 * 60 * 1000,
      "Quota period may have ended; obtain current evidence, do not infer recovery",
    );
    periodEnd = date.toISOString().slice(0, 10);
  } else
    requireThat(
      check.head_sha === b.head,
      "Cross-PR quota source lacks an explicit period",
    );
  const digest = new Bun.CryptoHasher("sha256")
    .update(
      JSON.stringify({
        id: check.id,
        app: check.app.id,
        name: check.name,
        status: check.status,
        conclusion: check.conclusion,
        head: check.head_sha,
        started: check.started_at,
        completed: check.completed_at,
        output: Object.fromEntries(
          Object.keys(check.output)
            .sort()
            .map((key) => [key, check.output[key]]),
        ),
      }),
    )
    .digest("hex");
  return {
    repository,
    check: check.id,
    head: check.head_sha,
    digest,
    pool: "headironc-personal",
    periodEnd,
    recoveryChecked: true,
    noKnownRecovery: true,
  };
}
async function recoveryChecks(
  api: Api,
  b: Binding,
  sourceHead: string,
  current?: Record[],
): Promise<Record[]> {
  const heads = [...new Set([b.head, sourceHead])];
  const pages = await Promise.all(
    heads.map((head) =>
      head === b.head && current
        ? current
        : api.pages(
            `/repos/${repository}/commits/${head}/check-runs?filter=all`,
            "check_runs",
          ),
    ),
  );
  return pages.flat();
}
function requireNoRecovery(
  b: Binding,
  original: Record,
  checks: Record[],
  reviews: Record[],
  selectedAt: number,
) {
  const beforeSelection = (completed: unknown) =>
    typeof completed === "string" &&
    Date.parse(completed) > Date.parse(original.completed_at) &&
    Date.parse(completed) <= selectedAt;
  const restored = checks.some(
    (c) =>
      c.app?.id === cubic.app &&
      c.name === "cubic · AI code reviewer" &&
      [b.head, original.head_sha].includes(c.head_sha) &&
      c.status === "completed" &&
      c.conclusion === "success" &&
      cubicCompletionSummary(c.output?.summary) &&
      cubicOutput(c.output) &&
      (c.output.text == null || c.output.text === "") &&
      beforeSelection(c.completed_at),
  );
  const restoredReport = reviews.some(
    (r) =>
      r.user?.id === cubic.bot &&
      r.user?.type === "Bot" &&
      (r.performed_via_github_app === undefined ||
        r.performed_via_github_app?.id === cubic.app) &&
      sha(r.commit_id) &&
      r.state === "COMMENTED" &&
      beforeSelection(r.submitted_at) &&
      cubicReport(r.body, { ...b, head: r.commit_id }),
  );
  requireThat(
    !restored && !restoredReport,
    "Completed Cubic review already contradicts quota selection",
  );
}
export async function prepareQuota(
  api: Api,
  b: Binding,
  id: number,
  now = Date.now(),
): Promise<QuotaSelection> {
  requireThat(
    Number.isSafeInteger(id) && id > 0,
    "Invalid quota source check ID",
  );
  const check = await api.get(`/repos/${repository}/check-runs/${id}`);
  requireThat(check.id === id, "Quota source identity differs");
  const selection = source(check, now, b);
  const [checks, reviews] = await Promise.all([
    recoveryChecks(api, b, check.head_sha),
    api.pages(`/repos/${repository}/pulls/${b.pr}/reviews`),
  ]);
  requireNoRecovery(b, check, checks, reviews, now);
  return selection;
}
export async function readQuota(
  api: Api,
  b: Binding,
  comments: Record[],
  checks: Record[],
  now = Date.now(),
  reviews: Record[] = [],
): Promise<QuotaEvidence | undefined> {
  const request = latestCodeRequest(comments, b);
  if (!request) return undefined;
  const candidates = comments.filter(
    (c) =>
      c.user?.id === ownerId &&
      c.user?.type === "User" &&
      ((c.id === request.id && codeRequest(c.body, b)?.selection) ||
        (typeof c.body === "string" &&
          c.body.startsWith(
            `${requestMarker(b)}\n\n<!-- cladiron-cubic-quota:v1 `,
          ))),
  );
  requireThat(
    candidates.every(
      (c) =>
        Number.isFinite(Date.parse(c.created_at)) &&
        Date.parse(c.created_at) <= now,
    ),
    "Quota statement timestamp unavailable or future",
  );
  const statements = candidates.filter(
    (c) =>
      c.id === request.id ||
      Date.parse(c.created_at) >= Date.parse(request.created_at),
  );
  statements.sort(
    (a, b) => Date.parse(b.created_at) - Date.parse(a.created_at),
  );
  const statement = statements[0];
  if (!statement) return undefined;
  requireThat(
    statements.length < 2 ||
      Date.parse(statements[1].created_at) !== Date.parse(statement.created_at),
    "Ambiguous quota routing statement",
  );
  requireThat(uneditedOwner(statement), "Quota routing statement was edited");
  const selection =
    codeRequest(statement.body, b)?.selection ??
    quotaStatement(statement.body, b);
  requireThat(
    selection &&
      (statement.id === request.id || selection.requestId === request.id),
    "Latest quota statement is unknown or references another Code request",
  );
  const selectedAt = Date.parse(statement.created_at);
  requireThat(
    Number.isFinite(selectedAt) && selectedAt <= now,
    "Quota selection timestamp unknown or future",
  );
  requireThat(
    selectedAt >= Date.parse(request.created_at),
    "Quota selection predates its referenced Code request",
  );
  const choices = comments.filter(
    (c) => uneditedOwner(c) && c.body === requestBodies(b)[0],
  );
  const withdrawals = comments.filter(
    (c) => uneditedOwner(c) && c.body === quotaWithdrawal(b),
  );
  requireThat(
    [...choices, ...withdrawals].every(
      (c) =>
        Number.isFinite(Date.parse(c.created_at)) &&
        Date.parse(c.created_at) <= now,
    ),
    "Quota control timestamp unknown or future",
  );
  requireThat(
    !choices.some((c) => Date.parse(c.created_at) === selectedAt),
    "Ambiguous same-time quota routing choice",
  );
  const laterChoice = choices.some(
    (c) => Date.parse(c.created_at) > selectedAt,
  );
  if (laterChoice) return undefined;
  const original = await api.get(
    `/repos/${repository}/check-runs/${selection.check}`,
  );
  const current = source(original, selectedAt, b);
  const { requestId, ...sourceSelection } = selection;
  requireThat(
    JSON.stringify(current) === JSON.stringify(sourceSelection),
    "Quota source/pool/period changed after selection",
  );
  requireThat(
    Date.parse(statement.created_at) > Date.parse(original.completed_at) &&
      Date.parse(statement.created_at) <= now,
    "Quota selection must follow the source refusal",
  );
  requireNoRecovery(
    b,
    original,
    await recoveryChecks(api, b, original.head_sha, checks),
    reviews,
    selectedAt,
  );
  const withdrawn = withdrawals.some(
    (c) => Date.parse(c.created_at) >= selectedAt,
  );
  requireThat(!withdrawn, "Owner withdrew the quota selection");
  return {
    [verified]: true,
    statement: statement.id,
    createdAt: statement.created_at,
    request: request.id,
    head: b.head,
    base: b.base,
    selection,
  };
}
