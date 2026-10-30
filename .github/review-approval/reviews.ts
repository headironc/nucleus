import { cubic, numericId, repository, requireThat, sha } from "./identity.ts";
import type { Api } from "./github.ts";
import type { Record } from "./providers.ts";

type Publication = Readonly<{
  repository: typeof repository;
  pr: number;
  id: number;
  node: string;
  head: string;
  author: Readonly<{ id: number; node: string; type: "Bot" }>;
  editor: Readonly<{ id: number; node: string; type: "Bot" }> | null;
  created: string;
  submitted: string;
  published: string;
  edited: string | null;
  updated: string;
  includesCreatedEdit: boolean;
  at: string;
}>;
const publications = new WeakMap<object, Publication>();
function time(value: unknown, now: number) {
  requireThat(
    typeof value === "string" &&
      Number.isFinite(Date.parse(value)) &&
      Date.parse(value) <= now,
    "Cubic review timestamp missing, invalid or future",
  );
  return Date.parse(value);
}
export function bindCubicReview(
  review: Record,
  node: Record,
  pr: number,
  now = Date.now(),
): Record {
  requireThat(
    Number.isSafeInteger(pr) &&
      pr > 0 &&
      review.user?.id === cubic.bot &&
      review.user.type === "Bot" &&
      (review.performed_via_github_app === undefined ||
        review.performed_via_github_app?.id === cubic.app) &&
      typeof review.user.node_id === "string" &&
      review.user.node_id.length > 0 &&
      typeof review.node_id === "string" &&
      review.node_id.length > 0 &&
      node?.id === review.node_id &&
      numericId(node.fullDatabaseId) === review.id &&
      node.repository?.nameWithOwner === repository &&
      node.pullRequest?.number === pr &&
      node.author?.__typename === "Bot" &&
      node.author.databaseId === cubic.bot &&
      node.author.id === review.user.node_id &&
      node.body === review.body &&
      typeof node.body === "string" &&
      node.state === review.state &&
      typeof node.state === "string" &&
      sha(review.commit_id) &&
      node.commit?.oid === review.commit_id &&
      node.submittedAt === review.submitted_at,
    "Cubic review identity/body/state/commit metadata mismatch",
  );
  requireThat(
    Object.hasOwn(node, "lastEditedAt") &&
      Object.hasOwn(node, "editor") &&
      typeof node.includesCreatedEdit === "boolean",
    "Cubic review edit metadata missing",
  );
  const created = time(node.createdAt, now),
    submitted = time(node.submittedAt, now),
    published = time(node.publishedAt, now),
    updated = time(node.updatedAt, now);
  requireThat(
    created <= submitted &&
      created <= published &&
      submitted <= updated &&
      published <= updated,
    "Cubic review publication timestamps contradict",
  );
  let editor: Publication["editor"] = null;
  if (node.lastEditedAt === null) {
    requireThat(
      node.editor === null && node.includesCreatedEdit === false,
      "Cubic review unedited metadata contradicts",
    );
  } else {
    const edited = time(node.lastEditedAt, now);
    requireThat(
      created <= edited &&
        edited <= updated &&
        node.editor?.__typename === "Bot" &&
        node.editor.databaseId === cubic.bot &&
        node.editor.id === node.author.id,
      "Cubic review edit time or editor is untrusted",
    );
    editor = Object.freeze({
      id: node.editor.databaseId,
      node: node.editor.id,
      type: "Bot",
    });
  }
  const boundaries = [
    node.submittedAt,
    node.publishedAt,
    ...(node.lastEditedAt === null ? [] : [node.lastEditedAt]),
  ];
  const at = boundaries.reduce((a, b) =>
    Date.parse(b) > Date.parse(a) ? b : a,
  );
  const publication: Publication = Object.freeze({
    repository,
    pr,
    id: review.id,
    node: node.id,
    head: review.commit_id,
    author: Object.freeze({
      id: node.author.databaseId,
      node: node.author.id,
      type: "Bot",
    }),
    editor,
    created: node.createdAt,
    submitted: node.submittedAt,
    published: node.publishedAt,
    edited: node.lastEditedAt,
    updated: node.updatedAt,
    includesCreatedEdit: node.includesCreatedEdit,
    at,
  });
  const bound = Object.freeze({
    ...review,
    user: Object.freeze({ ...review.user }),
    ...(review.performed_via_github_app
      ? {
          performed_via_github_app: Object.freeze({
            ...review.performed_via_github_app,
          }),
        }
      : {}),
  });
  publications.set(bound, publication);
  return bound;
}
export function reviewPublication(review: Record): Publication {
  const publication = publications.get(review);
  requireThat(publication, "Cubic review publication was not verified");
  return publication;
}
export async function readCubicReviews(
  api: Api,
  pr: number,
  now = Date.now(),
): Promise<Record[]> {
  const reviews = await api.pages(`/repos/${repository}/pulls/${pr}/reviews`);
  // Keep every review for existing decisions; only Cubic adds this body-publication contract.
  const targets = reviews.filter(
    (r) =>
      r.user?.id === cubic.bot || r.performed_via_github_app?.id === cubic.app,
  );
  const bound = new Map<number, Record>();
  requireThat(
    targets.every((r) => typeof r.node_id === "string") &&
      new Set(targets.map((r) => r.node_id)).size === targets.length,
    "Cubic review node identities missing or duplicated",
  );
  for (let offset = 0; offset < targets.length; offset += 100) {
    const batch = targets.slice(offset, offset + 100);
    const result = await api.graphql(
      `query($ids:[ID!]!){nodes(ids:$ids){... on PullRequestReview{id fullDatabaseId body state submittedAt createdAt publishedAt updatedAt lastEditedAt includesCreatedEdit commit{oid} author{__typename ... on Bot{id databaseId}} editor{__typename ... on Bot{id databaseId}} repository{nameWithOwner} pullRequest{number}}}}`,
      { ids: batch.map((r) => r.node_id) },
    );
    requireThat(
      Array.isArray(result.nodes) && result.nodes.length === batch.length,
      "Incomplete Cubic review edit metadata",
    );
    for (const review of batch) {
      const matches = result.nodes.filter(
        (node: Record) =>
          node?.id === review.node_id &&
          numericId(node.fullDatabaseId) === review.id,
      );
      requireThat(
        matches.length === 1,
        "Missing or duplicate Cubic review edit metadata",
      );
      bound.set(review.id, bindCubicReview(review, matches[0], pr, now));
    }
  }
  return reviews.map((r) => bound.get(r.id) ?? r);
}
