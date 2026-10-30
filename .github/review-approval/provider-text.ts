import { repository, type Binding } from "./identity.ts";
export const cubicCompletionSummary = (summary: unknown): boolean =>
  typeof summary === "string" &&
  /^AI review completed with [1-9][0-9]* reviews?\. (?:No issues found in \d+ files\.|All reported issues addressed\.)$/.test(
    summary,
  );
export function cubicOutput(value: unknown): boolean {
  if (value === null || typeof value !== "object" || Array.isArray(value))
    return false;
  const output = value as { [key: string]: unknown };
  return (
    Object.keys(output).every((key) =>
      [
        "title",
        "summary",
        "text",
        "annotations_count",
        "annotations_url",
      ].includes(key),
    ) &&
    output.annotations_count === 0 &&
    (output.annotations_url === undefined ||
      output.annotations_url === null ||
      typeof output.annotations_url === "string") &&
    [output.title, output.summary, output.text].every(
      (v) => v === undefined || v === null || typeof v === "string",
    )
  );
}

const quotaAdvice = RegExp.escape(
  " issues after cubic flagged them so far. [Enable flex capacity](https://www.cubic.dev/settings?tab=billing) to cover overages automatically and resume reviews now. [Learn how flex capacity works](https://docs.cubic.dev/account/flex-capacity?utm_source=github&utm_medium=review_line_limit). To help optimise your usage, you can tune cubic to get the most out of your usage limits: - Stop reviewing [test files](https://www.cubic.dev/ai-review?tab=setup&card=ai-review-card-ignore-patterns) - Turn off [incremental reviews](https://www.cubic.dev/ai-review?tab=setup&card=ai-review-card-incremental-reviews) - Use the [cubic CLI](https://docs.cubic.dev/ide/cli-review) locally - Enable [flex pricing](https://www.cubic.dev/settings?tab=billing) [Learn more →](https://docs.cubic.dev/account/ai-review-usage#tips-to-reduce-usage)",
);
const quotaNotice = new RegExp(
  String.raw`^You've reached your plan's monthly review limit\. cubic has reviewed ([0-9,]+) of the ([0-9,]+) allowed lines of code this month\.(?: Reviews resume on ([0-9]{1,2}) ([A-Za-z]+) ([0-9]{4}) \(in [0-9]+ days?\)\.)?(?: Your team has fixed [0-9]+${quotaAdvice})?$`,
);
export const cubicQuotaNotice = (text: unknown) =>
  typeof text === "string"
    ? text.replace(/\s+/g, " ").trim().match(quotaNotice)
    : null;
export function cubicCheckReport(
  value: unknown,
): "review" | "quota" | "merge-only" | null {
  if (!cubicOutput(value)) return null;
  const output = value as { [key: string]: unknown };
  const noTitle = output.title == null || output.title === "";
  const noText = output.text == null || output.text === "";
  if (
    noText &&
    (noTitle || output.title === "AI review completed") &&
    cubicCompletionSummary(output.summary)
  )
    return "review";
  const surfaces = [output.summary, output.text].filter(
    (v) => typeof v === "string" && v.trim().length > 0,
  );
  if (
    (noTitle || output.title === "AI review line limit reached") &&
    surfaces.length === 1 &&
    cubicQuotaNotice(surfaces[0])
  )
    return "quota";
  // The observed merge-only body has no accepted nonempty title contract yet.
  if (
    noTitle &&
    noText &&
    output.summary ===
      "This commit only merges another branch into the PR branch, so cubic completed this check without running an AI review. cubic will review your next non-merge commit."
  )
    return "merge-only";
  return null;
}

// Only generic provider explanation is literal. No original private report URL,
// task, review or delivery identity is stored here. Layout drift fails closed.
const codeAbout = `<details> <summary>ℹ️ About Codex in GitHub</summary>
<br/>

[Your team has set up Codex to review pull requests in this repo](https://chatgpt.com/codex/cloud/settings/general). Reviews are triggered when you
- Open a pull request for review
- Mark a draft as ready
- Comment "@codex review".

If Codex has suggestions, it will comment; otherwise it will react with 👍.




Codex can also answer questions or update the PR. Try commenting "@codex address that feedback".
${" ".repeat(12)}
</details>`;
const securityAbout = `<details> <summary>ℹ️ About Codex security reviews in GitHub</summary>
<br/>

This is an experimental Codex feature. Security reviews are triggered when:
- You comment "@codex security review"
- A regular code review gets triggered (for example, "@codex review" or when a PR is opened), and you’re opted in so security review runs alongside code review

Once complete, Codex will leave suggestions, or a comment if no findings are found.


</details>`;

export function codexReceipt(
  body: unknown,
): { kind: "code" | "security" } | null {
  if (typeof body !== "string") return null;
  const heads = [
    ...body.matchAll(/\*\*Reviewed commit:\*\* `([a-f0-9]{10})`/g),
  ];
  if (heads.length !== 1) return null;
  const head = heads[0][1];
  const code = `<!-- codex-review-receipt:v1 -->\n\nCodex Review: Didn't find any major issues. Keep it up!\n\n**Reviewed commit:** \`${head}\`\n\n${codeAbout}`;
  // Recognize the complete informational layout, without exposing the short
  // prefix as a candidate identity or a no-findings authorization result.
  if (body === code) return { kind: "code" };
  const tasks = [
    ...body.matchAll(
      /https:\/\/chatgpt\.com\/codex\/cloud\/tasks\/(task_e_[a-f0-9]{32})/g,
    ),
  ];
  if (tasks.length !== 1) return null;
  const security = `<!-- codex-review-receipt:v1 -->\n\n### 🛡️ Codex Security Review · _Automatically triggered_\n\nSecurity review completed. No security issues were found in this pull request.\n\n**Reviewed commit:** \`${head}\`\n\n[View security finding report](https://chatgpt.com/codex/cloud/tasks/${tasks[0][1]})\n\n_Only the user who started this review can view the report in Codex._\n\n${securityAbout}`;
  return body === security ? { kind: "security" } : null;
}

export function cubicReport(body: unknown, b: Binding): boolean {
  if (typeof body !== "string") return false;
  const summaries = [
    ...body.matchAll(
      /<!-- cubic:review-summary:start -->\n(\*\*No issues found\*\* across [0-9]+ files|\*\*All reported issues addressed\*\*)\n<!-- cubic:review-summary:end -->/g,
    ),
  ];
  const markers = [
    ...body.matchAll(
      /<!-- cubic:review-post:(ai_pr_review_github_delivery_[a-f0-9]{64}):([a-f0-9]{40}):([a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}) -->/g,
    ),
  ];
  if (
    summaries.length !== 1 ||
    markers.length !== 1 ||
    markers[0][2] !== b.head
  )
    return false;
  const delivery = markers[0][1],
    uuid = markers[0][3];
  const target = encodeURIComponent(
    `https://github.com/${repository}/pull/${b.pr}`,
  );
  const footer = `<sub>[View guided diff](https://www.cubic.dev/pr/${repository}/pull/${b.pr}?utm_source=github&utm_medium=review_footer) | [Turn on auto-fix](https://www.cubic.dev/action/auto-fix/pr/${repository}/${b.pr}?returnTo=${target}) | [Re-trigger cubic](https://www.cubic.dev/action/re-review/pr/${repository}/${b.pr}/${delivery}?returnTo=${target})</sub>`;
  return (
    body ===
    `${summaries[0][0]}\n\n${footer}\n\n<!-- cubic:review-post:${delivery}:${b.head}:${uuid} -->`
  );
}

const summaryAbout = `<details> <summary>ℹ️ About Codex in GitHub</summary>
<br/>

[Your team has set up Codex to review pull requests in this repo](https://chatgpt.com/codex/cloud/settings/general). Reviews are triggered when you
- Open a pull request for review
- Mark a draft as ready
- Comment "@codex review" or "@codex security review".

Codex reacts with 👀 while any review is running, comments if it has suggestions, and reacts with 👍 once all reviews finish with no findings.

</details>`;
export function codexSummary(
  body: unknown,
  state: any,
  security: any,
  allowRunning = false,
): { codeAt: string; securityAt: string } | null {
  if (typeof body !== "string") return null;
  const codes = [
    ...body.matchAll(
      /\| 📝 \*\*Code Review\*\* \| (?:✅ \*\*Completed\*\*|🔄 \*\*Running\*\* since) <relative-time datetime="([0-9T:.Z-]+)">/g,
    ),
  ];
  const securities = [
    ...body.matchAll(
      /\| 🔒 \*\*Security Review\*\* \| (?:✅ \*\*Completed\*\*|🔄 \*\*Running\*\* since) <relative-time datetime="([0-9T:.Z-]+)">/g,
    ),
  ];
  if (codes.length !== 1 || securities.length !== 1) return null;
  const codeAt = codes[0][1],
    securityAt = securities[0][1];
  const label = (status: unknown) =>
    status === "completed"
      ? "✅ **Completed**"
      : allowRunning && status === "running"
        ? "🔄 **Running** since"
        : null;
  const codeLabel = label(state.code_review.status),
    securityLabel = label(state.security_review.status);
  if (!codeLabel || !securityLabel) return null;
  const expected = `<!-- codex-pull-request-review-summary -->\n<!-- codex-review-state:v1 ${JSON.stringify(state)} -->\n<!-- codex-security-review:v1 ${JSON.stringify(security)} -->\n## Codex Review Summary\n\nThis comment shows the latest Codex review activity on this pull request.\n\n| Review | Status | Commit | Review trigger |\n| --- | --- | --- | --- |\n| 📝 **Code Review** | ${codeLabel} <relative-time datetime="${codeAt}">${codeAt}</relative-time> | \`${state.code_review.commit_sha.slice(0, 7)}\` | Manual request |\n| 🔒 **Security Review** | ${securityLabel} <relative-time datetime="${securityAt}">${securityAt}</relative-time> | \`${state.security_review.commit_sha.slice(0, 7)}\` | Manual request |\n\n\n\n${summaryAbout}`;
  return body === expected ? { codeAt, securityAt } : null;
}
