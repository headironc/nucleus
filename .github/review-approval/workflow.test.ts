import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { context } from "./main.ts";
import { stableWorkflows } from "./ci.ts";

const load = (file: string): any =>
  Bun.YAML.parse(
    readFileSync(new URL(`../workflows/${file}`, import.meta.url), "utf8"),
  );
test("credential-bearing workflow only dispatches owner trusted dev, serializes globally and never checks out PR code", () => {
  const workflow = load("review-approval.yml");
  expect(Object.keys(workflow.on)).toEqual(["workflow_dispatch"]);
  expect(workflow.concurrency).toEqual({
    group: "cladiron-review-approval",
    "cancel-in-progress": false,
  });
  expect(workflow.permissions).toEqual({
    contents: "read",
    actions: "read",
    checks: "read",
    "pull-requests": "read",
  });
  const steps = workflow.jobs.approval.steps;
  expect(
    steps.filter((s: any) => s.uses?.startsWith("actions/checkout@")),
  ).toEqual([
    {
      uses: "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1",
      with: { ref: "${{ github.workflow_sha }}", "persist-credentials": false },
    },
  ]);
  expect(workflow.jobs.approval.environment).toBe("dev-admission");
  const writer = steps.findIndex(
    (s: any) => s.name === "Verify evidence and perform selected operation",
  );
  expect(
    steps
      .slice(0, writer)
      .some(
        (s: any) =>
          s.uses?.startsWith("actions/upload-artifact@") &&
          s.with.name.startsWith("cladiron-approval-intent-"),
      ),
  ).toBe(true);
  expect(steps[writer].env.CLADIRON_PRIVATE_KEY).toBe(
    "${{ inputs.mode == 'approve' && secrets.NUCLEUS_DEV_PRIVATE_KEY || '' }}",
  );
  expect(steps[writer].if).toBe("${{ inputs.mode == 'approve' }}");
  expect(
    steps.find((s: any) => s.name === "Prepare review request text").env
      .CLADIRON_PRIVATE_KEY,
  ).toBeUndefined();
  expect(steps.filter((s: any) => s.env?.CLADIRON_PRIVATE_KEY)).toHaveLength(2);
  expect(
    steps.some((s: any) =>
      /docker|container|gh pr|@codex|@cubic/.test(s.run ?? ""),
    ),
  ).toBe(false);
});
test("all stable workflows and dedicated publication checks produce exactly one successful-candidate receipt", () => {
  for (const file of [...stableWorkflows, "halo-publish-checks.yml"]) {
    const workflow = load(file);
    const steps = Object.values(workflow.jobs).flatMap((j: any) => j.steps);
    const producer = steps.filter(
      (s: any) => s.run === "bun .github/review-approval/ci.ts",
    ) as any[];
    expect(producer).toHaveLength(1);
    expect(producer[0].if).toBeUndefined();
    const uploads = steps.filter(
      (s: any) =>
        s.with?.name ===
        "cladiron-ci-${{ github.run_id }}-${{ github.run_attempt }}",
    ) as any[];
    expect(uploads).toHaveLength(1);
    expect(uploads[0].with["if-no-files-found"]).toBe("error");
  }
});
test("running capture uploads only a successful prepare observation and exposes no App credential", () => {
  const workflow = load("review-approval.yml");
  const steps = workflow.jobs.approval.steps;
  const prepare = steps.find((s: any) => s.id === "prepare");
  const upload = steps.filter(
    (s: any) => s.name === "Upload review start evidence",
  );
  expect(prepare.if).toBe("${{ inputs.mode == 'prepare' }}");
  expect(prepare.env.REVIEW_START_PATH).toBe(
    "${{ runner.temp }}/review-start.json",
  );
  expect(Object.keys(prepare.env).sort()).toEqual([
    "CLADIRON_TRIGGERING_ACTOR_ID",
    "GITHUB_TOKEN",
    "REVIEW_START_PATH",
  ]);
  expect(upload).toHaveLength(1);
  expect(upload[0].if).toBe(
    "${{ inputs.mode == 'prepare' && steps.prepare.outputs.review_start_captured == 'true' }}",
  );
  expect(upload[0].uses).toBe(
    "actions/upload-artifact@cf430e030ddbb5b0abf93d22962f4752f3646cd9",
  );
  expect(upload[0].with).toEqual({
    name: "cladiron-review-start-${{ github.run_id }}-${{ github.run_attempt }}",
    path: prepare.env.REVIEW_START_PATH,
    "if-no-files-found": "error",
    "retention-days": 30,
  });
  expect(steps.indexOf(prepare)).toBeLessThan(steps.indexOf(upload[0]));
  for (const lane of ["code", "security"])
    expect(
      workflow.on.workflow_dispatch.inputs[`${lane}_start_artifact`],
    ).toMatchObject({ required: false, type: "string" });
});
test("context rejects wrong actor, rerunner, ref or event", () => {
  const env = {
    GITHUB_EVENT_NAME: "workflow_dispatch",
    GITHUB_REPOSITORY: "headironc/nucleus",
    GITHUB_REPOSITORY_ID: "1432619931",
    GITHUB_REF: "refs/heads/dev",
    GITHUB_ACTOR_ID: "19648989",
    CLADIRON_TRIGGERING_ACTOR_ID: "19648989",
  };
  context(env);
  for (const delta of [
    { GITHUB_EVENT_NAME: "pull_request_target" },
    { GITHUB_REF: "refs/heads/main" },
    { GITHUB_ACTOR_ID: "1" },
    { CLADIRON_TRIGGERING_ACTOR_ID: "1" },
  ])
    expect(() => context({ ...env, ...delta })).toThrow();
});
