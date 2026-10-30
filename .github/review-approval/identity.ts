export const repository = "headironc/nucleus";
export const repositoryId = 1432619931;
export const ownerId = 19648989;
export const appId = 3104501;
export const installationId = 175475891;
export const actionsId = 15368;
export const cubic = { app: 1082092, bot: 191113872 };
export const codex = { app: 1144995, bot: 199175422 };

export function requireThat(
  condition: unknown,
  gap: string,
): asserts condition {
  if (!condition) throw new Error(gap);
}
export function sha(value: unknown): value is string {
  return typeof value === "string" && /^[a-f0-9]{40}$/.test(value);
}
export function numericId(value: unknown): number {
  requireThat(
    typeof value === "number" ||
      (typeof value === "string" && /^[1-9][0-9]*$/.test(value)),
    "Missing database identity",
  );
  const id = Number(value);
  requireThat(
    Number.isSafeInteger(id) && id > 0,
    "Database identity cannot be represented exactly",
  );
  return id;
}
export type Binding = { pr: number; head: string; base: string; merge: string };
export function requestMarker(b: Binding) {
  return `<!-- cladiron-request:v1 head=${b.head} base=${b.base} target=dev -->`;
}
export function requestBodies(b: Binding) {
  return [
    "@cubic-dev-ai review this PR",
    "@codex review",
    "@codex security review",
  ].map((command) => `${command}\n\n${requestMarker(b)}`);
}
