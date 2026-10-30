import { describe, expect, it } from "vitest";
import {
  KEY,
  read,
  remove,
  save,
  type Active,
  type Material,
} from "./termination-material";

const operation = "4ef1d157-e547-4d18-bf84-45a159709a4b";
const account = "019354fd-933c-7e6a-8912-345678901234";
const person = "018f3c1a-0000-7000-8000-000000000001";
const grant = "018f3c1a-0000-7000-8000-000000000002";
const revision = operation;
const selection = {
  id: "ffffffff-ffff-ffff-ffff-ffffffffffff",
  revision,
};
const active: Active = {
  operation,
  account,
  person,
  grant,
  selection,
  outcome: "pending",
  notice: "pending",
  sent: true,
  retryAt: 0,
};
const material: Material = { version: 1, known: [], active };

function storage() {
  const values = new Map<string, string>();
  return {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => {
      values.set(key, value);
    },
    removeItem: (key: string) => {
      values.delete(key);
    },
    values,
  };
}

describe("retained termination material", () => {
  it("allows completed records from other accounts but rejects ambiguous account duplicates", () => {
    const store = storage();
    const result = {
      operation,
      account: "00000000-0000-4000-8000-000000000001",
      person: "00000000-0000-4000-8000-000000000002",
      grant,
      terminatedAt: "2026-10-24T00:00:00Z",
    };
    const value: Material = { ...material, known: [result] };
    save(store, value);
    expect(read(store)).toEqual(value);
    store.setItem(KEY, JSON.stringify({ ...value, known: [result, { ...result, grant: person }] }));
    expect(read(store)).toBe("missing");
  });

  it("round-trips one active original with a completed known result", () => {
    const store = storage();
    const value: Material = {
      version: 1,
      known: [{
        operation: "6ee1d157-e547-4d18-bf84-45a159709a4c",
        account,
        person,
        grant: "018f3c1a-0000-7000-8000-000000000003",
        terminatedAt: "2026-10-24T00:00:00Z",
      }],
      active: { ...active, outcome: "rejected", notice: "limited", retryAt: 10 },
    };
    save(store, value);
    expect(read(store)).toEqual(value);
    remove(store);
    expect(read(store)).toBeNull();
  });
  it("treats a record with neither result as no material", () => {
    const store = storage();
    save(store, { version: 1, known: [], active: null });
    expect(read(store)).toBeNull();
  });
  it.each(["account", "person", "grant"] as const)(
    "accepts any server UUID version for %s",
    (field) => {
      const store = storage();
      const value = {
        ...material,
        active: { ...active, [field]: account },
      };
      save(store, value);
      expect(read(store)).toEqual(value);
    },
  );
  it.each([null, 7, {}, "", "not-an-id", "019354fd-933c-7e6a-8912-34567890123z"])(
    "rejects malformed retained identities: %j",
    (value) => {
      const store = storage();
      for (const record of [
        { ...material, active: { ...active, account: value } },
        { ...material, active: { ...active, person: value } },
        { ...material, active: { ...active, grant: value } },
        {
          ...material,
          active: { ...active, selection: { ...selection, id: value } },
        },
      ]) {
        store.setItem(KEY, JSON.stringify(record));
        expect(read(store)).toBe("missing");
      }
    },
  );
  it.each([account, "00000000-0000-1000-8000-000000000001"])(
    "requires the local operation and selection revision to be UUIDv4: %s",
    (value) => {
      const store = storage();
      for (const record of [
        { ...material, active: { ...active, operation: value } },
        {
          ...material,
          active: { ...active, selection: { ...selection, revision: value } },
        },
      ]) {
        store.setItem(KEY, JSON.stringify(record));
        expect(read(store)).toBe("missing");
      }
    },
  );
  it.each([
    { ...material, active: { ...active, notice: "unknown" } },
    { ...material, active: { ...active, outcome: "complete" } },
    { ...material, active: { ...active, sent: "yes" } },
    { ...material, active: { ...active, retryAt: -1 } },
    { ...material, active: { ...active, retryAt: 1.5 } },
    { ...material, active: { ...active, sent: false, notice: "denied" } },
    { ...material, active: { ...active, outcome: "rejected", notice: "denied" } },
    { ...material, active: { ...active, notice: "mismatch", sent: false } },
    {
      ...material,
      known: [{
        operation,
        account,
        person,
        grant,
        terminatedAt: "not-a-time",
      }],
    },
    {
      ...material,
      active: { ...active, person: "00000000-0000-4000-8000-000000000001" },
      known: [{
        operation,
        account,
        person,
        grant,
        terminatedAt: "2026-10-24T00:00:00Z",
      }],
    },
  ])("rejects an inconsistent record %#", (value) => {
    const store = storage();
    store.setItem(KEY, JSON.stringify(value));
    expect(read(store)).toBe("missing");
  });
  it.each([
    "{}",
    "null",
    "[]",
    JSON.stringify({ version: 2, known: null, active: null }),
    JSON.stringify({ version: 1, known: null }),
    JSON.stringify({ ...material, extra: true }),
  ])("treats a damaged record as missing: %s", (bytes) => {
    const store = storage();
    store.setItem(KEY, bytes);
    expect(read(store)).toBe("missing");
  });
  it("fails a silent or read-back storage refusal", () => {
    const silent = {
      getItem: () => null,
      setItem: () => {},
      removeItem: () => {},
    };
    expect(() => save(silent, material)).toThrow(
      "termination_storage_unavailable",
    );
    expect(() =>
      remove({ ...silent, getItem: () => "stale" }),
    ).toThrow("termination_storage_unavailable");
  });
});
