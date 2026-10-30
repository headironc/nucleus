export type ReturnIntent =
  | `logout-result`
  | `all-logout-result`
  | `service-termination`
  | `mobile-change`;

/** The only accepted login returns are fixed, non-identifying local intents. */
export function returnIntent(search: string): ReturnIntent | null {
  const query = new URLSearchParams(search);
  const values = query.getAll(`return`);
  if (values.length !== 1) return null;
  return values[0] === `logout-result` ||
    values[0] === `all-logout-result` ||
    values[0] === `service-termination` ||
    values[0] === `mobile-change`
    ? values[0]
    : null;
}

export function logoutReturn(search: string): boolean {
  return returnIntent(search) === `logout-result`;
}

export function allLogoutReturn(search: string): boolean {
  return returnIntent(search) === `all-logout-result`;
}

export function terminationReturn(search: string): boolean {
  return returnIntent(search) === `service-termination`;
}

export function mobileChangeReturn(search: string): boolean {
  return returnIntent(search) === `mobile-change`;
}

export function loginPath(
  path: `/authentication/codes` | `/authentication/login`,
  search: string,
): string {
  const intent = returnIntent(search);
  return intent ? `${path}?return=${intent}` : path;
}
