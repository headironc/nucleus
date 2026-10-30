import { afterEach, describe, expect, it, vi } from "vitest";
import { Current } from "../authentication/current";
import {
  sameSelection,
  type BrowserSession,
  type Selection,
} from "../authentication/session";
import type { Account } from "../protocol/session";
import {
  AFFILIATION_CODES,
  type AffiliationCode,
  type AffiliationValuesResult,
  type AffiliationWriteResult,
  type CurrentAffiliationResult,
} from "../protocol/affiliation";
import { Affiliation } from "./controller";

const id = "00000000-0000-4000-8000-000000000001";
const other = "00000000-0000-4000-8000-000000000002";
const operation = "4ef1d157-e547-4d18-bf84-45a159709a4b";
const laterOperation = "ec7b7e2a-5e52-4d63-9b63-2e21f51b4d18";
const account = {
  id,
  personId: id,
  revisionId: id,
  name: "测试人员",
  idNumber: { suffix: "002X" },
  binding: null,
} as Account;
const names = [
  "中共党员",
  "中共预备党员",
  "共青团员",
  "民革会员",
  "民盟盟员",
  "民建会员",
  "民进会员",
  "农工党党员",
  "致公党党员",
  "九三学社社员",
  "台盟盟员",
  "无党派民主人士",
  "群众",
];
const options = AFFILIATION_CODES.map((value, index) => ({
  value,
  name: names[index],
}));
const record = { value: "04" as const, createdAt: "2026-10-25T03:04:05+08:00" };
const values: AffiliationValuesResult = {
  kind: "values",
  affiliations: options,
};
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((done, fail) => {
    resolve = done;
    reject = fail;
  });
  return { promise, resolve, reject };
}
function fixture() {
  let selected: BrowserSession | null = {
    version: 1,
    revision: id,
    session: { id, expiresAt: "2035-01-01T00:00:00Z" },
    attempt: null,
  };
  let now = 10_000;
  let operations = 0;
  const clear = vi.fn(async (expected: Selection) => {
    if (!sameSelection(selected, expected)) return false;
    selected = null;
    return true;
  });
  const current = new Current({
    sessions: { read: async () => selected, clear },
    current: async () => ({ kind: "account", account }),
    build: async () => true,
    now: () => now,
  });
  const read = vi
    .fn<
      (id: string, signal: AbortSignal) => Promise<CurrentAffiliationResult>
    >()
    .mockResolvedValue({ kind: "unset" });
  const readValues = vi
    .fn<(id: string, signal: AbortSignal) => Promise<AffiliationValuesResult>>()
    .mockResolvedValue(values);
  const create = vi
    .fn<
      (
        operation: string,
        id: string,
        code: AffiliationCode,
        signal: AbortSignal,
      ) => Promise<AffiliationWriteResult>
    >()
    .mockResolvedValue({ kind: "ok" });
  const affiliation = new Affiliation({
    current,
    read,
    values: readValues,
    create,
    build: async () => true,
    now: () => now,
    operation: () => (operations++ === 0 ? operation : laterOperation),
  });
  return {
    current,
    affiliation,
    read,
    readValues,
    create,
    clear,
    advance: (ms: number) => {
      now += ms;
    },
    select: () => {
      selected = {
        ...selected!,
        revision: other,
        session: { id: other, expiresAt: "2035-01-01T00:00:00Z" },
      };
    },
    ready: async () => {
      await current.verify();
    },
    choose: (code: AffiliationCode = "04") => {
      affiliation.startForm();
      affiliation.choose(code);
      affiliation.continueToConfirm();
    },
    close: () => {
      affiliation.close();
      current.suspend();
    },
  };
}
async function unset(f: ReturnType<typeof fixture>) {
  await f.ready();
  await vi.waitFor(() =>
    expect(f.affiliation.snapshot()).toMatchObject({
      panel: { kind: "unset" },
    }),
  );
}
afterEach(() => vi.useRealTimers());

describe("first affiliation reads", () => {
  it("opens all thirteen options only after trusted unset and complete names", async () => {
    const f = fixture();
    await unset(f);
    f.affiliation.startForm();
    f.affiliation.continueToConfirm();
    expect(f.affiliation.snapshot()).toMatchObject({
      panel: { kind: "form", choice: null },
      options,
    });
    expect(f.create).not.toHaveBeenCalled();
    f.close();
  });
  it("requires reload when the vocabulary is untrusted", async () => {
    const f = fixture();
    f.readValues.mockResolvedValue({ kind: "untrusted" });
    await f.ready();
    await vi.waitFor(() => expect(f.current.snapshot().kind).toBe("reload"));
    expect(f.affiliation.snapshot().kind).not.toBe("ready");
    f.close();
  });
  it.each(["unset", "record"] as const)(
    "does not open a form when names fail for %s",
    async (kind) => {
      const f = fixture();
      if (kind === "record")
        f.read.mockResolvedValue({ kind: "affiliation", affiliation: record });
      f.readValues.mockResolvedValue({ kind: "disconnected" });
      await f.ready();
      await vi.waitFor(() =>
        expect(f.affiliation.snapshot()).toMatchObject({
          panel: {
            kind:
              kind === "unset" ? "dictionary-error" : "value-dictionary-error",
          },
        }),
      );
      f.affiliation.startForm();
      expect(f.create).not.toHaveBeenCalled();
      f.readValues.mockResolvedValue(values);
      f.advance(5_000);
      f.affiliation.retryRead();
      await vi.waitFor(() =>
        expect(f.affiliation.snapshot()).toMatchObject({ panel: { kind } }),
      );
      f.close();
    },
  );
  it.each(["failure", "disconnected"] as const)(
    "preserves the dictionary cooldown alongside a %s current read",
    async (kind) => {
      const f = fixture();
      f.read.mockResolvedValue(
        kind === "disconnected"
          ? { kind }
          : {
              kind,
              status: 503,
              code: "dependency_unavailable",
              retryAfter: 0,
            },
      );
      f.readValues.mockResolvedValue({
        kind: "failure",
        status: 429,
        code: "rate_limited",
        retryAfter: 60_000,
      });
      await f.ready();
      await vi.waitFor(() =>
        expect(f.affiliation.snapshot()).toMatchObject({
          panel: { kind: "read-error", retryAt: 70_000, limited: true },
        }),
      );
      f.affiliation.startForm();
      f.advance(59_999);
      f.affiliation.retryRead();
      expect(f.read).toHaveBeenCalledTimes(1);
      f.advance(1);
      f.read.mockResolvedValue({ kind: "unset" });
      f.readValues.mockResolvedValue(values);
      f.affiliation.retryRead();
      await vi.waitFor(() =>
        expect(f.affiliation.snapshot()).toMatchObject({
          panel: { kind: "unset" },
        }),
      );
      expect(f.create).not.toHaveBeenCalled();
      f.close();
    },
  );
  it("preserves the longer current cooldown when both initial reads fail", async () => {
    const f = fixture();
    f.read.mockResolvedValue({
      kind: "failure",
      status: 429,
      code: "rate_limited",
      retryAfter: 60_000,
    });
    f.readValues.mockResolvedValue({
      kind: "failure",
      status: 503,
      code: "dependency_unavailable",
      retryAfter: 0,
    });
    await f.ready();
    await vi.waitFor(() =>
      expect(f.affiliation.snapshot()).toMatchObject({
        panel: { kind: "read-error", retryAt: 70_000, limited: true },
      }),
    );
    f.advance(5_000);
    f.affiliation.retryRead();
    expect(f.read).toHaveBeenCalledTimes(1);
    f.close();
  });
  it.each(["rejection", "timeout"] as const)(
    "retains an observed dictionary wait after the other read's %s",
    async (ending) => {
      vi.useFakeTimers();
      const f = fixture();
      const pending = deferred<CurrentAffiliationResult>();
      f.read.mockImplementationOnce(() => pending.promise);
      f.readValues.mockResolvedValue({
        kind: "failure",
        status: 429,
        code: "rate_limited",
        retryAfter: 60_000,
      });
      await f.ready();
      await vi.waitFor(() => expect(f.readValues).toHaveBeenCalledTimes(1));
      if (ending === "rejection") pending.reject(Error("read failed"));
      else {
        f.advance(35_000);
        await vi.advanceTimersByTimeAsync(35_000);
      }
      await vi.waitFor(() =>
        expect(f.affiliation.snapshot()).toMatchObject({
          panel: { kind: "read-error", retryAt: 70_000, limited: true },
        }),
      );
      f.affiliation.retryRead();
      expect(f.readValues).toHaveBeenCalledTimes(1);
      f.close();
      pending.resolve({ kind: "unset" });
    },
  );
});

describe("explicit writes and state refresh", () => {
  it("submits once after explicit confirmation and retains known success across failed reads", async () => {
    const f = fixture();
    await unset(f);
    f.choose();
    await f.affiliation.submit();
    expect(f.create).toHaveBeenCalledExactlyOnceWith(
      operation,
      id,
      "04",
      expect.any(AbortSignal),
    );
    expect(f.affiliation.snapshot()).toMatchObject({
      panel: { kind: "success" },
    });
    f.read.mockResolvedValueOnce({ kind: "affiliation", affiliation: record });
    f.affiliation.viewCurrent();
    await vi.waitFor(() =>
      expect(f.affiliation.snapshot()).toMatchObject({
        panel: { kind: "success" },
        current: { kind: "record", value: record },
      }),
    );
    f.read.mockResolvedValueOnce({ kind: "disconnected" });
    f.affiliation.viewCurrent();
    await vi.waitFor(() =>
      expect(f.affiliation.snapshot()).toMatchObject({
        panel: { kind: "success" },
        current: { kind: "error", retryAt: 15_000 },
      }),
    );
    expect(f.create).toHaveBeenCalledTimes(1);
    f.close();
  });
  it.each([
    { kind: "disconnected" },
    { kind: "untrusted" },
    {
      kind: "failure",
      status: 503,
      code: "outcome_unknown",
      retryAfter: 12_000,
    },
  ] as AffiliationWriteResult[])(
    "clears unknown input and only refreshes after $kind",
    async (result) => {
      const f = fixture();
      await unset(f);
      f.choose();
      f.create.mockResolvedValueOnce(result);
      await f.affiliation.submit();
      const retryAt = result.kind === "failure" ? 22_000 : 15_000;
      expect(f.affiliation.snapshot()).toMatchObject({
        panel: { kind: "unknown", retryAt },
      });
      const snapshot = f.affiliation.snapshot();
      if (snapshot.kind !== "ready") throw Error("expected ready");
      expect(snapshot.panel).not.toHaveProperty("choice");
      f.affiliation.viewCurrent();
      expect(f.read).toHaveBeenCalledTimes(1);
      f.advance(retryAt - 10_000);
      f.read.mockResolvedValueOnce({
        kind: "affiliation",
        affiliation: record,
      });
      f.affiliation.viewCurrent();
      await vi.waitFor(() =>
        expect(f.affiliation.snapshot()).toMatchObject({
          panel: { kind: "unknown" },
          current: { kind: "record", value: record },
        }),
      );
      f.affiliation.startForm();
      await f.affiliation.submit();
      expect(f.create).toHaveBeenCalledTimes(1);
      f.close();
    },
  );
  it("allows a new explicit choice after trusted unset without claiming the old write failed", async () => {
    const f = fixture();
    await unset(f);
    f.choose();
    f.create.mockResolvedValueOnce({ kind: "disconnected" });
    await f.affiliation.submit();
    f.advance(5_000);
    f.affiliation.viewCurrent();
    await vi.waitFor(() =>
      expect(f.affiliation.snapshot()).toMatchObject({
        panel: { kind: "unknown" },
        current: { kind: "unset" },
      }),
    );
    expect(f.create).toHaveBeenCalledTimes(1);
    f.affiliation.startForm();
    expect(f.affiliation.snapshot()).toMatchObject({
      panel: { kind: "form", choice: null },
    });
    f.affiliation.continueToConfirm();
    await f.affiliation.submit();
    expect(f.create).toHaveBeenCalledTimes(1);
    f.affiliation.choose("05");
    f.affiliation.continueToConfirm();
    await f.affiliation.submit();
    expect(f.create).toHaveBeenLastCalledWith(
      laterOperation,
      id,
      "05",
      expect.any(AbortSignal),
    );
    expect(f.affiliation.snapshot()).toMatchObject({
      panel: { kind: "success" },
    });
    f.close();
  });
  it.each([
    [429, "rate_limited", "submit-limited"],
    [503, "dependency_unavailable", "submit-failed"],
    [409, "state_conflict", "submit-conflict"],
    [409, "request_mismatch", "submit-mismatch"],
  ] as const)(
    "refreshes rather than replaying a %i %s response",
    async (status, code, kind) => {
      const f = fixture();
      await unset(f);
      f.choose();
      f.create.mockResolvedValueOnce({
        kind: "failure",
        status,
        code,
        retryAfter: 60_000,
      });
      await f.affiliation.submit();
      expect(f.affiliation.snapshot()).toMatchObject({ panel: { kind } });
      f.affiliation.startForm();
      await f.affiliation.submit();
      f.affiliation.viewCurrent();
      if (status !== 409) {
        expect(f.read).toHaveBeenCalledTimes(1);
        f.advance(60_000);
        f.affiliation.viewCurrent();
      }
      await vi.waitFor(() =>
        expect(f.affiliation.snapshot()).toMatchObject({
          panel: { kind: "unset" },
        }),
      );
      expect(f.create).toHaveBeenCalledTimes(1);
      f.close();
    },
  );
  it("requires another confirmation after trusted input refusal", async () => {
    const f = fixture();
    await unset(f);
    f.choose();
    f.create.mockResolvedValueOnce({
      kind: "failure",
      status: 422,
      code: "invalid_input",
      retryAfter: 0,
    });
    await f.affiliation.submit();
    f.affiliation.backToForm();
    await f.affiliation.submit();
    expect(f.create).toHaveBeenCalledTimes(1);
    f.affiliation.continueToConfirm();
    await f.affiliation.submit();
    expect(f.create).toHaveBeenLastCalledWith(
      laterOperation,
      id,
      "04",
      expect.any(AbortSignal),
    );
    f.close();
  });
  it.each([401, 403] as const)(
    "conceals the page after write status %i",
    async (status) => {
      const f = fixture();
      await unset(f);
      f.choose();
      f.create.mockResolvedValueOnce({
        kind: "failure",
        status,
        code: status === 401 ? "authentication_invalid" : "permission_denied",
        retryAfter: 0,
      });
      await f.affiliation.submit();
      expect(f.affiliation.snapshot()).toEqual({
        kind: status === 401 ? "identity-expired" : "identity-denied",
      });
      expect(f.clear).toHaveBeenCalledTimes(status === 401 ? 1 : 0);
      f.close();
    },
  );
  it("keeps unknown after read failure and respects the read cooldown", async () => {
    const f = fixture();
    await unset(f);
    f.choose();
    f.create.mockResolvedValueOnce({ kind: "disconnected" });
    await f.affiliation.submit();
    f.advance(5_000);
    const pending = deferred<CurrentAffiliationResult>();
    f.read.mockImplementationOnce(() => pending.promise);
    f.affiliation.viewCurrent();
    await vi.waitFor(() => expect(f.read).toHaveBeenCalledTimes(2));
    f.affiliation.viewCurrent();
    f.affiliation.retryRead();
    expect(f.read).toHaveBeenCalledTimes(2);
    expect(f.read.mock.calls[1][1].aborted).toBe(false);
    pending.resolve({
      kind: "failure",
      status: 429,
      code: "rate_limited",
      retryAfter: 60_000,
    });
    await vi.waitFor(() =>
      expect(f.affiliation.snapshot()).toMatchObject({
        panel: { kind: "unknown" },
        current: { kind: "error", retryAt: 75_000, limited: true },
      }),
    );
    f.advance(59_999);
    f.affiliation.retryRead();
    expect(f.read).toHaveBeenCalledTimes(2);
    f.advance(1);
    f.affiliation.retryRead();
    await vi.waitFor(() =>
      expect(f.affiliation.snapshot()).toMatchObject({
        panel: { kind: "unknown" },
        current: { kind: "unset" },
      }),
    );
    expect(f.create).toHaveBeenCalledTimes(1);
    f.close();
  });
});

describe("identity and late-response isolation", () => {
  it("masks immediately and ignores a late successful write", async () => {
    const f = fixture();
    await unset(f);
    f.choose();
    const pending = deferred<AffiliationWriteResult>();
    f.create.mockImplementationOnce(() => pending.promise);
    const sent = f.affiliation.submit();
    await vi.waitFor(() => expect(f.create).toHaveBeenCalledTimes(1));
    f.current.suspend();
    expect(f.affiliation.snapshot()).toEqual({ kind: "identity-loading" });
    expect(f.create.mock.calls[0][3].aborted).toBe(true);
    pending.resolve({ kind: "ok" });
    await sent;
    expect(f.affiliation.snapshot()).toEqual({ kind: "identity-loading" });
    await f.current.verify();
    await vi.waitFor(() =>
      expect(f.affiliation.snapshot()).toMatchObject({
        panel: { kind: "unset" },
      }),
    );
    expect(f.create).toHaveBeenCalledTimes(1);
    f.close();
  });
  it("ignores a timed-out write after refresh and a new explicit submission", async () => {
    vi.useFakeTimers();
    const f = fixture();
    await unset(f);
    f.choose();
    const pending = deferred<AffiliationWriteResult>();
    f.create.mockImplementationOnce(() => pending.promise);
    const sent = f.affiliation.submit();
    await vi.waitFor(() => expect(f.create).toHaveBeenCalledTimes(1));
    f.advance(35_000);
    await vi.advanceTimersByTimeAsync(35_000);
    expect(f.affiliation.snapshot()).toMatchObject({
      panel: { kind: "unknown" },
    });
    f.advance(5_000);
    f.affiliation.viewCurrent();
    await vi.waitFor(() =>
      expect(f.affiliation.snapshot()).toMatchObject({
        current: { kind: "unset" },
      }),
    );
    f.choose("05");
    await f.affiliation.submit();
    expect(f.affiliation.snapshot()).toMatchObject({
      panel: { kind: "success" },
    });
    pending.resolve({
      kind: "failure",
      status: 409,
      code: "state_conflict",
      retryAfter: 0,
    });
    await sent;
    expect(f.affiliation.snapshot()).toMatchObject({
      panel: { kind: "success" },
    });
    expect(f.create).toHaveBeenCalledTimes(2);
    f.close();
  });
  it("checks silent selection changes before publishing a refreshed unset", async () => {
    const f = fixture();
    await unset(f);
    f.choose();
    f.create.mockResolvedValueOnce({ kind: "disconnected" });
    await f.affiliation.submit();
    f.advance(5_000);
    const pending = deferred<CurrentAffiliationResult>();
    f.read.mockImplementationOnce(() => pending.promise);
    f.affiliation.viewCurrent();
    await vi.waitFor(() => expect(f.read).toHaveBeenCalledTimes(2));
    f.select();
    pending.resolve({ kind: "unset" });
    await vi.waitFor(() =>
      expect(f.current.binding()?.selection.id).toBe(other),
    );
    expect(f.affiliation.snapshot()).not.toMatchObject({
      current: { kind: "unset" },
    });
    f.close();
  });
  it("abandons initial reads from an older account binding", async () => {
    const f = fixture();
    const old = deferred<CurrentAffiliationResult>();
    f.read.mockImplementationOnce(() => old.promise);
    await f.ready();
    await vi.waitFor(() => expect(f.read).toHaveBeenCalledTimes(1));
    f.select();
    await f.current.verify();
    await vi.waitFor(() =>
      expect(f.affiliation.snapshot()).toMatchObject({
        panel: { kind: "unset" },
      }),
    );
    old.resolve({ kind: "affiliation", affiliation: record });
    await vi.waitFor(() => expect(f.read.mock.calls[0][1].aborted).toBe(true));
    expect(f.affiliation.snapshot()).toMatchObject({
      panel: { kind: "unset" },
    });
    f.affiliation.close();
    expect(f.affiliation.snapshot()).toEqual({ kind: "identity-loading" });
    f.close();
  });
});
