import { expect, test } from "bun:test";
import { data, Evaluator, Lexer, Parser } from "@actions/expressions";
import { truthy } from "@actions/expressions/result";
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { imageInputs } from "./halo-ci-image";

const read = (path: string) =>
  readFileSync(new URL(`../${path}`, import.meta.url), "utf8");

test("the dependency image binds actual tool pins and build inputs", () => {
  const original = imageInputs();
  expect(original.bun).toBe("1.4.2");
  expect(original.playwright).toBe(
    JSON.parse(read("halo/package.json")).devDependencies["@playwright/test"],
  );
  for (const path of [
    "fixture/postgresql.ts",
    ".github/halo-ci/Dockerfile",
    "rust-toolchain.toml",
    "tools/halo-ci-image.ts",
    "tools/check-bun.ts",
  ]) {
    const changed = imageInputs(
      (name) => read(name) + (name === path ? "\n" : ""),
    );
    expect(changed.digest, path).not.toBe(original.digest);
  }
  const unrelated = imageInputs((path) => {
    if (path !== "halo/package.json") return read(path);
    const manifest = JSON.parse(read(path));
    manifest.dependencies.react = "99.0.0";
    return JSON.stringify(manifest);
  });
  expect(unrelated.digest).toBe(original.digest);
});

test("a tool upgrade cannot silently use the previous image or mismatched browser base", () => {
  const changedBun = imageInputs((path) =>
    path === ".tool-versions" ? "bun 99.0.0\n" : read(path),
  );
  expect(changedBun.digest).not.toBe(imageInputs().digest);
  expect(() =>
    imageInputs((path) =>
      path === "halo/package.json"
        ? read(path).replace(
            '"@playwright/test": "1.64.1"',
            '"@playwright/test": "99.0.0"',
          )
        : read(path),
    ),
  ).toThrow("base image digests");
});

test("image publishing isolates source execution from package credentials", () => {
  const workflow = Bun.YAML.parse(
    read(".github/workflows/halo-image.yml"),
  ) as any;
  expect(Object.keys(workflow.on)).toEqual(["workflow_dispatch"]);
  expect(workflow.on).not.toHaveProperty("pull_request");
  expect(workflow.on).not.toHaveProperty("merge_group");
  expect(workflow.permissions).toEqual({ contents: "read" });
  expect(workflow.jobs.build.if).toBe(workflow.jobs.publish.if);
  expect(workflow.jobs.publish.permissions).toEqual({
    contents: "read",
    packages: "write",
  });
  const publishing = workflow.jobs.publish.steps
    .map((step: any) => step.run ?? step.uses)
    .join("\n");
  expect(publishing).not.toMatch(/checkout|docker run|bun tools|docker build/);
  expect(publishing).toContain("docker push");
  expect(read(".github/halo-ci/Dockerfile.dockerignore").split("\n")[0]).toBe(
    "**",
  );
  const final = read(".github/halo-ci/Dockerfile").split(
    "FROM environment\n",
  )[1];
  expect(final).toContain(
    "COPY --from=postgresql /opt/nucleus/target/postgresql",
  );
  expect(final).not.toContain("fixture/postgresql.ts");
});

test("image publication requires a dev dispatch and an exact source revision", () => {
  const workflow = Bun.YAML.parse(
    read(".github/workflows/halo-image.yml"),
  ) as any;
  const expression = workflow.jobs.publish.if.slice(3, -2);
  const tokens = new Lexer(expression).lex().tokens;
  const parsed = new Parser(tokens, ["github"], []).parse();
  const bootstrap = {
    event_name: "push",
    ref: "refs/heads/codex/halo-ci-image",
    actor: "headironc",
    run_attempt: "1",
    event: { created: true },
  };
  for (const [change, expected] of [
    [{}, false],
    [{ event: { created: false } }, false],
    [{ event: {} }, false],
    [{ actor: "another-user" }, false],
    [{ ref: "refs/heads/codex/halo-ci-image-copy" }, false],
    [{ ref: "refs/heads/dev" }, false],
    [{ run_attempt: "2" }, false],
    [{ event_name: "pull_request" }, false],
    [{ event_name: "merge_group" }, false],
    [{ event_name: "workflow_dispatch", ref: "refs/heads/dev" }, true],
    [{ event_name: "workflow_dispatch" }, false],
    [{ event_name: "workflow_dispatch", ref: "refs/heads/main" }, false],
  ] as const) {
    const context = JSON.parse(
      JSON.stringify({ github: { ...bootstrap, ...change } }),
      data.reviver,
    ) as data.Dictionary;
    expect(
      truthy(new Evaluator(parsed, context).evaluate()),
      JSON.stringify(change),
    ).toBe(expected);
  }
  const requirement = workflow.jobs.build.steps[0];
  const checkout = workflow.jobs.build.steps.find((step: any) =>
    step.uses?.startsWith("actions/checkout@"),
  );
  expect(requirement.env.REVISION).toBe("${{ inputs.revision }}");
  expect(checkout.with.ref).toBe(requirement.env.REVISION);
  for (const revision of [
    "a".repeat(40),
    "dev",
    "",
    "a".repeat(39),
    "$(exit 0)",
  ]) {
    const result = spawnSync("bash", ["-c", requirement.run], {
      env: { ...process.env, REVISION: revision },
    });
    expect(result.error).toBeUndefined();
    expect(result.status === 0).toBe(revision === "a".repeat(40));
  }
});

test("private package validation accepts absent source-page metadata without accepting a different repository", () => {
  const workflow = Bun.YAML.parse(
    read(".github/workflows/halo-image.yml"),
  ) as any;
  const step = workflow.jobs.publish.steps.find(
    (step: any) => step.name === "Verify private package metadata",
  );
  const condition = step.run.match(/jq --exit-status '([^']+)'/)[1];
  for (const [value, accepted] of [
    [{ visibility: "private", repository: "silence-force/nucleus" }, true],
    [{ visibility: "private", repository: null }, true],
    [{ visibility: "public", repository: null }, false],
    [{ visibility: "private", repository: "another/repository" }, false],
    [{ repository: null }, false],
  ] as const) {
    const result = spawnSync("jq", ["--exit-status", condition], {
      input: JSON.stringify(value),
    });
    expect(result.error).toBeUndefined();
    expect(result.status === 0, JSON.stringify(value)).toBe(accepted);
  }
});
