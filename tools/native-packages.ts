import { spawnSync } from "node:child_process";
import { appendFileSync } from "node:fs";

type Row = { package: string; shard?: string; group?: string };

function check(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

export function packageMatrix(names: unknown, value: unknown): Row[] {
  check(
    Array.isArray(names) &&
      names.length > 0 &&
      names.every(
        (name) => typeof name === "string" && /^[a-zA-Z0-9_-]+$/.test(name),
      ) &&
      new Set(names).size === names.length,
    "Invalid selected package list",
  );
  check(
    value !== null && typeof value === "object" && !Array.isArray(value),
    "Invalid package matrix",
  );
  check(
    Object.keys(value).join() === "include",
    "Invalid package matrix fields",
  );
  const rows: unknown = (value as { include: unknown }).include;
  check(Array.isArray(rows) && rows.length > 0, "Empty package matrix");
  // These partitions are owned by check-packages; discovery must include each whole package.
  const expected: Row[] = names.flatMap((name: string): Row[] =>
    name === "domain"
      ? [1, 2, 3, 4].map((index) => ({ package: name, shard: `${index}/4` }))
      : name === "nucleus"
        ? ["rest", "http"].map((group) => ({ package: name, group }))
        : [{ package: name }],
  );
  const key = (row: Row) =>
    JSON.stringify([row.package, row.shard ?? null, row.group ?? null]);
  for (const row of rows) {
    check(
      row && typeof row === "object" && !Array.isArray(row),
      "Invalid matrix row",
    );
    check(
      Object.keys(row).every((field) =>
        ["package", "shard", "group"].includes(field),
      ),
      "Unknown matrix field",
    );
  }
  const keys = rows.map((row) => key(row as Row));
  check(
    new Set(keys).size === keys.length &&
      keys.length === expected.length &&
      expected.every((row) => keys.includes(key(row))),
    "Incomplete or duplicate package matrix",
  );
  return rows as Row[];
}

export function selection(
  tier: string | undefined,
  affected: string | undefined,
) {
  check(
    tier && ["skip", "tools", "checks", "full"].includes(tier),
    "Invalid package tier",
  );
  check(affected !== undefined, "Missing affected packages");
  if (tier !== "full") {
    check(affected === "[]", "Non-full tier must have no affected packages");
    return false;
  }
  if (affected !== "all") {
    const names: unknown = JSON.parse(affected);
    check(
      Array.isArray(names) &&
        names.length > 0 &&
        names.every((name) => typeof name === "string" && name.length),
      "Invalid affected packages",
    );
  }
  return true;
}

export function packageResult(env: NodeJS.ProcessEnv) {
  check(env.CANCELLED === "false", "Package workflow cancelled or unknown");
  check(env.PLAN_RESULT === "success", "Package plan did not succeed");
  const full = selection(env.TIER, env.AFFECTED);
  if (full) {
    packageMatrix(
      JSON.parse(env.PACKAGE_NAMES ?? "null"),
      JSON.parse(env.MATRIX ?? "null"),
    );
    check(
      env.POSTGRESQL_RESULT === "success" && env.PACKAGES_RESULT === "success",
      "Selected package jobs did not all succeed",
    );
  } else {
    check(
      env.MATRIX === '{"include":[]}' && env.PACKAGE_NAMES === "[]",
      "Unexpected skipped package plan",
    );
    check(
      env.POSTGRESQL_RESULT === "skipped" && env.PACKAGES_RESULT === "skipped",
      "Non-full plan executed unexpected jobs",
    );
  }
  return full
    ? "All selected package checks passed"
    : `No package execution required for verified tier ${env.TIER}`;
}

function discover(args: string[]) {
  const result = spawnSync(
    process.execPath,
    ["tools/check-packages.ts", ...args],
    { encoding: "utf8" },
  );
  if (result.stderr) process.stderr.write(result.stderr);
  check(!result.error && result.status === 0, "Package discovery failed");
  return JSON.parse(result.stdout);
}

if (import.meta.main) {
  try {
    const command = process.argv[2];
    if (command === "plan" && process.argv.length === 3) {
      const full = selection(process.env.TIER, process.env.AFFECTED);
      const names = full
        ? discover(["--list", "--affected", process.env.AFFECTED!])
        : [];
      const matrix = full
        ? discover(["--matrix", "--affected", process.env.AFFECTED!])
        : { include: [] };
      if (full) packageMatrix(names, matrix);
      check(process.env.GITHUB_OUTPUT, "Missing GITHUB_OUTPUT");
      appendFileSync(
        process.env.GITHUB_OUTPUT,
        `matrix=${JSON.stringify(matrix)}\npackages=${JSON.stringify(names)}\n`,
      );
      console.log(
        full
          ? `Selected package matrix: ${JSON.stringify(matrix)}`
          : `Verified tier ${process.env.TIER}: no package jobs`,
      );
    } else if (command === "finish" && process.argv.length === 3) {
      console.log(packageResult(process.env));
    } else throw new Error("Usage: bun tools/native-packages.ts plan|finish");
  } catch (error) {
    console.error(
      error instanceof Error
        ? error.message
        : "Native package validation failed",
    );
    process.exitCode = 1;
  }
}
