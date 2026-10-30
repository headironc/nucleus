import { afterEach, describe, expect, it, vi } from "vitest";
import { directories } from "./directories";

afterEach(() => vi.unstubAllGlobals());
const selection = { code: "ok", groups: [], departments: [], positions: [], categories: [], disciplines: [], titles: [], rules: [], missing: [100] };
describe("existing member directory protocol", () => {
  it("reads the member aggregate with the selected session, leaving missing configuration explicit", async () => {
    const fetch = vi.fn(async () => Response.json(selection));
    vi.stubGlobal("fetch", fetch);
    expect(await directories("session", new AbortController().signal)).toEqual({ kind: "directories", selection });
    expect(fetch).toHaveBeenCalledWith("/api/v1/workforce/directories", expect.objectContaining({
      credentials: "same-origin", cache: "no-store", redirect: "error", headers: expect.objectContaining({ "Session-Id": "session", Accept: "application/json" }),
    }));
  });
  it("does not convert invalid content or unavailable configuration into an empty directory", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => Response.json({ code: "ok", groups: [] })));
    expect(await directories("session", new AbortController().signal)).toEqual({ kind: "untrusted" });
    vi.stubGlobal("fetch", vi.fn(async () => Response.json({ code: "dependency_unavailable" }, { status: 503 })));
    expect(await directories("session", new AbortController().signal)).toMatchObject({ kind: "failure", status: 503, code: "dependency_unavailable" });
  });
  it("preserves permission loss and retry-after for the owning controller", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => Response.json({ code: "permission_denied" }, { status: 403 })));
    expect(await directories("session", new AbortController().signal)).toMatchObject({ kind: "failure", status: 403 });
    vi.stubGlobal("fetch", vi.fn(async () => Response.json({ code: "rate_limited" }, { status: 429, headers: { "Retry-After": "8" } })));
    expect(await directories("session", new AbortController().signal)).toMatchObject({ kind: "failure", retryAfter: 8000 });
  });
});
