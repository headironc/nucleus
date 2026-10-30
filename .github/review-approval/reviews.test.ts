import { expect, test } from "bun:test";
import { EvidenceApi } from "./evidence.test.ts";
import {
  b,
  cubicReviewNode,
  fixtureReview,
  providerFixture,
} from "./fixtures.ts";
import {
  bindCubicReview,
  readCubicReviews,
  reviewPublication,
} from "./reviews.ts";

const now = Date.parse("2026-10-30T14:00:00Z");
const at = (clock: string) => `2026-10-30T${clock}Z`;
test("complete unedited REST/GraphQL review shape binds an immutable publication", async () => {
  const api = new EvidenceApi();
  const [review] = await readCubicReviews(api, b.pr, now);
  const p = reviewPublication(review);
  expect(p.at).toBe(at("10:35:04"));
  expect(p.edited).toBeNull();
  expect(p.editor).toBeNull();
  expect(p.author).toEqual({
    id: 191113872,
    node: "bot-191113872",
    type: "Bot",
  });
  expect(p.repository).toBe("headironc/nucleus");
  expect(p.pr).toBe(b.pr);
});
for (const edited of ["10:10:00", "10:40:00"])
  test(`draft or later trusted edit at ${edited} uses the latest public body boundary`, () => {
    const raw = fixtureReview(providerFixture().reviews[0]);
    const node = cubicReviewNode(raw);
    node.createdAt = at("10:00:00");
    node.publishedAt = at("10:36:00");
    node.lastEditedAt = at(edited);
    node.updatedAt = at("10:45:00");
    node.editor = { ...node.author };
    node.includesCreatedEdit = false; // This flag does not mean the body is unedited.
    const bound = bindCubicReview(raw, node, b.pr, now);
    expect(reviewPublication(bound).at).toBe(
      at(edited === "10:10:00" ? "10:36:00" : "10:40:00"),
    );
  });
test("bound body, actors and proof cannot be changed or copied into another record", () => {
  const raw = fixtureReview(providerFixture().reviews[0]);
  raw.performed_via_github_app = { id: 1082092 };
  const node = cubicReviewNode(raw);
  node.lastEditedAt = node.updatedAt = at("10:40:00");
  node.editor = { ...node.author };
  const bound = bindCubicReview(raw, node, b.pr, now),
    proof = reviewPublication(bound);
  expect(() => {
    bound.body = "P1: mutation";
  }).toThrow();
  expect(() => {
    bound.user.id = 1;
  }).toThrow();
  expect(() => {
    bound.performed_via_github_app.id = 1;
  }).toThrow();
  expect(() => {
    (proof.editor as any).id = 1;
  }).toThrow();
  expect(() => {
    (proof.author as any).node = "forged";
  }).toThrow();
  raw.body = "P1: changed API input";
  raw.user.id = 1;
  node.author.id = "changed-node";
  expect(bound.body).not.toBe(raw.body);
  expect(bound.user.id).toBe(191113872);
  expect(proof.author.node).toBe("bot-191113872");
  for (const copy of [
    { ...bound },
    structuredClone(bound),
    Object.freeze({ ...bound, body: "P1: copied proof" }),
  ])
    expect(() => reviewPublication(copy)).toThrow("not verified");
});
for (const [name, change] of Object.entries({
  "node ID": (n: any) => {
    n.id = "wrong-node";
  },
  "numeric ID": (n: any) => {
    n.fullDatabaseId = "999";
  },
  repository: (n: any) => {
    n.repository.nameWithOwner = "other/repository";
  },
  PR: (n: any) => {
    n.pullRequest.number = 2;
  },
  "author kind": (n: any) => {
    n.author.__typename = "User";
  },
  "author number": (n: any) => {
    n.author.databaseId = 1;
  },
  "author node": (n: any) => {
    n.author.id = "another-bot";
  },
  body: (n: any) => {
    n.body = "P1: changed current body";
  },
  state: (n: any) => {
    n.state = "APPROVED";
  },
  commit: (n: any) => {
    n.commit.oid = "f".repeat(40);
  },
  submission: (n: any) => {
    n.submittedAt = at("10:36:00");
  },
  "missing edit time": (n: any) => {
    delete n.lastEditedAt;
  },
  "missing editor": (n: any) => {
    delete n.editor;
  },
  "missing edit flag": (n: any) => {
    delete n.includesCreatedEdit;
  },
  "unedited editor": (n: any) => {
    n.editor = { ...n.author };
  },
  "unedited edit history": (n: any) => {
    n.includesCreatedEdit = true;
  },
  "missing creation": (n: any) => {
    delete n.createdAt;
  },
  "missing publication": (n: any) => {
    delete n.publishedAt;
  },
  "missing update": (n: any) => {
    delete n.updatedAt;
  },
  "invalid creation": (n: any) => {
    n.createdAt = "invalid";
  },
  "future update": (n: any) => {
    n.updatedAt = at("14:00:01");
  },
  "publication before creation": (n: any) => {
    n.publishedAt = at("10:00:00");
  },
  "update before submission": (n: any) => {
    n.updatedAt = at("10:00:00");
  },
  "unknown editor": (n: any) => {
    n.lastEditedAt = n.updatedAt = at("10:40:00");
    n.editor = null;
  },
  "foreign editor kind": (n: any) => {
    n.lastEditedAt = n.updatedAt = at("10:40:00");
    n.editor = { ...n.author, __typename: "User" };
  },
  "foreign editor number": (n: any) => {
    n.lastEditedAt = n.updatedAt = at("10:40:00");
    n.editor = { ...n.author, databaseId: 1 };
  },
  "foreign editor node": (n: any) => {
    n.lastEditedAt = n.updatedAt = at("10:40:00");
    n.editor = { ...n.author, id: "another-bot" };
  },
  "edit before creation": (n: any) => {
    n.lastEditedAt = at("10:00:00");
    n.editor = { ...n.author };
  },
  "edit after update": (n: any) => {
    n.lastEditedAt = at("10:40:00");
    n.editor = { ...n.author };
  },
}))
  test(`independent GraphQL review metadata rejects mismatched ${name}`, async () => {
    const api = new EvidenceApi();
    const node = cubicReviewNode(api.f.reviews[0]);
    change(node);
    api.reviewNodes.set(301, node);
    await expect(readCubicReviews(api, b.pr, now)).rejects.toThrow();
  });
test("explicit conflicting App or raw records cannot obtain trusted publication", () => {
  const raw = fixtureReview(providerFixture().reviews[0]),
    node = cubicReviewNode(raw);
  raw.performed_via_github_app = { id: 1 };
  expect(() => bindCubicReview(raw, node, b.pr, now)).toThrow(
    "metadata mismatch",
  );
  expect(() =>
    reviewPublication({ ...raw, updated_at: at("10:35:04") }),
  ).toThrow("not verified");
});
for (const shape of ["missing", "duplicate", "extra", "partial", "failure"])
  test(`review metadata batch ${shape} fails without a submission-time fallback`, async () => {
    const api = new EvidenceApi();
    api.f.reviews.push({ ...structuredClone(api.f.reviews[0]), id: 302 });
    const node = cubicReviewNode(api.f.reviews[0]);
    api.graphql = async () => {
      if (shape === "failure")
        throw new Error("Synthetic metadata API failure");
      return {
        nodes:
          shape === "missing"
            ? [node]
            : shape === "duplicate"
              ? [node, node]
              : shape === "extra"
                ? [node, node, node]
                : null,
      };
    };
    await expect(readCubicReviews(api, b.pr, now)).rejects.toThrow();
  });
test("the complete REST review collection receives bounded metadata batches", async () => {
  const api = new EvidenceApi(),
    original = api.f.reviews[0];
  api.f.reviews = Array.from({ length: 101 }, (_, i) => ({
    ...structuredClone(original),
    id: 1000 + i,
  }));
  const graphql = api.graphql.bind(api),
    sizes: number[] = [];
  api.graphql = async (query, variables) => {
    sizes.push(variables.ids.length);
    return graphql(query, variables);
  };
  const reviews = await readCubicReviews(api, b.pr, now);
  expect(sizes).toEqual([100, 1]);
  expect(reviews).toHaveLength(101);
  expect(reviews.map((r) => reviewPublication(r).id)).toEqual(
    api.f.reviews.map((r) => r.id),
  );
});
