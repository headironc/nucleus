import { afterEach, expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import {
  copyFileSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";

const environment = {
  ...Object.fromEntries(
    Object.entries(process.env).filter(
      ([key]) =>
        !key.startsWith("GIT_") &&
        ![
          "BASE_SHA",
          "HEAD_SHA",
          "QUEUE_SHA",
          "MERGE_SHA",
          "EVENT_NAME",
          "GITHUB_OUTPUT",
        ].includes(key),
    ),
  ),
  GIT_CONFIG_NOSYSTEM: "1",
  GIT_CONFIG_GLOBAL: "/dev/null",
  GIT_AUTHOR_NAME: "Nucleus fixture",
  GIT_AUTHOR_EMAIL: "fixture@example.invalid",
  GIT_COMMITTER_NAME: "Nucleus fixture",
  GIT_COMMITTER_EMAIL: "fixture@example.invalid",
};
const directories: string[] = [];
afterEach(() => {
  for (const directory of directories.splice(0))
    rmSync(directory, { recursive: true, force: true });
});

function repository() {
  const directory = mkdtempSync(join(tmpdir(), "nucleus input "));
  directories.push(directory);
  function git(...args: string[]) {
    const result = spawnSync(
      "git",
      ["-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null", ...args],
      {
        cwd: directory,
        env: environment,
        encoding: "utf8",
        timeout: 5_000,
      },
    );
    expect(result.error).toBeUndefined();
    expect(result.status, result.stderr).toBe(0);
    return result.stdout.trim();
  }
  function write(path: string, content = "changed\n") {
    mkdirSync(dirname(join(directory, path)), { recursive: true });
    writeFileSync(join(directory, path), content);
  }
  git("init", "--initial-branch=base", "--object-format=sha1", "--template=");
  write(".gitignore", "ignored/\n");
  write("docs/guide.md");
  write("halo/README.md");
  write("design/assets/logo.png");
  write("design/page.html");
  write("tools/placeholder.txt");
  write("src/lib.rs");
  write("domain/src/lib.rs");
  write("platform/src/lib.rs");
  write("deployment/roles.sql");
  write("assets/avatars/0-cat.webp");
  copyFileSync(
    new URL("./check-inputs.ts", import.meta.url),
    join(directory, "tools/check-inputs.ts"),
  );
  git("add", ".");
  git("commit", "-m", "Create fixture");
  const base = git("rev-parse", "HEAD");
  git("switch", "-c", "feature");
  function candidate(queue = false) {
    git("add", ".");
    git("commit", "--allow-empty", "-m", "Change fixture");
    const head = git("rev-parse", "HEAD");
    git("switch", "base");
    git(
      "merge",
      ...(queue ? ["--squash"] : ["--no-ff", "--no-edit"]),
      "feature",
    );
    if (queue) git("commit", "--allow-empty", "-m", "Create queue candidate");
    const merge = git("rev-parse", "HEAD");
    return {
      EVENT_NAME: queue ? "merge_group" : "pull_request",
      BASE_SHA: base,
      HEAD_SHA: head,
      QUEUE_SHA: queue ? merge : "",
      MERGE_SHA: merge,
    };
  }
  return { directory, git, write, base, candidate };
}

const workflows = Object.fromEntries(
  ["halo", "rust"].map((name) => [
    name,
    Bun.YAML.parse(
      readFileSync(
        new URL(`./fixtures/retired-workflows/${name}.yml`, import.meta.url),
        "utf8",
      ),
    ),
  ]),
) as Record<
  string,
  {
    on: Record<string, unknown>;
    jobs: Record<
      string,
      {
        if?: string;
        "continue-on-error"?: boolean;
        steps: {
          id?: string;
          if?: string;
          run?: string;
          uses?: string;
          env?: Record<string, string>;
          with?: Record<string, unknown>;
          "continue-on-error"?: boolean;
        }[];
      }
    >;
  }
>;

function classify(
  fixture: ReturnType<typeof repository>,
  revisions: NodeJS.ProcessEnv,
  workflow = "halo",
  extra: NodeJS.ProcessEnv = {},
) {
  const output = join(fixture.directory, ".git/classification");
  writeFileSync(output, "");
  const steps =
    workflows[workflow].jobs[workflow === "halo" ? "website" : "changes"].steps;
  const command = steps.find((step) => step.id === "changes")!.run!;
  const result = spawnSync("bash", ["--noprofile", "--norc", "-c", command], {
    cwd: fixture.directory,
    env: { ...environment, ...revisions, GITHUB_OUTPUT: output, ...extra },
    encoding: "utf8",
    timeout: 5_000,
  });
  expect(result.error).toBeUndefined();
  return { ...result, output: readFileSync(output, "utf8") };
}

test.each([
  ["src/identity/new.rs", ["nucleus"]],
  ["tests/http/new.rs", ["nucleus"]],
  ["domain/src/lib.rs", ["domain"]],
  ["platform/src/lib.rs", ["platform"]],
  ["deployment/src/main.rs", ["deployment"]],
  ["fixture/src/supplier.rs", ["fixture"]],
  ["fixture/postgresql.ts", ["fixture"]],
  ["fixture/postgresql.version", ["fixture"]],
  ["deployment/roles.sql", ["deployment", "domain"]],
  ["deployment/prepare.sql", ["deployment", "domain"]],
  ["deployment/migrations/V13__new.sql", ["deployment", "domain"]],
  ["assets/avatars/0-cat.webp", ["domain"]],
  ["assets/avatars/9-koala.webp", ["domain"]],
  ["Cargo.toml", "all"],
  ["domain/Cargo.toml", "all"],
  ["src/Cargo.lock", "all"],
  ["rust-toolchain.toml", "all"],
  [".cargo/config.toml", "all"],
  ["domain/src/build.rs", "all"],
  [".github/workflows/rust.yml", "all"],
  [".github/workflows/cache.yml", "all"],
  [".github/actions/package-setup/action.yml", "all"],
  ["tools/check-packages.ts", "all"],
  ["assets/avatars/10-new.webp", "all"],
  ["deployment/migrations/README.txt", "all"],
  ["deployment/nucleus.service", "all"],
  ["domain/new-input.txt", "all"],
  ["unknown.txt", "all"],
] as const)(
  "Rust roots follow actual input owners in PRs and queues: %s",
  (path, roots) => {
    for (const queue of [false, true]) {
      const fixture = repository();
      fixture.write(path, "updated\n");
      const result = classify(fixture, fixture.candidate(queue), "rust");
      expect(result.status, result.stderr).toBe(0);
      expect(result.output).toContain("tier=full\n");
      expect(result.output).toContain(
        `packages=${roots === "all" ? "all" : JSON.stringify(roots)}\n`,
      );
    }
  },
);

test("Rust roots union mixed inputs while unknown inputs retain every package", () => {
  for (const unknown of [false, true]) {
    const fixture = repository();
    fixture.write("domain/src/lib.rs", "updated domain\n");
    fixture.write("src/lib.rs", "updated application\n");
    fixture.write("deployment/src/lib.rs");
    fixture.write("tools/check-bun.ts");
    fixture.write("docs/guide.md", "updated guide\n");
    if (unknown) fixture.write("new-runtime-file");
    const result = classify(fixture, fixture.candidate(), "rust");
    expect(result.status, result.stderr).toBe(0);
    expect(result.output).toContain(
      `packages=${unknown ? "all" : '["deployment","domain","nucleus"]'}\n`,
    );
  }
  for (const path of ["docs/guide.md", "tools/check-bun.ts"]) {
    const fixture = repository();
    fixture.write(path, "updated\n");
    const result = classify(fixture, fixture.candidate(), "rust");
    expect(result.status, result.stderr).toBe(0);
    expect(result.output).toContain("packages=[]\n");
  }
});

test("Rust selection includes deletions and both owners of renamed paths", () => {
  for (const queue of [false, true]) {
    for (const [source, destination, expected] of [
      ["domain/src/lib.rs", "", ["domain"]],
      ["deployment/roles.sql", "", ["deployment", "domain"]],
      ["assets/avatars/0-cat.webp", "docs/cat.md", ["domain"]],
      ["domain/src/lib.rs", "platform/src/moved.rs", ["domain", "platform"]],
      ["domain/src/lib.rs", "docs/source.md", ["domain"]],
      ["docs/guide.md", "src/moved.rs", ["nucleus"]],
    ] as const) {
      const fixture = repository();
      if (destination)
        renameSync(
          join(fixture.directory, source),
          join(fixture.directory, destination),
        );
      else rmSync(join(fixture.directory, source));
      const result = classify(fixture, fixture.candidate(queue), "rust");
      expect(result.status, result.stderr).toBe(0);
      expect(result.output).toContain(`packages=${JSON.stringify(expected)}\n`);
    }
  }
});

test.each([
  "halo/app/routes/codes.tsx",
  "halo/README.md",
  "halo/bun.lock",
  "design/assets/tokens.css",
  "design/assets/logo.png",
  "design/assets/halo-botanical.svg",
  ".gitignore",
  ".tool-versions",
  "package.json",
  "bun.lock",
  "tsconfig.json",
  "src/lib.rs",
  "tests/http/keys.rs",
  "domain/src/lib.rs",
  "platform/src/lib.rs",
  "fixture/postgresql.ts",
  "deployment/migrations/V1__keys.sql",
  "assets/avatars/cat.webp",
  "Cargo.toml",
  "Cargo.lock",
  "rust-toolchain.toml",
  ".github/workflows/halo.yml",
  ".github/workflows/halo-image.yml",
  ".github/halo-ci/Dockerfile",
  ".github/halo-ci/Dockerfile.dockerignore",
  "tools/halo-ci-image.ts",
  "tools/halo-ci-image.test.ts",
  "tools/check-bun.ts",
  "tools/check-inputs.ts",
  "tools/check-inputs.test.ts",
  "tools/check-halo-test.ts",
  "tools/check-halo-test.test.ts",
  "tools/new-tool.ts",
  "unknown.txt",
  "client/protocol/response.ts",
  "docs/input.json",
  "design/assets/client.ts",
  "design/package.json",
])("Halo runs for a consumer input or unknown path: %s", (path) => {
  const fixture = repository();
  // Keep the actual classifier executable when its own input is changed.
  fixture.write(
    path,
    path === "tools/check-inputs.ts"
      ? readFileSync(new URL("./check-inputs.ts", import.meta.url), "utf8") +
          "\n"
      : "updated\n",
  );
  const result = classify(fixture, fixture.candidate());
  expect(result.status, result.stderr).toBe(0);
  expect(result.output).toContain("halo=run\n");
  if (path === "tools/check-inputs.ts")
    expect(result.output).toContain("tier=full\n");
});

test.each([
  "README.md",
  "AGENTS.md",
  "docs/guide.md",
  "docs/nested/space ' and\nnewline.md",
  "design/page.html",
  "design/assets/orbit.css",
  "design/assets/halo-home.js",
  "design/assets/image.png",
  "design/assets/image.svg",
  "design/README.md",
  "design/LICENSE.txt",
  ".prettierrc.json",
  ".markdownlint.jsonc",
  "rustfmt.toml",
  "tools/check-bun.test.ts",
  "tools/check-dependencies.ts",
  "tools/check-packages.ts",
  "tools/preview-design.ts",
  "tools/review-gate.ts",
  "tools/review-queue.test.ts",
  ".github/workflows/docs.yml",
  ".github/workflows/rust.yml",
  ".github/workflows/review-gate.yml",
])("Halo skips only verified unrelated inputs: %s", (path) => {
  const fixture = repository();
  fixture.write(path);
  const result = classify(fixture, fixture.candidate());
  expect(result.status, result.stderr).toBe(0);
  expect(result.output).toContain("halo=skip\n");
});

test("PR and queue comparisons include earlier commits, deletions and both rename sides", () => {
  for (const queue of [false, true]) {
    for (const operation of [
      "earlier",
      "delete",
      "rename-from",
      "rename-to",
      "odd-name",
    ]) {
      const fixture = repository();
      if (operation === "earlier") {
        fixture.write("halo/earlier.ts");
        fixture.git("add", ".");
        fixture.git("commit", "-m", "Add earlier input");
      } else if (operation === "delete") {
        rmSync(join(fixture.directory, "design/assets/logo.png"));
      } else if (operation === "rename-from") {
        renameSync(
          join(fixture.directory, "halo/README.md"),
          join(fixture.directory, "docs/moved.md"),
        );
      } else if (operation === "rename-to") {
        renameSync(
          join(fixture.directory, "docs/guide.md"),
          join(fixture.directory, "halo/moved.md"),
        );
      } else fixture.write("halo/space ' and\nnewline.md");
      fixture.write("docs/guide.md", "later documentation\n");
      const result = classify(fixture, fixture.candidate(queue));
      expect(result.status, result.stderr).toBe(0);
      expect(result.output).toContain("halo=run\n");
    }
  }
});

test.each([
  [".github/halo-ci/Dockerfile", "full", "all"],
  [".github/halo-ci/Dockerfile.dockerignore", "full", "all"],
  [".github/workflows/halo-image.yml", "full", "all"],
  ["tools/halo-ci-image.ts", "tools", "[]"],
  ["tools/check-bun.ts", "tools", "[]"],
] as const)("PR and queue candidates retain the Halo image consumer: %s", (path, tier, packages) => {
  for (const queue of [false, true]) {
    const fixture = repository();
    fixture.write(path);
    const result = classify(fixture, fixture.candidate(queue));
    expect(result.status, result.stderr).toBe(0);
    expect(result.output).toBe(`tier=${tier}\nhalo=run\npackages=${packages}\n`);
  }
});

test("both workflow commands fail closed on unknown or unbound events", () => {
  const fixture = repository();
  fixture.write("docs/guide.md");
  const revisions = fixture.candidate();
  for (const workflow of ["halo", "rust"]) {
    for (const override of [
      { EVENT_NAME: "" },
      { EVENT_NAME: "push" },
      { HEAD_SHA: revisions.BASE_SHA },
      { MERGE_SHA: revisions.HEAD_SHA },
    ]) {
      const result = classify(fixture, { ...revisions, ...override }, workflow);
      expect(result.status).toBe(0);
      expect(result.output).toBe("tier=full\nhalo=run\npackages=all\n");
      expect(result.stderr).toContain("Unverified comparison");
    }
  }
});

test.each(["docs", "nucleus", "merge-only", "delete", "rename"])(
  "depth-two PR classification uses the actual first parent after the target advances: %s",
  (change) => {
    const fixture = repository();
    fixture.git("switch", "base");
    fixture.write("domain/src/lib.rs", "target-only Rust change\n");
    fixture.git("add", ".");
    fixture.git("commit", "-m", "Advance target beyond the event baseline");
    const target = fixture.git("rev-parse", "HEAD");
    fixture.git("switch", "feature");
    fixture.write(
      change === "nucleus" ? "tests/http/new.rs" : "docs/guide.md",
      "PR change\n",
    );
    const revisions = fixture.candidate();
    if (change === "merge-only") fixture.write("src/merge.rs");
    else if (change === "delete") rmSync(join(fixture.directory, "src/lib.rs"));
    else if (change === "rename")
      renameSync(
        join(fixture.directory, "domain/src/lib.rs"),
        join(fixture.directory, "src/moved.rs"),
      );
    if (!["docs", "nucleus"].includes(change)) {
      fixture.git("add", ".");
      fixture.git("commit", "--amend", "--no-edit");
      revisions.MERGE_SHA = fixture.git("rev-parse", "HEAD");
    }
    const shallow = mkdtempSync(join(tmpdir(), "nucleus advanced input "));
    directories.push(shallow);
    fixture.git("clone", "--no-local", "--depth=2", fixture.directory, shallow);
    const missingBase = spawnSync("git", ["cat-file", "-e", fixture.base], {
      cwd: shallow,
      env: environment,
    });
    expect(missingBase.error).toBeUndefined();
    expect(missingBase.status).toBe(1);
    for (const available of [target, revisions.HEAD_SHA, revisions.MERGE_SHA])
      expect(fixture.git("-C", shallow, "cat-file", "-t", available)).toBe(
        "commit",
      );
    const expected =
      change === "docs"
        ? "tier=skip\nhalo=skip\npackages=[]\n"
        : `tier=full\nhalo=run\npackages=${JSON.stringify(change === "rename" ? ["domain", "nucleus"] : ["nucleus"])}\n`;
    for (const workflow of ["halo", "rust"]) {
      const result = classify(
        { ...fixture, directory: shallow },
        revisions,
        workflow,
      );
      expect(result.status, result.stderr).toBe(0);
      expect(result.output).toBe(expected);
      expect(result.stderr).not.toContain("Unverified comparison");
    }
  },
);

test("a bound PR candidate is independent of the old event baseline", () => {
  const fixture = repository();
  fixture.write("docs/guide.md");
  const revisions = fixture.candidate();
  for (const base of ["", "missing", "--help", revisions.HEAD_SHA]) {
    const result = classify(fixture, { ...revisions, BASE_SHA: base });
    expect(result.status, result.stderr).toBe(0);
    expect(result.output).toBe("tier=skip\nhalo=skip\npackages=[]\n");
  }
});

test("a PR requires exact existing head and merge commits", () => {
  const fixture = repository();
  const revisions = fixture.candidate();
  for (const variable of ["HEAD_SHA", "MERGE_SHA"] as const) {
    for (const value of [
      "",
      "HEAD",
      "--help",
      "missing",
      "0".repeat(40),
      revisions[variable].slice(0, 12),
    ]) {
      const result = classify(fixture, { ...revisions, [variable]: value });
      expect(result.status).toBe(0);
      expect(result.output).toBe("tier=full\nhalo=run\npackages=all\n");
    }
  }
});

test("a PR requires exactly its two event-bound parents", () => {
  for (const shape of ["root", "single", "three"]) {
    const fixture = repository();
    fixture.write("docs/guide.md", "updated\n");
    const revisions = fixture.candidate();
    let candidate = revisions.HEAD_SHA;
    if (shape === "root")
      candidate = fixture.git(
        "commit-tree",
        fixture.git("rev-parse", "HEAD^{tree}"),
        "-m",
        "Create root candidate",
      );
    if (shape === "three") {
      const tree = fixture.git("rev-parse", "HEAD^{tree}");
      const third = fixture.git(
        "commit-tree",
        tree,
        "-m",
        "Create extra parent",
      );
      candidate = fixture.git(
        "commit-tree",
        tree,
        "-p",
        revisions.BASE_SHA,
        "-p",
        revisions.HEAD_SHA,
        "-p",
        third,
        "-m",
        "Create unsupported candidate",
      );
    }
    fixture.git("reset", "--hard", candidate);
    const result = classify(fixture, { ...revisions, MERGE_SHA: candidate });
    expect(result.status).toBe(0);
    expect(result.output).toBe("tier=full\nhalo=run\npackages=all\n");
  }
});

test("a PR rejects a missing first or second parent commit", () => {
  for (const parent of ["BASE_SHA", "HEAD_SHA"] as const) {
    const fixture = repository();
    const revisions = fixture.candidate();
    const id = revisions[parent];
    rmSync(join(fixture.directory, ".git/objects", id.slice(0, 2), id.slice(2)));
    const result = classify(fixture, revisions);
    expect(result.status).toBe(0);
    expect(result.output).toBe("tier=full\nhalo=run\npackages=all\n");
    expect(result.stderr).toContain("Unverified comparison");
  }
});

test("Halo also fails closed for unavailable history and diff trees", () => {
  for (const queue of [false, true]) {
    const fixture = repository();
    fixture.write("docs/guide.md", "updated\n");
    const revisions = fixture.candidate(queue);
    for (const depth of [1, 2]) {
      const shallow = mkdtempSync(join(tmpdir(), "nucleus shallow input "));
      directories.push(shallow);
      fixture.git(
        "clone",
        "--no-local",
        `--depth=${depth}`,
        fixture.directory,
        shallow,
      );
      const result = classify({ ...fixture, directory: shallow }, revisions);
      expect(result.status, result.stderr).toBe(0);
      expect(result.output).toBe(
        depth === 1
          ? "tier=full\nhalo=run\npackages=all\n"
          : "tier=skip\nhalo=skip\npackages=[]\n",
      );
    }
    const tree = fixture.git("rev-parse", `${revisions.BASE_SHA}^{tree}`);
    rmSync(
      join(fixture.directory, ".git/objects", tree.slice(0, 2), tree.slice(2)),
    );
    const result = classify(fixture, revisions);
    expect(result.status).toBe(0);
    expect(result.output).toBe("tier=full\nhalo=run\npackages=all\n");
  }
});

test("undecodable Git filenames conservatively run every check", () => {
  const fixture = repository();
  writeFileSync(
    Buffer.concat([
      Buffer.from(`${fixture.directory}/docs/`),
      Buffer.from([0xff]),
      Buffer.from(".md"),
    ]),
    "changed\n",
  );
  const result = classify(fixture, fixture.candidate());
  expect(result.status).toBe(0);
  expect(result.output).toBe("tier=full\nhalo=run\npackages=all\n");
});

test.each([
  "docs/guide.md\n",
  "design/page.html\n",
  "README.md\n",
  "\uFEFFREADME.md",
  "docs/input.MD",
])("only a complete known suffix is exempt: %s", (path) => {
  const fixture = repository();
  fixture.write(path);
  const result = classify(fixture, fixture.candidate());
  expect(result.status).toBe(0);
  expect(result.output).toBe("tier=full\nhalo=run\npackages=all\n");
});

test("Git environment overrides cannot select a different repository or hide changes", () => {
  const fixture = repository();
  fixture.write("halo/changed.ts");
  const revisions = fixture.candidate();
  const unrelated = repository();
  const result = classify(fixture, revisions, "halo", {
    GIT_DIR: join(unrelated.directory, ".git"),
    GIT_WORK_TREE: unrelated.directory,
    GIT_INDEX_FILE: "/missing/index",
    GIT_CONFIG_COUNT: "1",
    GIT_CONFIG_KEY_0: "diff.external",
    GIT_CONFIG_VALUE_0: "/missing/program",
    GIT_OBJECT_DIRECTORY: "/missing/objects",
  });
  expect(result.status, result.stderr).toBe(0);
  expect(result.output).toContain("halo=run\n");
});

function local(
  fixture: ReturnType<typeof repository>,
  args = ["--base", "base"],
  cwd = fixture.directory,
) {
  return spawnSync(
    process.execPath,
    [join(fixture.directory, "tools/check-inputs.ts"), ...args],
    {
      cwd,
      env: environment,
      encoding: "utf8",
      timeout: 5_000,
    },
  );
}

test.each([
  [".github/workflows/review-gate.yml", "tools", "skip"],
  [".github/workflows/review-context.yml", "tools", "skip"],
  [".github/workflows/review-activity.yml", "tools", "skip"],
  [".github/workflows/new-review.yml", "full", "run"],
  [".github/workflows/halo-image.yml", "full", "run"],
  [".github/halo-ci/Dockerfile", "full", "run"],
  [".github/halo-ci/Dockerfile.dockerignore", "full", "run"],
  ["tools/halo-ci-image.ts", "tools", "run"],
  ["tools/halo-ci-image.test.ts", "tools", "run"],
  ["tools/check-bun.ts", "tools", "run"],
] as const)("local selection classifies the workflow's actual consumers: %s", (path, tier, halo) => {
  const fixture = repository();
  fixture.write(path);
  const result = local(fixture);
  expect(result.status, result.stderr).toBe(0);
  expect(result.stdout).toContain(`Rust validation tier: ${tier}\n`);
  expect(result.stdout).toContain(`Halo validation: ${halo}\n`);
  fixture.write("src/lib.rs", "changed consumer source\n");
  const mixed = local(fixture);
  expect(mixed.status, mixed.stderr).toBe(0);
  expect(mixed.stdout).toContain("Rust validation tier: full\n");
  expect(mixed.stdout).toContain("Halo validation: run\n");
});

test("local selection covers committed, staged, unstaged and untracked inputs", () => {
  for (const state of [
    "committed",
    "staged",
    "unstaged",
    "untracked",
    "tracked-ignored",
  ]) {
    const fixture = repository();
    if (state === "unstaged") fixture.write("halo/README.md", "updated\n");
    else if (state === "tracked-ignored") {
      fixture.write("ignored/input.ts");
      fixture.git("add", "--force", "ignored/input.ts");
    } else {
      fixture.write("halo/new.ts");
      if (state !== "untracked") fixture.git("add", ".");
      if (state === "committed")
        fixture.git("commit", "-m", "Commit Halo input");
    }
    const result = local(fixture);
    expect(result.status, result.stderr).toBe(0);
    expect(result.stdout).toContain("Halo validation: run");
  }
});

test("local Rust roots cover every worktree layer and report the consumer-aware checker command", () => {
  const fixture = repository();
  fixture.write("domain/src/lib.rs", "committed\n");
  fixture.git("add", ".");
  fixture.git("commit", "-m", "Commit domain input");
  fixture.write("platform/src/lib.rs", "staged\n");
  fixture.git("add", ".");
  fixture.write("src/lib.rs", "unstaged\n");
  fixture.write("fixture/src/new.rs");
  const result = local(fixture);
  expect(result.status, result.stderr).toBe(0);
  expect(result.stdout).toContain(
    'Rust affected package roots: ["domain","fixture","nucleus","platform"]',
  );
  expect(result.stdout).toContain(
    `bun tools/check-packages.ts --affected '["domain","fixture","nucleus","platform"]'`,
  );
  const failure = local(fixture, ["--base", "missing"]);
  expect(failure.status).toBe(0);
  expect(failure.stdout).toContain("Rust affected package roots: all");
  expect(failure.stdout).toContain(
    "bun tools/check-packages.ts --affected 'all'",
  );
});

test("local selection uses the merge base and ignores only nontracked ignored files", () => {
  const fixture = repository();
  fixture.write("docs/guide.md", "updated\n");
  fixture.write("ignored/input.ts");
  expect(local(fixture).stdout).toContain("Halo validation: skip");
  fixture.git("switch", "base");
  fixture.write("halo/README.md", "target change\n");
  fixture.git("add", ".");
  fixture.git("commit", "-m", "Advance target");
  fixture.git("switch", "feature");
  expect(local(fixture).stdout).toContain("Halo validation: skip");
  fixture.write("halo/README.md", "feature change\n");
  expect(local(fixture).stdout).toContain("Halo validation: run");
});

test("local staged and unstaged changes cannot cancel each other out", () => {
  for (const operation of ["restore", "delete", "rename"]) {
    const fixture = repository();
    const original = readFileSync(
      join(fixture.directory, "halo/README.md"),
      "utf8",
    );
    if (operation === "restore")
      fixture.write("halo/README.md", "staged change\n");
    else if (operation === "delete")
      rmSync(join(fixture.directory, "halo/README.md"));
    else
      renameSync(
        join(fixture.directory, "halo/README.md"),
        join(fixture.directory, "docs/moved.md"),
      );
    fixture.git("add", ".");
    fixture.write("halo/README.md", original);
    if (operation === "rename")
      rmSync(join(fixture.directory, "docs/moved.md"));
    const result = local(fixture);
    expect(result.status, result.stderr).toBe(0);
    expect(result.stdout).toContain("Halo validation: run");
  }
});

test("local selection preserves tier precedence and package roots across input states", () => {
  for (const [staged, unstaged, tier, packages] of [
    ["tools/check-bun.ts", "docs/guide.md", "tools", "[]"],
    ["rustfmt.toml", "tools/check-bun.ts", "checks", "[]"],
    ["tools/check-bun.ts", "rustfmt.toml", "checks", "[]"],
    ["tools/check-bun.ts", "src/lib.rs", "full", '["nucleus"]'],
    ["src/lib.rs", "tools/check-bun.ts", "full", '["nucleus"]'],
    ["domain/src/lib.rs", "src/lib.rs", "full", '["domain","nucleus"]'],
    ["domain/src/lib.rs", "unknown.txt", "full", "all"],
  ]) {
    const fixture = repository();
    fixture.write(staged, "staged change\n");
    fixture.git("add", ".");
    fixture.write(unstaged, "unstaged change\n");
    const result = local(fixture);
    expect(result.status, result.stderr).toBe(0);
    expect(result.stdout).toContain(`Rust validation tier: ${tier}\n`);
    expect(result.stdout).toContain(
      `Rust affected package roots: ${packages}\n`,
    );
  }
});

test("local comparison runs all checks for multiple best merge bases", () => {
  const fixture = repository();
  const tree = fixture.git("rev-parse", "HEAD^{tree}");
  const left = fixture.git(
    "commit-tree",
    tree,
    "-p",
    fixture.base,
    "-m",
    "Left history",
  );
  const right = fixture.git(
    "commit-tree",
    tree,
    "-p",
    fixture.base,
    "-m",
    "Right history",
  );
  const candidate = fixture.git(
    "commit-tree",
    tree,
    "-p",
    left,
    "-p",
    right,
    "-m",
    "Candidate merge",
  );
  const target = fixture.git(
    "commit-tree",
    tree,
    "-p",
    right,
    "-p",
    left,
    "-m",
    "Target merge",
  );
  fixture.git("reset", "--hard", candidate);
  expect(
    new Set(fixture.git("merge-base", "--all", target, candidate).split("\n")),
  ).toEqual(new Set([left, right]));
  const result = local(fixture, ["--base", target]);
  expect(result.status).toBe(0);
  expect(result.stdout).toContain(
    "Rust validation tier: full\nHalo validation: run",
  );
  expect(result.stderr).toContain("one merge base");
});

test("local unavailable baselines, wrong cwd and interface errors are explicit", () => {
  const fixture = repository();
  for (const base of ["missing", "--help"]) {
    const result = local(fixture, ["--base", base]);
    expect(result.status).toBe(0);
    expect(result.stdout).toContain("Halo validation: run");
    expect(result.stderr).toContain("Unverified comparison");
  }
  expect(
    local(fixture, ["--base", "base"], join(fixture.directory, "docs")).stdout,
  ).toContain("Halo validation: run");
  for (const args of [[], ["--base"], ["--unknown"], ["--ci"]]) {
    const result = local(fixture, args);
    expect(result.status).toBe(1);
    if (args[0] === "--ci") {
      expect(result.stderr).toContain("--ci requires GITHUB_OUTPUT");
      expect(result.stdout).toBe("");
    }
  }
});

test("Halo keeps a stable job and guards every heavy step with a verified skip", () => {
  const workflow = workflows.halo;
  expect(workflow.on).toEqual({
    pull_request: null,
    merge_group: { types: ["checks_requested"] },
  });
  expect(Object.keys(workflow.jobs)).toEqual(["website"]);
  const website = workflow.jobs.website;
  expect(website.if).toBeUndefined();
  expect(website["continue-on-error"]).not.toBe(true);
  const classification = website.steps.findIndex(
    (step) => step.id === "changes",
  );
  expect(classification).toBe(2);
  expect(website.steps[0].with?.["fetch-depth"]).toBe(2);
  expect(website.steps[0].with?.["persist-credentials"]).toBe(false);
  expect(website.steps[classification].env).toEqual(
    workflows.rust.jobs.changes.steps.find((step) => step.id === "changes")!
      .env,
  );
  const evidenceUpload = website.steps.find((step) =>
    step.uses?.startsWith("actions/upload-artifact@"),
  );
  for (const step of website.steps.slice(classification + 1)) {
    expect(step.if).toBe(
      step === evidenceUpload
        ? "${{ always() && steps.browser_acceptance.outcome != 'skipped' }}"
        : "${{ steps.changes.outputs.halo != 'skip' }}",
    );
    expect(step["continue-on-error"]).not.toBe(true);
  }
  const fixture = repository();
  const result = classify(fixture, fixture.candidate(), "halo", {
    GITHUB_OUTPUT: join(fixture.directory, "missing/output"),
  });
  expect(result.status).toBe(1);
  expect(result.output).toBe("");
  expect(
    workflows.rust.jobs.tools.steps.some((step) =>
      step.run?.includes("bun test tools/check-inputs.test.ts"),
    ),
  ).toBe(true);
});

test("the browser evidence upload is pinned and stays inside the evidence directory", () => {
  const website = workflows.halo.jobs.website;
  const acceptance = website.steps.find(
    (step) => step.id === "browser_acceptance",
  );
  expect(acceptance?.env?.HALO_TEST_EVIDENCE).toBe(
    "${{ github.workspace }}/target/halo-browser-evidence/${{ github.run_id }}-${{ github.run_attempt }}",
  );
  const upload = website.steps.find((step) =>
    step.uses?.startsWith("actions/upload-artifact@"),
  );
  expect(upload?.uses).toBe(
    "actions/upload-artifact@cf430e030ddbb5b0abf93d22962f4752f3646cd9",
  );
  expect(upload?.if).toBe(
    "${{ always() && steps.browser_acceptance.outcome != 'skipped' }}",
  );
  expect(upload?.with).toMatchObject({
    name: "halo-browser-evidence-${{ github.run_id }}-${{ github.run_attempt }}",
    path: "${{ github.workspace }}/target/halo-browser-evidence/${{ github.run_id }}-${{ github.run_attempt }}",
    "if-no-files-found": "error",
  });
  expect(JSON.stringify(upload?.with)).not.toContain("test-results");
});
