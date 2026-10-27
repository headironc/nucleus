import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  lstatSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  realpathSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";

// The fixed P carries this file verbatim with its pinned S bundle. No package code runs before verification.
const environment = {
  PATH: "/usr/bin:/bin",
  LANG: "C",
  GIT_CONFIG_GLOBAL: "/dev/null",
  GIT_CONFIG_NOSYSTEM: "1",
};
const hash = (bytes) => createHash("sha256").update(bytes).digest("hex");
const execute = (cwd, args) =>
  execFileSync("/usr/bin/git", ["-c", "core.hooksPath=/dev/null", ...args], {
    cwd,
    env: environment,
    timeout: 30_000,
    maxBuffer: 128 * 1024 * 1024,
    stdio: ["ignore", "pipe", "pipe"],
  });

export function unpackShared(bundleArg, expected, destination) {
  const bundle = realpathSync(bundleArg);
  if (
    !lstatSync(bundle).isFile() ||
    lstatSync(bundle).size > 128 * 1024 * 1024 ||
    !/^[a-f0-9]{64}$/.test(expected.distribution) ||
    hash(readFileSync(bundle)) !== expected.distribution ||
    !/^[a-f0-9]{40}$/.test(expected.commit) ||
    !/^[a-f0-9]{40}$/.test(expected.tree) ||
    !/^[a-f0-9]{64}$/.test(expected.lock) ||
    expected.lockPath !== "bun.lock"
  )
    throw new Error("Invalid or substituted shared distribution");
  if (
    !path.isAbsolute(destination) ||
    realpathSync(path.dirname(destination)) !== path.dirname(destination)
  )
    throw new Error("Shared destination must have a physical parent");
  const parent = lstatSync(path.dirname(destination));
  if (parent.uid !== process.getuid() || parent.mode & 0o022)
    throw new Error(
      "Shared destination parent must be owned exclusively by the preparer",
    );
  // A failed extraction is never a runnable source. Keep the final destination absent until complete.
  const staging = mkdtempSync(
    path.join(path.dirname(destination), ".shared-verify-"),
  );
  try {
    execute(staging, ["init", "--quiet"]);
    execute(staging, ["bundle", "verify", bundle]);
    const heads = execute(staging, ["bundle", "list-heads", bundle])
      .toString("utf8")
      .trim()
      .split("\n");
    if (heads.length !== 1 || heads[0].split(" ")[0] !== expected.commit)
      throw new Error("Distribution must expose exactly the pinned source");
    execute(staging, [
      "-c",
      "transfer.fsckObjects=true",
      "fetch",
      "--no-tags",
      bundle,
      expected.commit,
    ]);
    if (
      execute(staging, ["rev-parse", `${expected.commit}^{tree}`])
        .toString("utf8")
        .trim() !== expected.tree
    )
      throw new Error("Shared tree mismatch");
    const listing = execute(staging, [
      "ls-tree",
      "-rz",
      "--full-tree",
      expected.commit,
    ]);
    const decoder = new TextDecoder("utf-8", { fatal: true });
    for (const item of decoder.decode(listing).split("\0").filter(Boolean)) {
      const match = /^(100644|100755) blob ([a-f0-9]{40})\t(.+)$/s.exec(item);
      if (!match)
        throw new Error(
          "Shared source contains a symlink, submodule or special member",
        );
      const [, mode, object, file] = match;
      if (
        file.includes("\\") ||
        !file
          .split("/")
          .every((part) => part && ![".", "..", ".git"].includes(part))
      )
        throw new Error("Unsafe shared source path");
      const target = path.join(staging, file);
      mkdirSync(path.dirname(target), { recursive: true });
      writeFileSync(target, execute(staging, ["cat-file", "blob", object]), {
        flag: "wx",
        mode: mode === "100755" ? 0o755 : 0o644,
      });
    }
    if (
      hash(readFileSync(path.join(staging, expected.lockPath))) !==
      expected.lock
    )
      throw new Error("Shared dependency lock mismatch");
    execute(staging, ["update-ref", "HEAD", expected.commit]);
    execute(staging, ["read-tree", expected.commit]);
    if (
      execute(staging, ["status", "--porcelain", "--untracked-files=all"])
        .length
    )
      throw new Error("Unbound shared source member");
    // mkdir is exclusive, so an existing path is not replaced even if verification succeeded.
    mkdirSync(destination);
    renameSync(staging, destination);
    return realpathSync(destination);
  } finally {
    rmSync(staging, { recursive: true, force: true });
  }
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href
) {
  const [bundle, lock, destination] = process.argv.slice(2);
  if (!bundle || !lock || !destination || process.argv.length !== 5)
    throw new Error(
      "Expected shared bundle, fixed composition lock and new destination",
    );
  unpackShared(bundle, JSON.parse(readFileSync(lock, "utf8")), destination);
}
