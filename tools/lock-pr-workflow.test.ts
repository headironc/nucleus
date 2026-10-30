import { afterEach, expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const workflow = Bun.YAML.parse(
  readFileSync(
    new URL("../.github/workflows/lock-pr.yml", import.meta.url),
    "utf8",
  ),
) as {
  on: unknown;
  permissions: unknown;
  concurrency: { group: string; "cancel-in-progress": boolean };
  jobs: Record<
    string,
    {
      "timeout-minutes": number;
      steps: {
        run: string;
        shell: string;
        env: Record<string, string>;
        uses?: string;
      }[];
    }
  >;
};
const job = workflow.jobs.lock!;
const step = job.steps[0]!;
const directories: string[] = [];

afterEach(() => {
  for (const directory of directories.splice(0))
    rmSync(directory, { recursive: true, force: true });
});

test("closed events use a bounded metadata-only job with minimal permissions", () => {
  expect(workflow.on).toEqual({ pull_request_target: { types: ["closed"] } });
  expect(workflow.permissions).toEqual({ "pull-requests": "write" });
  expect(workflow.concurrency).toEqual({
    group: "${{ github.workflow }}-${{ github.event.pull_request.number }}",
    "cancel-in-progress": false,
  });
  expect(Object.keys(workflow.jobs)).toEqual(["lock"]);
  expect(job["timeout-minutes"]).toBe(3);
  expect(job.steps).toHaveLength(1);
  expect(step.uses).toBeUndefined();
  expect(step.env).toEqual({
    GH_TOKEN: "${{ github.token }}",
    REPOSITORY: "${{ github.repository }}",
    PR_NUMBER: "${{ github.event.pull_request.number }}",
  });
});

function run(
  state: string,
  locked: string,
  writeStatus = 0,
  readStatus = 0,
  finalReadStatus = 0,
) {
  const directory = mkdtempSync(join(tmpdir(), "nucleus-pr-lock-"));
  directories.push(directory);
  // The fake API records every argument and supplies independent server outcomes.
  writeFileSync(
    join(directory, "gh"),
    `#!/bin/bash
printf '%s\\n' "$*" >> "$CALLS"
if [[ "$*" == *"--method PUT"* ]]; then
  exit "$WRITE_STATUS"
elif [[ "$*" == *"@tsv"* ]]; then
  printf '%s\\n' "$INITIAL_STATE"
  exit "$READ_STATUS"
else
  printf '%s\\n' "$FINAL_LOCKED"
  exit "$FINAL_READ_STATUS"
fi
`,
    { mode: 0o700 },
  );
  const command = join(directory, "lock.sh");
  writeFileSync(command, step.run);
  const result = spawnSync(
    "bash",
    ["--noprofile", "--norc", "-euo", "pipefail", command],
    {
      cwd: directory,
      env: {
        PATH: `${directory}:${process.env.PATH}`,
        REPOSITORY: "example/project",
        PR_NUMBER: "17",
        INITIAL_STATE: state,
        FINAL_LOCKED: locked,
        WRITE_STATUS: String(writeStatus),
        READ_STATUS: String(readStatus),
        FINAL_READ_STATUS: String(finalReadStatus),
        CALLS: join(directory, "calls"),
      },
      encoding: "utf8",
      timeout: 5_000,
    },
  );
  expect(result.error).toBeUndefined();
  const calls = readFileSync(join(directory, "calls"), "utf8")
    .trim()
    .split("\n");
  return { ...result, calls };
}

const initialRead =
  "api --hostname github.com repos/example/project/pulls/17 --jq [.state, .locked] | @tsv";
const lockWrite =
  "api --hostname github.com --method PUT -H Content-Length: 0 repos/example/project/issues/17/lock";
const finalRead =
  "api --hostname github.com repos/example/project/pulls/17 --jq .locked";

test.each(["open\tfalse", "closed\ttrue"])("%s needs no write", (state) => {
  const result = run(state, "false");
  expect(result.status).toBe(0);
  expect(result.calls).toEqual([initialRead]);
});

test.each([0, 1])(
  "confirmed lock succeeds after write status %i without retry",
  (status) => {
    const result = run("closed\tfalse", "true", status);
    expect(result.status).toBe(0);
    expect(result.calls).toEqual([initialRead, lockWrite, finalRead]);
  },
);

test.each([0, 1])(
  "unconfirmed lock fails after write status %i without retry",
  (status) => {
    const result = run("closed\tfalse", "false", status);
    expect(result.status).toBe(1);
    expect(result.calls).toEqual([initialRead, lockWrite, finalRead]);
    expect(result.stdout).toContain(
      "::error::The PR lock could not be confirmed.",
    );
  },
);

test("failed initial read prevents a write", () => {
  const result = run("closed\tfalse", "true", 0, 1);
  expect(result.status).toBe(1);
  expect(result.calls).toEqual([initialRead]);
});

test("failed readback retains failure without another write", () => {
  const result = run("closed\tfalse", "true", 1, 0, 1);
  expect(result.status).toBe(1);
  expect(result.calls).toEqual([initialRead, lockWrite, finalRead]);
});
