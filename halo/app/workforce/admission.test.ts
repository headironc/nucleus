import { describe, expect, it, vi } from "vitest";
import { Admission } from "./admission";
import { PREFIX } from "./admission-material";
import type {
  AdmissionResult,
  Candidate,
  CandidateResult,
  Role,
} from "../protocol/admission";
import type { Account } from "../protocol/session";
import type { BrowserSession } from "../authentication/session";

const id = "00000000-0000-4000-8000-000000000001";
const target = "00000000-0000-4000-8000-000000000002";
const roleId = "00000000-0000-4000-8000-000000000003";
const operation = "00000000-0000-4000-8000-000000000004";
const member: Role = {
  id: roleId,
  changeId: roleId,
  kind: "default",
  name: "普通成员",
  level: 1,
  permissions: [],
};
const admin: Role = {
  ...member,
  id,
  changeId: id,
  kind: "full",
  level: 1000,
  name: "管理员",
  permissions: ["workforce.admission"],
};
const candidate: Candidate = {
  code: "ok",
  identity: {
    accountId: target,
    personId: target,
    revisionId: target,
    name: "同*",
    idNumber: { suffix: "002X" },
  },
  assignmentId: target,
  suspensionId: null,
  grant: null,
  count: 2,
  roles: [admin, member],
};
function fixture() {
  const bytes = new Map<string, string>();
  let selected = id;
  let now = 10_000;
  let denied = false;
  let writeFails = false;
  const account = () =>
    ({
      id: selected,
      personId: selected,
      revisionId: operation,
      name: selected === id ? "办理人" : "另一个账户",
    }) as Account;
  const read = vi.fn(async (): Promise<BrowserSession> => ({
    version: 1,
    revision: selected,
    session: { id: selected, expiresAt: "2035-01-01T00:00:00Z" },
    attempt: null,
  }));
  const find = vi
    .fn<
      (
        session: string,
        mobile: string,
        page: number,
        signal: AbortSignal,
      ) => Promise<CandidateResult>
    >()
    .mockResolvedValue({ kind: "candidate", candidate });
  const send = vi
    .fn<(...args: unknown[]) => Promise<AdmissionResult>>()
    .mockResolvedValue({ kind: "disconnected" });
  const controller = new Admission({
    storage: {
      getItem: (key) => bytes.get(key) ?? null,
      setItem: (key, value) => {
        if (writeFails) throw Error("unavailable");
        bytes.set(key, value);
      },
      removeItem: (key) => {
        bytes.delete(key);
      },
    },
    sessions: { read, clear: async () => true },
    current: async () => ({ kind: "account", account: account() }),
    role: async () => ({
      kind: "role",
      role: {
        ...admin,
        permissions: denied ? ["institution.roles"] : ["institution.workforce"],
      },
    }),
    build: async () => true,
    candidate: find,
    admit: send,
    now: () => now,
    uuid: () => operation,
    marker: () => false,
    mark: () => {},
  });
  async function ready() {
    await controller.verify();
    await vi.waitFor(() => expect(controller.snapshot().kind).toBe("content"));
  }
  return {
    controller,
    bytes,
    find,
    send,
    ready,
    advance: () => {
      now += 5_000;
    },
    deny: () => {
      denied = true;
    },
    switch: () => {
      selected = target;
    },
    failStorage: () => {
      writeFails = true;
    },
  };
}
function confirmed(f: ReturnType<typeof fixture>) {
  const state = f.controller.snapshot();
  if (state.kind !== "content" || state.content.kind !== "confirm")
    throw Error("expected confirmation");
  return state.content;
}
describe("institution admission", () => {
  it("selects the semantic default instead of the first administrator and persists only replay input", async () => {
    const f = fixture();
    await f.ready();
    await f.controller.find("13800000002");
    expect(confirmed(f).selected).toEqual(member);
    await f.controller.submit();
    const saved = JSON.parse(f.bytes.get(PREFIX + id)!);
    expect(saved.input).toEqual({
      entry: "institution",
      target: {
        accountId: target,
        personId: target,
        revisionId: target,
        mobile: "13800000002",
        assignmentId: target,
        suspensionId: null,
        grant: null,
      },
      role: { id: roleId, changeId: roleId },
    });
    expect(JSON.stringify(saved)).not.toMatch(/同\*|管理员|roles|姓名|suffix/);
    f.controller.close();
  });
  it("deduplicates repeated clicks and explicitly replays the same operation and entire input", async () => {
    const f = fixture();
    await f.ready();
    await f.controller.find("13800000002");
    await Promise.all([f.controller.submit(), f.controller.submit()]);
    expect(f.send).toHaveBeenCalledTimes(1);
    await f.controller.check();
    expect(f.send).toHaveBeenCalledTimes(1);
    f.advance();
    await f.controller.check();
    expect(f.send).toHaveBeenCalledTimes(2);
    expect(f.send.mock.calls[1].slice(0, 4)).toEqual(
      f.send.mock.calls[0].slice(0, 4),
    );
    await f.controller.verify();
    await vi.waitFor(() =>
      expect(f.controller.snapshot().kind).toBe("content"),
    );
    expect(f.send).toHaveBeenCalledTimes(2);
    f.controller.close();
  });
  it("keeps the original account isolated, discards late results and permits recovery after returning", async () => {
    const f = fixture();
    await f.ready();
    await f.controller.find("13800000002");
    let resolve!: (value: AdmissionResult) => void;
    f.send.mockImplementationOnce(
      () =>
        new Promise((done) => {
          resolve = done;
        }),
    );
    const sending = f.controller.submit();
    await vi.waitFor(() => expect(f.send).toHaveBeenCalledTimes(1));
    f.switch();
    f.controller.suspend();
    expect(f.controller.snapshot().kind).toBe("current");
    await f.ready();
    resolve({
      kind: "failure",
      status: 409,
      code: "target_changed",
      retryAfter: 0,
    });
    await sending;
    expect(f.controller.snapshot()).toEqual({
      kind: "content",
      content: { kind: "lookup" },
    });
    expect(f.bytes.has(PREFIX + id)).toBe(true);
    expect(f.bytes.has(PREFIX + target)).toBe(false);
    f.controller.close();
  });
  it.each([
    "target_changed",
    "role_changed",
    "already_authorized",
    "account_not_ready",
  ])("requires new confirmation after %s", async (code) => {
    const f = fixture();
    await f.ready();
    await f.controller.find("13800000002");
    f.send.mockResolvedValueOnce({
      kind: "failure",
      code,
      status: code === "account_not_ready" ? 422 : 409,
      retryAfter: 0,
    });
    await f.controller.submit();
    expect(f.controller.snapshot()).toMatchObject({
      kind: "content",
      content: { kind: "rejected" },
    });
    expect(f.bytes.size).toBe(0);
    f.controller.close();
  });
  it("never sends when first-save storage is unavailable", async () => {
    const f = fixture();
    await f.ready();
    await f.controller.find("13800000002");
    f.failStorage();
    await f.controller.submit();
    expect(f.send).not.toHaveBeenCalled();
    expect(f.controller.snapshot()).toMatchObject({
      kind: "content",
      content: { kind: "storage" },
    });
    f.controller.close();
  });
  it("hides confirmation on permission loss without losing an unknown original", async () => {
    const f = fixture();
    await f.ready();
    await f.controller.find("13800000002");
    await f.controller.submit();
    f.deny();
    await f.controller.verify();
    await vi.waitFor(() =>
      expect(f.controller.snapshot()).toMatchObject({
        kind: "permission",
        permission: { kind: "denied" },
      }),
    );
    expect(f.bytes.has(PREFIX + id)).toBe(true);
    f.advance();
    await f.controller.check();
    expect(f.send).toHaveBeenCalledTimes(1);
    f.controller.close();
  });
  it("finds the default through the bounded last page and keeps server pagination", async () => {
    const f = fixture();
    f.find.mockResolvedValueOnce({
      kind: "candidate",
      candidate: { ...candidate, count: 202, roles: [admin] },
    });
    f.find.mockResolvedValueOnce({
      kind: "candidate",
      candidate: { ...candidate, count: 202, roles: [member] },
    });
    await f.ready();
    await f.controller.find("13800000002");
    expect(f.find.mock.calls.map((call) => (call as unknown[])[2])).toEqual([
      1, 3,
    ]);
    expect(confirmed(f).selected?.kind).toBe("default");
    f.find.mockResolvedValueOnce({
      kind: "candidate",
      candidate: { ...candidate, count: 202, assignmentId: operation },
    });
    await f.controller.find("13800000002", 2);
    expect(f.controller.snapshot()).toMatchObject({
      kind: "content",
      content: { kind: "rejected" },
    });
    f.controller.close();
  });
  it("does not bypass paused accounts or existing active grants", async () => {
    for (const patch of [
      { suspensionId: target },
      { grant: { id: target, status: "active" as const } },
    ]) {
      const f = fixture();
      f.find.mockResolvedValueOnce({
        kind: "candidate",
        candidate: { ...candidate, ...patch },
      });
      await f.ready();
      await f.controller.find("13800000002");
      await f.controller.submit();
      expect(f.send).not.toHaveBeenCalled();
      f.controller.close();
    }
  });
  it("keeps request mismatch blocked across page verification", async () => {
    const f = fixture();
    await f.ready();
    await f.controller.find("13800000002");
    f.send.mockResolvedValueOnce({
      kind: "failure",
      status: 409,
      code: "request_mismatch",
      retryAfter: 0,
    });
    await f.controller.submit();
    await f.ready();
    f.advance();
    await f.controller.check();
    expect(f.send).toHaveBeenCalledTimes(1);
    expect(f.controller.snapshot()).toMatchObject({
      kind: "content",
      content: { kind: "pending", blocked: true },
    });
    f.controller.close();
  });
  it("honors candidate Retry-After in the controller across reset and verification", async () => {
    const f = fixture();
    await f.ready();
    f.find.mockResolvedValueOnce({
      kind: "failure",
      status: 429,
      code: "rate_limited",
      retryAfter: 20_000,
    });
    await f.controller.find("13800000002");
    f.controller.reset();
    await f.controller.find("13800000002", 2);
    expect(f.find).toHaveBeenCalledTimes(1);
    await f.ready();
    await f.controller.find("13800000002");
    expect(f.find).toHaveBeenCalledTimes(1);
    for (let i = 0; i < 4; i++) f.advance();
    f.controller.reset();
    await f.controller.find("13800000002");
    expect(f.find).toHaveBeenCalledTimes(2);
    f.controller.close();
  });
  it("returns from a permission recheck without resurrecting an old confirmation", async () => {
    const f = fixture();
    await f.ready();
    await f.controller.find("13800000002");
    expect(confirmed(f).selected).toBeTruthy();
    await f.controller.refreshPermission();
    expect(f.controller.snapshot()).toEqual({
      kind: "content",
      content: { kind: "lookup" },
    });
    f.controller.close();
  });
});
