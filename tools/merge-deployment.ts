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

// Both targets share one operation history. Platform activation is separate.
export const mergeDeployment: MergeDeployment | null = {
  ledger: {
    bot: 268568838n,
    issue: 16n,
    issueId: "I_kwDOVWQLm88AAAABZ-6hkw",
  },
  main: {
    updateRule: 25874774n,
    rules: [25798637n, 25798669n, 25874774n],
  },
  dev: {
    updateRule: 25874777n,
    rules: [25798637n, 25798667n, 25874777n],
  },
};
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
