import { expect } from "@playwright/test";
import { readFile, rename, writeFile } from "node:fs/promises";
import { join } from "node:path";

/** Ask the Rust fixture for one independent database fact snapshot. */
export async function affiliationObservation(
  personId: string,
  phase: string,
  position: "before" | "after",
  operation?: string,
) {
  const path = join(
    process.env.HALO_TEST_MATERIAL!,
    `affiliation-${phase}-${position}`,
  );
  await writeFile(
    `${path}.pending`,
    JSON.stringify({ personId, operation: operation ?? null }),
  );
  await rename(`${path}.pending`, `${path}.request`);
  await expect
    .poll(() => readFile(`${path}.ack`, "utf8").catch(() => ""), {
      timeout: 10_000,
    })
    .toBe("ok");
}
