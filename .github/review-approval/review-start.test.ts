import { expect, test } from "bun:test";
import { EvidenceApi } from "./evidence.test.ts";
import { b, providerFixture } from "./fixtures.ts";
import { requestBodies } from "./identity.ts";
import { snapshot } from "./evidence.ts";
import { verifyProviders, type Record as Data } from "./providers.ts";
import {
  captureReviewStart,
  readReviewStarts,
  startSelection,
  type StartProof,
} from "./review-start.ts";

const at = (time: string) => `2026-10-30T${time}Z`;
const env = (run = 700) => ({
  GITHUB_EVENT_NAME: "workflow_dispatch",
  GITHUB_REF: "refs/heads/dev",
  GITHUB_REPOSITORY: "headironc/nucleus",
  GITHUB_REPOSITORY_ID: "1432619931",
  GITHUB_WORKFLOW_REF:
    "headironc/nucleus/.github/workflows/review-approval.yml@refs/heads/dev",
  GITHUB_WORKFLOW_SHA: b.base,
  GITHUB_ACTOR_ID: "19648989",
  CLADIRON_TRIGGERING_ACTOR_ID: "19648989",
  GITHUB_RUN_ID: String(run),
  GITHUB_RUN_ATTEMPT: "1",
});
function comments(
  code: "running" | "completed",
  security: "running" | "completed",
  codeAt: string,
  securityAt: string,
) {
  const result = providerFixture().comments;
  const summary = result.find((c) => c.id === 201);
  const state = JSON.parse(
    summary.body.match(/<!-- codex-review-state:v1 (\{[^\n]*\}) -->/)![1],
  );
  const gate = JSON.parse(
    summary.body.match(/<!-- codex-security-review:v1 (\{[^\n]*\}) -->/)![1],
  );
  for (const [kind, status] of [
    ["code_review", code],
    ["security_review", security],
  ]) {
    state[kind].status = status;
    if (status === "running")
      state[kind].has_any_findings = state[kind].has_unresolved_findings = null;
  }
  gate.status = security;
  summary.node_id = "summary-201";
  summary.created_at = at("09:00:00"); // Persistent container predates this request.
  summary.updated_at = at(
    code === "running" || security === "running" ? "13:09:30" : "14:02:00",
  );
  const row = (label: string, status: string, time: string) =>
    `| ${label} | ${status === "running" ? "🔄 **Running** since" : "✅ **Completed**"} <relative-time datetime="${time}">${time}</relative-time> | \`1111111\` | Manual request |`;
  summary.body = summary.body
    .replace(
      /<!-- codex-review-state:v1 \{[^\n]*\} -->/,
      `<!-- codex-review-state:v1 ${JSON.stringify(state)} -->`,
    )
    .replace(
      /<!-- codex-security-review:v1 \{[^\n]*\} -->/,
      `<!-- codex-security-review:v1 ${JSON.stringify(gate)} -->`,
    )
    .replace(
      /^\| 📝 \*\*Code Review\*\*.*$/m,
      row("📝 **Code Review**", code, codeAt),
    )
    .replace(
      /^\| 🔒 \*\*Security Review\*\*.*$/m,
      row("🔒 **Security Review**", security, securityAt),
    );
  result.find((c) => c.id === 203).created_at = result.find(
    (c) => c.id === 203,
  ).updated_at = at("14:00:59");
  return result;
}
class StartApi extends EvidenceApi {
  artifacts = new Map<number, Data>();
  producers = new Map<number, Data>();
  captureJobs = new Map<number, Data[]>();
  proofs = new Map<number, StartProof>();
  downloads = 0;
  writes = 0;
  install(id: number, proof: StartProof) {
    const run = proof.producer.run;
    this.artifacts.set(id, {
      id,
      name: `cladiron-review-start-${run}-1`,
      expired: false,
      digest: `sha256:${"d".repeat(64)}`,
      workflow_run: {
        id: run,
        repository_id: 1432619931,
        head_repository_id: 1432619931,
        head_sha: b.base,
      },
    });
    this.producers.set(run, {
      id: run,
      run_attempt: 1,
      head_sha: b.base,
      head_branch: "dev",
      path: ".github/workflows/review-approval.yml",
      event: "workflow_dispatch",
      actor: { id: 19648989 },
      triggering_actor: { id: 19648989 },
      repository: { id: 1432619931 },
      head_repository: { id: 1432619931 },
      status: "completed",
      conclusion: "success",
      check_suite_id: 4000,
    });
    this.captureJobs.set(run, [
      {
        id: 3000,
        name: "approval",
        run_id: run,
        status: "completed",
        conclusion: "success",
        check_run_url:
          "https://api.github.com/repos/headironc/nucleus/check-runs/5000",
        steps: [
          "Prepare review request text",
          "Upload review start evidence",
        ].map((name) => ({ name, status: "completed", conclusion: "success" })),
      },
    ]);
    this.proofs.set(id, structuredClone(proof));
  }
  override async get(path: string): Promise<any> {
    const artifact = path.match(/\/actions\/artifacts\/(\d+)$/);
    if (artifact) return this.artifacts.get(Number(artifact[1]));
    const run = path.match(/\/actions\/runs\/(\d+)$/);
    if (run && this.producers.has(Number(run[1])))
      return this.producers.get(Number(run[1]));
    if (path.endsWith("/check-runs/5000"))
      return {
        id: 5000,
        app: { id: 15368 },
        check_suite: { id: 4000 },
        conclusion: "success",
      };
    return super.get(path);
  }
  override async pages(path: string, key?: string): Promise<Data[]> {
    const run = path.match(
      /\/actions\/runs\/(\d+)\/(?:attempts\/\d+\/jobs|artifacts)$/,
    );
    if (run && this.producers.has(Number(run[1]))) {
      if (key === "artifacts")
        return [...this.artifacts.values()].filter(
          (a) => a.workflow_run.id === Number(run[1]),
        );
      return this.captureJobs.get(Number(run[1]))!;
    }
    return super.pages(path, key);
  }
  override async receipt(artifact: Data, filename?: string) {
    if (filename === "review-start.json") {
      this.downloads++;
      return structuredClone(this.proofs.get(artifact.id));
    }
    return super.receipt(artifact);
  }
  override async post(): Promise<never> {
    this.writes++;
    throw new Error("No PR writes allowed");
  }
  override async put(): Promise<never> {
    this.writes++;
    throw new Error("No PR writes allowed");
  }
}
async function fixture() {
  const api = new StartApi();
  const running = comments(
    "running",
    "running",
    at("13:07:40"),
    at("13:09:08"),
  );
  const proof = await captureReviewStart(
    b,
    running,
    env(),
    async () => ({ b, comments: structuredClone(running) }),
    Date.parse(at("13:10:00")),
  );
  expect(proof).toBeDefined();
  api.install(501, proof!);
  api.f.comments = comments(
    "completed",
    "completed",
    at("14:00:00"),
    at("14:01:00"),
  );
  return {
    api,
    running,
    proof: proof!,
    now: Date.parse(at("14:30:00")),
    selection: { code: 501, security: 501 },
  };
}
test("persistent summary requires trusted starts and completes both lanes after observation", async () => {
  const f = await fixture();
  await expect(snapshot(f.api, b.pr, f.now)).rejects.toThrow(
    "unique completion summary",
  );
  const result = await snapshot(f.api, b.pr, f.now, f.selection);
  expect(result.evidence.providers.results).toContain(201);
  expect(result.evidence.reviewStarts?.code?.artifact.id).toBe(501);
  expect(f.api.downloads).toBe(1); // Same selected ID is one immutable response.
  expect(f.api.writes).toBe(0);
});
test("code and security can be captured at different times without borrowing the earlier security start", async () => {
  const api = new StartApi();
  const code = comments("running", "completed", at("13:07:40"), at("13:09:08"));
  const first = await captureReviewStart(
    b,
    code,
    env(),
    async () => ({ b, comments: code }),
    Date.parse(at("13:10:00")),
  );
  api.install(501, first!);
  const request = {
    id: 150,
    node_id: "security-150",
    user: { id: 19648989, type: "User" },
    created_at: at("13:12:00"),
    updated_at: at("13:12:00"),
    lastEditedAt: null,
    body: requestBodies(b)[2],
  };
  const security = comments(
    "completed",
    "running",
    at("13:11:00"),
    at("13:13:00"),
  );
  security.push(request);
  security.find((c) => c.id === 201).updated_at = at("13:13:30");
  const second = await captureReviewStart(
    b,
    security,
    env(701),
    async () => ({ b, comments: security }),
    Date.parse(at("13:14:00")),
  );
  api.install(502, second!);
  api.f.comments = comments(
    "completed",
    "completed",
    at("14:00:00"),
    at("14:01:00"),
  );
  api.f.comments.push(request);
  await expect(
    snapshot(api, b.pr, Date.parse(at("14:30:00")), {
      code: 501,
      security: 501,
    }),
  ).rejects.toThrow("request changed");
  expect(
    (
      await snapshot(api, b.pr, Date.parse(at("14:30:00")), {
        code: 501,
        security: 502,
      })
    ).evidence.providers.results,
  ).toContain(201);
  expect(api.writes).toBe(0);
});
test("prepare captures only running facts, rereads the candidate/requests, and never accesses App secrets", async () => {
  const f = await fixture();
  const metadata = env();
  Object.defineProperty(metadata, "CLADIRON_PRIVATE_KEY", {
    get() {
      throw new Error("Secret accessed");
    },
  });
  let rereads = 0;
  expect(
    await captureReviewStart(
      b,
      f.running,
      metadata,
      async () => {
        rereads++;
        return { b, comments: f.running };
      },
      Date.parse(at("13:10:00")),
    ),
  ).toBeDefined();
  expect(rereads).toBe(1);
  expect(
    await captureReviewStart(b, f.api.f.comments, metadata, async () => {
      throw new Error("No completed-state capture");
    }),
  ).toBeUndefined();
  await expect(
    captureReviewStart(
      b,
      f.running,
      metadata,
      async () => ({ b: { ...b, base: "f".repeat(40) }, comments: f.running }),
      Date.parse(at("13:10:00")),
    ),
  ).rejects.toThrow("Candidate/request changed");
});
for (const [name, change] of Object.entries({
  "expired artifact": (f: Awaited<ReturnType<typeof fixture>>) => {
    f.api.artifacts.get(501)!.expired = true;
  },
  "wrong artifact run": (f: Awaited<ReturnType<typeof fixture>>) => {
    f.api.artifacts.get(501)!.workflow_run.id = 701;
  },
  "wrong producer workflow": (f: Awaited<ReturnType<typeof fixture>>) => {
    f.api.producers.get(700)!.path = ".github/workflows/other.yml";
  },
  "wrong producer event": (f: Awaited<ReturnType<typeof fixture>>) => {
    f.api.producers.get(700)!.event = "pull_request";
  },
  "wrong producer ref": (f: Awaited<ReturnType<typeof fixture>>) => {
    f.api.producers.get(700)!.head_branch = "main";
  },
  "wrong repository": (f: Awaited<ReturnType<typeof fixture>>) => {
    f.api.artifacts.get(501)!.workflow_run.repository_id = 1;
  },
  "invalid digest": (f: Awaited<ReturnType<typeof fixture>>) => {
    f.api.artifacts.get(501)!.digest = "unknown";
  },
  "duplicate artifact": (f: Awaited<ReturnType<typeof fixture>>) => {
    f.api.artifacts.set(502, { ...f.api.artifacts.get(501), id: 502 });
  },
  "wrong producer actor": (f: Awaited<ReturnType<typeof fixture>>) => {
    f.api.producers.get(700)!.actor.id = 1;
  },
  "wrong triggering actor": (f: Awaited<ReturnType<typeof fixture>>) => {
    f.api.producers.get(700)!.triggering_actor.id = 1;
  },
  "producer wrong base": (f: Awaited<ReturnType<typeof fixture>>) => {
    f.api.producers.get(700)!.head_sha = "f".repeat(40);
  },
  "old attempt": (f: Awaited<ReturnType<typeof fixture>>) => {
    f.api.producers.get(700)!.run_attempt = 2;
  },
  "cancelled producer": (f: Awaited<ReturnType<typeof fixture>>) => {
    f.api.producers.get(700)!.conclusion = "cancelled";
  },
  "missing upload": (f: Awaited<ReturnType<typeof fixture>>) => {
    f.api.captureJobs.get(700)![0].steps[1].conclusion = "skipped";
  },
  "failed capture": (f: Awaited<ReturnType<typeof fixture>>) => {
    f.api.captureJobs.get(700)![0].steps[0].conclusion = "failure";
  },
  "forged head": (f: Awaited<ReturnType<typeof fixture>>) => {
    f.api.proofs.get(501)!.head = "f".repeat(40);
  },
  "summary replacement": (f: Awaited<ReturnType<typeof fixture>>) => {
    f.api.f.comments.find((c) => c.id === 201).id = 299;
  },
  "changed summary node": (f: Awaited<ReturnType<typeof fixture>>) => {
    f.api.f.comments.find((c) => c.id === 201).node_id = "another-node";
  },
  "changed request ID": (f: Awaited<ReturnType<typeof fixture>>) => {
    f.api.proofs.get(501)!.requests.code.id = 299;
  },
  "edited request": (f: Awaited<ReturnType<typeof fixture>>) => {
    f.api.proofs.get(501)!.requests.security.lastEditedAt = at("13:09:00");
  },
  "wrong provider App": (f: Awaited<ReturnType<typeof fixture>>) => {
    f.api.proofs.get(501)!.summary.performed_via_github_app.id = 1;
  },
  "wrong final provider App": (f: Awaited<ReturnType<typeof fixture>>) => {
    f.api.f.comments.find((c) => c.id === 201).performed_via_github_app.id = 1;
  },
  "wrong provider bot": (f: Awaited<ReturnType<typeof fixture>>) => {
    f.api.proofs.get(501)!.summary.user.id = 1;
  },
  "missing lane": (f: Awaited<ReturnType<typeof fixture>>) => {
    delete f.api.proofs.get(501)!.starts.security;
  },
  "observation before response": (f: Awaited<ReturnType<typeof fixture>>) => {
    f.api.proofs.get(501)!.capturedAt = at("13:09:00");
  },
  "future observation": (f: Awaited<ReturnType<typeof fixture>>) => {
    f.api.proofs.get(501)!.capturedAt = at("14:31:00");
  },
  "bad start": (f: Awaited<ReturnType<typeof fixture>>) => {
    f.api.proofs.get(501)!.summary.body = f.api.proofs
      .get(501)!
      .summary.body.replaceAll(at("13:07:40"), "invalid");
  },
  "same-time start": (f: Awaited<ReturnType<typeof fixture>>) => {
    f.api.proofs.get(501)!.summary.body = f.api.proofs
      .get(501)!
      .summary.body.replaceAll(at("13:07:40"), at("10:33:50"));
  },
  "completed before capture": (f: Awaited<ReturnType<typeof fixture>>) => {
    f.api.f.comments = comments(
      "completed",
      "completed",
      at("13:09:40"),
      at("13:09:50"),
    );
  },
  "null without receipt": (f: Awaited<ReturnType<typeof fixture>>) => {
    f.api.f.comments = f.api.f.comments.filter((c) => c.id !== 203);
  },
  "receipt predates capture": (f: Awaited<ReturnType<typeof fixture>>) => {
    f.api.f.comments.find((c) => c.id === 203).created_at = at("13:09:59");
  },
  "new finding": (f: Awaited<ReturnType<typeof fixture>>) => {
    const c = f.api.f.comments.find((c) => c.id === 201);
    c.body = c.body.replaceAll(
      '"has_unresolved_findings":false',
      '"has_unresolved_findings":true',
    );
  },
  "unknown final body": (f: Awaited<ReturnType<typeof fixture>>) => {
    f.api.f.comments.find((c) => c.id === 201).body += "\nP1: undisposed";
  },
  "unknown formal report": (f: Awaited<ReturnType<typeof fixture>>) => {
    f.api.f.reviews.push({
      id: 999,
      user: { id: 199175422, type: "Bot" },
      body: "Unknown formal report",
    });
  },
  "unresolved thread": (f: Awaited<ReturnType<typeof fixture>>) => {
    f.api.afterPage = true;
    f.api.unresolved = true;
  },
}))
  test(`start evidence refuses ${name} without PR writes`, async () => {
    const f = await fixture();
    change(f);
    await expect(snapshot(f.api, b.pr, f.now, f.selection)).rejects.toThrow();
    expect(f.api.writes).toBe(0);
  });
test("explicit proof selection changes the digest; local claimed trust never replaces the artifact", async () => {
  const f = await fixture();
  const first = await snapshot(f.api, b.pr, f.now, f.selection);
  const proof = await captureReviewStart(
    b,
    f.running,
    env(701),
    async () => ({ b, comments: f.running }),
    Date.parse(at("13:11:00")),
  );
  f.api.install(502, proof!);
  const second = await snapshot(f.api, b.pr, f.now, {
    code: 502,
    security: 502,
  });
  expect(second.digest).not.toBe(first.digest);
  expect(second.evidence.reviewStarts?.code?.artifact.id).toBe(502);
  expect(() =>
    verifyProviders(
      b,
      f.api.f.comments,
      f.api.f.reviews,
      f.api.f.checks,
      undefined,
      {
        code: { proof: f.proof, start: at("13:07:40") },
        security: { proof: f.proof, start: at("13:09:08") },
      } as any,
    ),
  ).toThrow("not verified");
  await expect(
    readReviewStarts(f.api, b, f.api.f.comments, { code: 999 }, f.now),
  ).rejects.toThrow();
  expect(f.api.writes).toBe(0);
});
test("artifact input IDs reject invalid supplied values instead of defaulting", () => {
  expect(
    startSelection({
      code_start_artifact: "501",
      security_start_artifact: "501",
    }),
  ).toEqual({ code: 501, security: 501 });
  for (const value of [" ", "0", "-1", "1,2", "9007199254740993", null])
    expect(() => startSelection({ code_start_artifact: value })).toThrow();
});
