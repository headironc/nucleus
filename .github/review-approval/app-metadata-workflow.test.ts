import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";

test("metadata workflow has a separate owner-only trusted dev entry and one credential step", () => {
  const workflow: any = Bun.YAML.parse(
    readFileSync(
      new URL("../workflows/app-metadata.yml", import.meta.url),
      "utf8",
    ),
  );
  expect(Object.keys(workflow.on)).toEqual(["workflow_dispatch"]);
  expect(workflow.on.workflow_dispatch).toBeNull();
  expect(workflow.permissions).toEqual({ contents: "read" });
  expect(workflow.concurrency).toEqual({
    group: "cladiron-app-metadata",
    "cancel-in-progress": false,
  });
  expect(Object.keys(workflow.jobs)).toEqual(["metadata"]);
  const job = workflow.jobs.metadata;
  expect(job.environment).toBe("dev-admission");
  expect(job["timeout-minutes"]).toBe(5);
  expect(job.if).toBe(
    "${{ github.ref == 'refs/heads/dev' && github.repository == 'headironc/nucleus' && github.repository_id == '1432619931' && github.repository_owner_id == '19648989' && github.actor_id == '19648989' && github.triggering_actor == github.repository_owner }}",
  );
  expect(job.steps.filter((s: any) => s.uses)).toEqual([
    {
      uses: "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1",
      with: { ref: "${{ github.workflow_sha }}", "persist-credentials": false },
    },
    {
      uses: "oven-sh/setup-bun@0c5077e51419868618aeaa5fe8019c62421857d6",
      with: { "bun-version-file": ".tool-versions" },
    },
  ]);
  expect(job.steps.filter((s: any) => s.run).map((s: any) => s.run)).toEqual([
    "bun tools/check-bun.ts",
    "bun --no-env-file .github/review-approval/app-metadata.ts",
  ]);
  const credentialSteps = job.steps.filter((s: any) =>
    JSON.stringify(s).includes("secrets."),
  );
  expect(credentialSteps).toHaveLength(1);
  expect(credentialSteps[0].env).toEqual({
    CLADIRON_TRIGGERING_ACTOR_ID:
      "${{ github.triggering_actor == github.repository_owner && github.repository_owner_id || '0' }}",
    CLADIRON_CLIENT_ID: "${{ vars.CLADIRON_CLIENT_ID }}",
    CLADIRON_PRIVATE_KEY: "${{ secrets.NUCLEUS_DEV_PRIVATE_KEY }}",
  });
  expect(credentialSteps[0].run).toBe(
    "bun --no-env-file .github/review-approval/app-metadata.ts",
  );
  expect(
    job.steps.some(
      (s: any) =>
        s.if ||
        /main\.ts|invalidate|docker|gh |installationToken/.test(s.run ?? ""),
    ),
  ).toBe(false);
});
