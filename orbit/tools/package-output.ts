import { lstat, readdir } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../", import.meta.url));

/** Build and package tests select the same named output, never a source or external path. */
export async function packageOutput(name = process.env.ORBIT_BUILD_OUTPUT ?? "build", projectRoot = root) {
  if (!/^build(?:-[a-z0-9]+(?:-[a-z0-9]+)*)?$/.test(name)) {
    throw new Error("Expected build or a named build-<name> package output");
  }
  const directory = join(projectRoot, name);
  async function inspect(path: string, outputRoot = false) {
    let entry;
    try { entry = await lstat(path); }
    catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") return;
      throw error;
    }
    if (entry.isSymbolicLink() || (entry.isFile() && entry.nlink !== 1)) {
      throw new Error("Package output cannot contain symbolic or hard-link aliases");
    }
    if (outputRoot && !entry.isDirectory()) throw new Error("Package output must be a directory");
    if (entry.isDirectory()) {
      for (const child of await readdir(path)) await inspect(join(path, child));
    } else if (!entry.isFile()) throw new Error("Package output contains an unsupported filesystem entry");
  }
  await inspect(directory, true);
  return { name, directory };
}
