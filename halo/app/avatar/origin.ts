import { createContext, RouterContextProvider } from "react-router";

/** Deployer-supplied avatar media origin, validated once at Node startup. */
export const avatarOriginContext = createContext<string>();

const ERROR =
  "HALO_AVATAR_ORIGIN must be an HTTPS origin without credentials, a path, query or fragment";

/**
 * Accept the same canonical origins as the Avatar link owner. URL owns host
 * and port syntax; this gate rejects shapes the parser would silently repair.
 */
export function avatarOrigin(value: string | undefined): string {
  if (!value || value !== value.trim()) throw new Error(ERROR);
  if (
    value.includes("*") ||
    [...value].some((character) => {
      const code = character.codePointAt(0) ?? 0;
      return code <= 0x20 || code === 0x7f;
    })
  )
    throw new Error(ERROR);
  const separator = value.indexOf("://");
  if (
    separator < 0 ||
    !value
      .slice(0, separator)
      .toLowerCase()
      .match(/^https$/)
  )
    throw new Error(ERROR);
  const raw = value.slice(separator + 3);
  const authority = raw.endsWith("/") ? raw.slice(0, -1) : raw;
  // URL accepts these separators in a host, but CSP interprets them as
  // directive or policy boundaries rather than part of one source origin.
  if (!authority || authority.endsWith(":") || /[/@?#\\%;,]/.test(authority))
    throw new Error(ERROR);
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    throw new Error(ERROR);
  }
  if (
    url.protocol !== "https:" ||
    !url.hostname ||
    url.username !== "" ||
    url.password !== "" ||
    url.pathname !== "/" ||
    url.search !== "" ||
    url.hash !== ""
  )
    throw new Error(ERROR);
  return url.origin;
}

/** Build the per-request React Router load context from the one startup value. */
export function createLoadContext(value: string | undefined) {
  const origin = avatarOrigin(value);
  return () => {
    const context = new RouterContextProvider();
    context.set(avatarOriginContext, origin);
    return context;
  };
}
