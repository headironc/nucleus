import path from "node:path";
import { realpathSync } from "node:fs";
import { BoundInputs, verifySource } from "../.shared/ci/binding.ts";
import type { Check } from "../.shared/ci/contract.ts";
import { VerifiedExecution, verifyChecks } from "../.shared/ci/execution.ts";
import { selectChecks } from "../tools/check-inputs.ts";

const loadedPolicy = realpathSync(path.resolve(import.meta.dirname, ".."));
const scope = "nucleus.docs";

export function checks(inputs: BoundInputs): Check[] {
  BoundInputs.requireVerified(inputs);
  if (
    inputs.target.repository !== "headironc/nucleus" ||
    inputs.target.repositoryId !== "1432619931" ||
    inputs.target.target !== "dev" ||
    inputs.target.workflow !== ".github/workflows/docs.yml" ||
    inputs.candidate.event !== "pull_request_target"
  )
    throw new Error("Unexpected Nucleus Docs policy, repository or target");
  const selection = selectChecks(inputs.changedPaths);
  if (selection.tier !== "skip" || selection.halo !== "skip")
    throw new Error("Candidate requires checks outside the Nucleus Docs slice");
  const bun = inputs.paths.tools.find((tool) => tool.name === "bun");
  const lychee = inputs.paths.tools.find((tool) => tool.name === "lychee");
  if (
    bun?.version !== "1.4.2" ||
    lychee?.version !== "lychee 0.24.2" ||
    JSON.stringify(bun.versionArgs) !== '["--version"]' ||
    JSON.stringify(lychee.versionArgs) !== '["--version"]'
  )
    throw new Error("Nucleus Docs requires pinned Bun and lychee tools");
  const { policy, candidate } = inputs.paths;
  const required: Check[] = ["format", "markdown", "links"].map((mode) => ({
    name: `docs.${mode}`,
    argv: [
      bun.executable,
      path.join(policy, "tools/check-ci-docs.ts"),
      mode,
      candidate,
    ],
    cwd: policy,
    env: {},
    timeoutMs: 180_000,
  }));
  required.push({
    name: "docs.preview",
    argv: [
      bun.executable,
      "test",
      path.join(policy, "tools/preview-design.test.ts"),
    ],
    cwd: policy,
    env: { NUCLEUS_DESIGN_CANDIDATE: candidate },
    timeoutMs: 180_000,
  });
  return required;
}

export function verifyDocs(
  execution: VerifiedExecution,
  inputs: BoundInputs,
): void {
  verifySource(loadedPolicy, inputs.composition.policy);
  verifyChecks(execution, inputs, scope, checks(inputs));
}

export async function runDocs(
  inputs: BoundInputs,
  options: Parameters<typeof VerifiedExecution.run>[3],
): Promise<VerifiedExecution> {
  if (inputs.paths.policy !== loadedPolicy)
    throw new Error("The live Docs runner must use the loaded policy source");
  if (options.preparation.copies.length || options.preparation.writable.length)
    throw new Error(
      "Nucleus Docs requires no candidate preparation or writable output",
    );
  const execution = await VerifiedExecution.run(
    inputs,
    scope,
    checks(inputs),
    options,
  );
  verifyDocs(execution, inputs);
  return execution;
}
