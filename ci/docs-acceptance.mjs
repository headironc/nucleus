import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import {
  cpSync,
  mkdirSync,
  readFileSync,
  writeFileSync,
  chmodSync,
  chownSync,
  appendFileSync,
} from "node:fs";
import path from "node:path";

// Preserve the signed P object and fixed S distribution. Only H/I below are fixtures.
assert.equal(process.getuid?.(), 0);
assert.equal(readFileSync("/proc/1/comm", "utf8").trim(), "docker-init");
const root = "/work",
  policy = root + "/policy",
  shared = policy + "/.shared";
const git = (cwd, args, input) =>
  execFileSync(
    "/usr/bin/git",
    [
      "-c",
      "safe.directory=" + cwd,
      "-c",
      "user.name=Composition fixture",
      "-c",
      "user.email=fixture@example.invalid",
      "-c",
      "commit.gpgsign=false",
      ...args,
    ],
    {
      cwd,
      input,
      encoding: "utf8",
      env: {
        PATH: "/usr/bin:/bin",
        HOME: "/tmp",
        GIT_CONFIG_GLOBAL: "/dev/null",
        GIT_CONFIG_NOSYSTEM: "1",
      },
    },
  ).trim();
const init = (directory) => {
  git(directory, ["init", "-q"]);
  git(directory, ["add", "."]);
  const expected = JSON.parse(
    readFileSync("/snapshot/policy-identity.json", "utf8"),
  );
  assert.equal(git(directory, ["write-tree"]), expected.tree);
  const commit = git(
    directory,
    ["hash-object", "-t", "commit", "-w", "--stdin"],
    readFileSync("/snapshot/policy-commit"),
  );
  assert.equal(commit, expected.commit);
  writeFileSync(path.join(directory, ".git/shallow"), commit + "\n");
  git(directory, ["update-ref", "HEAD", commit]);
  git(directory, ["checkout", "-q", "--detach", commit]);
};
cpSync("/snapshot/policy", policy, { recursive: true });
init(policy);
chownSync(policy, 0, 0);
const { unpackShared } = await import(policy + "/ci/bootstrap.mjs");
const fixed = JSON.parse(readFileSync(policy + "/ci/shared.lock.json", "utf8"));
const distribution = policy + "/ci/shared.bundle";
unpackShared(distribution, fixed, shared);
cpSync("/policy-deps", policy + "/node_modules", {
  recursive: true,
  verbatimSymlinks: true,
});
cpSync("/shared-deps", shared + "/node_modules", {
  recursive: true,
  verbatimSymlinks: true,
});
mkdirSync(root + "/tools");
for (const name of ["node", "bun", "lychee"]) {
  cpSync("/host-tools/" + name, root + "/tools/" + name);
  chownSync(root + "/tools/" + name, 0, 0);
  chmodSync(root + "/tools/" + name, 0o755);
}
const { requireSupervisor } = await import(shared + "/ci/sandbox.ts");
requireSupervisor("local-container");
const { BoundInputs, fileDigest } = await import(shared + "/ci/binding.ts");
const { VerifiedExecution } = await import(shared + "/ci/execution.ts");
const { checks, runDocs, verifyDocs } = await import(policy + "/ci/docs.ts");
const pin = (directory) => ({
  commit: git(directory, ["rev-parse", "HEAD"]),
  tree: git(directory, ["rev-parse", "HEAD^{tree}"]),
  lockPath: "bun.lock",
  lock: fileDigest(directory + "/bun.lock"),
});
const composition = {
  policy: pin(policy),
  shared: pin(shared),
  distribution: fileDigest(distribution),
};
assert.equal(composition.shared.commit, fixed.commit);
assert.equal(composition.shared.tree, fixed.tree);
assert.equal(composition.shared.lock, fixed.lock);
assert.equal(composition.distribution, fixed.distribution);
const target = {
  repository: "headironc/nucleus",
  repositoryId: "1432619931",
  target: "dev",
  workflow: ".github/workflows/docs.yml",
};
const tools = [
  ["node", "v26.8.1"],
  ["bun", "1.4.2"],
  ["lychee", "lychee 0.24.2"],
].map(([name, version]) => ({
  name,
  executable: root + "/tools/" + name,
  sha256: fileDigest(root + "/tools/" + name),
  versionArgs: ["--version"],
  version,
}));
function candidate(name, file, content) {
  const directory = root + "/" + name;
  cpSync("/snapshot/policy", directory, { recursive: true });
  init(directory);
  const base = git(directory, ["rev-parse", "HEAD"]);
  mkdirSync(path.dirname(directory + "/" + file), { recursive: true });
  writeFileSync(directory + "/" + file, content);
  git(directory, ["add", file]);
  git(directory, ["commit", "-qm", "test: candidate change"]);
  return bindCandidate(directory, base, git(directory, ["rev-parse", "HEAD"]));
}
function bindCandidate(directory, base, head) {
  const tree = git(directory, ["rev-parse", "HEAD^{tree}"]);
  const integration = git(
    directory,
    ["commit-tree", tree, "-p", base, "-p", head],
    "test: ordered integration fixture\n",
  );
  git(directory, ["checkout", "-q", "--detach", integration]);
  const identity = {
    event: "pull_request_target",
    base,
    head,
    integration,
    tree,
  };
  return BoundInputs.verify(
    composition,
    identity,
    { policy, shared, candidate: directory, tools },
    distribution,
    target,
  );
}
const outcomes = [];
const inputs = candidate(
  "candidate",
  "docs/ci-composition-fixture.md",
  "# Composition acceptance\n\nA disposable fixture.\n",
);
const required = checks(inputs);
assert.deepEqual(
  required.map((c) => c.name),
  ["docs.format", "docs.markdown", "docs.links", "docs.preview"],
);
assert.ok(
  required.every(
    (c) =>
      c.argv[0] === root + "/tools/bun" &&
      c.cwd === policy &&
      c.timeoutMs === 180000,
  ),
);
mkdirSync(root + "/reports", { mode: 0o700 });
const execution = await runDocs(inputs, {
  provenance: { kind: "local-container" },
  preparation: { copies: [], writable: [] },
  reportDirectory: root + "/reports",
  signal: new AbortController().signal,
});
verifyDocs(execution, inputs);
writeFileSync(
  "/evidence/positive-report.json",
  JSON.stringify(execution.report, null, 2) + "\n",
);
outcomes.push({
  scenario: "actual-four-docs",
  result: "passed",
  observations: execution.report.observations,
});
assert.throws(() => checks({ ...inputs }), /Unverified execution inputs/);
assert.throws(
  () => verifyDocs({ report: execution.report }, inputs),
  /Unverified execution provenance/,
);
assert.throws(
  () => VerifiedExecution.fromHosted({ report: execution.report }),
  /Unverified artifact provenance/,
);
outcomes.push({
  scenario: "plain-input-report-artifact-rejected",
  result: "passed",
});
const mixed = candidate(
  "mixed",
  "src/composition-fixture.rs",
  "// Disposable fixture.\n",
);
assert.throws(() => checks(mixed), /outside the Nucleus Docs slice/);
outcomes.push({ scenario: "mixed-scope-rejected", result: "passed" });
const renamedDirectory = root + "/renamed";
cpSync("/snapshot/policy", renamedDirectory, { recursive: true });
init(renamedDirectory);
writeFileSync(renamedDirectory + "/src/ci_rename_fixture.rs", "");
appendFileSync(renamedDirectory + "/src/lib.rs", "\nmod ci_rename_fixture;\n");
git(renamedDirectory, ["add", "src/ci_rename_fixture.rs", "src/lib.rs"]);
git(renamedDirectory, [
  "commit",
  "-qm",
  "test: valid empty Rust module baseline",
]);
const renameBase = git(renamedDirectory, ["rev-parse", "HEAD"]);
git(renamedDirectory, [
  "mv",
  "src/ci_rename_fixture.rs",
  "docs/ci-rename-fixture.md",
]);
git(renamedDirectory, ["commit", "-qm", "test: move Rust module into Docs"]);
const renamed = bindCandidate(
  renamedDirectory,
  renameBase,
  git(renamedDirectory, ["rev-parse", "HEAD"]),
);
writeFileSync(
  "/evidence/rename-inputs.json",
  JSON.stringify(
    { candidate: renamed.candidate, changedPaths: renamed.changedPaths },
    null,
    2,
  ) + "\n",
);
assert.ok(
  renamed.changedPaths.includes("src/ci_rename_fixture.rs"),
  "Renames must preserve the deleted Rust source input",
);
assert.ok(renamed.changedPaths.includes("docs/ci-rename-fixture.md"));
assert.throws(() => checks(renamed), /outside the Nucleus Docs slice/);
outcomes.push({ scenario: "renamed-rust-source-rejected", result: "passed" });
mkdirSync(root + "/missing-reports", { mode: 0o700 });
const incomplete = await VerifiedExecution.run(
  inputs,
  "nucleus.docs",
  required.slice(0, 3),
  {
    provenance: { kind: "local-container" },
    preparation: { copies: [], writable: [] },
    reportDirectory: root + "/missing-reports",
    signal: new AbortController().signal,
  },
);
assert.throws(() => verifyDocs(incomplete, inputs), /completeness mismatch/);
writeFileSync(
  "/evidence/missing-report.json",
  JSON.stringify(incomplete.report, null, 2) + "\n",
);
outcomes.push({
  scenario: "authentic-missing-preview-rejected",
  result: "passed",
});
const broken = candidate(
  "broken",
  "docs/ci-composition-fixture.md",
  "# Broken\n\n[Missing](./absent-composition-target.md)\n",
);
mkdirSync(root + "/broken-reports", { mode: 0o700 });
await assert.rejects(
  runDocs(broken, {
    provenance: { kind: "local-container" },
    preparation: { copies: [], writable: [] },
    reportDirectory: root + "/broken-reports",
    signal: new AbortController().signal,
  }),
  /docs.links/,
);
const failed = JSON.parse(
  readFileSync(root + "/broken-reports/report.json", "utf8"),
);
assert.equal(
  failed.observations.find((c) => c.request.name === "docs.links").exitCode,
  1,
);
assert.equal(
  failed.observations.find((c) => c.request.name === "docs.links").settled,
  true,
);
writeFileSync(
  "/evidence/broken-report.json",
  JSON.stringify(failed, null, 2) + "\n",
);
outcomes.push({
  scenario: "real-broken-link-rejected-and-settled",
  result: "passed",
});
writeFileSync(
  "/evidence/result.json",
  JSON.stringify(
    {
      kind: "local-container",
      sharedDistributionFixed: true,
      runtimeSourceActivated: false,
      composition,
      target,
      outcomes,
    },
    null,
    2,
  ) + "\n",
);
console.log(
  JSON.stringify({
    kind: "local-container",
    sharedDistributionFixed: true,
    runtimeSourceActivated: false,
    scenarios: outcomes.map(({ scenario, result }) => ({ scenario, result })),
  }),
);
