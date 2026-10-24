import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { currentBuild } from "./build";

const buildId = "known-client-build";
const fetchBuild = vi.fn<typeof fetch>();

beforeEach(() => {
  vi.stubGlobal("__BUILD_ID__", buildId);
  vi.stubGlobal("window", { __halo: { reloadRequired: false } });
  vi.stubGlobal("fetch", fetchBuild);
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  fetchBuild.mockReset();
});

describe("current website build check", () => {
  it("matches the loaded build with an anonymous uncached read bounded to five seconds", async () => {
    fetchBuild.mockResolvedValue(Response.json({ buildId }));
    const timeout = vi.spyOn(AbortSignal, "timeout");
    expect(await currentBuild()).toBe(true);
    expect(fetchBuild).toHaveBeenCalledExactlyOnceWith("/build.json", {
      cache: "no-store",
      credentials: "omit",
      redirect: "error",
      signal: expect.any(AbortSignal),
    });
    expect(timeout).toHaveBeenCalledExactlyOnceWith(5000);
    expect(fetchBuild.mock.calls[0][1]?.signal).toBe(timeout.mock.results[0].value);
    expect(window.__halo?.reloadRequired).toBe(false);
  });

  it("accepts JSON media parameters and unrelated fields without widening the identity match", async () => {
    fetchBuild.mockResolvedValue(new Response(
      JSON.stringify({ buildId, description: "public metadata" }),
      { headers: { "Content-Type": "application/json; charset=utf-8" } },
    ));
    expect(await currentBuild()).toBe(true);
  });

  it.each([
    { buildId: "newer-build" },
    { buildId: 42 },
    {},
    null,
    [{ buildId }],
    buildId,
  ])("rejects mismatched or unusable metadata %j", async (body) => {
    fetchBuild.mockResolvedValue(Response.json(body));
    expect(await currentBuild()).toBe(false);
    expect(window.__halo?.reloadRequired).toBe(false);
  });

  it.each([null, "text/html", "text/plain", "APPLICATION/JSON"])(
    "rejects the existing unsupported media shape %s before reading the body",
    async (media) => {
      const response = Response.json({ buildId });
      if (media === null) response.headers.delete("content-type");
      else response.headers.set("content-type", media);
      const json = vi.spyOn(response, "json");
      fetchBuild.mockResolvedValue(response);
      expect(await currentBuild()).toBe(false);
      expect(json).not.toHaveBeenCalled();
    },
  );

  it("does not trust build metadata in an unsuccessful HTTP response", async () => {
    const response = Response.json({ buildId }, { status: 503 });
    const json = vi.spyOn(response, "json");
    fetchBuild.mockResolvedValue(response);
    expect(await currentBuild()).toBe(false);
    expect(json).not.toHaveBeenCalled();
  });

  it("fails closed when the JSON body is malformed or cannot be read", async () => {
    fetchBuild.mockResolvedValueOnce(new Response("{", {
      headers: { "Content-Type": "application/json" },
    }));
    expect(await currentBuild()).toBe(false);
    const response = Response.json({ buildId });
    vi.spyOn(response, "json").mockRejectedValue(new Error("body disconnected"));
    fetchBuild.mockResolvedValueOnce(response);
    expect(await currentBuild()).toBe(false);
  });

  it.each([undefined, { reloadRequired: true }])(
    "does not fetch when the early resource observer requires reload: %j",
    async (observer) => {
      window.__halo = observer;
      expect(await currentBuild()).toBe(false);
      expect(fetchBuild).not.toHaveBeenCalled();
    },
  );

  it("keeps a resource failure raised while reading a matching response as a reload barrier", async () => {
    let resolve!: (value: unknown) => void;
    const body = new Promise((done) => { resolve = done; });
    const response = Response.json({ buildId });
    const json = vi.spyOn(response, "json").mockReturnValue(body);
    fetchBuild.mockResolvedValue(response);
    const check = currentBuild();
    await vi.waitFor(() => expect(json).toHaveBeenCalledOnce());
    window.__halo = { reloadRequired: true };
    resolve({ buildId });
    expect(await check).toBe(false);
  });

  it.each([
    new TypeError("network disconnected"),
    new DOMException("deadline reached", "TimeoutError"),
  ])("leaves fetch rejection to its caller: %s", async (failure) => {
    fetchBuild.mockRejectedValue(failure);
    await expect(currentBuild()).rejects.toBe(failure);
    expect(fetchBuild).toHaveBeenCalledOnce();
  });
});
