import { describe, expect, it } from "vitest";
import { KEY, read, save, type Material } from "./material";

const localId = "4ef1d157-e547-4d18-bf84-45a159709a4b";
const original: Material = {
  version: 1,
  operation: localId,
  account: "019354fd-933c-7e6a-8912-345678901234",
  selection: { id: "ffffffff-ffff-ffff-ffff-ffffffffffff", revision: localId },
  outcome: "pending",
  notice: "pending",
  retryAt: 0,
  revokedAt: null,
};

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
  };
}

describe("retained logout identities", () => {
  for (const field of ["outcome", "notice"] as const) {
    it.each([null, 0, true, {}, [], ["pending"], ["rejected"], ["denied"]])(
      `rejects a non-string ${field} from JSON storage: %j`,
      (value) => {
        const store = storage();
        store.setItem(KEY, JSON.stringify({ ...original, [field]: value }));
        expect(read(store)).toBe("missing");
      },
    );
  }
  it.each([
    "019354fd-933c-7e6a-8912-345678901234",
    "00000000-0000-0000-0000-000000000000",
    "ffffffff-ffff-ffff-ffff-ffffffffffff",
  ])("accepts the server UUID value %s for both identity owners", (id) => {
    const store = storage();
    const material = {
      ...original,
      account: id,
      selection: { ...original.selection, id },
    };
    save(store, material);
    expect(read(store)).toEqual(material);
  });
  it.each([null, 7, {}, "not-an-id", "019354fd-933c-7e6a-8912-34567890123z"])(
    "rejects malformed retained server identities: %j",
    (id) => {
      const store = storage();
      for (const material of [
        { ...original, account: id },
        { ...original, selection: { ...original.selection, id } },
      ]) {
        store.setItem(KEY, JSON.stringify(material));
        expect(read(store)).toBe("missing");
      }
    },
  );
  it("keeps the local random operation and selection revision distinct from server identity parsing", () => {
    const store = storage();
    for (const material of [
      { ...original, operation: original.account },
      {
        ...original,
        selection: { ...original.selection, revision: original.account },
      },
    ]) {
      store.setItem(KEY, JSON.stringify(material));
      expect(read(store)).toBe("missing");
    }
  });
});
