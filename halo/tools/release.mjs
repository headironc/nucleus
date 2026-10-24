import { readFile, readdir, lstat } from "node:fs/promises";
import { createHash } from "node:crypto";
import { join } from "node:path";

/** @param {import("node:crypto").BinaryLike} bytes */
export const digest = (bytes) =>
  createHash("sha256").update(bytes).digest("hex");
/** @param {string} directory @param {string} prefix @returns {Promise<string[]>} */
export async function files(directory, prefix = "") {
  const result = [];
  for (const name of (await readdir(directory)).sort()) {
    const relative = prefix + name;
    const info = await lstat(join(directory, name));
    if (info.isSymbolicLink())
      throw new Error("Release symlinks are not allowed");
    if (info.isDirectory())
      result.push(...(await files(join(directory, name), `${relative}/`)));
    else if (info.isFile()) result.push(relative);
    else throw new Error("Unexpected release entry");
  }
  return result;
}

const types = {
  ".js": "text/javascript",
  ".css": "text/css",
  ".png": "image/png",
  ".svg": "image/svg+xml",
  ".jpg": "image/jpeg",
  ".woff2": "font/woff2",
};
/** @param {string} path */
export function mediaType(path) {
  const extension = path.slice(path.lastIndexOf("."));
  if (!types[extension])
    throw new Error(`Undeclared public resource type: ${extension}`);
  return types[extension];
}

/**
 * @typedef {{file: string, sha256: string}} Entry
 * @typedef {Entry & {path: string, mediaType: string}} Resource
 * @typedef {{version: 1, buildId: string, resources: Resource[], server: Entry[], runtime: Entry[]}} Manifest
 */
/** @param {string} root @returns {Promise<Manifest>} */
export async function verifyRelease(root) {
  const manifest = JSON.parse(
    await readFile(join(root, "build/release.json"), "utf8"),
  );
  if (manifest.version !== 1 || !/^[0-9a-f]{20}$/.test(manifest.buildId))
    throw new Error("Invalid release manifest");
  const actual = await files(join(root, "build/client"));
  if (
    JSON.stringify(actual) !==
    JSON.stringify(manifest.resources.map((item) => item.file).sort())
  ) {
    throw new Error("Public resource manifest mismatch");
  }
  for (const item of manifest.resources) {
    if (
      !/^[a-zA-Z0-9_./-]+$/.test(item.file) ||
      item.file
        .split("/")
        .some((segment) => !segment || segment === "." || segment === "..") ||
      item.path !== `/assets/${manifest.buildId}/${item.file}` ||
      item.mediaType !== mediaType(item.file) ||
      digest(await readFile(join(root, "build/client", item.file))) !==
        item.sha256
    ) {
      throw new Error("Public resource verification failed");
    }
  }
  const serverFiles = await files(join(root, "build/server"));
  if (
    JSON.stringify(serverFiles) !==
    JSON.stringify(manifest.server.map((item) => item.file).sort())
  ) {
    throw new Error("Server manifest mismatch");
  }
  for (const item of manifest.server) {
    if (
      digest(await readFile(join(root, "build/server", item.file))) !==
      item.sha256
    )
      throw new Error("Server verification failed");
  }
  if (
    JSON.stringify(manifest.runtime.map((item) => item.file).sort()) !==
    JSON.stringify([
      "bun.lock",
      "package.json",
      "server.mjs",
      "tools/release.mjs",
    ])
  ) {
    throw new Error("Runtime manifest mismatch");
  }
  for (const item of manifest.runtime) {
    if (
      !["server.mjs", "tools/release.mjs", "package.json", "bun.lock"].includes(
        item.file,
      ) ||
      digest(await readFile(join(root, item.file))) !== item.sha256
    )
      throw new Error("Runtime verification failed");
  }
  return manifest;
}
