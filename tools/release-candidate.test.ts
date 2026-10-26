import { afterEach, expect, test } from "bun:test";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import { git, prepareRelease, validateRelease } from "./release-candidate";
import { release } from "./main-promotion";
import { repositoryId, repository as remote } from "./promotion-deployment";
import type { Api } from "./promotion-credentials";
import type { Json } from "./review-json";

const directories: string[] = [];
afterEach(() => {
  for (const path of directories.splice(0))
    rmSync(path, { recursive: true, force: true });
});

function repository() {
  const cwd = mkdtempSync(join(tmpdir(), "nucleus release "));
  directories.push(cwd);
  const run = (...args: string[]) => git(args, cwd);
  run("init", "--initial-branch=dev");
  run("config", "user.name", "Release Test");
  run("config", "user.email", "release@example.invalid");
  run("config", "commit.gpgsign", "false");
  writeFileSync(join(cwd, "product"), "initial\n");
  run("add", "product");
  run("commit", "-m", "initial");
  const base = run("rev-parse", "HEAD");
  run("update-ref", "refs/remotes/origin/main", base);
  function advance(content: string) {
    writeFileSync(join(cwd, "product"), content);
    run("add", "product");
    run("commit", "-m", "accepted feature");
    const head = run("rev-parse", "HEAD");
    run("update-ref", "refs/remotes/origin/dev", head);
    return head;
  }
  return { cwd, run, base, advance };
}

test("two signed promotions preserve accepted dev history without main flowing back into dev", () => {
  const { cwd, run, base, advance } = repository();
  const key = join(cwd, "signing-key");
  const generated = spawnSync("ssh-keygen", [
    "-q",
    "-t",
    "ed25519",
    "-N",
    "",
    "-f",
    key,
  ]);
  expect(generated.status).toBe(0);
  run("config", "gpg.format", "ssh");
  run("config", "user.signingkey", key);
  writeFileSync(
    join(cwd, "allowed-signers"),
    `release@example.invalid ${readFileSync(key + ".pub", "utf8")}`,
  );
  run("config", "gpg.ssh.allowedSignersFile", join(cwd, "allowed-signers"));
  const d1 = advance("first accepted release\n");
  const p1 = prepareRelease(d1, base, "release/one", cwd);
  expect(p1).toBe(d1);
  const tree1 = validateRelease(d1, base, p1, d1, cwd);
  const m1 = run(
    "commit-tree",
    "-S",
    tree1,
    "-p",
    base,
    "-p",
    p1,
    "-m",
    "first promotion",
  );
  run("verify-commit", m1);
  run("update-ref", "refs/remotes/origin/main", m1);
  const d2 = advance("second accepted release\n");
  const p2 = prepareRelease(d2, m1, "release/two", cwd);
  expect(run("show", "-s", "--format=%P", p2)).toBe(`${d2} ${m1}`);
  run("verify-commit", p2);
  const tree2 = validateRelease(d2, m1, p2, d2, cwd);
  expect(tree2).toBe(run("rev-parse", `${d2}^{tree}`));
  const m2 = run(
    "commit-tree",
    "-S",
    tree2,
    "-p",
    m1,
    "-p",
    p2,
    "-m",
    "second promotion",
  );
  run("verify-commit", m2);
  expect(run("show", "-s", "--format=%P", m2)).toBe(`${m1} ${p2}`);
  expect(run("rev-parse", "dev")).toBe(d2);
  expect(() => run("merge-base", "--is-ancestor", m1, d2)).toThrow();
  expect(() => prepareRelease(d1, m1, "release/two", cwd)).toThrow();
  expect(run("rev-parse", "release/two")).toBe(p2);
});

test("release validation rejects unaccepted content, wrong parents, unknown dev and stale main", () => {
  const { cwd, run, base, advance } = repository();
  const dev = advance("accepted\n");
  const tree = run("rev-parse", `${dev}^{tree}`);
  const extra = advance("unaccepted release-only change\n");
  const altered = run(
    "commit-tree",
    run("rev-parse", `${extra}^{tree}`),
    "-p",
    dev,
    "-p",
    base,
    "-m",
    "extra",
  );
  expect(() => validateRelease(dev, base, altered, dev, cwd)).toThrow(
    "unaccepted content",
  );
  const reversed = run(
    "commit-tree",
    tree,
    "-p",
    base,
    "-p",
    dev,
    "-m",
    "reversed",
  );
  expect(() => validateRelease(dev, base, reversed, dev, cwd)).toThrow(
    "exact dev/main merge",
  );
  expect(() => validateRelease(extra, base, extra, dev, cwd)).toThrow();
  expect(() => prepareRelease(dev, extra, "release/stale", cwd)).toThrow(
    "fetched main",
  );
  expect(() => prepareRelease(dev, base, "dev", cwd)).toThrow("release/");
  expect(() => validateRelease(dev.slice(0, 7), base, dev, dev, cwd)).toThrow(
    "full commit",
  );
});

test("conflicts stop candidate preparation without changing the worktree or creating a release ref", () => {
  const { cwd, run, base, advance } = repository();
  const dev = advance("dev change\n");
  run("switch", "--detach", base);
  const main = advance("conflicting main change\n");
  run("update-ref", "refs/remotes/origin/dev", dev);
  run("update-ref", "refs/remotes/origin/main", main);
  const before = run("status", "--porcelain");
  expect(() => prepareRelease(dev, main, "release/conflict", cwd)).toThrow();
  expect(run("status", "--porcelain")).toBe(before);
  expect(run("rev-parse", "HEAD")).toBe(main);
  expect(() =>
    run("show-ref", "--verify", "refs/heads/release/conflict"),
  ).toThrow();
});

test("release eligibility checks real Git objects, signature and current refs before admission", async () => {
  const { cwd, base, advance } = repository();
  const dev = advance("accepted\n");
  const wanted = { pr: 42n, dev, base, head: dev };
  for (const change of ["none", "signature", "closed", "main", "controller"]) {
    const read: Api = async (path): Promise<Json> => {
      if (path.endsWith("/pulls/42"))
        return {
          number: 42n,
          node_id: "node",
          url: `https://api.github.com/repos/${remote}/pulls/42`,
          base: { sha: base, ref: "main", repo: { id: repositoryId } },
          head: { sha: dev, ref: "release/one", repo: { id: repositoryId } },
          state: change === "closed" ? "closed" : "open",
          draft: false,
          merged: false,
        };
      if (path.endsWith("/git/ref/heads/main"))
        return {
          ref: "refs/heads/main",
          object: { sha: change === "main" ? dev : base },
        };
      if (path.endsWith("/git/ref/heads/dev"))
        return {
          ref: "refs/heads/dev",
          object: { sha: change === "controller" ? base : dev },
        };
      if (path.endsWith(`/commits/${dev}`))
        return {
          sha: dev,
          commit: { verification: { verified: change !== "signature" } },
        };
      throw new Error(`Unexpected request ${path}`);
    };
    if (change === "none")
      expect((await release(wanted, dev, read, cwd)).id).toBe("node");
    else await expect(release(wanted, dev, read, cwd)).rejects.toThrow();
  }
});
