//! Non-native self-proof driver for the private native mini fixture.
//!
//! The real detached Orbit Controller runs through a minimal in-memory platform whose request
//! method uses real HTTP, normal storage and the real protocol checks. This is not the native
//! mini client and does not claim native acceptance; it exists to make the fixture observably
//! exercise its API, recovery and independent facts.

import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import type {
  Controller as OrbitController,
  View,
} from "../../../orbit/src/controller";
import { pathToFileURL } from "node:url";
import { join } from "node:path";
import { request } from "node:http";
import { secret, type Platform, type Reply } from "../../../orbit/src/platform";
import { storageKey, type Records } from "../../../orbit/src/storage";

type Control = {
  send(command: Record<string, unknown>, timeoutMs?: number): Promise<any>;
};

/** One real connection per request, with no implicit write retry or redirect. */
function http(
  url: string,
  options: {
    method?: string;
    headers?: Record<string, string>;
    body?: string;
    signal?: AbortSignal;
    redirect?: string;
  } = {},
): Promise<Response> {
  const target = new URL(url);
  if (target.protocol !== "http:" || target.hostname !== "127.0.0.1")
    throw new Error("non-loopback self-proof request");
  return new Promise((resolveReply, reject) => {
    const headers = { ...options.headers };
    if (options.body !== undefined)
      headers["content-length"] = String(Buffer.byteLength(options.body));
    const outgoing = request(
      target,
      { method: options.method, headers, signal: options.signal, agent: false },
      (incoming) => {
        const chunks: Buffer[] = [];
        let length = 0;
        incoming.on("data", (chunk: Buffer) => {
          length += chunk.length;
          if (length > 1024 * 1024)
            incoming.destroy(new Error("self-proof response limit"));
          else chunks.push(chunk);
        });
        incoming.once("error", () =>
          reject(new Error("loopback response interrupted")),
        );
        incoming.once("end", () => {
          const headers = new Headers();
          for (const [name, value] of Object.entries(incoming.headers))
            if (value !== undefined)
              headers.set(
                name,
                Array.isArray(value) ? value.join(", ") : value,
              );
          resolveReply(
            new Response(Buffer.concat(chunks), {
              status: incoming.statusCode,
              headers,
            }),
          );
        });
      },
    );
    outgoing.setTimeout(35_000, () =>
      outgoing.destroy(new Error("loopback request timeout")),
    );
    outgoing.once("error", (error: NodeJS.ErrnoException) =>
      reject(new Error(`loopback transport ${error.code ?? error.name}`)),
    );
    outgoing.end(options.body);
  });
}

export type DriverOptions = {
  control: Control;
  run: string;
  origin: string;
  relayBase: string;
  clientDir: string;
};

class MemoryPlatform implements Platform {
  private values = new Map<string, unknown>();

  constructor(
    private origin: string,
    private relayBase: string,
  ) {}

  private target(url: string): string | undefined {
    if (url === this.origin || url.startsWith(`${this.origin}/`)) {
      return `${this.relayBase}${url.slice(this.origin.length)}`;
    }
    return undefined;
  }

  getRandomValues(options: {
    length: number;
    success(result: { randomValues: ArrayBuffer }): void;
    fail(): void;
  }) {
    try {
      const bytes = new Uint8Array(options.length);
      crypto.getRandomValues(bytes);
      options.success({ randomValues: bytes.buffer });
    } catch {
      options.fail();
    }
  }

  getStorageSync(key: string): unknown {
    const value = this.values.get(key);
    return value === undefined ? undefined : JSON.parse(JSON.stringify(value));
  }

  setStorageSync(key: string, value: unknown) {
    this.values.set(key, JSON.parse(JSON.stringify(value)));
  }

  request(options: {
    url: string;
    method: "GET" | "POST" | "DELETE";
    header: Record<string, string>;
    data?: string;
    dataType: "text";
    responseType: "text";
    timeout: number;
    success(result: Reply): void;
    fail(): void;
  }): { abort(): void } {
    const target = this.target(options.url);
    if (!target) {
      queueMicrotask(options.fail);
      return { abort() {} };
    }
    const abort = new AbortController();
    const timer = setTimeout(() => abort.abort(), options.timeout + 2_000);
    void (async () => {
      try {
        const response = await http(target, {
          method: options.method,
          headers: options.header,
          body: options.data,
          redirect: "manual",
          signal: abort.signal,
        });
        const data = await response.text();
        options.success({
          statusCode: response.status,
          header: Object.fromEntries(response.headers.entries()),
          data,
        });
      } catch {
        options.fail();
      } finally {
        clearTimeout(timer);
      }
    })();
    return {
      abort() {
        abort.abort();
      },
    };
  }
}

type Waiter = {
  predicate(view: View): boolean;
  resolve(view: View): void;
  reject(error: Error): void;
  timer: ReturnType<typeof setTimeout>;
};

class Views {
  private current: View;
  private waiters: Waiter[] = [];

  constructor(controller: OrbitController) {
    this.current = controller.snapshot();
    controller.subscribe((view) => {
      this.current = view;
      for (const waiter of [...this.waiters]) {
        if (!waiter.predicate(view)) continue;
        clearTimeout(waiter.timer);
        this.waiters = this.waiters.filter((value) => value !== waiter);
        waiter.resolve(view);
      }
    });
  }

  wait(
    predicate: (view: View) => boolean,
    label: string,
    timeoutMs = 45_000,
  ): Promise<View> {
    if (predicate(this.current)) return Promise.resolve(this.current);
    return new Promise((resolveView, reject) => {
      const timer = setTimeout(() => {
        this.waiters = this.waiters.filter((value) => value.timer !== timer);
        reject(new Error(`client did not reach ${label}`));
      }, timeoutMs);
      this.waiters.push({ predicate, resolve: resolveView, reject, timer });
    });
  }
}

type ApiOptions = {
  operation: string;
  token?: string;
  body?: unknown;
  forged?: boolean;
  expectDrop?: boolean;
};

type ApiResult = { status: number; value: any; dropped: boolean };

class ControlClient {
  constructor(
    private control: Control,
    private run: string,
  ) {}

  async arm(caseName: string, binding: Record<string, unknown> = {}) {
    const reply = await this.control.send({
      command: "arm",
      run: this.run,
      case: caseName,
      operation: binding.operation ?? null,
      actor: binding.actor ?? null,
      target: binding.target ?? null,
      registration: binding.registration ?? null,
    });
    if (!reply?.ok)
      throw new Error(`arm ${caseName} rejected: ${String(reply?.code)}`);
    return reply;
  }

  async checkpoint(caseName: string, binding: Record<string, unknown> = {}) {
    const reply = await this.control.send(
      {
        command: "checkpoint",
        run: this.run,
        case: caseName,
        operation: binding.operation ?? null,
        actor: binding.actor ?? null,
        target: binding.target ?? null,
        later_session: binding.laterSession ?? null,
        foreign_session: binding.foreignSession ?? null,
      },
      60_000,
    );
    if (!reply?.ok)
      throw new Error(
        `checkpoint ${caseName} rejected: ${String(reply?.code)}`,
      );
    return reply;
  }

  async snapshot() {
    const reply = await this.control.send({
      command: "snapshot",
      run: this.run,
    });
    if (!reply?.ok)
      throw new Error(`snapshot rejected: ${String(reply?.code)}`);
    return reply;
  }
}

function operation(): string {
  return randomUUID();
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function sleepUntil(deadline: number): Promise<void> {
  const delay = Math.max(0, deadline - Date.now()) + 100;
  await sleep(Math.min(delay, 60_000));
}

function records(platform: MemoryPlatform): Records {
  return (
    (platform.getStorageSync(storageKey) as Records | undefined) ?? {
      version: 1,
    }
  );
}

export async function drive(
  options: DriverOptions,
): Promise<Record<string, boolean>> {
  const { Controller } = await import(
    pathToFileURL(join(options.clientDir, "orbit/src/controller.ts")).href
  );
  const platform = new MemoryPlatform(options.origin, options.relayBase);
  const controller = new Controller(platform, options.origin);
  const views = new Views(controller);
  const control = new ControlClient(options.control, options.run);
  const observations: Record<string, boolean> = {
    avatar_read: false,
    registration_recovered: false,
    current_503_retained: false,
    later_login_used: false,
    other_account_isolated: false,
    revocation_reads_read_only: false,
    registration_retry_no_extra_exchange: false,
  };

  const api = async (
    method: "GET" | "POST" | "DELETE",
    path: string,
    input: ApiOptions,
  ): Promise<ApiResult> => {
    // Exercise the real shared budget from one source; never retry a write or rotate IPs.
    if (method === "DELETE") await sleep(550);
    const headers: Record<string, string> = {
      Accept: "application/json",
      "Operation-Id": input.operation,
    };
    if (input.token) headers.Authorization = `Bearer ${input.token}`;
    if (input.body !== undefined) headers["Content-Type"] = "application/json";
    if (input.forged) {
      headers["X-Real-IP"] = "192.0.2.99";
      headers["X-Forwarded-For"] = "198.51.100.3";
      headers.Forwarded = "for=198.51.100.3;proto=https";
    }
    const abort = new AbortController();
    const timer = setTimeout(() => abort.abort(), 40_000);
    try {
      const response = await http(`${options.relayBase}${path}`, {
        method,
        headers,
        body: input.body === undefined ? undefined : JSON.stringify(input.body),
        redirect: "manual",
        signal: abort.signal,
      });
      const text = await response.text();
      let value: any = null;
      try {
        value = text ? JSON.parse(text) : null;
      } catch {
        value = null;
      }
      return { status: response.status, value, dropped: false };
    } catch (error) {
      if (input.expectDrop) return { status: 0, value: null, dropped: true };
      throw new Error(
        `${method} ${path.split("?")[0]} transport failed: ${error instanceof Error ? error.message : "unknown"}`,
      );
    } finally {
      clearTimeout(timer);
    }
  };

  const expect = (result: ApiResult, status: number, code?: string) => {
    if (result.status !== status || (code && result.value?.code !== code)) {
      throw new Error(
        `expected HTTP ${status}${code ? ` ${code}` : ""}, got ${result.status} ${String(result.value?.code)}`,
      );
    }
  };
  const original = (result: ApiResult, facts: any) => {
    expect(result, 200);
    assert.deepEqual(result.value.original, {
      scope: facts.scope,
      initiatorId: facts.initiatorId,
    });
    assert.equal(result.value.count, facts.count);
    assert.deepEqual(
      result.value.revocations.map((item: any) => item.id),
      facts.revokedIds,
    );
    for (const item of result.value.revocations)
      assert.equal(item.revokedAt, facts.revokedAt);
  };

  const loginStart = async (code: string, forged = false) => {
    const result = await api("POST", "/api/v1/sessions", {
      operation: operation(),
      forged,
      body: {
        secret: await secret(platform),
        credential: { type: "mobile", code },
      },
    });
    expect(result, 200);
    return result.value;
  };

  const sessionFor = async (code: string, forged = false) => {
    const value = await loginStart(code, forged);
    assert.ok(
      value.session?.id && value.session?.token,
      `${code} produced a Session`,
    );
    return value.session as { id: string; token: string };
  };

  const register = async (
    proof: { id: string; token: string },
    name: string,
    idNumber: string,
  ) => {
    const registrationOperation = operation();
    const result = await api("POST", "/api/v1/registrations", {
      operation: registrationOperation,
      body: { id: proof.id, token: proof.token, name, idNumber },
    });
    assert.ok(
      result.status === 200 || result.status === 201,
      "registration succeeded",
    );
    assert.ok(
      result.value.session?.id && result.value.session?.token,
      "registration produced a Session",
    );
    return {
      session: result.value.session as { id: string; token: string },
      operation: registrationOperation,
    };
  };

  try {
    await controller.show();
    await views.wait(
      (view) => view.phase === "login" && view.canLogin,
      "login",
    );

    await controller.authorize("a-register");
    await views.wait(
      (view) => view.phase === "registration",
      "registration form",
    );
    await control.arm("registration", {
      registration: records(platform).registration?.proof.id,
    });
    expect(
      await api("GET", "/api/v1/registrations", { operation: operation() }),
      404,
    );
    const proof = records(platform).registration!.proof;
    expect(
      await api("POST", "/api/v1/registrations", {
        operation: operation(),
        body: {
          id: operation(),
          token: proof.token,
          name: "隔离甲",
          idNumber: "00000020000229007X",
        },
      }),
      410,
      "verification_required",
    );
    await controller.register("隔离甲", "00000020000229007X");
    await views.wait(
      (view) => view.phase === "pending",
      "registration pending",
    );
    const pending = records(platform).registration?.pending;
    assert.ok(pending?.operation, "registration retained its operation");
    const r2 = pending.operation;
    const retryAt = pending.retryAt;
    const retryBaseline = await control.snapshot();
    await sleepUntil(retryAt);
    await controller.recover();
    await views.wait((view) => view.phase === "account", "recovered account");
    const retryAfter = await control.snapshot();
    if (
      JSON.stringify(retryAfter.counts) !==
        JSON.stringify(retryBaseline.counts) ||
      retryAfter.supplier.phoneExchanges !==
        retryBaseline.supplier.phoneExchanges
    ) {
      throw new Error(
        "registration retry changed durable facts or exported another phone code",
      );
    }
    observations.registration_retry_no_extra_exchange = true;

    const registration = await control.checkpoint("registration", {
      operation: r2,
    });
    assert.equal(registration.capture.status, 201);
    assert.equal(registration.capture.dropped, true);
    if (
      registration.facts.counts.accounts !== 1 ||
      registration.facts.counts.persons !== 1 ||
      registration.facts.counts.sessions !== 1 ||
      registration.facts.counts.receipts !== 1 ||
      registration.facts.counts.handoffs !== 1 ||
      registration.facts.sessionUsable !== true
    ) {
      throw new Error(
        "registration facts are not one usable account and Session",
      );
    }
    observations.registration_recovered = true;

    const selected = records(platform).session;
    assert.ok(
      selected?.id && selected?.token,
      "the Controller retained its Session",
    );
    assert.equal(selected.id, registration.facts.sessionId);
    const accountId = registration.facts.accountId as string;
    assert.equal(selected.accountId, accountId);
    const first = { id: selected.id, token: selected.token };

    const avatarView = controller.snapshot();
    assert.ok(
      avatarView.avatar.startsWith(`${options.origin}/api/v1/avatars/`),
      "avatar uses the task origin",
    );
    const avatarResponse = await http(
      `${options.relayBase}${avatarView.avatar.slice(options.origin.length)}`,
      {
        redirect: "manual",
      },
    );
    assert.equal(avatarResponse.status, 200);
    assert.match(
      avatarResponse.headers.get("content-type") ?? "",
      /^image\/webp/,
    );
    assert.ok((await avatarResponse.arrayBuffer()).byteLength > 0);
    observations.avatar_read = true;

    const beforeExtra = await control.snapshot();
    const extra = await sessionFor("a-extra-current", true);
    const afterExtra = await control.snapshot();
    if (
      afterExtra.relay.strippedSourceRequests <
        beforeExtra.relay.strippedSourceRequests + 1 ||
      afterExtra.relay.fixedSource !== "203.0.113.77"
    ) {
      throw new Error("relay did not replace forged client source headers");
    }

    await control.arm("logout_current", { actor: first.id });
    const wrongActor = await api("DELETE", "/api/v1/sessions/current", {
      operation: operation(),
      token: extra.token,
    });
    expect(wrongActor, 200);
    const wrongPath = await api("GET", "/api/v1/sessions", {
      operation: operation(),
      token: first.token,
    });
    expect(wrongPath, 200);
    await controller.logout("current");
    await views.wait(
      (view) => view.phase === "pending",
      "current Logout pending",
    );
    const currentLogout = records(platform).logouts?.[0]?.operation;
    assert.ok(currentLogout, "current Logout retained its operation");
    assert.equal(records(platform).session?.id, first.id);
    await controller.check();
    await views.wait(
      (view) => view.phase === "login",
      "old current Session rejected",
    );
    await controller.authorize("a-return-current");
    await views.wait(
      (view) => view.phase === "account",
      "later current-account login",
    );
    const second = records(platform).session;
    assert.ok(second?.id && second?.token);
    assert.notEqual(second.id, first.id);
    const current = await control.checkpoint("logout_current", {
      operation: currentLogout,
      actor: first.id,
      laterSession: second.id,
    });
    const material = JSON.stringify(records(platform).logouts);
    assert.ok(
      records(platform).logouts?.some(
        (item) => item.operation === currentLogout,
      ),
    );
    await control.arm("current_503", { actor: second.id });
    expect(
      await api("GET", "/api/v1/sessions", {
        operation: operation(),
        token: second.token,
      }),
      200,
    );
    expect(
      await api("GET", "/api/v1/accounts/current", {
        operation: operation(),
        token: first.token,
      }),
      401,
    );
    await controller.show("account");
    await views.wait(
      (view) =>
        view.phase === "pending" && view.canRecover && view.logoutPending,
      "original recovery after current 503",
    );
    assert.equal(JSON.stringify(records(platform).logouts), material);
    assert.equal(records(platform).session?.id, second.id);
    expect(
      await api("GET", "/api/v1/accounts/current", {
        operation: operation(),
        token: second.token,
      }),
      200,
    );
    await control.checkpoint("current_503", { actor: second.id });
    await control.arm("sessions_503", { actor: second.id });
    await controller.show("sessions");
    await views.wait(
      (view) =>
        view.phase === "pending" && view.canRecover && view.logoutPending,
      "original recovery after list 503",
    );
    assert.equal(JSON.stringify(records(platform).logouts), material);
    assert.equal(records(platform).session?.id, second.id);
    expect(
      await api("GET", "/api/v1/sessions", {
        operation: operation(),
        token: second.token,
      }),
      200,
    );
    await control.checkpoint("sessions_503", { actor: second.id });
    observations.current_503_retained = true;
    const beforeCurrentRead = await control.snapshot();
    await controller.recover();
    await views.wait(
      (view) => view.phase === "complete",
      "current Logout result read",
    );
    original(
      await api("GET", `/api/v1/revocations?operation_id=${currentLogout}`, {
        operation: operation(),
        token: second.token,
      }),
      current.facts,
    );
    const afterCurrentRead = await control.snapshot();
    if (
      JSON.stringify(beforeCurrentRead.counts) !==
      JSON.stringify(afterCurrentRead.counts)
    ) {
      throw new Error(
        "reading the original current Logout wrote durable facts",
      );
    }
    observations.later_login_used = true;
    if (
      current.facts.checks?.scope !== "current" ||
      current.facts.checks?.laterLoginUsable !== true ||
      current.facts.checks?.laterLoginExcluded !== true
    ) {
      throw new Error(
        "current Logout facts do not preserve the original initiator and later login",
      );
    }
    await controller.show("account");
    await views.wait(
      (view) => view.phase === "account",
      "account before all Logout",
    );

    const allPeer = await sessionFor("a-all-peer");
    const allBefore = await api("GET", "/api/v1/sessions", {
      operation: operation(),
      token: second.token,
    });
    expect(allBefore, 200);
    const ids = allBefore.value.sessions.map((item: any) => item.id);
    assert.deepEqual(ids, [...ids].sort());
    for (const item of allBefore.value.sessions) {
      assert.deepEqual(Object.keys(item).sort(), [
        "createdAt",
        "current",
        "id",
        "source",
      ]);
      assert.equal(item.source, "mini");
      assert.equal(item.current, item.id === second.id);
      assert.ok(Number.isFinite(Date.parse(item.createdAt)));
    }
    assert.deepEqual(
      allBefore.value.sessions.map((item: any) => item.id).sort(),
      [second.id, allPeer.id].sort(),
    );
    await control.arm("logout_all", { actor: second.id });
    await controller.logout("all");
    await views.wait((view) => view.phase === "pending", "all Logout pending");
    const allLogout = records(platform).logouts?.[0]?.operation;
    assert.ok(allLogout, "all Logout retained its operation");
    await controller.check();
    await views.wait(
      (view) => view.phase === "login",
      "all Logout cleared the Session",
    );
    await controller.authorize("a-return-all");
    await views.wait(
      (view) => view.phase === "account",
      "later all-account login",
    );
    const third = records(platform).session;
    assert.ok(third?.id && third?.token);
    assert.notEqual(third.id, second.id);
    const beforeAllRead = await control.snapshot();
    await controller.recover();
    await views.wait(
      (view) => view.phase === "complete",
      "all Logout result read",
    );
    const afterAllRead = await control.snapshot();
    if (
      JSON.stringify(beforeAllRead.counts) !==
      JSON.stringify(afterAllRead.counts)
    ) {
      throw new Error("reading the original all Logout wrote durable facts");
    }
    const all = await control.checkpoint("logout_all", {
      operation: allLogout,
      actor: second.id,
      laterSession: third.id,
    });
    if (
      all.facts.checks?.scope !== "all" ||
      all.facts.checks?.laterLoginUsable !== true ||
      all.facts.checks?.laterLoginExcluded !== true
    ) {
      throw new Error("all Logout facts do not exclude the later login");
    }
    observations.later_login_used = true;
    const allOriginal = await api(
      "GET",
      `/api/v1/revocations?operation_id=${allLogout}`,
      { operation: operation(), token: third.token },
    );
    expect(allOriginal, 200);
    original(allOriginal, all.facts);
    assert.deepEqual(
      allOriginal.value.revocations.map((item: any) => item.id).sort(),
      [second.id, allPeer.id].sort(),
    );
    assert.equal(records(platform).session?.id, third.id);
    expect(
      await api("GET", "/api/v1/accounts/current", {
        operation: operation(),
        token: third.token,
      }),
      200,
    );

    const fourth = await sessionFor("a-other-login");
    const target = await sessionFor("a-other-target");
    const bStart = await loginStart("b-register");
    assert.ok(bStart.registration?.id && bStart.registration?.token);
    const b = await register(
      bStart.registration,
      "隔离乙",
      "000000200002290029",
    );
    const bSession = b.session;
    assert.notEqual(bSession.id, fourth.id);
    expect(
      await api("DELETE", `/api/v1/sessions/${bSession.id}`, {
        operation: operation(),
        token: fourth.token,
      }),
      404,
      "resource_not_found",
    );

    const otherOperation = operation();
    await control.arm("logout_other", {
      operation: otherOperation,
      actor: fourth.id,
      target: target.id,
    });
    expect(
      await api("DELETE", `/api/v1/sessions/${target.id}`, {
        operation: operation(),
        token: fourth.token,
        body: {},
      }),
      400,
    );
    const droppedOther = await api("DELETE", `/api/v1/sessions/${target.id}`, {
      operation: otherOperation,
      token: fourth.token,
      expectDrop: true,
    });
    assert.equal(droppedOther.dropped, true);
    const beforeOtherRead = await control.snapshot();
    const otherResult = await api(
      "GET",
      `/api/v1/revocations?operation_id=${otherOperation}`,
      {
        operation: operation(),
        token: fourth.token,
      },
    );
    expect(otherResult, 200);
    if (
      otherResult.value.original?.scope !== "other" ||
      otherResult.value.original?.initiatorId !== fourth.id ||
      otherResult.value.count !== 1 ||
      otherResult.value.revocations?.[0]?.id !== target.id
    ) {
      throw new Error(
        "other Logout result does not preserve its initiator and exact target",
      );
    }

    const foreign = await api(
      "GET",
      `/api/v1/revocations?operation_id=${b.operation}`,
      {
        operation: operation(),
        token: fourth.token,
      },
    );
    expect(foreign, 200);
    if (
      foreign.value.original !== null ||
      foreign.value.count !== 0 ||
      foreign.value.revocations.length !== 0
    ) {
      throw new Error("another account read an A Logout result");
    }
    const bRead = await api(
      "GET",
      `/api/v1/revocations?operation_id=${String(otherOperation)}`,
      {
        operation: operation(),
        token: bSession.token,
      },
    );
    expect(bRead, 200);
    if (bRead.value.original !== null || bRead.value.count !== 0) {
      throw new Error("the B account observed an A Logout result");
    }
    const afterOtherRead = await control.snapshot();
    if (
      JSON.stringify(beforeOtherRead.counts) !==
      JSON.stringify(afterOtherRead.counts)
    ) {
      throw new Error("revocation result reads wrote durable facts");
    }
    const other = await control.checkpoint("logout_other", {
      operation: otherOperation,
      actor: fourth.id,
      target: target.id,
      foreignSession: bSession.id,
    });
    original(otherResult, other.facts);
    if (
      other.facts.checks?.scope !== "other" ||
      other.facts.checks?.initiatorPreserved !== true ||
      other.facts.checks?.otherAccountIsolated !== true
    ) {
      throw new Error(
        "other Logout facts do not preserve the exact singleton target",
      );
    }
    observations.other_account_isolated = true;

    const currentTarget = await api("DELETE", `/api/v1/sessions/${fourth.id}`, {
      operation: operation(),
      token: fourth.token,
    });
    expect(currentTarget, 409, "current_session");
    const staleTarget = await api("DELETE", `/api/v1/sessions/${first.id}`, {
      operation: operation(),
      token: fourth.token,
    });
    expect(staleTarget, 404, "resource_not_found");
    const fourthCurrent = await api("GET", "/api/v1/accounts/current", {
      operation: operation(),
      token: fourth.token,
    });
    expect(fourthCurrent, 200);

    const final = await control.snapshot();
    if (
      final.supplier.phoneExchanges !== 8 ||
      final.supplier.distinctCodes !== 8 ||
      final.supplier.tokenExchanges !== 1
    ) {
      throw new Error(
        "supplier exchanges do not match the finite fixture codes",
      );
    }
    observations.revocation_reads_read_only =
      JSON.stringify(beforeCurrentRead.counts) ===
        JSON.stringify(afterCurrentRead.counts) &&
      JSON.stringify(beforeAllRead.counts) ===
        JSON.stringify(afterAllRead.counts);

    return observations;
  } finally {
    controller.suspend();
  }
}
