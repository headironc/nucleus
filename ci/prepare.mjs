import { readFileSync } from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { unpackShared } from "./bootstrap.mjs";

if (process.version !== "v26.8.1")
  throw new Error("Shared CI preparation requires Node 26.8.1");
const root = path.resolve(import.meta.dirname, "..");
const lock = JSON.parse(
  readFileSync(path.join(root, "ci/shared.lock.json"), "utf8"),
);
const shared = unpackShared(
  path.join(root, "ci/shared.bundle"),
  lock,
  path.join(root, ".shared"),
);
const installation = spawnSync(
  "bun",
  ["install", "--frozen-lockfile", "--ignore-scripts"],
  { cwd: shared, stdio: "inherit" },
);
if (installation.error || installation.status !== 0 || installation.signal)
  throw new Error("Shared CI dependency preparation failed", {
    cause: installation.error,
  });
