import { describe, expect, it, vi } from "vitest";
import { KEY, read, remove, save, type Material, type Store } from "./material";

const sessionId = "f9bd2899-fb4d-4212-a831-15168b01e783";
const revision = "4a9b6c1d-6f5a-4182-9d2e-3f4a5b6c7d8e";
const operation = "3f8a6a6e-1b2c-4d5e-8f90-123456789abc";
const accountId = "0b6e3f8a-3c2d-4e5f-8a9b-0c1d2e3f4a5b";
const personId = "1c7f4a9b-4d3e-4f60-9b0c-1d2e3f4a5b6c";
const revisionId = "2d8a5b0c-5e4f-4071-8c1d-2e3f4a5b6c7d";
const result = {
  id: "9e2f1a3b-6c7d-4e8f-9a0b-1c2d3e4f5a6b",
  cancelledAt: "2026-10-25T12:34:56Z",
};

const prepared: Material = {
  version: 1,
  operation,
  target: { accountId, personId, revisionId },
  selection: { id: sessionId, revision },
  phase: "prepared",
  outcome: "pending",
  notice: "pending",
  retryAt: 0,
  result: null,
  hidden: false,
  clearedRevision: null,
};
const possibly: Material = { ...prepared, phase: "possibly-sent" };
const complete: Material = {
  ...possibly,
  outcome: "complete",
  notice: "pending",
  result,
};
const refused: Material = {
  ...possibly,
  outcome: "refused",
  notice: "target",
  retryAt: 0,
  result: null,
};
const unknown: Material = {
  ...possibly,
  outcome: "pending",
  notice: "mismatch",
  retryAt: 0,
  result: null,
};

function store(): Store & { values: Map<string, string> } {
  const values = new Map<string, string>();
  return {
    values,
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => {
      values.set(key, value);
    },
    removeItem: (key: string) => {
      values.delete(key);
    },
  };
}

describe("cancellation material", () => {
  it.each([
    ["prepared", prepared],
    ["possibly sent", possibly],
    ["complete", complete],
    ["hidden complete", { ...complete, hidden: true }],
    ["refused", refused],
    ["hidden refused", { ...refused, hidden: true }],
    [
      "limited refusal",
      { ...refused, notice: "limited" as const, retryAt: 1_000 },
    ],
    ["unknown", unknown],
    ["hidden unknown", { ...unknown, hidden: true }],
  ])("round-trips a %s record with exact byte readback", (_name, material) => {
    const storage = store();
    save(storage, material);
    expect(read(storage)).toEqual(material);
    expect(storage.values.get(KEY)).toBe(JSON.stringify(material));
  });

  it("distinguishes an absent record, a tombstone and a corrupt record", () => {
    const storage = store();
    expect(read(storage)).toBeNull();
    save(storage, "missing");
    expect(read(storage)).toBe("missing");
    storage.values.set(KEY, "{");
    expect(read(storage)).toBe("missing");
    storage.values.set(KEY, JSON.stringify({ ...prepared, version: 2 }));
    expect(read(storage)).toBe("missing");
  });

  it("rejects a silently failed save or removal", () => {
    const storage = store();
    const silent: Store = {
      getItem: () => null,
      setItem: () => {},
      removeItem: () => {},
    };
    expect(() => save(silent, prepared)).toThrow(
      "cancellation_storage_unavailable",
    );
    save(storage, prepared);
    const stuck: Store = {
      getItem: () => JSON.stringify(prepared),
      setItem: () => {},
      removeItem: () => {},
    };
    expect(() => remove(stuck)).toThrow("cancellation_storage_unavailable");
  });

  it.each([
    ["extra key", { ...prepared, name: "private" }],
    ["missing key", { ...prepared, retryAt: undefined }],
    ["bad operation", { ...prepared, operation: "operation" }],
    [
      "bad target ID",
      { ...prepared, target: { ...prepared.target, accountId: "account" } },
    ],
    [
      "extra target key",
      { ...prepared, target: { ...prepared.target, mobile: "13900000001" } },
    ],
    [
      "bad selection ID",
      { ...prepared, selection: { ...prepared.selection, id: "session" } },
    ],
    [
      "bad selection revision",
      {
        ...prepared,
        selection: { ...prepared.selection, revision: "revision" },
      },
    ],
    ["prepared with result", { ...prepared, result }],
    ["prepared hidden", { ...prepared, hidden: true }],
    ["complete without result", complete && { ...complete, result: null }],
    [
      "complete hidden with bad result",
      {
        ...complete,
        hidden: true,
        result: { ...result, cancelledAt: "later" },
      },
    ],
    ["refused with pending notice", { ...refused, notice: "pending" }],
    ["invalid clear revision", { ...refused, clearedRevision: "revision" }],
    [
      "prepared with clear revision",
      { ...prepared, clearedRevision: revision },
    ],
    ["unknown with a refusal notice", { ...unknown, notice: "target" }],
    ["negative retry", { ...refused, retryAt: -1 }],
    ["fractional retry", { ...refused, retryAt: 1.5 }],
    ["hidden non-boolean", { ...unknown, hidden: "yes" }],
  ])("rejects %s", (_name, value) => {
    const storage = store();
    storage.values.set(KEY, JSON.stringify(value));
    expect(read(storage)).toBe("missing");
  });

  it("does not invent an expiry for a known completed result", () => {
    const storage = store();
    save(storage, {
      ...complete,
      result: { ...result, cancelledAt: "2020-01-01T00:00:00Z" },
    });
    expect(read(storage)).toEqual({
      ...complete,
      result: { ...result, cancelledAt: "2020-01-01T00:00:00Z" },
    });
  });

  it("propagates an unavailable storage read to the controller", () => {
    const storage: Store = {
      getItem: vi.fn(() => {
        throw new Error("blocked");
      }),
      setItem: () => {},
      removeItem: () => {},
    };
    expect(() => read(storage)).toThrow("blocked");
  });
});
