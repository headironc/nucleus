import { expect, test } from "bun:test";
import { verifyProviders } from "./providers.ts";
import { b, providerFixture } from "./fixtures.ts";
import { requestBodies } from "./identity.ts";

test("observed completed shapes plus synthetic bound requests accept null security only with explicit receipt", () => {
  const f = providerFixture();
  expect(verifyProviders(b, f.comments, f.reviews, f.checks).results).toEqual([
    401, 301, 201, 203,
  ]);
});
for (const [label, change] of Object.entries({
  "forged owner login": (f: any) => {
    f.comments[0].user = { ...f.comments[0].user, id: 1 };
  },
  "edited owner request": (f: any) => {
    f.comments[0].updated_at = "2026-10-30T10:33:51Z";
  },
  "old baseline request": (f: any) => {
    f.comments[0].body = f.comments[0].body.replace(b.base, "f".repeat(40));
  },
  "same-second owner edit": (f: any) => {
    f.comments[0].lastEditedAt = f.comments[0].created_at;
  },
  "unmarked historical request": (f: any) => {
    f.comments[0].body = "@cubic-dev-ai review this PR";
  },
  "provider starts before request": (f: any) => {
    f.checks[0].started_at = "2026-10-30T10:33:49Z";
  },
  "forged provider App": (f: any) => {
    f.checks[0].app.id = 1;
  },
  "Cubic merge-only green": (f: any) => {
    f.checks[0].output.summary =
      "This commit only merges another branch into the PR branch, so cubic completed this check without running an AI review. cubic will review your next non-merge commit.";
  },
  "Cubic quota neutral": (f: any) => {
    f.checks[0].conclusion = "neutral";
    f.checks[0].output.summary =
      "You've reached your plan's monthly review limit.";
  },
  "Cubic wrong full head": (f: any) => {
    f.reviews[0].commit_id = "f".repeat(40);
  },
  "Cubic missing delivery association": (f: any) => {
    f.reviews[0].body = "**No issues found** across 41 files";
  },
  "Codex null without receipt": (f: any) => {
    f.comments.pop();
  },
  "forged Codex receipt App": (f: any) => {
    f.comments.at(-1).performed_via_github_app.id = 1;
  },
  "Codex receipt wrong abbreviated head": (f: any) => {
    f.comments.at(-1).body = f.comments
      .at(-1)
      .body.replace(b.head.slice(0, 10), "0000000000");
  },
  "unknown security flags": (f: any) => {
    f.comments[2].body = f.comments[2].body.replace(
      '"has_unresolved_findings":null',
      '"has_unresolved_findings":true',
    );
  },
  "P0 threshold cannot hide unresolved findings": (f: any) => {
    f.comments[2].body = f.comments[2].body.replace(
      '"has_unresolved_findings":false',
      '"has_unresolved_findings":true',
    );
  },
  "unknown threadless blocker": (f: any) => {
    f.comments.push({
      ...f.comments[3],
      id: 999,
      body: "P1: Authentication can be bypassed.",
    });
  },
  "historical Cubic finding not disposed by new no-findings": (f: any) => {
    f.reviews.push({
      ...f.reviews[0],
      id: 999,
      commit_id: "f".repeat(40),
      submitted_at: "2026-10-30T10:30:00Z",
      body: "**1 issue found**",
    });
  },
}))
  test(label, () => {
    const f = providerFixture();
    change(f);
    expect(() => verifyProviders(b, f.comments, f.reviews, f.checks)).toThrow();
  });
test("Codex summary rejects additional machine findings and contradictory completion time", () => {
  const f = providerFixture();
  f.comments[2].body = f.comments[2].body.replace(
    '"code_review":{',
    '"code_review":{"new_findings":["P1"],',
  );
  expect(() => verifyProviders(b, f.comments, f.reviews, f.checks)).toThrow(
    "state fields",
  );
  const early = providerFixture();
  early.comments[2].body = early.comments[2].body.replaceAll(
    "2026-10-30T13:07:40.000001Z",
    "2026-10-30T10:00:00.000001Z",
  );
  expect(() =>
    verifyProviders(b, early.comments, early.reviews, early.checks),
  ).toThrow("completion timestamps");
});
test("an older-head formal change request remains until that reviewer supersedes it", () => {
  const f = providerFixture();
  const author = { id: 9001, type: "User" };
  f.reviews.push({
    id: 801,
    user: author,
    state: "CHANGES_REQUESTED",
    commit_id: "f".repeat(40),
    submitted_at: "2026-10-30T10:10:00Z",
    body: "P1: unresolved finding",
  });
  expect(() => verifyProviders(b, f.comments, f.reviews, f.checks)).toThrow(
    "advancing head",
  );
  f.reviews.push({
    id: 802,
    user: author,
    state: "APPROVED",
    commit_id: b.head,
    submitted_at: "2026-10-30T13:10:00Z",
    body: "Finding fixed and verified",
  });
  expect(verifyProviders(b, f.comments, f.reviews, f.checks).cubic).toBe(
    "passed",
  );
});
test("explicit finding dispositions preserve historical findings", () => {
  const f = providerFixture();
  f.reviews.push({
    ...f.reviews[0],
    id: 999,
    submitted_at: "2026-10-30T10:30:00Z",
    body: "**1 issue found**",
  });
  f.reviews[0].body = f.reviews[0].body.replace(
    "**No issues found** across 41 files",
    "**All reported issues addressed**",
  );
  f.checks[0].output.summary =
    "AI review completed with 1 review. All reported issues addressed.";
  f.comments[2].body = f.comments[2].body.replaceAll(
    '"has_any_findings":false',
    '"has_any_findings":true',
  );
  expect(
    verifyProviders(b, f.comments, f.reviews, f.checks).findings,
  ).toContain(999);
});
for (const state of ["COMMENTED", "CHANGES_REQUESTED"])
  test(`Cubic addressed cannot dispose a later old-head ${state} report`, () => {
    const f = providerFixture();
    f.reviews[0].body = f.reviews[0].body.replace(
      "**No issues found** across 41 files",
      "**All reported issues addressed**",
    );
    f.checks[0].output.summary =
      "AI review completed with 1 review. All reported issues addressed.";
    f.reviews.push({
      ...f.reviews[0],
      id: 902,
      state,
      commit_id: "f".repeat(40),
      submitted_at: "2026-10-30T13:10:00Z",
      body: "P1: late unresolved finding",
    });
    expect(() => verifyProviders(b, f.comments, f.reviews, f.checks)).toThrow(
      "later explicit disposition",
    );
  });
test("a later formal Cubic change request cannot borrow an earlier disposition", () => {
  const f = providerFixture();
  f.reviews[0].body = f.reviews[0].body.replace(
    "**No issues found** across 41 files",
    "**All reported issues addressed**",
  );
  f.checks[0].output.summary =
    "AI review completed with 1 review. All reported issues addressed.";
  f.reviews.push({
    ...providerFixture({ ...b, head: "f".repeat(40) }).reviews[0],
    id: 903,
    state: "CHANGES_REQUESTED",
    submitted_at: "2026-10-30T13:10:00Z",
  });
  expect(() => verifyProviders(b, f.comments, f.reviews, f.checks)).toThrow(
    "later explicit disposition",
  );
});
for (const timestamp of ["2026-10-30T10:35:04Z", "invalid"])
  test(`Cubic disposition refuses ambiguous report time ${timestamp}`, () => {
    const f = providerFixture();
    f.reviews[0].body = f.reviews[0].body.replace(
      "**No issues found** across 41 files",
      "**All reported issues addressed**",
    );
    f.checks[0].output.summary =
      "AI review completed with 1 review. All reported issues addressed.";
    f.reviews.push({
      ...f.reviews[0],
      id: 904,
      commit_id: "f".repeat(40),
      submitted_at: timestamp,
      body: "P1: unresolved finding",
    });
    expect(() => verifyProviders(b, f.comments, f.reviews, f.checks)).toThrow();
  });
for (const submitted of ["10:30:00", "13:10:00"])
  test(`unknown Codex formal-review body at ${submitted} is not disposed by a clean summary`, () => {
    const f = providerFixture();
    f.reviews.push({
      id: 901,
      user: { id: 199175422, type: "Bot" },
      performed_via_github_app: { id: 1144995 },
      state: "COMMENTED",
      commit_id: b.head,
      submitted_at: `2026-10-30T${submitted}Z`,
      body: "P1: unresolved threadless finding",
    });
    expect(() => verifyProviders(b, f.comments, f.reviews, f.checks)).toThrow(
      "Unknown Codex formal-review report 901",
    );
  });
test("explicit security request cannot borrow an already-started summary", () => {
  const f = providerFixture();
  f.comments.push({
    id: 200,
    user: { id: 19648989, type: "User" },
    body: requestBodies(b)[2],
    lastEditedAt: null,
    created_at: "2026-10-30T13:06:00Z",
    updated_at: "2026-10-30T13:06:00Z",
  });
  expect(() => verifyProviders(b, f.comments, f.reviews, f.checks)).toThrow(
    "start/completion",
  );
});
test("explicit mismatched Cubic App, extra report or inconsistent scope is rejected", () => {
  const forged = providerFixture();
  forged.reviews[0].performed_via_github_app = { id: 1 };
  expect(() =>
    verifyProviders(b, forged.comments, forged.reviews, forged.checks),
  ).toThrow();
  const report = providerFixture();
  report.checks[0].output.text = "P1: unresolved threadless finding";
  expect(() =>
    verifyProviders(b, report.comments, report.reviews, report.checks),
  ).toThrow("unparsed report");
  const scope = providerFixture();
  scope.reviews[0].body = scope.reviews[0].body.replace(
    "across 41 files",
    "across 12 files",
  );
  expect(() =>
    verifyProviders(b, scope.comments, scope.reviews, scope.checks),
  ).toThrow("scope");
});
for (const kind of ["code", "security", "cubic", "summary"])
  test(`complete ${kind} no-findings receipt refuses appended or embedded finding`, () => {
    for (const embedded of [false, true]) {
      const f = providerFixture();
      const target =
        kind === "cubic"
          ? f.reviews[0]
          : f.comments[kind === "summary" ? 2 : kind === "code" ? 3 : 4];
      target.body = embedded
        ? target.body.replace(
            kind === "cubic" ? "</sub>" : "</details>",
            `P1: unresolved finding\n${kind === "cubic" ? "</sub>" : "</details>"}`,
          )
        : `${target.body}\nP1: unresolved finding`;
      expect(() =>
        verifyProviders(b, f.comments, f.reviews, f.checks),
      ).toThrow();
    }
  });
