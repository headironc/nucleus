import { spawnSync } from "node:child_process";
import { sha } from "./review-queue";

export function git(args: string[], cwd = process.cwd()): string {
  const result = spawnSync("git", ["-c", "core.hooksPath=/dev/null", ...args], {
    cwd,
    env: {
      ...Object.fromEntries(
        Object.entries(process.env).filter(([key]) => !key.startsWith("GIT_")),
      ),
      GIT_NO_REPLACE_OBJECTS: "1",
      GIT_CONFIG_NOSYSTEM: "1",
    },
    timeout: 60_000,
    maxBuffer: 16 * 1024 * 1024,
    encoding: "utf8",
  });
  if (result.error || result.status !== 0)
    throw new Error(`Release Git ${args[0]} failed; no candidate was accepted`);
  return result.stdout.trim();
}

export function commit(value: string): string {
  if (!sha(value)) throw new Error("Release inputs require full commit IDs");
  return value;
}

export function ancestor(base: string, head: string, cwd?: string): void {
  git(["merge-base", "--is-ancestor", commit(base), commit(head)], cwd);
}

export function releaseTree(dev: string, base: string, cwd?: string): string {
  const tree = git(
    ["merge-tree", "--write-tree", commit(dev), commit(base)],
    cwd,
  ).split("\n")[0];
  if (!sha(tree)) throw new Error("Release merge did not produce a tree");
  return tree;
}

// Candidate objects are inspected, never checked out or executed by the controller.
export function validateRelease(
  dev: string,
  base: string,
  head: string,
  tip: string,
  cwd?: string,
): string {
  ancestor(dev, tip, cwd);
  ancestor(base, head, cwd);
  if (
    head !== dev &&
    git(["show", "-s", "--format=%P", commit(head)], cwd) !== `${dev} ${base}`
  )
    throw new Error(
      "Release candidate must be dev or the exact dev/main merge",
    );
  const tree = git(["rev-parse", `${commit(head)}^{tree}`], cwd);
  if (tree !== releaseTree(dev, base, cwd))
    throw new Error("Release candidate contains unaccepted content");
  return tree;
}

export function prepareRelease(
  dev: string,
  base: string,
  branch: string,
  cwd?: string,
): string {
  if (!/^release\/[a-zA-Z0-9][a-zA-Z0-9._/-]*$/.test(branch))
    throw new Error("Use a release/ branch name");
  git(["check-ref-format", `refs/heads/${branch}`], cwd);
  const tip = commit(git(["rev-parse", "refs/remotes/origin/dev"], cwd));
  if (git(["rev-parse", "refs/remotes/origin/main"], cwd) !== commit(base))
    throw new Error("Release base differs from fetched main");
  ancestor(commit(dev), tip, cwd);
  const tree = releaseTree(dev, base, cwd);
  let head = dev;
  if (git(["merge-base", dev, base], cwd) !== base)
    head = commit(
      git(
        [
          "commit-tree",
          "-S",
          tree,
          "-p",
          dev,
          "-p",
          base,
          "-m",
          `chore(release): prepare accepted dev ${dev}`,
        ],
        cwd,
      ),
    );
  validateRelease(dev, base, head, tip, cwd);
  // The all-zero old value creates only; it never overwrites an existing branch.
  git(["update-ref", `refs/heads/${branch}`, head, "0".repeat(40)], cwd);
  return head;
}

if (import.meta.main) {
  const [dev, base, branch, ...rest] = process.argv.slice(2);
  if (!dev || !base || !branch || rest.length)
    throw new Error(
      "Usage: bun tools/release-candidate.ts DEV_SHA MAIN_SHA release/NAME",
    );
  console.log(prepareRelease(dev, base, branch));
}
