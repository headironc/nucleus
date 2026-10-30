import { Controller } from "../src/controller";
import { Protocol } from "../src/protocol";
import type { Platform, Reply } from "../src/platform";
import { storageKey, type Records } from "../src/storage";

export const ids = {
  account: "00000000-0000-4000-8000-000000000001",
  otherAccount: "00000000-0000-4000-8000-000000000002",
  session: "00000000-0000-4000-8000-000000000003",
  later: "00000000-0000-4000-8000-000000000004",
  otherSession: "00000000-0000-4000-8000-000000000005",
  registration: "00000000-0000-4000-8000-000000000006",
};
export const epoch = Date.parse("2026-10-30T08:00:00Z");
export const token = "A".repeat(43);
export function session(id = ids.session) { return { id, token, expiresAt: new Date(epoch + 30 * 86400000).toISOString() }; }
export function account(id = ids.account) {
  return { id, personId: "00000000-0000-4000-8000-000000000007", revisionId: "00000000-0000-4000-8000-000000000008",
    name: id === ids.otherAccount ? "李明" : "张宁", idNumber: { suffix: "002X" }, binding: null,
    avatar: { revision: "00000000-0000-4000-8000-000000000009", mediaType: "image/webp", width: 256, height: 256,
      href: `https://avatar.example.test/api/v1/avatars/${"a".repeat(64)}.webp?key_id=1&expires_at=1793347200&signature=${token}`,
      expiresAt: "2026-10-30T08:00:00Z" } };
}
export function proof() { return { id: ids.registration, token, expiresAt: new Date(epoch + 600000).toISOString() }; }
export type NativeRequest = Parameters<Platform["request"]>[0];
export class FakePlatform implements Platform {
  saved: Record<string, unknown> = {};
  writes = 0;
  storageFails = false;
  randomFails = false;
  randomCounter = 0;
  requests: NativeRequest[] = [];
  aborted: number[] = [];
  private waiters = new Map<number, (value: NativeRequest) => void>();
  getRandomValues(options: Parameters<Platform["getRandomValues"]>[0]) {
    if (this.randomFails) return options.fail();
    const bytes = new Uint8Array(options.length);
    bytes.fill(++this.randomCounter);
    options.success({ randomValues: bytes.buffer });
  }
  getStorageSync(key: string) {
    if (this.storageFails) throw new Error("Storage unavailable");
    return this.saved[key] === undefined ? "" : JSON.parse(JSON.stringify(this.saved[key]));
  }
  setStorageSync(key: string, value: unknown) {
    if (this.storageFails) throw new Error("Storage unavailable");
    this.writes++;
    this.saved[key] = JSON.parse(JSON.stringify(value));
  }
  request(options: NativeRequest) {
    const index = this.requests.push(options) - 1;
    this.waiters.get(index)?.(options);
    this.waiters.delete(index);
    return { abort: () => { this.aborted.push(index); } };
  }
  next(index: number): Promise<NativeRequest> {
    if (this.requests[index]) return Promise.resolve(this.requests[index]);
    return new Promise(resolve => this.waiters.set(index, resolve));
  }
  reply(index: number, value: unknown, statusCode = 200, headers: Record<string, string> = {}) {
    this.requests[index].success({ statusCode, header: { "Content-Type": "application/json", ...headers }, data: JSON.stringify(value) });
  }
  disconnect(index: number) { this.requests[index].fail(); }
  records(): Records { return this.getStorageSync(storageKey) as Records; }
}
export const controllers: Controller[] = [];
export function setup(records?: Records) {
  const platform = new FakePlatform();
  if (records) platform.setStorageSync(storageKey, records);
  let now = epoch;
  const controller = new Controller(platform, "https://api.example.test", new Protocol(), () => now);
  controllers.push(controller);
  return { platform, controller, time: (value: number) => { now = value; } };
}
export async function login(harness: ReturnType<typeof setup>, id = ids.session, accountId = ids.account) {
  const { controller, platform } = harness;
  const index = platform.requests.length;
  const work = controller.authorize("phone-authorize-code");
  await platform.next(index);
  platform.reply(index, { code: "ok", session: session(id) });
  await platform.next(index + 1);
  platform.reply(index + 1, { code: "ok", account: account(accountId) });
  await work;
}
