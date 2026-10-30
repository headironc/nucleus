import * as vitest from "vitest";
import * as material from "./material";

const localId = `4ef1d157-e547-4d18-bf84-45a159709a4b`;
const account = `019354fd-933c-7e6a-8912-345678901234`;
const session = `ffffffff-ffff-ffff-ffff-ffffffffffff`;
const revokedAt = `2026-10-23T00:00:00Z`;
const selection = { id: session, revision: localId };
const pending: material.AllMaterial = {
  version: 1,
  scope: `all`,
  operation: localId,
  account,
  selection,
  outcome: `pending`,
  notice: `pending`,
  binding: "pending",
  count: null,
  record: null,
  retryAt: 0,
  revokedAt: null,
};
const record = { count: 2, revokedAt };
const complete: material.AllMaterial = {
  ...pending,
  outcome: `complete`,
  binding: "matched",
  count: 2,
  revokedAt,
};
const unverified: material.AllMaterial = {
  ...pending,
  notice: `mismatch`,
  binding: "mismatch",
  record,
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

vitest.describe(`all-scope logout material`, () => {
  vitest.it.each([pending, complete, unverified])(
    `round-trips a strictly consistent record %#`,
    (value) => {
      const store = storage();
      material.saveAll(store, value);
      vitest.expect(material.readAll(store)).toEqual(value);
    },
  );
  vitest.it.each([
    { ...pending, extra: 1 },
    { ...pending, version: 2 },
    { ...pending, scope: `current` },
    { ...pending, operation: `not-an-id` },
    { ...pending, account: `not-an-id` },
    { ...pending, selection: { ...selection, revision: account } },
    { ...pending, outcome: `done` },
    { ...pending, notice: `done` },
    { ...pending, binding: `yes` },
    { ...pending, count: 0 },
    { ...pending, count: -1 },
    { ...pending, count: 1.5 },
    { ...pending, count: 1 },
    { ...pending, revokedAt },
    { ...complete, binding: "mismatch" },
    { ...complete, count: null },
    { ...complete, revokedAt: null },
    { ...complete, revokedAt: "October 23, 2026" },
    { ...complete, record },
    { ...unverified, binding: "matched" },
    { ...unverified, record: { ...record, count: 0 } },
    { ...unverified, record: { ...record, revokedAt: `yesterday` } },
    { ...unverified, record: { ...record, extra: 1 } },
  ])(`closes a malformed or contradictory record %j`, (value) => {
    const store = storage();
    store.setItem(material.ALL_KEY, JSON.stringify(value));
    vitest.expect(material.readAll(store)).toBe(`missing`);
  });
  vitest.it(
    `uses a scope-specific key and does not read current material`,
    () => {
      const store = storage();
      const current: material.Material = {
        version: 1,
        operation: localId,
        account,
        selection,
        outcome: `pending`,
        notice: `pending`,
        retryAt: 0,
        revokedAt: null,
      };
      material.save(store, current);
      vitest.expect(material.readAll(store)).toBeNull();
      material.saveAll(store, pending);
      vitest.expect(material.read(store)).toEqual(current);
      vitest.expect(material.ALL_KEY).not.toBe(material.KEY);
    },
  );
  vitest.it(`requires a successful first write and complete removal`, () => {
    const store = storage();
    store.setItem = () => {};
    store.getItem = () => `different`;
    vitest.expect(() => material.saveAll(store, pending)).toThrow();
    store.removeItem = () => {};
    store.getItem = () => JSON.stringify(pending);
    vitest.expect(() => material.removeAll(store)).toThrow();
  });
});
