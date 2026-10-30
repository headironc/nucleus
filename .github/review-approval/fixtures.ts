import { requestBodies, type Binding } from "./identity.ts";
import { readFileSync } from "node:fs";
const bodies: {
  summary: string;
  code: string;
  security: string;
  cubic: string;
} = JSON.parse(
  readFileSync(new URL("./provider-bodies.json", import.meta.url), "utf8"),
);

// Complete provider layouts are deidentified. All PR/commit/comment/review/check
// identities here are synthetic; App/Bot identities are the public provider contract.
export const b: Binding = {
  pr: 1,
  head: "1".repeat(40),
  base: "2".repeat(40),
  merge: "a".repeat(40),
};
export function providerFixture(binding = b) {
  const owner = { id: 19648989, login: "headironc", type: "User" };
  const codex = {
    id: 199175422,
    login: "chatgpt-codex-connector[bot]",
    type: "Bot",
  };
  const at = (value: string) => `2026-10-30T${value}Z`;
  const comments: any[] = [
    ...requestBodies(binding)
      .slice(0, 2)
      .map((body, i) => ({
        id: 100 + i,
        node_id: `request-${100 + i}`,
        lastEditedAt: null,
        user: owner,
        body,
        created_at: at("10:33:50"),
        updated_at: at("10:33:50"),
      })),
    {
      id: 201,
      user: codex,
      performed_via_github_app: { id: 1144995 },
      created_at: at("13:05:00"),
      updated_at: at("13:09:08"),
      body: bodies.summary
        .replaceAll("1".repeat(40), binding.head)
        .replaceAll("`1111111`", `\`${binding.head.slice(0, 7)}\``)
        .replaceAll(
          '"pullRequestNumber":1',
          `"pullRequestNumber":${binding.pr}`,
        )
        .replaceAll("example/repository", "headironc/nucleus"),
    },
    {
      id: 202,
      user: codex,
      performed_via_github_app: { id: 1144995 },
      created_at: at("13:07:00"),
      updated_at: at("13:07:00"),
      body: bodies.code.replaceAll("1111111111", binding.head.slice(0, 10)),
    },
    {
      id: 203,
      user: codex,
      performed_via_github_app: { id: 1144995 },
      created_at: at("13:09:05"),
      updated_at: at("13:09:05"),
      body: bodies.security.replaceAll("1111111111", binding.head.slice(0, 10)),
    },
  ];
  const reviews: any[] = [
    {
      id: 301,
      user: { id: 191113872, login: "cubic-dev-ai[bot]", type: "Bot" },
      state: "COMMENTED",
      commit_id: binding.head,
      submitted_at: at("10:35:04"),
      body: bodies.cubic
        .replaceAll("example/repository", "headironc/nucleus")
        .replaceAll("example%2Frepository", "headironc%2Fnucleus")
        .replaceAll("1".repeat(40), binding.head)
        .replaceAll("/pull/1?", `/pull/${binding.pr}?`)
        .replaceAll("/nucleus/1?", `/nucleus/${binding.pr}?`)
        .replaceAll("/nucleus/1/", `/nucleus/${binding.pr}/`)
        .replaceAll("%2Fpull%2F1", `%2Fpull%2F${binding.pr}`),
    },
  ];
  const checks: any[] = [
    {
      id: 401,
      app: { id: 1082092 },
      name: "cubic · AI code reviewer",
      head_sha: binding.head,
      status: "completed",
      conclusion: "success",
      started_at: at("10:33:55"),
      completed_at: at("10:35:08"),
      output: {
        annotations_count: 0,
        summary:
          "AI review completed with 1 review. No issues found in 41 files.",
        text: null,
      },
    },
  ];
  return { comments, reviews, checks };
}
