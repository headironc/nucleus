import { describe, expect, it } from "vitest";
import { avatarOrigin } from "./origin";

describe("avatar media origin", () => {
  it.each([
    ["https://example.com", "https://example.com"],
    ["https://example.com/", "https://example.com"],
    ["https://example.com:443", "https://example.com"],
    ["https://example.com:8443/", "https://example.com:8443"],
    ["https://[::1]:443/", "https://[::1]"],
  ])("accepts %s as %s", (value, expected) => {
    expect(avatarOrigin(value)).toBe(expected);
  });

  it.each([
    undefined,
    "",
    "https://example.com/a/..",
    "https://example.com/%2e",
    "https://@example.com",
    "https://user@example.com",
    "https://example.com//",
    "https://example.com?",
    "https://example.com#",
    "https://example.com\\",
    "https://%65xample.com",
    "https://example.com:",
    " https://example.com",
    "https://example.com ",
    "https:/example.com",
    "http://example.com",
    "https://*.example.com",
    "https://example.com:99999",
    "https://exa\tmple.com",
    "https://avatars.example;upgrade-insecure-requests",
    "https://avatars.example,other",
  ])("rejects %s", (value) => {
    expect(() => avatarOrigin(value)).toThrow(
      "HALO_AVATAR_ORIGIN must be an HTTPS origin without credentials, a path, query or fragment",
    );
  });

  it("does not echo supplied input in the failure", () => {
    expect(() => avatarOrigin("https://secret.invalid/path")).toThrow();
    try {
      avatarOrigin("https://secret.invalid/path");
    } catch (error) {
      expect(String(error)).not.toContain("secret.invalid");
    }
  });
});
