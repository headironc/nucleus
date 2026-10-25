import { expect, test } from "bun:test";
import { data, Evaluator, Lexer, Parser } from "@actions/expressions";
import { truthy } from "@actions/expressions/result";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
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
  expect(workflow.jobs.publish).not.toHaveProperty("needs");
  expect(workflow.jobs.build).not.toHaveProperty("permissions");
  expect(workflow.jobs.publish.permissions).toEqual({
    contents: "read",
    actions: "read",
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

test("build and publication require separate dev dispatches in the exact repository", () => {
  const workflow = Bun.YAML.parse(
    read(".github/workflows/halo-image.yml"),
  ) as any;
  const bootstrap = {
    event_name: "workflow_dispatch",
    ref: "refs/heads/dev",
    repository_id: "1432619931",
  };
  for (const job of ["build", "publish"]) {
    const tokens = new Lexer(workflow.jobs[job].if.slice(3, -2)).lex().tokens;
    const parsed = new Parser(tokens, ["github", "inputs"], []).parse();
    for (const [change, allowed] of [
      [{}, true],
      [{ event_name: "push" }, false],
      [{ event_name: "pull_request" }, false],
      [{ event_name: "merge_group" }, false],
      [{ ref: "refs/heads/main" }, false],
      [{ ref: "refs/heads/codex/image" }, false],
      [{ repository_id: "1185290396" }, false],
      [{ repository_id: "" }, false],
    ] as const) {
      for (const operation of ["build", "publish", "", "unknown"]) {
        const context = JSON.parse(
          JSON.stringify({
            github: { ...bootstrap, ...change },
            inputs: { operation },
          }),
          data.reviver,
        ) as data.Dictionary;
        expect(
          truthy(new Evaluator(parsed, context).evaluate()),
          JSON.stringify({ job, change, operation }),
        ).toBe(allowed && operation === job);
      }
    }
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

test("published package must belong to the exact new repository and expected visibility", () => {
  const workflow = Bun.YAML.parse(
    read(".github/workflows/halo-image.yml"),
  ) as any;
  const step = workflow.jobs.publish.steps.find(
    (step: any) => step.name === "Verify package identity and visibility",
  );
  const condition = step.run.match(/'(.name ==[^']+)'/)[1];
  const base = {
    name: "nucleus-ci",
    visibility: "private",
    repository_id: 1432619931,
    repository: "silence-force/nucleus",
  };
  for (const [change, visibility, accepted] of [
    [{}, "private", true],
    [{ visibility: "public" }, "public", true],
    [{ visibility: "public" }, "private", false],
    [{}, "public", false],
    [{ repository_id: 1185290396 }, "private", false],
    [{ repository: null, repository_id: null }, "private", false],
    [{ repository: "another/repository" }, "private", false],
    [{ name: "nucleus-halo-ci" }, "private", false],
  ] as const) {
    const result = spawnSync(
      "jq",
      ["--exit-status", "--arg", "visibility", visibility, condition],
      {
        input: JSON.stringify({ ...base, ...change }),
      },
    );
    expect(result.error).toBeUndefined();
    expect(result.status === 0, JSON.stringify(change)).toBe(accepted);
  }
});

test("publication rejects invalid inputs and artifacts outside a successful same-repository build", () => {
  const workflow = Bun.YAML.parse(
    read(".github/workflows/halo-image.yml"),
  ) as any;
  const step = workflow.jobs.publish.steps[0];
  const run = {
    id: 123,
    repository: { id: 1432619931 },
    head_repository: { id: 1432619931 },
    head_branch: "dev",
    path: ".github/workflows/halo-image.yml",
    event: "workflow_dispatch",
    status: "completed",
    conclusion: "success",
  };
  const artifact = {
    id: 456,
    name: "nucleus-ci-image",
    expired: false,
    workflow_run: {
      id: 123,
      repository_id: 1432619931,
      head_repository_id: 1432619931,
      head_branch: "dev",
    },
  };
  const directory = mkdtempSync(join(tmpdir(), "nucleus-image-publication-"));
  const environment = {
    ...process.env,
    RUNNER_TEMP: directory,
    REVISION: "a".repeat(40),
    BUILD_RUN: "123",
    ARTIFACT_ID: "456",
    IMAGE_SHA256: "b".repeat(64),
    VISIBILITY: "private",
  };
  const mock = `gh() {
    case "$4" in
      repos/silence-force/nucleus/actions/runs/123) cat "$RUNNER_TEMP/run-response.json" ;;
      repos/silence-force/nucleus/actions/artifacts/456) cat "$RUNNER_TEMP/artifact-response.json" ;;
      *) return 99 ;;
    esac
  }\n`;
  try {
    for (const [runValue, artifactValue, accepted] of [
      [run, artifact, true],
      [{ ...run, conclusion: "failure" }, artifact, false],
      [{ ...run, status: "in_progress" }, artifact, false],
      [{ ...run, event: "pull_request" }, artifact, false],
      [{ ...run, path: ".github/workflows/another.yml" }, artifact, false],
      [{ ...run, repository: { id: 1185290396 } }, artifact, false],
      [{ ...run, head_repository: { id: 1185290396 } }, artifact, false],
      [{ ...run, head_branch: "main" }, artifact, false],
      [run, { ...artifact, expired: true }, false],
      [run, { ...artifact, id: 999 }, false],
      [run, { ...artifact, name: "another-image" }, false],
      [
        run,
        { ...artifact, workflow_run: { ...artifact.workflow_run, id: 999 } },
        false,
      ],
      [
        run,
        {
          ...artifact,
          workflow_run: { ...artifact.workflow_run, repository_id: 1185290396 },
        },
        false,
      ],
      [run, { ...artifact, workflow_run: null }, false],
    ] as const) {
      writeFileSync(
        join(directory, "run-response.json"),
        JSON.stringify(runValue),
      );
      writeFileSync(
        join(directory, "artifact-response.json"),
        JSON.stringify(artifactValue),
      );
      const result = spawnSync("bash", ["-c", mock + step.run], {
        env: environment,
        encoding: "utf8",
      });
      expect(result.error).toBeUndefined();
      expect(result.status === 0, result.stderr).toBe(accepted);
    }
    for (const change of [
      { REVISION: "dev" },
      { BUILD_RUN: "123; exit 0" },
      { ARTIFACT_ID: "456,789" },
      { IMAGE_SHA256: "b".repeat(63) },
      { VISIBILITY: "internal" },
    ]) {
      const result = spawnSync(
        "bash",
        ["-c", "gh() { echo UNEXPECTED_API; return 99; }\n" + step.run],
        { env: { ...environment, ...change }, encoding: "utf8" },
      );
      expect(result.error).toBeUndefined();
      expect(result.status).not.toBe(0);
      expect(result.stdout).not.toContain("UNEXPECTED_API");
    }
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test("publication verifies the inspected archive and provenance before Docker receives it", () => {
  const workflow = Bun.YAML.parse(
    read(".github/workflows/halo-image.yml"),
  ) as any;
  const steps = workflow.jobs.publish.steps;
  const verify = steps.findIndex(
    (step: any) => step.name === "Verify the inspected bytes before loading",
  );
  expect(verify).toBeLessThan(
    steps.findIndex((step: any) => step.run?.includes("docker load")),
  );
  const download = steps.find((step: any) =>
    step.uses?.startsWith("actions/download-artifact@"),
  );
  expect(download.with["artifact-ids"]).toBe("${{ inputs.artifact-id }}");
  expect(download.with["run-id"]).toBe("${{ inputs.build-run }}");
  expect(download.with.repository).toBe("silence-force/nucleus");
  const directory = mkdtempSync(join(tmpdir(), "nucleus-image-bytes-"));
  const artifact = join(directory, "nucleus-ci");
  mkdirSync(artifact);
  const bytes = "synthetic image archive";
  const sha = createHash("sha256").update(bytes).digest("hex");
  const provenance = {
    revision: "a".repeat(40),
    run: "123",
    image_sha256: sha,
  };
  try {
    for (const [image, source, accepted] of [
      [bytes, provenance, true],
      [bytes + "changed", provenance, false],
      [bytes, { ...provenance, revision: "b".repeat(40) }, false],
      [bytes, { ...provenance, run: "124" }, false],
      [bytes, { ...provenance, image_sha256: "c".repeat(64) }, false],
    ] as const) {
      writeFileSync(join(artifact, "nucleus-ci.tar"), image);
      writeFileSync(join(artifact, "provenance.json"), JSON.stringify(source));
      const result = spawnSync("bash", ["-c", steps[verify].run], {
        env: {
          ...process.env,
          RUNNER_TEMP: directory,
          REVISION: provenance.revision,
          BUILD_RUN: "123",
          IMAGE_SHA256: sha,
        },
        encoding: "utf8",
      });
      expect(result.error).toBeUndefined();
      expect(result.status === 0, result.stderr).toBe(accepted);
    }
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test("the build exports provenance that the publisher accepts without rewriting it", () => {
  const workflow = Bun.YAML.parse(
    read(".github/workflows/halo-image.yml"),
  ) as any;
  const exportStep = workflow.jobs.build.steps.find(
    (step: any) => step.name === "Export the complete image for inspection",
  );
  const verifyStep = workflow.jobs.publish.steps.find(
    (step: any) => step.name === "Verify the inspected bytes before loading",
  );
  const directory = mkdtempSync(join(tmpdir(), "nucleus-image-export-"));
  const bytes = "synthetic image archive";
  const sha = createHash("sha256").update(bytes).digest("hex");
  const env = {
    ...process.env,
    RUNNER_TEMP: directory,
    REVISION: "d".repeat(40),
    GITHUB_RUN_ID: "123",
    GITHUB_RUN_ATTEMPT: "2",
    GITHUB_STEP_SUMMARY: join(directory, "summary"),
    BUILD_RUN: "123",
    IMAGE_SHA256: sha,
  };
  const producers = `git() { printf '%s\\n' "$REVISION"; }
docker() {
  case "$1 $2" in
    'save halo-ci:build') printf 'synthetic image archive' > "$4" ;;
    'image inspect') printf '[]\\n' ;;
    'image history') printf '{}\\n' ;;
    *) return 99 ;;
  esac
}\n`;
  try {
    const build = spawnSync("bash", ["-c", producers + exportStep.run], {
      env,
      encoding: "utf8",
    });
    expect(build.error).toBeUndefined();
    expect(build.status, build.stderr).toBe(0);
    expect(
      JSON.parse(
        readFileSync(join(directory, "nucleus-ci/provenance.json"), "utf8"),
      ),
    ).toEqual({
      revision: env.REVISION,
      image_sha256: sha,
      run: "123",
      attempt: "2",
    });
    const verify = spawnSync("bash", ["-c", verifyStep.run], {
      env,
      encoding: "utf8",
    });
    expect(verify.error).toBeUndefined();
    expect(verify.status, verify.stderr).toBe(0);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test("published manifest must retain the inspected image configuration", () => {
  const workflow = Bun.YAML.parse(
    read(".github/workflows/halo-image.yml"),
  ) as any;
  const step = workflow.jobs.publish.steps.find(
    (step: any) => step.name === "Publish the reviewed dependency image",
  );
  const condition = step.run.match(/'(select\(\(.SchemaV2Manifest[^']+)'/)[1];
  const imageId = `sha256:${"a".repeat(64)}`;
  const manifestDigest = `sha256:${"b".repeat(64)}`;
  for (const [format, configuration, digest, accepted] of [
    ["SchemaV2Manifest", imageId, manifestDigest, true],
    ["OCIManifest", imageId, manifestDigest, true],
    ["SchemaV2Manifest", `sha256:${"c".repeat(64)}`, manifestDigest, false],
    ["OCIManifest", `sha256:${"c".repeat(64)}`, manifestDigest, false],
    ["SchemaV2Manifest", null, manifestDigest, false],
    ["SchemaV2Manifest", imageId, "latest", false],
  ] as const) {
    const result = spawnSync(
      "jq",
      [
        "--exit-status",
        "--raw-output",
        "--arg",
        "image_id",
        imageId,
        condition,
      ],
      {
        input: JSON.stringify({
          [format]: { config: { digest: configuration } },
          Descriptor: { digest },
        }),
        encoding: "utf8",
      },
    );
    expect(result.error).toBeUndefined();
    expect(result.status === 0).toBe(accepted);
    if (accepted) expect(result.stdout.trim()).toBe(manifestDigest);
  }
});
