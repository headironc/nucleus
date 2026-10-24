import { describe, expect, it } from "vitest";
import {
  KEY,
  LIFETIME,
  expired,
  maintain,
  messageDeadline,
  read,
  remove,
  save,
  type Active,
  type Material,
} from "./material";

const operation = "4ef1d157-e547-4d18-bf84-45a159709a4b";
const account = "019354fd-933c-7e6a-8912-345678901234";
const person = "018f3c1a-0000-7000-8000-000000000001";
const message = "018f3c1a-0000-7000-8000-000000000002";
const transfer = "018f3c1a-0000-7000-8000-000000000003";
const selection = {
  id: "ffffffff-ffff-ffff-ffff-ffffffffffff",
  revision: operation,
};
const createdAt = 1_800_000_000_000;
const expiresAt = createdAt + LIFETIME;

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

function active(step: Active["step"], extra: Partial<Active> = {}): Active {
  return {
    step,
    account,
    person,
    selection,
    createdAt,
    expiresAt,
    retryAt: 0,
    attempted: false,
    ...extra,
  };
}

function material(
  value: Active | null,
  known: Material["known"] = [],
): Material {
  return { version: 1, known, active: value, closed: null };
}

const known = {
  account,
  person,
  transfer,
  completedAt: "2026-10-24T12:00:00Z",
};

describe("mobile change material", () => {
  it("requires original attempted proof for a persisted sending result", () => {
    const store = storage();
    for (const step of ["current-send", "new-send"] as const) {
      const pending = active(step, {
        operation,
        attempted: true,
        token: "original-proof",
        delivery: "sending",
        ...(step === "current-send"
          ? { currentMobile: "13900000000" }
          : { transfer, newMobile: "13800000000" }),
      });
      store.setItem(KEY, JSON.stringify(material(pending)));
      expect(read(store)).toEqual(material(pending));
      for (const changed of [
        { operation: undefined },
        { token: undefined },
        { attempted: false },
        { delivery: "sent" },
        step === "current-send"
          ? { currentMobile: undefined }
          : { newMobile: undefined },
      ]) {
        store.setItem(
          KEY,
          JSON.stringify(material({ ...pending, ...changed } as Active)),
        );
        expect(read(store)).toBe("missing");
      }
    }
  });

  it("round-trips every stage with only its own facts", () => {
    const store = storage();
    const values: Active[] = [
      active("current-send", {
        operation,
        currentMobile: "13900000000",
        token: "candidate",
        attempted: true,
      }),
      active("current-code", {
        operation,
        currentMobile: "13900000000",
        messageId: message,
        messageExpiresAt: expiresAt,
        delivery: "sent",
      }),
      active("establish", {
        operation,
        currentMobile: "13900000000",
        messageId: message,
        messageExpiresAt: expiresAt,
        delivery: "sent",
        code: "012345",
        attempted: true,
      }),
      active("transfer", {
        currentMobile: "13900000000",
        transfer,
        transferExpiresAt: expiresAt,
      }),
      active("new-send", {
        operation,
        transfer,
        transferExpiresAt: expiresAt,
        currentMobile: "13900000000",
        newMobile: "13800000000",
        token: "candidate",
        attempted: true,
      }),
      active("new-code", {
        operation,
        transfer,
        transferExpiresAt: expiresAt,
        currentMobile: "13900000000",
        newMobile: "13800000000",
        messageId: message,
        messageExpiresAt: expiresAt,
        delivery: "sending",
      }),
      active("complete", {
        operation,
        transfer,
        transferExpiresAt: expiresAt,
        currentMobile: "13900000000",
        newMobile: "13800000000",
        messageId: message,
        messageExpiresAt: expiresAt,
        code: "012345",
        attempted: true,
      }),
    ];
    for (const value of values) {
      const record = material(value, [known]);
      save(store, record);
      expect(read(store)).toEqual(record);
    }
  });

  it("rejects damaged, extended and cross-stage records as missing", () => {
    const store = storage();
    const valid = material(
      active("complete", {
        operation,
        transfer,
        transferExpiresAt: expiresAt,
        currentMobile: "13900000000",
        newMobile: "13800000000",
        messageId: message,
        code: "012345",
      }),
    );
    save(store, valid);
    expect(read(store)).toEqual(valid);
    for (const value of [
      { ...valid, extra: true },
      { ...valid, active: { ...valid.active, extra: true } },
      { ...valid, active: { ...valid.active, code: "12345" } },
      { ...valid, active: { ...valid.active, code: "1234567" } },
      { ...valid, active: { ...valid.active, operation: "not-a-uuid" } },
      { ...valid, active: { ...valid.active, messageId: "not-a-uuid" } },
      { ...valid, active: { ...valid.active, newMobile: "+8613800000000" } },
      { ...valid, active: { ...valid.active, expiresAt: expiresAt + 1 } },
      { ...valid, active: { ...valid.active, expiresAt: createdAt } },
      { ...valid, active: { ...valid.active, step: "transfer" as const } },
      { ...valid, active: { ...valid.active, token: "x".repeat(2049) } },
      { ...valid, known: [{ ...known, completedAt: "tomorrow" }] },
    ])
      store.setItem(KEY, JSON.stringify(value));
    expect(read(store)).toBe("missing");
  });

  it("keeps absent and closed placeholders distinct from a damaged record", () => {
    const store = storage();
    expect(read(store)).toBeNull();
    store.setItem(KEY, "{");
    expect(read(store)).toBe("missing");
    const closed = {
      version: 1 as const,
      known: [],
      active: null,
      closed: "unknown" as const,
    };
    save(store, closed);
    expect(read(store)).toEqual(closed);
  });

  it("requires a read-back and deletes without leaving bytes", () => {
    const values = new Map<string, string>();
    const store = {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: () => {},
      removeItem: () => {},
    };
    expect(() => save(store, material(active("transfer")))).toThrow(
      "mobile_change_storage_unavailable",
    );
    values.set(KEY, "kept");
    expect(() => remove(store)).toThrow("mobile_change_storage_unavailable");
  });

  it("closes secrets at the proof deadline but retains the transfer locator", () => {
    const store = storage();
    const value = material(
      active("complete", {
        operation,
        transfer,
        transferExpiresAt: expiresAt,
        currentMobile: "13900000000",
        newMobile: "13800000000",
        messageId: message,
        code: "012345",
        attempted: true,
        retryAt: expiresAt + 5_000,
        reason: "unavailable",
      }),
      [known],
    );
    save(store, value);
    expect(expired(value.active!, expiresAt)).toBe(true);
    expect(maintain(store, expiresAt)).toBe(true);
    expect(read(store)).toEqual({
      version: 1,
      known: [known],
      active: {
        step: "transfer",
        account,
        person,
        selection,
        createdAt,
        expiresAt,
        retryAt: expiresAt + 5_000,
        attempted: true,
        transfer,
        transferExpiresAt: expiresAt,
        reason: "unavailable",
      },
      closed: null,
    });
  });

  it("turns an expired attempted record without a locator into an unknown placeholder", () => {
    const store = storage();
    save(
      store,
      material(
        active("current-send", {
          operation,
          currentMobile: "13900000000",
          token: "candidate",
          attempted: true,
        }),
      ),
    );
    expect(maintain(store, expiresAt)).toBe(true);
    expect(read(store)).toEqual({
      version: 1,
      known: [],
      active: null,
      closed: "unknown",
    });
  });

  it("keeps an unknown completion closed when only its SMS has expired", () => {
    const store = storage();
    save(
      store,
      material(
        active("complete", {
          operation,
          transfer,
          transferExpiresAt: expiresAt,
          newMobile: "13800000000",
          messageId: message,
          code: "012345",
          expiresAt: createdAt + 60_000,
          attempted: true,
        }),
      ),
    );
    expect(maintain(store, createdAt + 61_000)).toBe(true);
    expect(read(store)).toEqual(
      material(
        active("transfer", {
          transfer,
          transferExpiresAt: expiresAt,
          expiresAt: createdAt + 60_000,
          attempted: true,
          reason: "expired",
        }),
      ),
    );
  });

  it.each(["current-send", "new-send"] as const)(
    "requires the original limited %s proof even before acceptance",
    (step) => {
      const store = storage();
      const value = active(step, {
        operation,
        token: "original-token",
        reason: "limited",
        ...(step === "current-send"
          ? { currentMobile: "13900000000" }
          : {
              newMobile: "13800000000",
              transfer,
              transferExpiresAt: expiresAt,
            }),
      });
      save(store, material(value));
      expect(read(store)).toEqual(material(value));
      for (const key of [
        "operation",
        "token",
        step === "current-send" ? "currentMobile" : "newMobile",
      ]) {
        const damaged = { ...value };
        delete damaged[key as keyof Active];
        store.setItem(KEY, JSON.stringify(material(damaged)));
        expect(read(store)).toBe("missing");
      }
    },
  );

  it("clears an unsubmitted expired draft without claiming a result", () => {
    const store = storage();
    save(
      store,
      material(
        active("current-send", {
          operation,
          currentMobile: "13900000000",
          token: "candidate",
        }),
      ),
    );
    expect(maintain(store, expiresAt)).toBe(true);
    expect(read(store)).toBeNull();
    expect(store.values.has(KEY)).toBe(false);
  });

  it("closes a damaged record as unknown without guessing", () => {
    const store = storage();
    store.setItem(
      KEY,
      JSON.stringify({ version: 1, known: [], active: { step: "x" } }),
    );
    expect(maintain(store, createdAt)).toBe(true);
    expect(read(store)).toEqual({
      version: 1,
      known: [],
      active: null,
      closed: "unknown",
    });
  });

  it("closes use on clock rollback and preserves valid records", () => {
    const store = storage();
    const valid = material(
      active("new-send", {
        operation,
        transfer,
        newMobile: "13800000000",
        token: "candidate",
      }),
    );
    save(store, valid);
    expect(maintain(store, createdAt - 1)).toBe(true);
    expect(read(store)).toEqual({
      version: 1,
      known: [],
      active: {
        step: "transfer",
        account,
        person,
        selection,
        createdAt,
        expiresAt,
        retryAt: 0,
        attempted: false,
        transfer,
        reason: "expired",
      },
      closed: null,
    });
    expect(maintain(store, createdAt)).toBe(true);
    expect(read(store)).toEqual({
      version: 1,
      known: [],
      active: {
        step: "transfer",
        account,
        person,
        selection,
        createdAt,
        expiresAt,
        retryAt: 0,
        attempted: false,
        transfer,
        reason: "expired",
      },
      closed: null,
    });
  });

  it("narrows a server message deadline to the fixed local proof window", () => {
    expect(
      messageDeadline(createdAt, new Date(createdAt + 1_000).toISOString()),
    ).toBe(createdAt + 1_000);
    expect(
      messageDeadline(createdAt, new Date(expiresAt + 99_000).toISOString()),
    ).toBe(expiresAt);
  });
});
