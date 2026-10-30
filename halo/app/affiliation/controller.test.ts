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
  type AffiliationValue,
  type AffiliationValuesResult,
  type AffiliationWriteResult,
  type CurrentAffiliationResult,
  type NamedAffiliation,
} from "../protocol/affiliation";
import { Affiliation } from "./controller";
import type { MarkerState } from "./marker";

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
const options: NamedAffiliation[] = AFFILIATION_CODES.map((value, index) => ({
  value,
  name: names[index],
}));
const record: AffiliationValue = {
  value: "04",
  createdAt: "2026-10-25T03:04:05+08:00",
};
const values: AffiliationValuesResult = { kind: "values", affiliations: options };

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
  let markerState: MarkerState = "absent";
  let markerWritable = true;
  let operationIndex = 0;
  const clear = vi.fn(async (expected: Selection) => {
    if (!sameSelection(selected, expected)) return false;
    selected = null;
    return true;
  });
  const identity = vi.fn(async () => ({ kind: "account" as const, account }));
  const current = new Current({
    sessions: { read: async () => selected, clear },
    current: identity,
    build: async () => true,
    now: () => now,
  });
  const read = vi
    .fn<(id: string, signal: AbortSignal) => Promise<CurrentAffiliationResult>>()
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
  const marker = {
    read: vi.fn(() => markerState),
    mark: vi.fn(() => {
      if (!markerWritable) return false;
      markerState = "present";
      return true;
    }),
    clear: vi.fn(() => {
      markerState = "absent";
    }),
  };
  const affiliation = new Affiliation({
    current,
    read,
    values: readValues,
    create,
    marker,
    build: async () => true,
    now: () => now,
    operation: () => [operation, laterOperation][operationIndex++] ?? laterOperation,
  });
  return {
    current,
    affiliation,
    read,
    readValues,
    create,
    marker,
    identity,
    clear,
    selected: () => selected,
    select: (session = other, revision = other) => {
      selected = {
        ...selected!,
        revision,
        session: { id: session, expiresAt: "2035-01-01T00:00:00Z" },
      };
    },
    advance: (ms: number) => {
      now += ms;
    },
    markerState: (state: MarkerState) => {
      markerState = state;
    },
    markerAvailable: (available: boolean) => {
      markerWritable = available;
    },
    ready: async () => {
      await current.verify();
      await vi.waitFor(() =>
        expect(affiliation.snapshot().kind).toBe("ready"),
      );
    },
    choose: async (code: AffiliationCode = "04") => {
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

afterEach(() => vi.useRealTimers());

describe("first affiliation initial state", () => {
  it.each(["present", "corrupt"] as const)(
    "never reopens a fill from a %s marker, even when the record reads unset",
    async (state) => {
      const f = fixture();
      f.markerState(state);
      await f.ready();
      await vi.waitFor(() =>
        expect(f.affiliation.snapshot()).toMatchObject({
          kind: "ready",
          panel: { kind: "unknown", canRetry: false },
        }),
      );
      expect(f.read).not.toHaveBeenCalled();
      expect(f.readValues).not.toHaveBeenCalled();
      f.affiliation.startForm();
      expect(f.affiliation.snapshot()).toMatchObject({
        panel: { kind: "unknown", canRetry: false },
      });
      f.close();
    },
  );

  it("offers only an independent read when the marker is unavailable", async () => {
    const f = fixture();
    f.markerState("denied");
    await f.ready();
    await vi.waitFor(() =>
      expect(f.affiliation.snapshot()).toMatchObject({
        kind: "ready",
        panel: { kind: "marker-unavailable" },
      }),
    );
    expect(f.read).not.toHaveBeenCalled();
    f.affiliation.viewCurrent();
    expect(f.affiliation.snapshot()).toMatchObject({
      current: { kind: "loading" },
    });
    f.close();
  });

  it("opens a 13-item form only after a trusted unset and complete vocabulary", async () => {
    const f = fixture();
    await f.ready();
    await vi.waitFor(() =>
      expect(f.affiliation.snapshot()).toMatchObject({
        kind: "ready",
        panel: { kind: "unset" },
        current: { kind: "idle" },
      }),
    );
    expect(f.marker.read).toHaveBeenCalledWith(id);
    expect(f.read).toHaveBeenCalledWith(id, expect.any(AbortSignal));
    f.affiliation.startForm();
    const snapshot = f.affiliation.snapshot();
    expect(snapshot.kind).toBe("ready");
    if (snapshot.kind !== "ready") throw new Error("unreachable");
    expect(snapshot.panel).toEqual({ kind: "form", choice: null });
    expect(snapshot.options.map((entry) => entry.value)).toEqual(AFFILIATION_CODES);
    expect(snapshot.options).toHaveLength(13);
    expect(f.create).not.toHaveBeenCalled();
    f.close();
  });

  it("requires reload when the protocol rejects the vocabulary", async () => {
    const f = fixture();
    f.readValues.mockResolvedValue({ kind: "untrusted" });
    await f.ready();
    await vi.waitFor(() =>
      expect(f.current.snapshot().kind).toBe("reload"),
    );
    expect(f.affiliation.snapshot().kind).not.toBe("ready");
    f.close();
  });

  it.each([
    { result: { kind: "disconnected" } as AffiliationValuesResult, retryAt: 15_000 },
    { result: { kind: "failure", status: 503, code: "dependency_unavailable", retryAfter: 12_000 } as AffiliationValuesResult, retryAt: 22_000 },
  ])("preserves an existing record when vocabulary fails: $result.kind", async ({ result, retryAt }) => {
    const f = fixture();
    f.read.mockResolvedValue({ kind: "affiliation", affiliation: record });
    f.readValues.mockResolvedValue(result);
    await f.ready();
    await vi.waitFor(() =>
      expect(f.affiliation.snapshot()).toMatchObject({
        panel: { kind: "value-dictionary-error", retryAt, limited: false },
      }),
    );
    f.affiliation.startForm();
    f.affiliation.retryRead();
    expect(f.read).toHaveBeenCalledTimes(1);
    expect(f.readValues).toHaveBeenCalledTimes(1);
    expect(f.create).not.toHaveBeenCalled();
    f.advance(retryAt - 10_000);
    f.readValues.mockResolvedValue(values);
    f.affiliation.retryRead();
    await vi.waitFor(() =>
      expect(f.affiliation.snapshot()).toMatchObject({
        panel: { kind: "record", value: record, name: "民革会员" },
      }),
    );
    f.close();
  });

  it("does not treat read failure as unset", async () => {
    const f = fixture();
    f.read.mockResolvedValue({
      kind: "failure",
      status: 503,
      code: "dependency_unavailable",
      retryAfter: 0,
    });
    await f.ready();
    await vi.waitFor(() =>
      expect(f.affiliation.snapshot()).toMatchObject({
        panel: { kind: "read-error", retryAt: 15_000 },
      }),
    );
    f.affiliation.startForm();
    expect(f.affiliation.snapshot()).toMatchObject({
      panel: { kind: "read-error" },
    });
    f.close();
  });

  it.each(["failure", "disconnected"] as const)("preserves the dictionary cooldown when the current read is %s", async (kind) => {
    const f = fixture();
    f.read.mockResolvedValue(kind === "disconnected"
      ? { kind }
      : { kind, status: 503, code: "dependency_unavailable", retryAfter: 0 });
    f.readValues.mockResolvedValue({ kind: "failure", status: 429, code: "rate_limited", retryAfter: 60_000 });
    await f.ready();
    await vi.waitFor(() => expect(f.affiliation.snapshot()).toMatchObject({
      panel: { kind: "read-error", retryAt: 70_000, limited: true },
    }));
    f.advance(5_000);
    f.affiliation.retryRead();
    f.advance(54_999);
    f.affiliation.retryRead();
    expect(f.read).toHaveBeenCalledTimes(1);
    expect(f.readValues).toHaveBeenCalledTimes(1);
    f.read.mockResolvedValue({ kind: "unset" });
    f.readValues.mockResolvedValue(values);
    f.advance(1);
    f.affiliation.retryRead();
    await vi.waitFor(() => expect(f.affiliation.snapshot()).toMatchObject({ panel: { kind: "unset" } }));
    expect(f.read).toHaveBeenCalledTimes(2);
    expect(f.readValues).toHaveBeenCalledTimes(2);
    expect(f.create).not.toHaveBeenCalled();
    f.close();
  });

  it("preserves the longer current cooldown when both initial reads fail", async () => {
    const f = fixture();
    f.read.mockResolvedValue({ kind: "failure", status: 429, code: "rate_limited", retryAfter: 60_000 });
    f.readValues.mockResolvedValue({ kind: "failure", status: 503, code: "dependency_unavailable", retryAfter: 0 });
    await f.ready();
    await vi.waitFor(() => expect(f.affiliation.snapshot()).toMatchObject({
      panel: { kind: "read-error", retryAt: 70_000, limited: true },
    }));
    f.advance(5_000);
    f.affiliation.retryRead();
    expect(f.read).toHaveBeenCalledTimes(1);
    expect(f.readValues).toHaveBeenCalledTimes(1);
    f.close();
  });

  it.each(["rejection", "timeout"] as const)("retains an observed dictionary cooldown after the other read's %s", async (ending) => {
    vi.useFakeTimers();
    const f = fixture();
    const pending = deferred<CurrentAffiliationResult>();
    f.read.mockImplementationOnce(() => pending.promise);
    f.readValues.mockResolvedValue({ kind: "failure", status: 429, code: "rate_limited", retryAfter: 60_000 });
    await f.ready();
    await vi.waitFor(() => expect(f.readValues).toHaveBeenCalledTimes(1));
    if (ending === "rejection") pending.reject(new Error("read failed"));
    else {
      f.advance(35_000);
      await vi.advanceTimersByTimeAsync(35_000);
    }
    await vi.waitFor(() => expect(f.affiliation.snapshot()).toMatchObject({
      panel: { kind: "read-error", retryAt: 70_000, limited: true },
    }));
    f.affiliation.retryRead();
    expect(f.readValues).toHaveBeenCalledTimes(1);
    f.close();
    pending.resolve({ kind: "unset" });
  });
});

describe("first affiliation write", () => {
  it("submits the exact original operation after marker save and confirms success", async () => {
    const f = fixture();
    await f.ready();
    await vi.waitFor(() =>
      expect(f.affiliation.snapshot()).toMatchObject({
        panel: { kind: "unset" },
      }),
    );
    await f.choose("04");
    expect(f.affiliation.snapshot()).toMatchObject({
      panel: { kind: "confirm", choice: "04" },
    });
    await f.affiliation.submit();
    expect(f.affiliation.snapshot()).toMatchObject({
      kind: "ready",
      panel: { kind: "success" },
    });
    expect(f.create).toHaveBeenCalledWith(
      operation,
      id,
      "04",
      expect.any(AbortSignal),
    );
    expect(f.marker.mark).toHaveBeenCalledWith(id);
    expect(f.marker.mark.mock.invocationCallOrder[0]).toBeLessThan(
      f.create.mock.invocationCallOrder[0],
    );
    expect(f.marker.clear).toHaveBeenCalledTimes(1);
    f.close();
  });

  it("has no default selection and cannot continue without one", async () => {
    const f = fixture();
    await f.ready();
    await vi.waitFor(() =>
      expect(f.affiliation.snapshot()).toMatchObject({
        panel: { kind: "unset" },
      }),
    );
    f.affiliation.startForm();
    f.affiliation.continueToConfirm();
    expect(f.affiliation.snapshot()).toMatchObject({
      panel: { kind: "form", choice: null },
    });
    expect(f.create).not.toHaveBeenCalled();
    f.close();
  });

  it.each([
    {
      result: {
        kind: "failure" as const,
        status: 429,
        code: "rate_limited",
        retryAfter: 0,
      },
      panel: { kind: "submit-limited", choice: "04", retryAt: 15_000 },
      retry: true,
    },
    {
      result: {
        kind: "failure" as const,
        status: 400,
        code: "invalid_input",
        retryAfter: 0,
      },
      panel: { kind: "submit-invalid", choice: "04" },
      retry: false,
    },
    {
      result: {
        kind: "failure" as const,
        status: 409,
        code: "state_conflict",
        retryAfter: 0,
      },
      panel: { kind: "submit-conflict", choice: "04" },
      retry: false,
    },
    {
      result: {
        kind: "failure" as const,
        status: 409,
        code: "request_mismatch",
        retryAfter: 0,
      },
      panel: { kind: "submit-mismatch", choice: "04" },
      retry: false,
    },
    {
      result: {
        kind: "failure" as const,
        status: 503,
        code: "dependency_unavailable",
        retryAfter: 0,
      },
      panel: { kind: "submit-failed", choice: "04", retryAt: 15_000 },
      retry: true,
    },
    {
      result: {
        kind: "failure" as const,
        status: 500,
        code: "outcome_unknown",
        retryAfter: 0,
      },
      panel: { kind: "unknown", choice: "04", canRetry: true },
      retry: true,
    },
    {
      result: { kind: "untrusted" as const },
      panel: { kind: "unknown", choice: "04", canRetry: true },
      retry: true,
    },
    {
      result: { kind: "disconnected" as const },
      panel: { kind: "unknown", choice: "04", canRetry: true },
      retry: true,
    },
  ])("classifies a first send as $panel.kind", async ({ result, panel, retry }) => {
    const f = fixture();
    await f.ready();
    await vi.waitFor(() =>
      expect(f.affiliation.snapshot()).toMatchObject({
        panel: { kind: "unset" },
      }),
    );
    await f.choose("04");
    f.create.mockResolvedValueOnce(result);
    await f.affiliation.submit();
    expect(f.affiliation.snapshot()).toMatchObject({ panel });
    if (retry) {
      f.advance(5_000);
      f.create.mockResolvedValueOnce({ kind: "ok" });
      await f.affiliation.retrySubmit();
      expect(f.create).toHaveBeenLastCalledWith(
        operation,
        id,
        "04",
        expect.any(AbortSignal),
      );
      expect(f.affiliation.snapshot()).toMatchObject({
        panel: { kind: "success" },
      });
    } else {
      await f.affiliation.retrySubmit();
      expect(f.create).toHaveBeenCalledTimes(1);
    }
    f.close();
  });

  it.each([401, 403] as const)(
    "conceals on first %i without showing an application code",
    async (status) => {
      const f = fixture();
      await f.ready();
      await vi.waitFor(() =>
        expect(f.affiliation.snapshot()).toMatchObject({
          panel: { kind: "unset" },
        }),
      );
      await f.choose("04");
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
      f.close();
    },
  );

  it("does not send when the marker cannot be saved", async () => {
    const f = fixture();
    f.markerAvailable(false);
    await f.ready();
    await vi.waitFor(() =>
      expect(f.affiliation.snapshot()).toMatchObject({
        panel: { kind: "unset" },
      }),
    );
    await f.choose("04");
    await f.affiliation.submit();
    expect(f.create).not.toHaveBeenCalled();
    expect(f.affiliation.snapshot()).toMatchObject({
      panel: { kind: "submit-storage", choice: "04", retry: false },
    });
    await f.affiliation.retrySubmit();
    expect(f.create).not.toHaveBeenCalled();
    f.affiliation.reconfirm();
    expect(f.affiliation.snapshot()).toMatchObject({
      panel: { kind: "confirm", choice: "04" },
    });
    f.markerAvailable(true);
    await f.affiliation.submit();
    expect(f.create).toHaveBeenCalledWith(
      laterOperation,
      id,
      "04",
      expect.any(AbortSignal),
    );
    f.close();
  });

  it("retains the original operation for an exact retry and stops after a mismatch", async () => {
    const f = fixture();
    await f.ready();
    await vi.waitFor(() =>
      expect(f.affiliation.snapshot()).toMatchObject({
        panel: { kind: "unset" },
      }),
    );
    await f.choose("04");
    f.create.mockResolvedValueOnce({ kind: "disconnected" });
    await f.affiliation.submit();
    expect(f.affiliation.snapshot()).toMatchObject({
      panel: { kind: "unknown", choice: "04", canRetry: true },
    });
    await f.affiliation.retrySubmit();
    expect(f.create).toHaveBeenCalledTimes(1);
    f.advance(5_000);
    f.create.mockResolvedValueOnce({
      kind: "failure",
      status: 409,
      code: "request_mismatch",
      retryAfter: 0,
    });
    await f.affiliation.retrySubmit();
    expect(f.create).toHaveBeenLastCalledWith(
      operation,
      id,
      "04",
      expect.any(AbortSignal),
    );
    expect(f.affiliation.snapshot()).toMatchObject({
      panel: { kind: "retry-mismatch", choice: "04" },
    });
    await f.affiliation.retrySubmit();
    expect(f.create).toHaveBeenCalledTimes(2);
    f.close();
  });

  it("retains an unknown original across a marker failure and can retry it", async () => {
    const f = fixture();
    await f.ready();
    await vi.waitFor(() =>
      expect(f.affiliation.snapshot()).toMatchObject({
        panel: { kind: "unset" },
      }),
    );
    await f.choose("04");
    f.create.mockResolvedValueOnce({ kind: "disconnected" });
    await f.affiliation.submit();
    f.advance(5_000);
    f.markerAvailable(false);
    await f.affiliation.retrySubmit();
    expect(f.affiliation.snapshot()).toMatchObject({
      panel: { kind: "submit-storage", choice: "04", retry: true, uncertain: true },
    });
    expect(f.create).toHaveBeenCalledTimes(1);
    f.markerAvailable(true);
    f.create.mockResolvedValueOnce({ kind: "ok" });
    await f.affiliation.retrySubmit();
    expect(f.create).toHaveBeenLastCalledWith(
      operation,
      id,
      "04",
      expect.any(AbortSignal),
    );
    expect(f.affiliation.snapshot()).toMatchObject({
      panel: { kind: "success" },
    });
    f.close();
  });

  it("keeps the original retry warning state after a retry rate limit", async () => {
    const f = fixture();
    await f.ready();
    await vi.waitFor(() =>
      expect(f.affiliation.snapshot()).toMatchObject({
        panel: { kind: "unset" },
      }),
    );
    await f.choose("04");
    f.create.mockResolvedValueOnce({ kind: "disconnected" });
    await f.affiliation.submit();
    f.advance(5_000);
    f.create.mockResolvedValueOnce({
      kind: "failure",
      status: 429,
      code: "rate_limited",
      retryAfter: 9_000,
    });
    await f.affiliation.retrySubmit();
    expect(f.affiliation.snapshot()).toMatchObject({
      panel: { kind: "retry-wait", choice: "04", retryAt: 24_000 },
    });
    await f.affiliation.retrySubmit();
    expect(f.create).toHaveBeenCalledTimes(2);
    f.advance(9_000);
    f.create.mockResolvedValueOnce({ kind: "ok" });
    await f.affiliation.retrySubmit();
    expect(f.create).toHaveBeenLastCalledWith(
      operation,
      id,
      "04",
      expect.any(AbortSignal),
    );
    f.close();
  });
});

describe("known rejection retries", () => {
  it("keeps repeated definite refusals definite and preserves the original request", async () => {
    const f = fixture();
    await f.ready();
    await vi.waitFor(() => expect(f.affiliation.snapshot()).toMatchObject({ panel: { kind: "unset" } }));
    await f.choose("04");
    f.create.mockResolvedValueOnce({ kind: "failure", status: 429, code: "rate_limited", retryAfter: 0 });
    await f.affiliation.submit();
    f.advance(5_000);
    f.create.mockResolvedValueOnce({ kind: "failure", status: 503, code: "dependency_unavailable", retryAfter: 8_000 });
    await f.affiliation.retrySubmit();
    expect(f.affiliation.snapshot()).toMatchObject({ panel: { kind: "submit-failed", retryAt: 23_000 } });
    await f.affiliation.retrySubmit();
    expect(f.create).toHaveBeenCalledTimes(2);
    f.advance(8_000);
    f.create.mockResolvedValueOnce({ kind: "failure", status: 429, code: "rate_limited", retryAfter: 0 });
    await f.affiliation.retrySubmit();
    expect(f.affiliation.snapshot()).toMatchObject({ panel: { kind: "submit-limited", retryAt: 28_000 } });
    expect(f.create.mock.calls.map(([key, session, value]) => [key, session, value])).toEqual([
      [operation, id, "04"], [operation, id, "04"], [operation, id, "04"],
    ]);
    f.close();
  });
});

describe("independent current reads", () => {
  it.each(["present", "denied"] as const)(
    "keeps an active read and honors its cooldown with a %s marker",
    async (marker) => {
      const f = fixture();
      f.markerState(marker);
      await f.ready();
      const pending = deferred<CurrentAffiliationResult>();
      f.read.mockImplementationOnce(() => pending.promise);
      f.affiliation.viewCurrent();
      await vi.waitFor(() => expect(f.read).toHaveBeenCalledTimes(1));
      f.affiliation.viewCurrent();
      f.affiliation.retryRead();
      expect(f.read).toHaveBeenCalledTimes(1);
      expect(f.read.mock.calls[0][1].aborted).toBe(false);
      pending.resolve({ kind: "failure", status: 429, code: "rate_limited", retryAfter: 60_000 });
      await vi.waitFor(() => expect(f.affiliation.snapshot()).toMatchObject({
        current: { kind: "error", retryAt: 70_000, limited: true },
      }));
      f.affiliation.viewCurrent();
      f.advance(59_999);
      f.affiliation.viewCurrent();
      f.affiliation.retryRead();
      expect(f.read).toHaveBeenCalledTimes(1);
      f.advance(1);
      f.affiliation.viewCurrent();
      await vi.waitFor(() => expect(f.read).toHaveBeenCalledTimes(2));
      expect(f.create).not.toHaveBeenCalled();
      f.close();
    },
  );

  it("never downgrades a known success when a later read fails", async () => {
    const f = fixture();
    await f.ready();
    await vi.waitFor(() =>
      expect(f.affiliation.snapshot()).toMatchObject({
        panel: { kind: "unset" },
      }),
    );
    await f.choose("04");
    await f.affiliation.submit();
    expect(f.affiliation.snapshot()).toMatchObject({
      panel: { kind: "success" },
    });
    f.read.mockResolvedValueOnce({
      kind: "affiliation",
      affiliation: record,
    });
    f.affiliation.viewCurrent();
    expect(f.affiliation.snapshot()).toMatchObject({
      panel: { kind: "success" },
      current: { kind: "loading" },
    });
    await vi.waitFor(() =>
      expect(f.affiliation.snapshot()).toMatchObject({
        panel: { kind: "success" },
        current: { kind: "record", value: record, name: "民革会员" },
      }),
    );
    f.read.mockResolvedValueOnce({
      kind: "failure",
      status: 503,
      code: "dependency_unavailable",
      retryAfter: 0,
    });
    f.affiliation.viewCurrent();
    await vi.waitFor(() =>
      expect(f.affiliation.snapshot()).toMatchObject({
        panel: { kind: "success" },
        current: { kind: "error", retryAt: 15_000 },
      }),
    );
    f.close();
  });

  it("keeps unknown unknown when the independent read finds unset", async () => {
    const f = fixture();
    await f.ready();
    await vi.waitFor(() =>
      expect(f.affiliation.snapshot()).toMatchObject({
        panel: { kind: "unset" },
      }),
    );
    await f.choose("04");
    f.create.mockResolvedValueOnce({ kind: "disconnected" });
    await f.affiliation.submit();
    f.read.mockResolvedValueOnce({ kind: "unset" });
    f.affiliation.viewCurrent();
    await vi.waitFor(() =>
      expect(f.affiliation.snapshot()).toMatchObject({
        panel: { kind: "unknown", choice: "04", canRetry: true },
        current: { kind: "unset" },
      }),
    );
    f.affiliation.startForm();
    expect(f.affiliation.snapshot()).toMatchObject({
      panel: { kind: "unknown" },
    });
    expect(f.create).toHaveBeenCalledTimes(1);
    f.close();
  });

  it.each([
    ["state_conflict", 409, "retry-conflict"],
    ["request_mismatch", 409, "retry-mismatch"],
    ["invalid_input", 400, "retry-stopped"],
  ] as const)("keeps %s retry outcomes unknown when an independent read finds a record", async (code, status, kind) => {
    const f = fixture();
    await f.ready();
    await vi.waitFor(() =>
      expect(f.affiliation.snapshot()).toMatchObject({
        panel: { kind: "unset" },
      }),
    );
    await f.choose("04");
    f.create.mockResolvedValueOnce({ kind: "disconnected" });
    await f.affiliation.submit();
    f.advance(5_000);
    f.create.mockResolvedValueOnce({
      kind: "failure",
      status,
      code,
      retryAfter: 0,
    });
    await f.affiliation.retrySubmit();
    expect(f.affiliation.snapshot()).toMatchObject({
      panel: { kind, choice: "04" },
    });
    f.read.mockResolvedValueOnce({
      kind: "affiliation",
      affiliation: record,
    });
    f.affiliation.viewCurrent();
    expect(f.affiliation.snapshot()).toMatchObject({
      panel: { kind, choice: "04" },
      current: { kind: "loading" },
    });
    await vi.waitFor(() =>
      expect(f.affiliation.snapshot()).toMatchObject({
        panel: { kind, choice: "04" },
        current: { kind: "record", value: record, name: "民革会员" },
      }),
    );
    f.close();
  });

  it("uses an independent read instead of a fresh form after a conflict", async () => {
    const f = fixture();
    await f.ready();
    await vi.waitFor(() =>
      expect(f.affiliation.snapshot()).toMatchObject({
        panel: { kind: "unset" },
      }),
    );
    await f.choose("04");
    f.create.mockResolvedValueOnce({
      kind: "failure",
      status: 409,
      code: "state_conflict",
      retryAfter: 0,
    });
    await f.affiliation.submit();
    f.read.mockResolvedValueOnce({
      kind: "affiliation",
      affiliation: record,
    });
    f.affiliation.viewCurrent();
    await vi.waitFor(() =>
      expect(f.affiliation.snapshot()).toMatchObject({
        panel: { kind: "record", value: record, name: "民革会员" },
      }),
    );
    f.close();
  });
});

describe("identity and late-result races", () => {
  it("clears the private projection when the page controller closes", async () => {
    const f = fixture();
    f.read.mockResolvedValue({ kind: "affiliation", affiliation: record });
    await f.ready();
    await vi.waitFor(() => expect(f.affiliation.snapshot()).toMatchObject({ panel: { kind: "record" } }));
    f.affiliation.close();
    expect(f.affiliation.snapshot()).toEqual({ kind: "identity-loading" });
    f.close();
  });

  it("checks a silent selection change before publishing an independent unset read", async () => {
    const f = fixture();
    f.markerState("present");
    await f.ready();
    const pending = deferred<CurrentAffiliationResult>();
    f.read.mockImplementationOnce(() => pending.promise);
    f.affiliation.viewCurrent();
    await vi.waitFor(() => expect(f.read).toHaveBeenCalledTimes(1));
    f.select();
    pending.resolve({ kind: "unset" });
    await vi.waitFor(() => expect(f.current.binding()?.selection.id).toBe(other));
    expect(f.affiliation.snapshot()).not.toMatchObject({ current: { kind: "unset" } });
    f.close();
  });

  it("masks immediately and rejects a late write result", async () => {
    const f = fixture();
    await f.ready();
    await vi.waitFor(() =>
      expect(f.affiliation.snapshot()).toMatchObject({
        panel: { kind: "unset" },
      }),
    );
    await f.choose("04");
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
    f.close();
  });

  it("does not reopen a form after refresh when the request may have been sent", async () => {
    const f = fixture();
    await f.ready();
    await vi.waitFor(() =>
      expect(f.affiliation.snapshot()).toMatchObject({
        panel: { kind: "unset" },
      }),
    );
    await f.choose("04");
    const pending = deferred<AffiliationWriteResult>();
    f.create.mockImplementationOnce(() => pending.promise);
    const sent = f.affiliation.submit();
    await vi.waitFor(() => expect(f.create).toHaveBeenCalledTimes(1));
    f.current.suspend();
    pending.resolve({ kind: "ok" });
    await sent;
    await f.current.verify();
    await vi.waitFor(() =>
      expect(f.affiliation.snapshot()).toMatchObject({
        kind: "ready",
        panel: { kind: "unknown", canRetry: false },
      }),
    );
    f.affiliation.startForm();
    expect(f.affiliation.snapshot()).toMatchObject({
      panel: { kind: "unknown", canRetry: false },
    });
    f.close();
  });

  it("abandons a read when the account selection changes", async () => {
    const f = fixture();
    const old = deferred<CurrentAffiliationResult>();
    f.read
      .mockImplementationOnce(() => old.promise)
      .mockResolvedValueOnce({ kind: "unset" });
    const first = f.current.verify();
    await vi.waitFor(() => expect(f.read).toHaveBeenCalledTimes(1));
    f.select();
    await f.current.verify();
    await vi.waitFor(() =>
      expect(f.affiliation.snapshot()).toMatchObject({
        kind: "ready",
        panel: { kind: "unset" },
      }),
    );
    old.resolve({ kind: "affiliation", affiliation: record });
    await first;
    expect(f.affiliation.snapshot()).toMatchObject({
      panel: { kind: "unset" },
    });
    expect(f.read.mock.calls[0][1].aborted).toBe(true);
    f.close();
  });

  it("clears the original retry when the current binding no longer matches", async () => {
    const f = fixture();
    await f.ready();
    await vi.waitFor(() =>
      expect(f.affiliation.snapshot()).toMatchObject({
        panel: { kind: "unset" },
      }),
    );
    await f.choose("04");
    f.create.mockResolvedValueOnce({ kind: "disconnected" });
    await f.affiliation.submit();
    f.current.suspend();
    await f.current.verify();
    await vi.waitFor(() =>
      expect(f.affiliation.snapshot()).toMatchObject({
        kind: "ready",
        panel: { kind: "unknown", canRetry: false },
      }),
    );
    f.create.mockClear();
    await f.affiliation.retrySubmit();
    expect(f.create).not.toHaveBeenCalled();
    f.close();
  });
});
