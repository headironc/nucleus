// Enabling promotion is a separately reviewed deployment change, never an input.
export type PromotionDeployment = {
  bot: bigint;
  issue: bigint;
  issueId: string;
  updateRule: bigint;
  rules: bigint[];
};

export const promotionDeployment: PromotionDeployment | null = null;
export const repository = "headironc/nucleus";
export const repositoryId = 1432619931n;
export const ownerId = 19648989n;
export const appId = 3104501n;
export const workflow = ".github/workflows/main-promotion.yml";

export function deployment(): PromotionDeployment {
  const value = promotionDeployment;
  if (value === null)
    throw new Error("Personal main promotion is not deployed");
  return value;
}
