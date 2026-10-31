/** Linux readback for this run's disposable PostgreSQL, independent of Rust Drop and reports. */
import { execFileSync } from "node:child_process";
import {
  readFileSync,
  readdirSync,
  readlinkSync,
  realpathSync,
  statSync,
} from "node:fs";
import { join, sep } from "node:path";

type Identity = {
  pid: number;
  started: string;
  executable: string;
  data: string;
};
function start(pid: number): string {
  const value = readFileSync(`/proc/${pid}/stat`, "utf8");
  return value.slice(value.lastIndexOf(")") + 2).split(" ")[19];
}

function owned(root: string, executable: string): Identity[] {
  const privateRoot = realpathSync(root);
  const expected = realpathSync(executable);
  const result = new Map<number, Identity>();
  for (const name of readdirSync("/proc")) {
    if (!/^\d+$/.test(name)) continue;
    let actual: string;
    try {
      actual = readlinkSync(`/proc/${name}/exe`);
    } catch (error) {
      if (
        ["ENOENT", "EACCES", "EPERM"].includes(
          (error as NodeJS.ErrnoException).code ?? "",
        )
      )
        continue;
      throw error;
    }
    if (actual !== expected) continue;
    let data: string;
    try {
      data = realpathSync(readlinkSync(`/proc/${name}/cwd`));
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") continue;
      throw error;
    }
    if (!data.startsWith(privateRoot + sep)) continue;
    const pid = Number(
      readFileSync(join(data, "postmaster.pid"), "utf8").split("\n")[0],
    );
    if (
      !Number.isSafeInteger(pid) ||
      pid <= 0 ||
      statSync(`/proc/${pid}`).uid !== process.getuid?.()
    )
      throw new Error("owned PostgreSQL PID is unverified");
    const args = readFileSync(`/proc/${pid}/cmdline`, "utf8").split("\0");
    if (
      readlinkSync(`/proc/${pid}/exe`) !== expected ||
      realpathSync(args[args.indexOf("-D") + 1] ?? "") !== data ||
      readFileSync(join(data, "PG_VERSION"), "utf8").trim() !== "18"
    )
      throw new Error("owned PostgreSQL executable or data identity changed");
    result.set(pid, { pid, started: start(pid), executable: expected, data });
  }
  return [...result.values()];
}

export function readback(
  root: string,
  repository: string,
): { clean: boolean; forced: boolean; errors: string[]; remaining: number } {
  const executable = join(repository, "target/postgresql/18.4/bin/postgres");
  const errors: string[] = [];
  let forced = false;
  try {
    const leaked = owned(root, executable);
    for (const identity of leaked) {
      forced = true;
      if (start(identity.pid) !== identity.started)
        throw new Error("owned PostgreSQL PID was reused");
      try {
        execFileSync(
          join(repository, "target/postgresql/18.4/bin/pg_ctl"),
          ["-D", identity.data, "-m", "fast", "-w", "-t", "10", "stop"],
          { timeout: 15_000, stdio: "ignore" },
        );
      } catch {
        errors.push("owned PostgreSQL fallback stop failed");
      }
    }
    const remaining = owned(root, executable).length;
    if (leaked.length)
      errors.push("PostgreSQL leaked beyond the original foreground exit");
    return {
      clean: !forced && remaining === 0 && errors.length === 0,
      forced,
      errors,
      remaining,
    };
  } catch {
    errors.push("PostgreSQL identity or terminal state could not be verified");
    return { clean: false, forced, errors, remaining: -1 };
  }
}
