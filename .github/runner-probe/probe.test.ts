import { afterEach, expect, test } from "bun:test";
import { data, Evaluator, Lexer, Parser } from "@actions/expressions";
import { truthy } from "@actions/expressions/result";
import { spawnSync } from "node:child_process";
import {
  chmodSync,
  existsSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const root = resolve(import.meta.dir, "../..");
type Step = {
  name?: string;
  run?: string;
  uses?: string;
  if?: string;
  with?: Record<string, unknown>;
  "continue-on-error"?: unknown;
};
const source = readFileSync(
  join(root, ".github/workflows/runner-probe.yml"),
  "utf8",
);
const workflow = Bun.YAML.parse(source) as {
  on: unknown;
  permissions: unknown;
  concurrency: unknown;
  defaults: unknown;
  jobs: Record<
    string,
    {
      if: string;
      "runs-on": string;
      "timeout-minutes": number;
      permissions: unknown;
      container: { image: string; credentials: unknown; options?: string };
      env: Record<string, string>;
      steps: Step[];
    }
  >;
};
const job = workflow.jobs.platform!;
const step = (name: string) => job.steps.find((value) => value.name === name)!;
const directories: string[] = [];
afterEach(() => {
  for (const path of directories.splice(0))
    rmSync(path, { recursive: true, force: true });
});
function temporary() {
  const path = mkdtempSync(join(tmpdir(), "runner-probe-test-"));
  directories.push(path);
  return path;
}
function execute(selected: Step, env: NodeJS.ProcessEnv, cwd = root) {
  return spawnSync(
    "bash",
    ["--noprofile", "--norc", "-eo", "pipefail", "-c", selected.run!],
    {
      cwd,
      env: { ...process.env, ...env },
      encoding: "utf8",
      timeout: 10_000,
    },
  );
}
const context = {
  GITHUB_EVENT_NAME: "workflow_dispatch",
  GITHUB_REPOSITORY_ID: "1432619931",
  GITHUB_REPOSITORY: "headironc/nucleus",
  GITHUB_REF: "refs/heads/dev",
  EXPECTED_REVISION: "a".repeat(40),
  GITHUB_SHA: "a".repeat(40),
  GITHUB_WORKFLOW_SHA: "a".repeat(40),
};

test("only manual dev work can enter the dedicated runner job", () => {
  expect(workflow.on).toEqual({
    workflow_dispatch: {
      inputs: {
        expected_revision: {
          description: "Reviewed dev commit (full SHA)",
          required: true,
          type: "string",
        },
        fail_after_checks: {
          description:
            "Fail after successful platform checks to verify runner reclamation",
          required: false,
          type: "boolean",
          default: false,
        },
        cancel_after_checks: {
          description:
            "Wait with active PostgreSQL after checks for ordinary cancellation",
          required: false,
          type: "boolean",
          default: false,
        },
      },
    },
  });
  function allowed(
    extra: Partial<{
      event_name: string;
      repository_id: string;
      repository: string;
      ref: string;
    }> = {},
  ) {
    const github = {
      event_name: "workflow_dispatch",
      repository_id: "1432619931",
      repository: "headironc/nucleus",
      ref: "refs/heads/dev",
      ...extra,
    };
    const expression = new Parser(
      new Lexer(job.if).lex().tokens,
      ["github"],
      [],
    ).parse();
    return truthy(
      new Evaluator(
        expression,
        new data.Dictionary({
          key: "github",
          value: new data.Dictionary(
            ...Object.entries(github).map(([key, value]) => ({
              key,
              value: new data.StringData(value),
            })),
          ),
        }),
      ).evaluate(),
    );
  }
  expect(allowed()).toBe(true);
  for (const extra of [
    { event_name: "pull_request" },
    { repository_id: "1" },
    { repository: "other/nucleus" },
    { ref: "refs/heads/main" },
    { ref: "refs/tags/dev" },
  ])
    expect(allowed(extra)).toBe(false);
  expect(Object.keys(workflow.jobs)).toEqual(["platform"]);
  expect(job["runs-on"]).toBe("nucleus-runners");
  expect(
    Bun.YAML.parse(readFileSync(join(root, ".github/actionlint.yaml"), "utf8")),
  ).toEqual({ "self-hosted-runner": { labels: ["nucleus-runners"] } });
  expect(job["timeout-minutes"]).toBe(45);
  expect(workflow.concurrency).toEqual({
    group: "nucleus-runner-first-consumer",
    "cancel-in-progress": false,
  });
});

test("actual source guard rejects drift, missing values and shell payloads", () => {
  const guard = step("Bind reviewed dev revision");
  expect(execute(guard, context).status).toBe(0);
  for (const key of Object.keys(context)) {
    expect(execute(guard, { ...context, [key]: "" }).status).not.toBe(0);
    expect(
      execute(guard, { ...context, [key]: "b".repeat(40) }).status,
    ).not.toBe(0);
  }
  for (const value of [
    "dev",
    "a".repeat(39),
    "A".repeat(40),
    "a".repeat(41),
    "$(exit 0)",
    "a\n".repeat(40),
  ])
    expect(
      execute(guard, {
        ...context,
        EXPECTED_REVISION: value,
        GITHUB_SHA: value,
        GITHUB_WORKFLOW_SHA: value,
      }).status,
    ).not.toBe(0);
  const checkout = job.steps.find((value) =>
    value.uses?.startsWith("actions/checkout@"),
  )!;
  expect(job.steps.indexOf(guard)).toBeLessThan(job.steps.indexOf(checkout));
  expect(checkout.with).toEqual({
    ref: "${{ inputs.expected_revision }}",
    "persist-credentials": false,
  });
});

test("native initialization alone receives the read-only package credential", () => {
  expect(workflow.permissions).toEqual({ contents: "read" });
  expect(job.permissions).toEqual({ contents: "read", packages: "read" });
  expect(job.container).toEqual({
    image:
      "ghcr.io/headironc/nucleus-ci@sha256:80ae416d4bc7287532c0974d25390c5bc9b27cd62defe4e64860b501414b4592",
    credentials: {
      username: "${{ github.actor }}",
      password: "${{ github.token }}",
    },
  });
  expect(job.env).toEqual({
    CARGO_BUILD_JOBS: "2",
    EXPECTED_REVISION: "${{ inputs.expected_revision }}",
    FAIL_AFTER_CHECKS: "${{ inputs.fail_after_checks }}",
    CANCEL_AFTER_CHECKS: "${{ inputs.cancel_after_checks }}",
  });
  expect(source).not.toMatch(
    /secrets\.|docker login|sudo|chmod|actions\/cache|rust-cache|CARGO_HOME|RUSTUP_HOME/,
  );
  for (const entry of job.steps) {
    expect(entry["continue-on-error"]).toBeUndefined();
    if (entry.uses) expect(entry.uses).toMatch(/^[\w-]+\/[\w-]+@[0-9a-f]{40}$/);
  }
});

test("actual checkout guard rejects a different revision and inaccessible mounts", () => {
  const head = spawnSync("git", ["rev-parse", "HEAD"], {
    cwd: root,
    encoding: "utf8",
  });
  expect(head.status).toBe(0);
  const path = temporary();
  const env = {
    ...context,
    EXPECTED_REVISION: head.stdout.trim(),
    GITHUB_WORKSPACE: root,
    RUNNER_TEMP: path,
    HOME: path,
  };
  const guard = step("Verify checkout and container access");
  expect(execute(guard, env).status).toBe(0);
  expect(
    execute(guard, { ...env, EXPECTED_REVISION: "0".repeat(40) }).status,
  ).not.toBe(0);
  chmodSync(path, 0o500);
  try {
    expect(execute(guard, env).status).not.toBe(0);
    expect(execute(guard, { ...env, RUNNER_TEMP: root }).status).not.toBe(0);
  } finally {
    chmodSync(path, 0o700);
  }
  // The runtime UID belongs to Runner; the workflow must reject root itself.
  writeFileSync(join(path, "id"), '#!/bin/bash\nprintf "0\\n"\n', {
    mode: 0o700,
  });
  expect(
    execute(guard, { ...env, PATH: `${path}:${process.env.PATH}` }).status,
  ).not.toBe(0);
});

test("actual logged commands preserve failures and call the complete consumer in order", () => {
  expect(workflow.defaults).toEqual({ run: { shell: "bash" } });
  const path = temporary();
  // Record the executable boundary without running product checks or a database.
  writeFileSync(
    join(path, "bun"),
    '#!/bin/bash\nprintf "%s\\n" "$*" >> "$CALLS"\nprintf "probe output\\n"\nexit "$STATUS"\n',
    { mode: 0o700 },
  );
  const selected = [
    "Verify installed image and PostgreSQL",
    "Create private fixture scope",
    "Check complete platform package",
    "Check and clean private PostgreSQL scope",
  ].map(step);
  expect(selected.map((entry) => job.steps.indexOf(entry))).toEqual(
    [...selected.map((entry) => job.steps.indexOf(entry))].sort(
      (a, b) => a - b,
    ),
  );
  const env = {
    PATH: `${path}:${process.env.PATH}`,
    RUNNER_TEMP: path,
    CALLS: join(path, "calls"),
    STATUS: "0",
  };
  for (const entry of selected) expect(execute(entry, env).status).toBe(0);
  expect(readFileSync(env.CALLS, "utf8").trim().split("\n")).toEqual([
    "tools/halo-ci-image.ts verify",
    "tools/package-postgresql.ts temp",
    "tools/check-packages.ts --package platform --test-threads 2",
    "tools/package-postgresql.ts cleanup",
  ]);
  for (const entry of selected)
    expect(execute(entry, { ...env, STATUS: "23" }).status).toBe(23);
  for (const name of ["image", "temp", "platform", "cleanup"])
    expect(
      readFileSync(join(path, `runner-probe-${name}.log`), "utf8"),
    ).toContain("probe output");
});

test("only an explicit failure request after successful checks emits the bound marker", () => {
  const failure = step("Fail after successful platform checks");
  const platform = step("Check complete platform package");
  const cleanup = step("Check and clean private PostgreSQL scope");
  expect(job.steps.indexOf(failure)).toBe(job.steps.indexOf(platform) + 1);
  expect(job.steps.indexOf(cleanup)).toBeGreaterThan(
    job.steps.indexOf(failure),
  );
  expect(failure.run).not.toContain("${{");
  for (const successful of [false, true]) {
    for (const requested of [undefined, false, true]) {
      const path = temporary();
      const functions = [
        {
          name: "success",
          minArgs: 0,
          maxArgs: 0,
          call: () => new data.BooleanData(successful),
        },
      ];
      const inputs = new data.Dictionary();
      if (requested !== undefined)
        inputs.add("fail_after_checks", new data.BooleanData(requested));
      const enabled = truthy(
        new Evaluator(
          new Parser(
            new Lexer(failure.if!).lex().tokens,
            ["inputs"],
            functions,
          ).parse(),
          new data.Dictionary({ key: "inputs", value: inputs }),
          new Map(functions.map((fn) => [fn.name, fn])),
        ).evaluate(),
      );
      expect(enabled).toBe(successful && requested === true);
      if (enabled) {
        const result = execute(failure, {
          RUNNER_TEMP: path,
          GITHUB_RUN_ID: "123456",
          GITHUB_RUN_ATTEMPT: "1",
          GITHUB_SHA: "a".repeat(40),
        });
        expect(result.status).toBe(42);
        const expected =
          `RUNNER_PROBE_EXPECTED_FAILURE run=123456 attempt=1 source=${"a".repeat(40)} ` +
          "reason=runner-reclamation-check exit=42\n";
        expect(result.stdout).toBe(expected);
        expect(
          readFileSync(join(path, "runner-probe-failure.log"), "utf8"),
        ).toBe(expected);
      } else {
        expect(existsSync(join(path, "runner-probe-failure.log"))).toBe(false);
      }
    }
  }
});

test("failure marker write errors remain failures instead of claiming exit 42", () => {
  const path = temporary();
  const result = execute(step("Fail after successful platform checks"), {
    RUNNER_TEMP: join(path, "absent"),
    GITHUB_RUN_ID: "123456",
    GITHUB_RUN_ATTEMPT: "1",
    GITHUB_SHA: "a".repeat(40),
  });
  expect(result.status).not.toBe(0);
  expect(result.status).not.toBe(42);
  expect(existsSync(join(path, "absent", "runner-probe-failure.log"))).toBe(
    false,
  );
});

test("cleanup and diagnostics remain eligible after failure or cancellation, which cannot pass", () => {
  const cleanup = step("Check and clean private PostgreSQL scope");
  const upload = job.steps.findLast((value) =>
    value.uses?.startsWith("actions/upload-artifact@"),
  )!;
  const reject = step("Reject cancelled probe");
  for (const cancelled of [false, true]) {
    const functions = [
      {
        name: "always",
        minArgs: 0,
        maxArgs: 0,
        call: () => new data.BooleanData(true),
      },
      {
        name: "cancelled",
        minArgs: 0,
        maxArgs: 0,
        call: () => new data.BooleanData(cancelled),
      },
    ];
    const enabled = (entry: Step) =>
      truthy(
        new Evaluator(
          new Parser(new Lexer(entry.if!).lex().tokens, [], functions).parse(),
          new data.Dictionary(),
          new Map(functions.map((fn) => [fn.name, fn])),
        ).evaluate(),
      );
    expect(enabled(cleanup)).toBe(true);
    expect(enabled(upload)).toBe(true);
    expect(enabled(reject)).toBe(cancelled);
  }
  expect(job.steps.indexOf(cleanup)).toBeLessThan(job.steps.indexOf(upload));
  expect(upload.with).toEqual({
    name: "runner-probe-${{ github.run_id }}-${{ github.run_attempt }}",
    path: "${{ runner.temp }}/runner-probe-*.log",
    "retention-days": 7,
    "if-no-files-found": "warn",
  });
  expect(execute(reject, {}).status).toBe(1);
});

test("conflicting modes are rejected before checkout or private resource preparation", () => {
  const guard = step("Bind reviewed dev revision");
  for (const failure of ["", "false", "true"])
    for (const cancel of ["", "false", "true"])
      expect(
        execute(guard, {
          ...context,
          FAIL_AFTER_CHECKS: failure,
          CANCEL_AFTER_CHECKS: cancel,
        }).status,
      ).toBe(failure === "true" && cancel === "true" ? 1 : 0);
  expect(job.steps[0]).toBe(guard);
});

test("active PG is uploaded before interruptible waiting and cleaned before the independent scan", () => {
  const selected = [
    "Check complete platform package",
    "Start private cancellation workload",
    "Preserve pre-interrupt diagnostics",
    "Wait for ordinary cancellation",
    "Stop private cancellation workload",
    "Check and clean private PostgreSQL scope",
  ].map(step);
  const indices = selected.map((entry) => job.steps.indexOf(entry));
  expect(indices).toEqual([...indices].sort((a, b) => a - b));
  const upload = step("Preserve pre-interrupt diagnostics");
  expect(upload.with?.name).toBe(
    "runner-probe-pre-interrupt-${{ github.run_id }}-${{ github.run_attempt }}",
  );
  expect(upload.with?.["if-no-files-found"]).toBe("error");
  const path = temporary();
  writeFileSync(
    join(path, "bun"),
    '#!/bin/bash\nprintf "%s\\n" "$*"\nexit "$STATUS"\n',
    { mode: 0o700 },
  );
  for (const [name, command] of [
    ["Start private cancellation workload", "start"],
    ["Wait for ordinary cancellation", "wait"],
    ["Stop private cancellation workload", "cleanup"],
  ]) {
    const entry = step(name!);
    expect(entry.run).not.toContain("${{");
    const result = execute(entry, {
      PATH: `${path}:${process.env.PATH}`,
      RUNNER_TEMP: path,
      STATUS: "27",
    });
    expect(result.status).toBe(27);
    expect(result.stdout.trim()).toBe(
      `.github/runner-probe/cancel.ts ${command}`,
    );
  }
  for (const successful of [false, true])
    for (const requested of [undefined, false, true]) {
      const functions = [
        {
          name: "success",
          minArgs: 0,
          maxArgs: 0,
          call: () => new data.BooleanData(successful),
        },
        {
          name: "always",
          minArgs: 0,
          maxArgs: 0,
          call: () => new data.BooleanData(true),
        },
      ];
      const inputs = new data.Dictionary();
      if (requested !== undefined)
        inputs.add("cancel_after_checks", new data.BooleanData(requested));
      for (const entry of selected.slice(1, 5)) {
        const enabled = truthy(
          new Evaluator(
            new Parser(
              new Lexer(entry.if!).lex().tokens,
              ["inputs"],
              functions,
            ).parse(),
            new data.Dictionary({ key: "inputs", value: inputs }),
            new Map(functions.map((fn) => [fn.name, fn])),
          ).evaluate(),
        );
        expect(enabled).toBe(
          requested === true &&
            (entry.name === "Stop private cancellation workload" || successful),
        );
      }
    }
});
