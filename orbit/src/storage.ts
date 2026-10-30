import type { Platform } from "./platform";
import type { Protocol, Session, Registration, LoginInput, RegistrationInput } from "./protocol";

export const storageKey = "nucleus.orbit.identity.v1";
export type Pending<T> = {
  operation: string;
  input: T;
  createdAt: number;
  deadline: number;
  retryAt: number;
  stopped: boolean;
  uncertain: boolean;
};
export type Selected = Session & { accountId?: string };
export type Logout = {
  operation: string;
  accountId: string;
  sessionId: string;
  scope: "current" | "all";
  createdAt: number;
  deadline: number;
};
export type Records = {
  version: 1;
  session?: Selected;
  authentication?: Pending<LoginInput>;
  registration?: { proof: Registration; createdAt: number; deadline: number; pending?: Pending<RegistrationInput> };
  logouts?: Logout[];
  expiredLogouts?: string[];
  unresolved?: "authentication" | "registration";
};

const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
function time(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}
function window(value: { createdAt: number; deadline: number }, now: number): boolean {
  return time(value.createdAt) && time(value.deadline) &&
    value.createdAt <= now && now < value.deadline &&
    value.deadline - value.createdAt <= 30 * 60000;
}

export class Storage {
  constructor(private platform: Platform, private protocol: Protocol) {}

  load(now: number): Records {
    const raw = this.platform.getStorageSync(storageKey);
    if (raw === "" || raw === undefined || raw === null) return { version: 1 };
    if (!raw || typeof raw !== "object" || Array.isArray(raw)) throw new Error("Invalid identity storage");
    const records = JSON.parse(JSON.stringify(raw)) as Records;
    if (records.version !== 1) throw new Error("Invalid identity storage");
    if (records.session) {
      const { accountId, ...session } = records.session;
      if (!this.protocol.validSession(session) || (accountId !== undefined && !uuid.test(accountId))) {
        throw new Error("Invalid selected session");
      }
      if (Date.parse(session.expiresAt) <= now) delete records.session;
    }
    const validPending = <T>(value: Pending<T>, valid: (input: unknown) => boolean) => {
      if (!uuid.test(value.operation) || !valid(value.input) || !time(value.retryAt) || typeof value.stopped !== "boolean" || typeof value.uncertain !== "boolean") {
        throw new Error("Invalid recovery record");
      }
      return window(value, now);
    };
    if (records.authentication && !validPending(records.authentication, input => this.protocol.validLogin(input))) {
      delete records.authentication;
      records.unresolved = "authentication";
    }
    if (records.registration) {
      const value = records.registration;
      if (!this.protocol.validRegistration(value.proof)) throw new Error("Invalid registration proof");
      // Submitted R2 owns its recovery window even after its form proof expires.
      if (value.pending) {
        if (!validPending(value.pending, input => this.protocol.validRegistrationInput(input))) {
          delete records.registration;
          records.unresolved = "registration";
        }
      } else if (!window(value, now) || Date.parse(value.proof.expiresAt) <= now) {
        delete records.registration;
        records.unresolved = "registration";
      }
    }
    if (records.logouts) {
      if (!Array.isArray(records.logouts)) throw new Error("Invalid logout records");
      const accounts = new Set<string>();
      records.logouts = records.logouts.filter(value => {
      if (!uuid.test(value.operation) || !uuid.test(value.accountId) || !uuid.test(value.sessionId) || !["current", "all"].includes(value.scope)) {
        throw new Error("Invalid logout record");
      }
      if (accounts.has(value.accountId)) throw new Error("Duplicate account logout record");
      accounts.add(value.accountId);
      if (window(value, now)) return true;
      records.expiredLogouts = [...new Set([...(records.expiredLogouts ?? []), value.accountId])];
      return false;
      });
    }
    if (records.expiredLogouts && (!Array.isArray(records.expiredLogouts) || records.expiredLogouts.some(value => !uuid.test(value)))) {
      throw new Error("Invalid expired logout markers");
    }
    if (records.unresolved !== undefined && !["authentication", "registration"].includes(records.unresolved)) {
      throw new Error("Invalid recovery placeholder");
    }
    if (JSON.stringify(records) !== JSON.stringify(raw)) this.save(records);
    return records;
  }

  save(records: Records): void {
    // Native storage must acknowledge the exact complete material before any write.
    this.platform.setStorageSync(storageKey, records);
    if (JSON.stringify(this.platform.getStorageSync(storageKey)) !== JSON.stringify(records)) {
      throw new Error("Identity storage did not retain the write");
    }
  }
}
