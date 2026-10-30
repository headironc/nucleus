import { afterEach, expect, test } from "bun:test";
import { data, Evaluator, Lexer, Parser } from "@actions/expressions";
import { spawnSync } from "node:child_process";
import {
  copyFileSync,
  chmodSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../", import.meta.url));
type Step = {
  name?: string;
  run?: string;
  id?: string;
  env?: Record<string, string>;
  uses?: string;
  with?: Record<string, unknown>;
  if?: string;
  "continue-on-error"?: boolean;
};
const workflow = Bun.YAML.parse(
  readFileSync(join(root, ".github/workflows/docs.yml"), "utf8"),
) as {
  on: Record<string, unknown>;
  permissions: Record<string, string>;
  defaults: { run: { shell: string } };
  concurrency: { group: string; "cancel-in-progress": boolean };
  jobs: Record<
    string,
    {
      name: string;
      needs?: string | string[];
      "runs-on": string;
      container?: { image: string; credentials: unknown };
      "timeout-minutes": number;
      if?: string;
      permissions?: unknown;
      "continue-on-error"?: boolean;
      steps: Step[];
    }
  >;
};
const job = workflow.jobs.docs!;
const action = Bun.YAML.parse(
  readFileSync(join(root, ".github/actions/docs-check/action.yml"), "utf8"),
) as { runs: { using: string; steps: Step[] } };
const [preview, documents, links] = action.runs.steps
  .slice(-3)
  .map((step) => step.run!);
const directories: string[] = [];

afterEach(() => {
  for (const directory of directories.splice(0))
    rmSync(directory, { recursive: true, force: true });
});

function run(command: string, cwd = root, env = process.env) {
  return spawnSync(
    "bash",
    ["--noprofile", "--norc", "-eo", "pipefail", "-c", command],
    {
      cwd,
      env,
      encoding: "utf8",
      timeout: 30_000,
    },
  );
}

function fixture() {
  const directory = mkdtempSync(join(tmpdir(), "nucleus docs "));
  directories.push(directory);
  for (const path of ["tools", "docs", "design", "halo"])
    mkdirSync(join(directory, path));
  for (const path of [
    "package.json",
    ".tool-versions",
    ".prettierrc.json",
    ".markdownlint.jsonc",
    "tools/check-bun.ts",
  ])
    copyFileSync(join(root, path), join(directory, path));
  symlinkSync(
    join(root, "node_modules"),
    join(directory, "node_modules"),
    "dir",
  );
  for (const path of [
    "README.md",
    "AGENTS.md",
    "halo/README.md",
    "docs/example.md",
    "design/example.md",
  ])
    writeFileSync(join(directory, path), "# Fixture\n");
  return directory;
}

function identityEnv(): NodeJS.ProcessEnv {
  const directory = mkdtempSync(join(tmpdir(), "nucleus docs runner "));
  directories.push(directory);
  return {
    RUNNER_TEMP: directory,
    GITHUB_OUTPUT: join(directory, "output"),
    GITHUB_PATH: join(directory, "path"),
    GITHUB_REPOSITORY: "headironc/nucleus",
    GITHUB_REPOSITORY_ID: "1432619931",
    GITHUB_EVENT_NAME: "pull_request",
    PR_NUMBER: "55",
    BASE_SHA: "b".repeat(40),
    HEAD_SHA: "c".repeat(40),
    GITHUB_SHA: "a".repeat(40),
    GITHUB_WORKFLOW_SHA: "d".repeat(40),
    GITHUB_RUN_ID: "123",
    GITHUB_RUN_ATTEMPT: "1",
    EXECUTOR: "hosted",
  };
}

function expression(source: string, values: unknown, cancelled = false) {
  const context = JSON.parse(
    JSON.stringify(values),
    data.reviver,
  ) as data.Dictionary;
  const functions = [
    {
      name: "cancelled",
      minArgs: 0,
      maxArgs: 0,
      call: () => new data.BooleanData(cancelled),
    },
    {
      name: "always",
      minArgs: 0,
      maxArgs: 0,
      call: () => new data.BooleanData(true),
    },
  ];
  return new Evaluator(
    new Parser(
      new Lexer(source.replace(/^\$\{\{|\}\}$/g, "")).lex().tokens,
      ["github", "needs"],
      functions,
    ).parse(),
    context,
    new Map(functions.map((fn) => [fn.name, fn])),
  )
    .evaluate()
    .coerceString();
}

const routeStep = workflow.jobs.route!.steps[0]!;
const source = {
  github: {
    event: {
      pull_request: {
        number: 55,
        base: {
          ref: "dev",
          repo: { id: 1432619931, full_name: "headironc/nucleus" },
        },
        head: { repo: { id: 1432619931, fork: false } },
        user: { id: 19648989 },
      },
    },
  },
};

function routeEnv(values = source): NodeJS.ProcessEnv {
  return Object.fromEntries(
    Object.entries(routeStep.env!).map(([key, value]) => [
      key,
      value.replace(/\$\{\{(.*?)\}\}/g, (_, expr: string) =>
        expression(expr, values),
      ),
    ]),
  );
}

test("published preparation routes every source to hosted without checkout", () => {
  expect(workflow.jobs.route!.steps.every((step) => !step.uses)).toBe(true);
  for (const draft of [true, false]) {
    const values = structuredClone(source);
    Object.assign(values.github.event.pull_request, { draft });
    const env = { ...process.env, ...identityEnv(), ...routeEnv(values) };
    const result = run(routeStep.run!, root, env);
    expect(result.status, result.stderr).toBe(0);
    expect(readFileSync(env.GITHUB_OUTPUT!, "utf8")).toBe(
      `executor=hosted\ncandidate=${env.GITHUB_SHA}\n`,
    );
  }
});

test("staged source eligibility is narrow and missing source fields fall back to hosted", () => {
  const accepted = routeEnv();
  for (const [extra, selected] of [
    [{}, "vm"],
    [{ CANARY_PR: "56" }, "hosted"],
    [{ AUTHOR_ID: "123" }, "hosted"],
    [{ AUTHOR_ID: "" }, "hosted"],
    [{ HEAD_REPOSITORY_ID: "999" }, "hosted"],
    [{ HEAD_FORK: "true" }, "hosted"],
    [{ HEAD_FORK: "" }, "hosted"],
    [{ HEAD_REPOSITORY_ID: "" }, "hosted"],
    [{ VM_ENABLED: "false" }, "hosted"],
  ] as const) {
    const env: NodeJS.ProcessEnv = {
      ...process.env,
      ...identityEnv(),
      ...accepted,
      VM_ENABLED: "true",
      CANARY_PR: "55",
      ...extra,
    };
    const result = run(routeStep.run!, root, env);
    expect(result.status, result.stderr).toBe(0);
    expect(readFileSync(env.GITHUB_OUTPUT!, "utf8")).toContain(
      `executor=${selected}\n`,
    );
    for (const executor of ["hosted", "vm"]) {
      expect(
        expression(workflow.jobs[executor]!.if!, {
          needs: { route: { outputs: { executor: selected } } },
        }),
      ).toBe(String(executor === selected));
    }
  }
});

test("contradictory repository, event, candidate and route inputs fail before outputs", () => {
  for (const extra of [
    { GITHUB_EVENT_NAME: "push" },
    { GITHUB_REPOSITORY_ID: "1" },
    { GITHUB_REPOSITORY: "other/nucleus" },
    { BASE_REPOSITORY_ID: "1" },
    { BASE_REPOSITORY: "other/nucleus" },
    { BASE_REF: "other" },
    { PR_NUMBER: "" },
    { PR_NUMBER: "55; exit 0" },
    { GITHUB_SHA: "" },
    { VM_ENABLED: "maybe" },
  ]) {
    const env = { ...process.env, ...identityEnv(), ...routeEnv(), ...extra };
    expect(
      run(routeStep.run!, root, env).status,
      JSON.stringify(extra),
    ).not.toBe(0);
  }
});

test("required aggregate accepts only the selected success and rejects cancellation and identity drift", () => {
  const command = job.steps[0]!.run!;
  for (const cancelled of [false, true]) {
    expect(expression(job.if!, {}, cancelled)).toBe("true");
    expect(expression(job.steps[0]!.if!, {}, cancelled)).toBe(
      String(!cancelled),
    );
    expect(expression(job.steps[1]!.if!, {}, cancelled)).toBe(
      String(cancelled),
    );
  }
  expect(run(job.steps[1]!.run!).status).not.toBe(0);
  const states = ["success", "skipped", "failure", "cancelled", "", "unknown"];
  for (const executor of ["hosted", "vm", "", "invalid"])
    for (const hosted of states)
      for (const vm of states) {
        const expected =
          (executor === "hosted" && hosted === "success" && vm === "skipped") ||
          (executor === "vm" && hosted === "skipped" && vm === "success");
        const values = {
          needs: {
            route: {
              result: "success",
              outputs: { executor, candidate: "a".repeat(40) },
            },
            hosted: { result: hosted, outputs: { candidate: "a".repeat(40) } },
            vm: { result: vm, outputs: { candidate: "a".repeat(40) } },
          },
        };
        const env: NodeJS.ProcessEnv = {
          ...process.env,
          GITHUB_SHA: "a".repeat(40),
          ...Object.fromEntries(
            Object.entries(job.steps[0]!.env!).map(([key, value]) => [
              key,
              value.replace(/\$\{\{(.*?)\}\}/g, (_, expr: string) =>
                expression(expr, values),
              ),
            ]),
          ),
        };
        expect(run(command, root, env).status === 0).toBe(expected);
        if (expected)
          for (const extra of [
            { CANCELLED: "true" },
            { ROUTE_RESULT: "failure" },
            { ROUTE_RESULT: "skipped" },
            { ROUTE_CANDIDATE: "b".repeat(40) },
            executor === "hosted"
              ? { HOSTED_CANDIDATE: "" }
              : { VM_CANDIDATE: "" },
          ])
            expect(run(command, root, { ...env, ...extra }).status).not.toBe(0);
      }
});

test("every dev/main PR event, including retargets and non-document changes, requires one complete executor", () => {
  expect(workflow.on).toEqual({
    pull_request: {
      branches: ["dev", "main"],
      types: ["opened", "reopened", "synchronize", "edited"],
    },
  });
  expect(Object.keys(workflow.jobs)).toEqual(["route", "hosted", "vm", "docs"]);
  expect(job.name).toBe("Nucleus Docs");
  expect(job.if).toBe("always()");
  expect(job.needs).toEqual(["route", "hosted", "vm"]);
  expect(job["continue-on-error"]).toBeUndefined();
  expect(job["timeout-minutes"]).toBeGreaterThan(0);
  for (const step of job.steps) {
    expect(step["continue-on-error"]).toBeUndefined();
  }
  // The three commands own four obligations: format and Markdown share one entrypoint.
  for (const [index, command] of [
    "bun test tools/preview-design.test.ts",
    "bun run check:docs",
    "bun run check:docs:links",
  ].entries())
    expect([preview, documents, links][index]).toContain(command);
  expect(
    action.runs.steps.find((step) => step.name === "Install dependencies")?.run,
  ).toContain("bun install --frozen-lockfile --ignore-scripts");
  expect(action.runs.using).toBe("composite");
  for (const step of action.runs.steps) {
    expect(step.if).toBeUndefined();
    expect(step["continue-on-error"]).toBeUndefined();
    expect(step.run).toContain("set -euo pipefail");
  }
  for (const name of ["hosted", "vm"]) {
    const executor = workflow.jobs[name]!;
    expect(executor["timeout-minutes"]).toBe(15);
    expect(executor.needs).toBe("route");
    expect(
      executor.steps
        .filter((step) => step.uses === "./.github/actions/docs-check")
        .map((step) => step.with),
    ).toEqual([{ executor: name }]);
  }
  expect(workflow.defaults.run.shell).toBe("bash");
});

test("candidate code has only read access and uses the event checkout with pinned actions", () => {
  expect(workflow.permissions).toEqual({ contents: "read" });
  expect(job.permissions).toBeUndefined();
  for (const executor of [workflow.jobs.hosted!, workflow.jobs.vm!]) {
    const checkout = executor.steps[0]!;
    expect(checkout.uses).toMatch(/^actions\/checkout@[0-9a-f]{40}$/);
    expect(checkout.with).toEqual({ "persist-credentials": false });
  }
  expect(workflow.jobs.hosted!.permissions).toBeUndefined();
  expect(workflow.jobs.vm!.permissions).toEqual({
    contents: "read",
    packages: "read",
  });
  expect(workflow.jobs.vm!["runs-on"]).toBe("nucleus-runners");
  expect(workflow.jobs.vm!.container?.image).toBe(
    "ghcr.io/headironc/nucleus-ci@sha256:80ae416d4bc7287532c0974d25390c5bc9b27cd62defe4e64860b501414b4592",
  );
  const upload = workflow.jobs.vm!.steps.find((step) =>
    step.uses?.startsWith("actions/upload-artifact@"),
  )!;
  expect(upload.if).toBe("always()");
  expect(upload.with).toEqual({
    name: "docs-vm-${{ github.run_id }}-${{ github.run_attempt }}",
    path: "${{ runner.temp }}/docs-*.log",
    "retention-days": 7,
    "if-no-files-found": "error",
  });
  for (const executor of Object.values(workflow.jobs)) {
    expect(executor["continue-on-error"]).toBeUndefined();
    expect(
      executor.steps.every((step) => step["continue-on-error"] === undefined),
    ).toBe(true);
  }
  for (const step of Object.values(workflow.jobs)
    .flatMap((job) => job.steps)
    .filter((step) => step.uses && !step.uses.startsWith("./")))
    expect(step.uses).toMatch(/^[\w-]+\/[\w-]+@[0-9a-f]{40}$/);
  expect(
    workflow.jobs.hosted!.steps.find((step) =>
      step.uses?.startsWith("oven-sh/setup-bun@"),
    )?.with,
  ).toEqual({
    "bun-version-file": ".tool-versions",
  });
  expect(
    workflow.jobs.hosted!.steps.find((step) =>
      step.uses?.startsWith("actions/setup-node@"),
    )?.with,
  ).toEqual({
    "node-version-file": ".tool-versions",
    "package-manager-cache": false,
  });
});

test("obsolete runs cancel within one PR while different PRs remain independent", () => {
  expect(workflow.concurrency["cancel-in-progress"]).toBe(true);
  function group(number: number) {
    const context = JSON.parse(
      JSON.stringify({
        github: { workflow: "Docs", event: { pull_request: { number } } },
      }),
      data.reviver,
    ) as data.Dictionary;
    return workflow.concurrency.group.replace(
      /\$\{\{(.*?)\}\}/g,
      (_, expression: string) => {
        const tokens = new Lexer(expression).lex().tokens;
        return new Evaluator(
          new Parser(tokens, ["github"], []).parse(),
          context,
        )
          .evaluate()
          .coerceString();
      },
    );
  }
  expect(group(42)).toBe(group(42));
  expect(group(42)).not.toBe(group(43));
});

test("the candidate log step rejects a checkout that does not match the event SHA", () => {
  const command = action.runs.steps.find(
    (step) => step.name === "Record candidate and tools",
  )!.run!;
  const head = spawnSync("git", ["rev-parse", "HEAD"], {
    cwd: root,
    encoding: "utf8",
  });
  expect(head.status).toBe(0);
  const matching = run(command, root, {
    ...process.env,
    ...identityEnv(),
    GITHUB_SHA: head.stdout.trim(),
  });
  expect(matching.status, matching.stderr).toBe(0);
  const different = run(command, root, {
    ...process.env,
    ...identityEnv(),
    GITHUB_SHA: "0".repeat(40),
  });
  expect(different.status).toBe(1);
});

test("the real identity command rejects Node version drift before accepting the candidate", () => {
  const directory = fixture();
  const pins = readFileSync(join(directory, ".tool-versions"), "utf8");
  writeFileSync(
    join(directory, ".tool-versions"),
    pins.replace(/^nodejs .*$/m, "nodejs 0.0.1"),
  );
  const head = spawnSync("git", ["rev-parse", "HEAD"], {
    cwd: root,
    encoding: "utf8",
  }).stdout.trim();
  // Run in the real checkout while only substituting the version reader's input.
  const bin = join(directory, "bin");
  mkdirSync(bin);
  writeFileSync(
    join(bin, "node"),
    `#!/bin/bash\ncd '${directory}'\nexec '${spawnSync("which", ["node"], { encoding: "utf8" }).stdout.trim()}' "$@"\n`,
  );
  chmodSync(join(bin, "node"), 0o700);
  const result = run(action.runs.steps[0]!.run!, root, {
    ...process.env,
    ...identityEnv(),
    GITHUB_SHA: head,
    PATH: `${bin}:${process.env.PATH}`,
  });
  expect(result.status).not.toBe(0);
  expect(result.stdout + result.stderr).toContain(
    "Node version does not match",
  );
});

test("the real lychee installer rejects corrupted bytes before unpacking", () => {
  const directory = fixture();
  const bin = join(directory, "bin");
  mkdirSync(bin);
  writeFileSync(
    join(bin, "curl"),
    '#!/bin/bash\nwhile [[ "$1" != --output ]]; do shift; done\nprintf corrupt > "$2"\n',
  );
  writeFileSync(
    join(bin, "sha256sum"),
    '#!/bin/bash\nexec shasum -a 256 "$@"\n',
  );
  chmodSync(join(bin, "curl"), 0o700);
  chmodSync(join(bin, "sha256sum"), 0o700);
  const result = run(
    action.runs.steps.find((step) => step.name === "Install lychee")!.run!,
    root,
    { ...process.env, ...identityEnv(), PATH: `${bin}:${process.env.PATH}` },
  );
  expect(result.status).not.toBe(0);
  expect(result.stdout + result.stderr).toMatch(/FAILED|did NOT match/);
});

test("the workflow document commands accept valid local documents", () => {
  const directory = fixture();
  for (const command of [documents, links]) {
    const result = run(command, directory, {
      ...process.env,
      ...identityEnv(),
    });
    expect(result.error).toBeUndefined();
    expect(result.status, result.stdout + result.stderr).toBe(0);
  }
});

test.each([
  [
    "formatting",
    "README.md",
    "# Fixture\n\ntext   \n",
    documents,
    /Code style issues/,
  ],
  ["Markdown", "README.md", "# Fixture\n\n### Skipped\n", documents, /MD001/],
  [
    "local links",
    "README.md",
    "# Fixture\n\n[Missing](missing.md)\n",
    links,
    /missing\.md/,
  ],
  ["Bun pin", ".tool-versions", "bun 1.4.3\n", documents, /pins.*1\.4\.3/],
] as const)(
  "a real %s failure remains unsuccessful",
  (_, path, content, command, diagnostic) => {
    const directory = fixture();
    writeFileSync(join(directory, path), content);
    const result = run(command, directory, {
      ...process.env,
      ...identityEnv(),
    });
    expect(result.error).toBeUndefined();
    expect(result.status).toBeGreaterThan(0);
    expect(result.stdout + result.stderr).toMatch(diagnostic);
  },
);

test("the actual preview command fails when its candidate design is incomplete", () => {
  const result = run(preview, root, {
    ...process.env,
    ...identityEnv(),
    NUCLEUS_DESIGN_CANDIDATE: fixture(),
  });
  expect(result.error).toBeUndefined();
  expect(result.status).toBe(1);
  expect(result.stdout + result.stderr).toMatch(/halo-home\.html/);
});
