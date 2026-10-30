import { reloadRequired } from "../integrity";

export async function currentBuild() {
  if (reloadRequired()) return false;
  const response = await fetch("/build.json", {
    cache: "no-store",
    credentials: "omit",
    redirect: "error",
    signal: AbortSignal.timeout(5000),
  });
  if (
    !response.ok ||
    response.headers.get("content-type")?.split(";")[0] !== "application/json"
  )
    return false;
  try {
    const value = await response.json();
    return (
      !reloadRequired() &&
      value !== null &&
      typeof value === "object" &&
      !Array.isArray(value) &&
      value.buildId === __BUILD_ID__
    );
  } catch {
    return false;
  }
}
