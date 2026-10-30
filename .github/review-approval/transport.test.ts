import { afterEach, expect, test } from "bun:test";
import { Github } from "./github.ts";
import { uncertainIntents } from "./intent.ts";
import { EvidenceApi } from "./evidence.test.ts";
import { b } from "./fixtures.ts";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const originalFetch = globalThis.fetch;
afterEach(() => {
  globalThis.fetch = originalFetch;
});
for (const filename of [
  "receipt.json",
  "operation.json",
  "review-start.json",
] as const)
  test(`artifact transport authenticates only GitHub and reads a single bounded ${filename} without extraction`, async () => {
    const packed = spawnSync(
      "/usr/bin/python3",
      [
        "-I",
        "-S",
        "-c",
        "import io,sys,zipfile\nb=io.BytesIO()\nwith zipfile.ZipFile(b,'w') as z:z.writestr(sys.argv[1],sys.stdin.buffer.read())\nsys.stdout.buffer.write(b.getvalue())",
        filename,
      ],
      {
        cwd: "/",
        env: {},
        input: JSON.stringify({ version: 1, head: b.head }),
        timeout: 10000,
      },
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
    await expect(
      api.receipt(
        artifact,
        filename === "receipt.json" ? "operation.json" : "receipt.json",
      ),
    ).rejects.toThrow("Cannot read CI receipt archive");
  });
for (const kind of ["module", "package"])
  test(`artifact parsing ignores checkout ${kind}, startup hooks, PATH and credential environment`, () => {
    const directory = mkdtempSync(join(tmpdir(), "receipt-python-"));
    try {
      const marker = join(directory, "module-marker"),
        siteMarker = join(directory, "site-marker"),
        pathMarker = join(directory, "path-marker"),
        canary = "synthetic-credential-canary";
      const poison = `import importlib.machinery,importlib.util,os,pathlib,sys,sysconfig\npathlib.Path(${JSON.stringify(marker)}).write_text(os.environ.get('CLADIRON_PRIVATE_KEY','missing'))\ns=importlib.machinery.PathFinder.find_spec('zipfile',[sysconfig.get_path('stdlib')])\nm=importlib.util.module_from_spec(s)\nsys.modules['zipfile']=m\ns.loader.exec_module(m)\nZipFile=m.ZipFile\n`;
      const modulePath =
        kind === "module"
          ? join(directory, "zipfile.py")
          : join(directory, "zipfile", "__init__.py");
      if (kind === "package") mkdirSync(join(directory, "zipfile"));
      writeFileSync(modulePath, poison);
      writeFileSync(
        join(directory, "sitecustomize.py"),
        `import os,pathlib\npathlib.Path(${JSON.stringify(siteMarker)}).write_text(os.environ.get('CLADIRON_PRIVATE_KEY','missing'))\n`,
      );
      const bin = join(directory, "bin");
      mkdirSync(bin);
      writeFileSync(
        join(bin, "python3"),
        '#!/bin/sh\nprintf fake > "$PYTHON_PATH_MARKER"\nexec /usr/bin/python3 "$@"\n',
        { mode: 0o755 },
      );
      const exposed = {
        PATH: bin,
        PYTHONPATH: directory,
        CLADIRON_PRIVATE_KEY: canary,
        PYTHON_PATH_MARKER: pathMarker,
      };
      const control = spawnSync("/usr/bin/python3", ["-c", "import zipfile"], {
        cwd: directory,
        env: exposed,
        encoding: "utf8",
        timeout: 10000,
      });
      expect(control.status, control.stderr).toBe(0);
      expect(readFileSync(marker, "utf8")).toBe(canary);
      expect(readFileSync(siteMarker, "utf8")).toBe(canary);
      const pathControl = spawnSync("python3", ["-I", "-S", "-c", "pass"], {
        cwd: directory,
        env: exposed,
        encoding: "utf8",
        timeout: 10000,
      });
      expect(pathControl.status, pathControl.stderr).toBe(0);
      expect(readFileSync(pathMarker, "utf8")).toBe("fake");
      for (const path of [marker, siteMarker, pathMarker]) rmSync(path);
      const script = `
        import { mock } from "bun:test";
        const realSpawn = (await import("node:child_process")).spawnSync;
        let probes = 0;
        mock.module("node:child_process", () => ({spawnSync(command,args,options) {
          const probe = realSpawn(command,[...args.slice(0,args.indexOf("-c")+1),"import json,os;print(json.dumps(dict(os.environ)))"],{...options,input:undefined,encoding:"utf8"});
          if (probe.status !== 0) throw new Error("Environment probe failed");
          const environment = JSON.parse(probe.stdout);
          for (const name of ["CLADIRON_PRIVATE_KEY","GITHUB_TOKEN","TEST_WRITE_TOKEN","PYTHONPATH","PYTHONHOME","PYTHONOPTIMIZE","PATH"])
            if (name in environment) throw new Error("Inherited child environment: "+name);
          probes++;
          return realSpawn(command,args,options);
        }}));
        const { Github } = await import(${JSON.stringify(new URL("./github.ts", import.meta.url).href)});
        const api = new Github("synthetic-read-token","synthetic-write-token");
        for (const filename of ["receipt.json","operation.json","review-start.json"]) {
          const packed = realSpawn("/usr/bin/python3",["-I","-S","-c","import io,sys,zipfile\\nb=io.BytesIO()\\nwith zipfile.ZipFile(b,'w') as z:z.writestr(sys.argv[1],b'{\\\"version\\\":1}')\\nsys.stdout.buffer.write(b.getvalue())",filename],{cwd:"/",env:{},timeout:10000});
          if (packed.status !== 0) throw new Error("Packing failed");
          let downloads = 0;
          globalThis.fetch = async () => ++downloads === 1 ? new Response(null,{status:302,headers:{location:"https://fixture.blob.core.windows.net/synthetic"}}) : new Response(packed.stdout);
          const result = await api.receipt({id:900,size_in_bytes:packed.stdout.length},filename);
          if (result.version !== 1 || downloads !== 2) throw new Error("Receipt changed");
          downloads = 0;
          let rejected = false;
          try { await api.receipt({id:900,size_in_bytes:packed.stdout.length},filename === "receipt.json" ? "operation.json" : "receipt.json"); }
          catch (error) { rejected = error.message === "Cannot read CI receipt archive"; }
          if (!rejected) throw new Error("Unexpected member accepted with PYTHONOPTIMIZE");
        }
        console.log(JSON.stringify({parsed:3,probes}));
      `;
      const parsed = spawnSync(process.execPath, ["--eval", script], {
        cwd: directory,
        env: {
          ...exposed,
          PYTHONHOME: join(directory, "invalid-python-home"),
          PYTHONOPTIMIZE: "1",
          GITHUB_TOKEN: canary,
          TEST_WRITE_TOKEN: canary,
        },
        encoding: "utf8",
        timeout: 30000,
      });
      expect(parsed.status, parsed.stderr).toBe(0);
      expect(JSON.parse(parsed.stdout)).toEqual({ parsed: 3, probes: 6 });
      for (const path of [marker, siteMarker, pathMarker])
        expect(existsSync(path)).toBe(false);
    } finally {
      rmSync(directory, { recursive: true, force: true });
    }
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
for (const key of ["jobs", "artifacts", "workflow_runs", "check_runs"])
  for (const count of [
    undefined,
    null,
    "0",
    -1,
    1.5,
    Number.MAX_SAFE_INTEGER + 1,
  ])
    test(`${key} pagination refuses missing or invalid total ${count}`, async () => {
      globalThis.fetch = (async () =>
        Response.json({
          total_count: count,
          [key]: [],
        })) as unknown as typeof fetch;
      await expect(
        new Github("synthetic-read-token").pages(
          "/repos/headironc/nucleus/actions/runs/101/attempts/1/jobs",
          key,
        ),
      ).rejects.toThrow("total_count");
    });
test("keyed pagination accepts an explicit empty collection and a complete stable multi-page collection", async () => {
  const api = new Github("synthetic-read-token");
  globalThis.fetch = (async () =>
    Response.json({ total_count: 0, jobs: [] })) as unknown as typeof fetch;
  expect(
    await api.pages(
      "/repos/headironc/nucleus/actions/runs/101/attempts/1/jobs",
      "jobs",
    ),
  ).toEqual([]);
  globalThis.fetch = (async (input: any) => {
    const first = new URL(String(input)).searchParams.get("page") === "1";
    return Response.json({
      total_count: 101,
      jobs: first
        ? Array.from({ length: 100 }, (_, i) => ({ id: i + 1 }))
        : [{ id: 101 }],
    });
  }) as unknown as typeof fetch;
  expect(
    await api.pages(
      "/repos/headironc/nucleus/actions/runs/101/attempts/1/jobs",
      "jobs",
    ),
  ).toHaveLength(101);
});
for (const problem of [
  "first count missing",
  "second count missing",
  "count drift",
  "short count",
  "duplicate identity",
])
  test(`keyed pagination refuses ${problem}`, async () => {
    globalThis.fetch = (async (input: any) => {
      const first = new URL(String(input)).searchParams.get("page") === "1";
      const total =
        (first && problem === "first count missing") ||
        (!first && problem === "second count missing")
          ? undefined
          : !first && problem === "count drift"
            ? 102
            : 101;
      return Response.json({
        total_count: total,
        jobs: first
          ? Array.from({ length: 100 }, (_, i) => ({ id: i + 1 }))
          : problem === "short count"
            ? []
            : problem === "count drift"
              ? [{ id: 101 }, { id: 102 }]
              : [{ id: problem === "duplicate identity" ? 1 : 101 }],
      });
    }) as unknown as typeof fetch;
    const result = new Github("synthetic-read-token").pages(
      "/repos/headironc/nucleus/actions/runs/101/attempts/1/jobs",
      "jobs",
    );
    if (problem === "count drift")
      await expect(result).rejects.toThrow("changed between pages");
    else await expect(result).rejects.toThrow();
  });
for (const response of [null, [], "unexpected"])
  test(`keyed pagination rejects a non-object response ${JSON.stringify(response)}`, async () => {
    globalThis.fetch = (async () =>
      Response.json(response)) as unknown as typeof fetch;
    await expect(
      new Github("synthetic-read-token").pages(
        "/repos/headironc/nucleus/actions/runs/101/attempts/1/jobs",
        "jobs",
      ),
    ).rejects.toThrow("keyed collection");
  });
test("queued audit cannot treat the real transport's missing jobs total as an unstarted empty run", async () => {
  const run = {
    id: 101,
    path: ".github/workflows/review-approval.yml",
    repository: { id: 1432619931 },
    event: "workflow_dispatch",
    head_branch: "dev",
    run_attempt: 1,
    status: "pending",
    conclusion: null,
  };
  globalThis.fetch = (async (input: any) => {
    const path = new URL(String(input)).pathname;
    if (path.includes("/workflows/"))
      return Response.json({ total_count: 1, workflow_runs: [run] });
    if (path.endsWith("/attempts/1/jobs")) return Response.json({ jobs: [] });
    if (path.endsWith("/runs/101")) return Response.json(run);
    throw new Error("Unexpected audit network request");
  }) as unknown as typeof fetch;
  await expect(
    uncertainIntents(new Github("synthetic-read-token"), b.pr, 100, 1),
  ).rejects.toThrow("total_count");
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
test("a bound outcome distinguishes no approval POST from unresolved approval transport", async () => {
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
function pendingFixture(id = 101, status = "pending", jobs: any[] = []) {
  const api = new EvidenceApi();
  const run: any = {
    id,
    path: ".github/workflows/review-approval.yml",
    repository: { id: 1432619931 },
    event: "workflow_dispatch",
    head_branch: "dev",
    run_attempt: 1,
    status,
    conclusion: null,
  };
  let reads = 0;
  api.pages = async (_path, key) => {
    if (key === "workflow_runs") return [structuredClone(run)];
    if (key === "jobs") return structuredClone(jobs);
    throw new Error("Unexpected historical artifact read");
  };
  api.get = async (path) => {
    expect(path).toBe(`/repos/headironc/nucleus/actions/runs/${id}`);
    reads++;
    return structuredClone(run);
  };
  return { api, run, jobs, reads: () => reads };
}
const queuedJob = (run = 101) => ({
  id: 1,
  run_id: run,
  status: "queued",
  started_at: null,
  completed_at: null,
  conclusion: null,
  steps: [
    {
      name: "Verify evidence and perform selected operation",
      number: 1,
      status: "queued",
      started_at: null,
      completed_at: null,
      conclusion: null,
    },
  ],
});
for (const id of [50, 101])
  for (const status of ["queued", "pending"])
    for (const withJobs of [false, true])
      test(`unstarted first attempt ${id}/${status} does not block the current executor, jobs=${withJobs}`, async () => {
        const f = pendingFixture(id, status, withJobs ? [queuedJob(id)] : []);
        expect(await uncertainIntents(f.api, b.pr, 100, 1)).toEqual([]);
        expect(f.reads()).toBe(2);
      });
for (const change of [
  "started job",
  "started step",
  "completed job",
  "skipped step",
  "missing steps",
  "missing timestamp",
  "wrong job run",
  "rerun",
  "unknown attempt",
  "missing conclusion",
  "wrong workflow",
  "wrong event",
  "wrong ref",
  "wrong repository",
])
  test(`queued exception refuses ${change}`, async () => {
    const f = pendingFixture(101, "pending", [queuedJob()]);
    const job = f.jobs[0];
    if (change === "started job") job.started_at = "2026-10-30T10:00:00Z";
    if (change === "started step")
      job.steps[0].started_at = "2026-10-30T10:00:00Z";
    if (change === "completed job") {
      job.status = "completed";
      job.conclusion = "success";
    }
    if (change === "skipped step") {
      job.steps[0].status = "completed";
      job.steps[0].conclusion = "skipped";
    }
    if (change === "missing steps") delete job.steps;
    if (change === "missing timestamp") delete job.started_at;
    if (change === "wrong job run") job.run_id = 102;
    if (change === "rerun") f.run.run_attempt = 2;
    if (change === "unknown attempt") delete f.run.run_attempt;
    if (change === "missing conclusion") delete f.run.conclusion;
    const get = f.api.get.bind(f.api);
    f.api.get = async (path) => {
      const run = await get(path);
      if (change === "wrong workflow") run.path = ".github/workflows/other.yml";
      if (change === "wrong event") run.event = "push";
      if (change === "wrong ref") run.head_branch = "main";
      if (change === "wrong repository") run.repository.id = 1;
      return run;
    };
    await expect(uncertainIntents(f.api, b.pr, 100, 1)).rejects.toThrow();
  });
for (const failure of [
  "first read",
  "jobs read",
  "last read",
  "attempt drift",
  "state drift",
])
  test(`queued observation ${failure} remains unknown rather than becoming an empty success`, async () => {
    const f = pendingFixture();
    const get = f.api.get.bind(f.api),
      pages = f.api.pages.bind(f.api);
    f.api.get = async (path) => {
      if (failure === "first read") throw new Error("Read failed");
      const run = await get(path);
      if (f.reads() === 2) {
        if (failure === "last read") throw new Error("Read failed");
        if (failure === "attempt drift") run.run_attempt = 2;
        if (failure === "state drift") run.status = "in_progress";
      }
      return run;
    };
    f.api.pages = async (path, key) => {
      if (key === "jobs" && failure === "jobs read")
        throw new Error("Incomplete jobs pages");
      return pages(path, key);
    };
    await expect(uncertainIntents(f.api, b.pr, 100, 1)).rejects.toThrow();
  });
test("a large completed failed run still requires its original intent audit", async () => {
  const f = pendingFixture(101, "completed", [queuedJob()]);
  f.run.conclusion = "failure";
  f.api.pages = async (_path, key) =>
    key === "workflow_runs"
      ? [f.run]
      : key === "artifacts"
        ? []
        : [
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
  await expect(uncertainIntents(f.api, b.pr, 100, 1)).rejects.toThrow(
    "lost its intent",
  );
});
