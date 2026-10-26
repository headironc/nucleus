export type Ledger = {
  bot: bigint;
  issue: bigint;
  issueId: string;
};
export type Protection = {
  updateRule: bigint;
  rules: bigint[];
};
export type MergeDeployment = {
  ledger: Ledger;
  main: Protection;
  dev: Protection;
};

// Both targets share one operation history. Enabling is a reviewed deployment.
export const mergeDeployment: MergeDeployment | null = null;
export const repository = "headironc/nucleus";
export const repositoryId = 1432619931n;
export const ownerId = 19648989n;
export const appId = 3104501n;
export const workflow = ".github/workflows/main-promotion.yml";
export const devWorkflow = ".github/workflows/dev-admission.yml";

export function deployment(): MergeDeployment {
  const value = mergeDeployment;
  if (value === null)
    throw new Error("Personal trusted merging is not deployed");
  return value;
}
