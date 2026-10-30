import { describe, expect, it, vi } from "vitest";
import { expireMaterial, STORAGE_KEY } from "./material";

const now = Date.parse("2035-01-01T00:00:00Z");
const proof = {
  version: 1,
  operation: "fdb0e5e6-2cd3-46c2-8ae3-3b4783beadbe",
  input: { mobile: "13900000000", token: "original-proof" },
  createdAt: now,
  expiresAt: now + 30 * 60 * 1000,
  retryAt: now + 5000,
  attempted: true,
};

function fixture(value: unknown = proof) {
  const records = new Map([
    [STORAGE_KEY, JSON.stringify(value)],
    ["unrelated", "keep-other-material"],
  ]);
  const storage = {
    getItem: vi.fn((key: string) => records.get(key) ?? null),
    setItem: vi.fn((key: string, bytes: string) => {
      records.set(key, bytes);
    }),
  };
  return { records, storage };
}

describe("C1 stored material lifetime", () => {
  it("preserves exact unexpired material without extending its deadline", () => {
    const { records, storage } = fixture();
    const before = records.get(STORAGE_KEY);
    expect(expireMaterial(storage, proof.expiresAt - 1)).toBe(true);
    expect(records.get(STORAGE_KEY)).toBe(before);
    expect(storage.setItem).not.toHaveBeenCalled();
  });

  it.each([proof.expiresAt, proof.expiresAt + 1, now - 1, NaN, Infinity])(
    "closes expired or unreliable time %s without retaining secrets",
    (time) => {
      const { records, storage } = fixture();
      expect(expireMaterial(storage, time)).toBe(true);
      expect(JSON.parse(records.get(STORAGE_KEY)!)).toEqual({
        version: 1,
        closed: true,
      });
      expect(records.get("unrelated")).toBe("keep-other-material");
    },
  );

  it("honors an earlier deadline supplied by the original result", () => {
    const { records, storage } = fixture({ ...proof, expiresAt: now + 5000 });
    expireMaterial(storage, now + 5000);
    expect(records.get(STORAGE_KEY)).not.toContain("original-proof");
  });

  it.each([
    { version: 1, closed: true },
    ...["sent", "failed", "unknown"].map((delivery) => ({
      version: 1,
      closed: true,
      delivery,
    })),
    { version: 1, closed: true, reason: "verification_invalid" },
  ])("preserves the known closed result %j", (closed) => {
    const { records, storage } = fixture(closed);
    expireMaterial(storage, proof.expiresAt + 1);
    expect(records.get(STORAGE_KEY)).toBe(JSON.stringify(closed));
    expect(storage.setItem).not.toHaveBeenCalled();
  });

  it.each([
    null,
    [],
    { ...proof, operation: "invalid" },
    { ...proof, input: { ...proof.input, mobile: "invalid" } },
    { ...proof, createdAt: null },
    { ...proof, expiresAt: null },
    { ...proof, expiresAt: proof.expiresAt + 1 },
    { ...proof, retryAt: null },
    { ...proof, attempted: "true" },
    { ...proof, reason: "unsupported" },
    { ...proof, closed: true },
    { version: 1, closed: true, token: "original-proof" },
    { version: 1, closed: true, delivery: "sending" },
    { version: 1, closed: true, reason: { token: "original-proof" } },
  ])("closes malformed material %j", (value) => {
    const { records, storage } = fixture(value);
    expect(expireMaterial(storage, now)).toBe(true);
    expect(records.get(STORAGE_KEY)).toBe('{"version":1,"closed":true}');
  });

  it.each(["", "{"])("closes invalid JSON %j", (bytes) => {
    const { records, storage } = fixture();
    records.set(STORAGE_KEY, bytes);
    expect(expireMaterial(storage, now)).toBe(true);
    expect(records.get(STORAGE_KEY)).toBe('{"version":1,"closed":true}');
  });

  it.each(["read", "write", "readback"])(
    "retries a temporary %s failure on a later check",
    (failure) => {
      const { records, storage } = fixture();
      const before = records.get(STORAGE_KEY);
      if (failure === "read")
        storage.getItem.mockImplementationOnce(() => {
          throw new Error("denied");
        });
      else if (failure === "write")
        storage.setItem.mockImplementationOnce(() => {
          throw new Error("denied");
        });
      else
        storage.getItem.mockReturnValueOnce(before!).mockReturnValueOnce(null);
      expect(expireMaterial(storage, proof.expiresAt)).toBe(false);
      if (failure !== "readback") expect(records.get(STORAGE_KEY)).toBe(before);
      if (failure === "read") expect(storage.setItem).not.toHaveBeenCalled();
      expect(expireMaterial(storage, proof.expiresAt)).toBe(true);
      expect(records.get(STORAGE_KEY)).toBe('{"version":1,"closed":true}');
    },
  );

  it("reads a replacement operation's own deadline after a failed cleanup", () => {
    const { records, storage } = fixture();
    storage.setItem.mockImplementationOnce(() => {
      throw new Error("denied");
    });
    expect(expireMaterial(storage, proof.expiresAt)).toBe(false);
    const later = {
      ...proof,
      operation: "560b94be-ee54-48b2-a84c-c7283802dd93",
      createdAt: proof.expiresAt,
      expiresAt: proof.expiresAt + 30 * 60 * 1000,
    };
    records.set(STORAGE_KEY, JSON.stringify(later));
    expect(expireMaterial(storage, proof.expiresAt + 1)).toBe(true);
    expect(records.get(STORAGE_KEY)).toBe(JSON.stringify(later));
  });

  it("does not initialize an operation when material is absent", () => {
    const { records, storage } = fixture();
    records.delete(STORAGE_KEY);
    expect(expireMaterial(storage, now)).toBe(true);
    expect(storage.setItem).not.toHaveBeenCalled();
    expect(records.has(STORAGE_KEY)).toBe(false);
  });
});
