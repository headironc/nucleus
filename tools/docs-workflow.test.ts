import { afterEach, expect, test } from "bun:test";
import { data, Evaluator, Lexer, Parser } from "@actions/expressions";
import { spawnSync } from "node:child_process";
import {
  copyFileSync,
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
      "timeout-minutes": number;
      if?: string;
      permissions?: unknown;
      "continue-on-error"?: boolean;
      steps: Step[];
    }
  >;
};
const job = workflow.jobs.docs!;
const [preview, documents, links] = job.steps
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

test("every dev/main PR event, including retargets and non-document changes, runs the same job", () => {
  expect(workflow.on).toEqual({
    pull_request: {
      branches: ["dev", "main"],
      types: ["opened", "reopened", "synchronize", "edited"],
    },
  });
  expect(Object.keys(workflow.jobs)).toEqual(["docs"]);
  expect(job.name).toBe("Nucleus Docs");
  expect(job.if).toBeUndefined();
  expect(job["continue-on-error"]).toBeUndefined();
  expect(job["timeout-minutes"]).toBeGreaterThan(0);
  for (const step of job.steps) {
    expect(step.if).toBeUndefined();
    expect(step["continue-on-error"]).toBeUndefined();
  }
  // The three commands own four obligations: format and Markdown share one entrypoint.
  expect([preview, documents, links]).toEqual([
    "bun test tools/preview-design.test.ts",
    "bun run check:docs",
    "bun run check:docs:links",
  ]);
  expect(
    job.steps.find((step) => step.name === "Install dependencies")?.run,
  ).toBe("bun install --frozen-lockfile --ignore-scripts");
  expect(workflow.defaults.run.shell).toBe("bash");
});

test("candidate code has only read access and uses the event checkout with pinned actions", () => {
  expect(workflow.permissions).toEqual({ contents: "read" });
  expect(job.permissions).toBeUndefined();
  const checkout = job.steps[0]!;
  expect(checkout.uses).toMatch(/^actions\/checkout@[0-9a-f]{40}$/);
  expect(checkout.with).toEqual({ "persist-credentials": false });
  for (const step of job.steps.filter((step) => step.uses))
    expect(step.uses).toMatch(/^[\w-]+\/[\w-]+@[0-9a-f]{40}$/);
  expect(
    job.steps.find((step) => step.uses?.startsWith("oven-sh/setup-bun@"))?.with,
  ).toEqual({
    "bun-version-file": ".tool-versions",
  });
  expect(
    job.steps.find((step) => step.uses?.startsWith("actions/setup-node@"))
      ?.with,
  ).toEqual({
    "node-version": "24",
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
  const command = job.steps.find(
    (step) => step.name === "Record candidate and tools",
  )!.run!;
  const head = spawnSync("git", ["rev-parse", "HEAD"], {
    cwd: root,
    encoding: "utf8",
  });
  expect(head.status).toBe(0);
  const matching = run(command, root, {
    ...process.env,
    GITHUB_SHA: head.stdout.trim(),
  });
  expect(matching.status, matching.stderr).toBe(0);
  const different = run(command, root, {
    ...process.env,
    GITHUB_SHA: "0".repeat(40),
  });
  expect(different.status).toBe(1);
});

test("the workflow document commands accept valid local documents", () => {
  const directory = fixture();
  for (const command of [documents, links]) {
    const result = run(command, directory);
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
    const result = run(command, directory);
    expect(result.error).toBeUndefined();
    expect(result.status).toBeGreaterThan(0);
    expect(result.stdout + result.stderr).toMatch(diagnostic);
  },
);

test("the actual preview command fails when its candidate design is incomplete", () => {
  const result = run(preview, root, {
    ...process.env,
    NUCLEUS_DESIGN_CANDIDATE: fixture(),
  });
  expect(result.error).toBeUndefined();
  expect(result.status).toBe(1);
  expect(result.stderr).toMatch(/halo-home\.html/);
});
