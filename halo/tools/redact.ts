// Sanitize browser diagnostics before they leave the test process. The
// evidence is uploaded, so it must stay useful without carrying credentials,
// signed links or the fictional private material used by the fixtures.

const DEFAULT_LIMIT = 8_000;
const HARD_LIMIT = 64_000;

const SENSITIVE_HEADER =
  /((?:\\?["'])?(?:authorization|proxy-authorization|cookie|set-cookie|x-api-key|x-auth-token)(?:\\?["'])?\s*[:=]\s*)(?:\\+"(?:\\.|[^"\\])*?\\+"(?=\s*[,}\]\r\n;]|$)|"(?:\\.|[^"\\])*"(?=\s*[,}\]\r\n;]|$)|'(?:\\.|[^'\\])*'(?=\s*[,}\]\r\n;]|$)|[^\r\n]*)/gi;

// Keys rather than free text, so a diagnostic such as
// `Expected: "code"` keeps its assertion wording. Cookie-object diffs use the
// generic `value` key, so its contents are conservatively treated as secret.
// Anchor at the key boundary: retrying a greedy key at every character makes
// long ordinary assertion values quadratic to sanitize.
const KEY =
  /(?<![A-Za-z0-9_.-])(?:\\?["'])?([A-Za-z0-9_.-]+)(?:\\?["'])?\s*[:=]\s*/gi;
const VALUE =
  /(?:\\+"(?:\\.|[^"\\])*?\\+"(?=\s*[,}\]\r\n;]|$)|"(?:\\.|[^"\\])*"(?=\s*[,}\]\r\n;]|$)|'(?:\\.|[^'\\])*'(?=\s*[,}\]\r\n;]|$)|[^\s,;&}\]]+)/y;
const SENSITIVE_KEY_NAME =
  /token|secret|password|passwd|credential|api[-_]?key|private[-_]?key|session[-_]?id|signature|^value$|^__Host-nucleus-session-/i;

const AUTH_SCHEME = /\b(Bearer|Basic)\s+[A-Za-z0-9._~+/=-]+/g;
const PRIVATE_KEY =
  /-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z0-9 ]*PRIVATE KEY-----/g;
const JWT = /\beyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+/g;
// The browser fixtures use these fictional mobiles. Their values are not
// credentials, but they are the private material the suite creates per run.
const FIXTURE_MOBILE = /\b139\d{8}\b/g;
// Match both ordinary URLs and the escaped form that appears inside JSON
// diagnostics. The fragment is removed entirely below, so a token cannot
// survive merely because it was placed after `#`.
const URL_PATTERN = /https?:(?:\\?\/){2}[^\s"')}\]]+/gi;

const SENSITIVE_QUERY =
  /(?:token|secret|signature|sig|credential|password|passwd|session|auth|expires|key|^code$)/i;

function decoded(name: string): string | null {
  try {
    return decodeURIComponent(name.replace(/\+/g, " "));
  } catch {
    return null;
  }
}

function redactUrl(raw: string): string {
  const url = raw.replace(/\\\//g, "/");
  const fragment = url.indexOf("#");
  const withoutFragment = fragment === -1 ? url : url.slice(0, fragment);
  const separator = withoutFragment.indexOf("?");
  const path = separator === -1 ? withoutFragment : withoutFragment.slice(0, separator);
  const query = separator === -1 ? "" : withoutFragment.slice(separator + 1);
  const authority = path.replace(
    /^https?:\/\/[^/]*@/i,
    (match) => `${match.slice(0, match.indexOf("//") + 2)}<redacted>@`,
  );
  if (!query) return authority;
  if (/\/api\/v1\/avatars\//i.test(authority))
    return `${authority}?<redacted-query>`;
  const kept = query.split("&").map((pair) => {
    const equals = pair.indexOf("=");
    if (equals === -1) return pair ? "<redacted>" : pair;
    const name = decoded(pair.slice(0, equals));
    if (name === null || SENSITIVE_QUERY.test(name))
      return `${pair.slice(0, equals)}=<redacted>`;
    return pair;
  });
  return `${authority}?${kept.join("&")}`;
}

export type RedactedText = {
  text: string;
  redactions: number;
  truncated: boolean;
};

export function redactText(
  input: unknown,
  limit = DEFAULT_LIMIT,
): RedactedText {
  let text = typeof input === "string" ? input : String(input ?? "");
  // Playwright's assertion formatter colors the expected/received values.
  // Strip ANSI so the operator and values remain readable in evidence.
  text = text.replace(/\u001b\[[0-9;]*m/g, "");
  let truncated = false;
  let redactions = 0;
  const replace = (
    pattern: RegExp,
    replacement: string | ((...args: any[]) => string),
  ) => {
    text = text.replace(pattern, (...args: any[]) => {
      redactions += 1;
      return typeof replacement === "string"
        ? replacement.replace(/\$(\d)/g, (_, index: string) => String(args[Number(index)] ?? ""))
        : replacement(...args);
    });
  };

  replace(SENSITIVE_HEADER, "$1<redacted>");
  const parts: string[] = [];
  let copied = 0;
  KEY.lastIndex = 0;
  for (let key = KEY.exec(text); key; key = KEY.exec(text)) {
    if (!SENSITIVE_KEY_NAME.test(key[1]!)) continue;
    VALUE.lastIndex = KEY.lastIndex;
    const value = VALUE.exec(text);
    if (!value) continue;
    parts.push(text.slice(copied, KEY.lastIndex), "<redacted>");
    copied = VALUE.lastIndex;
    KEY.lastIndex = copied;
    redactions += 1;
  }
  parts.push(text.slice(copied));
  text = parts.join("");
  replace(AUTH_SCHEME, "$1 <redacted>");
  replace(PRIVATE_KEY, "<redacted-private-key>");
  replace(JWT, "<redacted-jwt>");
  replace(FIXTURE_MOBILE, "<redacted-phone>");
  replace(URL_PATTERN, (match) => redactUrl(match));

  // Sanitize the complete input before shortening it. Truncating first could
  // leave a credential's prefix on the evidence artifact.
  if (text.length > HARD_LIMIT) {
    text = text.slice(0, HARD_LIMIT);
    truncated = true;
  }
  if (text.length > limit) {
    text = `${text.slice(0, limit)}…[truncated]`;
    truncated = true;
  }
  return { text, redactions, truncated };
}
