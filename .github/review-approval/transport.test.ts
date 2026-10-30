import { afterEach, expect, test } from "bun:test";
import { Github } from "./github.ts";
import { uncertainIntents } from "./intent.ts";
import { EvidenceApi } from "./evidence.test.ts";
import { b } from "./fixtures.ts";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";

const originalFetch = globalThis.fetch;
afterEach(() => {
  globalThis.fetch = originalFetch;
});
for (const filename of ["operation.json", "review-start.json"] as const)
  test(`artifact transport authenticates only GitHub and reads a single bounded ${filename} without extraction`, async () => {
    const packed = spawnSync(
      "python3",
      [
        "-c",
        "import io,sys,zipfile\nb=io.BytesIO()\nwith zipfile.ZipFile(b,'w') as z:z.writestr(sys.argv[1],sys.stdin.buffer.read())\nsys.stdout.buffer.write(b.getvalue())",
        filename,
      ],
      { input: JSON.stringify({ version: 1, head: b.head }), timeout: 10000 },
    );
    expect(packed.status).toBe(0);
    const bytes = packed.stdout;
    const calls: any[] = [];
    globalThis.fetch = (async (input: any, init: any) => {
      calls.push({ url: String(input), init });
      return calls.length === 1
        ? new Response(null, {
            status: 302,
            headers: {
              location: "https://fixture.blob.core.windows.net/synthetic",
            },
          })
        : new Response(bytes);
    }) as unknown as typeof fetch;
    const artifact = {
      id: 900,
      size_in_bytes: bytes.length,
      digest: `sha256:${createHash("sha256").update(bytes).digest("hex")}`,
    };
    const api = new Github("synthetic-read-token");
    expect(await api.receipt(artifact, filename)).toEqual({
      version: 1,
      head: b.head,
    });
    expect(calls[0].init.headers.Authorization).toBe(
      "Bearer synthetic-read-token",
    );
    expect(calls[1].init.headers).toBeUndefined();
    calls.length = 0;
    await expect(api.receipt(artifact)).rejects.toThrow(
      "Cannot read CI receipt archive",
    );
  });
test("artifact transport refuses an unrelated redirect origin before sending any credential", async () => {
  let count = 0;
  globalThis.fetch = (async () => {
    count++;
    return new Response(null, {
      status: 302,
      headers: { location: "https://example.invalid/report" },
    });
  }) as unknown as typeof fetch;
  await expect(
    new Github("synthetic-read-token").receipt({ id: 900, size_in_bytes: 200 }),
  ).rejects.toThrow("storage origin");
  expect(count).toBe(1);
});
test("REST consumes later pages and cannot turn an unread page into success", async () => {
  const api = new Github("synthetic-read-token");
  const seen: string[] = [];
  globalThis.fetch = (async (input: any) => {
    const url = String(input);
    seen.push(url);
    const page = Number(new URL(url).searchParams.get("page"));
    return Response.json(
      page === 1
        ? Array.from({ length: 100 }, (_, i) => ({ id: i + 1 }))
        : page === 2
          ? [{ id: 101 }]
          : [],
    );
  }) as typeof fetch;
  expect(
    await api.pages("/repos/headironc/nucleus/pulls/27/reviews"),
  ).toHaveLength(101);
  expect(seen).toHaveLength(2);
  globalThis.fetch = (async (input: any) =>
    new URL(String(input)).searchParams.get("page") === "1"
      ? Response.json(Array.from({ length: 100 }, (_, i) => ({ id: i + 1 })))
      : new Response("unavailable", { status: 503 })) as typeof fetch;
  await expect(
    api.pages("/repos/headironc/nucleus/pulls/27/reviews"),
  ).rejects.toThrow("503");
});
test("partial keyed pagination and GraphQL errors are failures", async () => {
  const api = new Github("synthetic-read-token");
  globalThis.fetch = (async () =>
    Response.json({
      total_count: 2,
      check_runs: [{ id: 1 }],
    })) as unknown as typeof fetch;
  await expect(
    api.pages(
      "/repos/headironc/nucleus/commits/fixture/check-runs",
      "check_runs",
    ),
  ).rejects.toThrow("Truncated");
  globalThis.fetch = (async () =>
    Response.json({
      data: { repository: {} },
      errors: [{ message: "Synthetic partial result" }],
    })) as unknown as typeof fetch;
  await expect(api.graphql("query{}", {})).rejects.toThrow("partial");
});
test("read credential cannot accidentally perform a write", async () => {
  const api = new Github("synthetic-read-token");
  let calls = 0;
  globalThis.fetch = (async () => {
    calls++;
    throw new Error("No network expected");
  }) as unknown as typeof fetch;
  await expect(
    api.post("/repos/headironc/nucleus/pulls/27/reviews", {}),
  ).rejects.toThrow("No scoped token");
  await expect(api.get("/repos/other/repo/pulls/1")).rejects.toThrow(
    "outside repository",
  );
  expect(calls).toBe(0);
});
test("write is sent once to exact repository using scoped write token", async () => {
  const api = new Github("synthetic-read-token", "synthetic-write-token");
  const seen: any[] = [];
  globalThis.fetch = (async (input: any, init: any) => {
    seen.push({ url: String(input), ...init });
    throw new Error("Synthetic unknown transport");
  }) as unknown as typeof fetch;
  await expect(
    api.post("/repos/headironc/nucleus/pulls/27/reviews", {
      event: "APPROVE",
      commit_id: b.head,
    }),
  ).rejects.toThrow();
  expect(seen).toHaveLength(1);
  expect(seen[0].headers.Authorization).toBe("Bearer synthetic-write-token");
  expect(JSON.parse(seen[0].body).commit_id).toBe(b.head);
});
test("failed earlier run retains an intent, including an earlier attempt of current run", async () => {
  const api = new EvidenceApi();
  const intent = {
    version: 1 as const,
    repositoryId: 1432619931,
    run: 99,
    attempt: 1,
    b,
    digest: "d".repeat(64),
  };
  api.pages = async (path: string, key?: string) =>
    key === "jobs"
      ? [
          {
            steps: [
              {
                name: "Verify evidence and perform selected operation",
                status: "completed",
                conclusion: "failure",
              },
            ],
          },
        ]
      : path.includes("/workflows/")
        ? [
            {
              id: 99,
              path: ".github/workflows/review-approval.yml",
              repository: { id: 1432619931 },
              status: "in_progress",
              run_attempt: 2,
            },
          ]
        : [
            {
              id: 1,
              name: "cladiron-approval-intent-99-1",
              expired: false,
              workflow_run: { id: 99, repository_id: 1432619931 },
            },
          ];
  api.receipt = async () => intent;
  expect(await uncertainIntents(api, b.pr, 99, 2)).toEqual([intent]);
});
test("lost intent after writer started remains unknown and blocks a new dispatch", async () => {
  const api = new EvidenceApi();
  api.pages = async (path: string, key?: string) =>
    key === "workflow_runs"
      ? [
          {
            id: 98,
            path: ".github/workflows/review-approval.yml",
            repository: { id: 1432619931 },
            status: "completed",
            conclusion: "cancelled",
            run_attempt: 1,
          },
        ]
      : key === "artifacts"
        ? []
        : [
            {
              steps: [
                {
                  name: "Verify evidence and perform selected operation",
                  status: "completed",
                  conclusion: "cancelled",
                },
              ],
            },
          ];
  await expect(uncertainIntents(api, b.pr, 99, 1)).rejects.toThrow(
    "lost its intent",
  );
});
test("a bound outcome records known no-write failure separately from unresolved transport", async () => {
  const api = new EvidenceApi();
  const intent = {
    version: 1 as const,
    repositoryId: 1432619931,
    run: 98,
    attempt: 1,
    b,
    digest: "d".repeat(64),
  };
  api.pages = async (_path: string, key?: string) =>
    key === "workflow_runs"
      ? [
          {
            id: 98,
            path: ".github/workflows/review-approval.yml",
            repository: { id: 1432619931 },
            status: "completed",
            conclusion: "failure",
            run_attempt: 1,
          },
        ]
      : key === "jobs"
        ? [
            {
              steps: [
                {
                  name: "Verify evidence and perform selected operation",
                  status: "completed",
                  conclusion: "failure",
                },
              ],
            },
          ]
        : [
            {
              id: 1,
              name: "cladiron-approval-intent-98-1",
              expired: false,
              workflow_run: { id: 98, repository_id: 1432619931 },
            },
            {
              id: 2,
              name: "cladiron-approval-result-98-1",
              expired: false,
              workflow_run: { id: 98, repository_id: 1432619931 },
            },
          ];
  let attempted = false;
  api.receipt = async (artifact) =>
    artifact.id === 1
      ? intent
      : { ...intent, outcome: { attempted, resolved: false, reviews: [] } };
  expect(await uncertainIntents(api, b.pr, 99, 1)).toEqual([]);
  attempted = true;
  expect(await uncertainIntents(api, b.pr, 99, 1)).toEqual([intent]);
});
test("older safe attempt cannot mask a later writer with missing intent", async () => {
  const api = new EvidenceApi();
  const visited: string[] = [];
  const intent = {
    version: 1 as const,
    repositoryId: 1432619931,
    run: 98,
    attempt: 1,
    b,
    digest: "d".repeat(64),
  };
  api.pages = async (path, key) => {
    visited.push(path);
    if (key === "workflow_runs")
      return [
        {
          id: 98,
          path: ".github/workflows/review-approval.yml",
          repository: { id: 1432619931 },
          status: "completed",
          conclusion: "failure",
          run_attempt: 2,
        },
      ];
    if (key === "jobs")
      return [
        {
          steps: [
            {
              name: "Verify evidence and perform selected operation",
              status: "completed",
              conclusion: "failure",
            },
          ],
        },
      ];
    return [1, 2].map((id) => ({
      id,
      name: `cladiron-approval-${id === 1 ? "intent" : "result"}-98-1`,
      expired: false,
      workflow_run: { id: 98, repository_id: 1432619931 },
    }));
  };
  api.receipt = async (a) =>
    a.id === 1
      ? intent
      : {
          ...intent,
          outcome: { attempted: false, resolved: false, reviews: [] },
        };
  await expect(uncertainIntents(api, b.pr, 99, 1)).rejects.toThrow(
    "98/2 lost its intent",
  );
  expect(visited.some((p) => p.includes("attempts/1/jobs"))).toBe(true);
  expect(visited.some((p) => p.includes("attempts/2/jobs"))).toBe(true);
});
test("failed read-only prepare does not create an unknown approval operation", async () => {
  const api = new EvidenceApi();
  api.pages = async (_path, key) =>
    key === "workflow_runs"
      ? [
          {
            id: 98,
            path: ".github/workflows/review-approval.yml",
            repository: { id: 1432619931 },
            status: "completed",
            conclusion: "failure",
            run_attempt: 1,
          },
        ]
      : key === "artifacts"
        ? []
        : [
            {
              steps: [
                {
                  name: "Prepare review request text",
                  status: "completed",
                  conclusion: "failure",
                },
                {
                  name: "Verify evidence and perform selected operation",
                  status: "completed",
                  conclusion: "skipped",
                },
              ],
            },
          ];
  expect(await uncertainIntents(api, b.pr, 99, 1)).toEqual([]);
});
