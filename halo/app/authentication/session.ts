import { validSession, type Session } from "../protocol/session";

export type BrowserSession = Readonly<{
  version: 1;
  revision: string;
  session: Session | null;
  attempt: string | null;
}>;

export type Attempt = Readonly<{ operation: string; revision: string }>;
export type Selection = Readonly<{ id: string; revision: string }>;

const DATABASE = "halo.session";
const STORE = "selection";
const KEY = "current";
const UUID_V4 =
  /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const TIMEOUT = 5_000;

export class SessionStorageError extends Error {
  constructor(readonly kind: "unavailable" | "invalid" = "unavailable") {
    super("Browser session storage is unavailable or invalid");
    this.name = "SessionStorageError";
  }
}

function record(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function session(value: unknown): value is Session {
  return (
    record(value) && Object.keys(value).length === 2 && validSession(value)
  );
}

function parse(value: unknown): BrowserSession | null {
  if (value === undefined) return null;
  if (
    !record(value) ||
    Object.keys(value).length !== 4 ||
    value.version !== 1 ||
    typeof value.revision !== "string" ||
    !UUID_V4.test(value.revision) ||
    !(value.session === null || session(value.session)) ||
    !(
      value.attempt === null ||
      (typeof value.attempt === "string" && UUID_V4.test(value.attempt))
    )
  ) {
    throw new SessionStorageError("invalid");
  }
  return value as BrowserSession;
}

function revision(): string {
  const value = globalThis.crypto.randomUUID();
  if (!UUID_V4.test(value)) throw new SessionStorageError();
  return value;
}

export function sameAttempt(
  current: BrowserSession | null,
  expected: Attempt,
): boolean {
  return (
    current?.attempt === expected.operation &&
    current.revision === expected.revision
  );
}

export function sameSelection(
  current: BrowserSession | null,
  expected: Selection,
): boolean {
  return (
    current?.session?.id === expected.id &&
    current.revision === expected.revision
  );
}

/**
 * Each comparison and replacement shares one local transaction. No request or
 * current-account data is allowed inside this database.
 */
export class Sessions {
  private database?: Promise<IDBDatabase>;
  private channel?: BroadcastChannel;
  private unavailable = false;

  private communication(): BroadcastChannel {
    if (this.unavailable) throw new SessionStorageError();
    try {
      return (this.channel ??= new BroadcastChannel(DATABASE));
    } catch {
      this.unavailable = true;
      throw new SessionStorageError();
    }
  }

  watch(listener: () => void): () => void {
    try {
      const channel = this.communication();
      channel.addEventListener("message", listener);
      return () => channel.removeEventListener("message", listener);
    } catch {
      this.unavailable = true;
      // The next read reports the controlled storage state to the page.
      return () => {};
    }
  }

  private notify() {
    let temporary: BroadcastChannel | undefined;
    try {
      // A transaction can finish after its page closes the listening channel.
      const channel =
        this.channel ?? (temporary = new BroadcastChannel(DATABASE));
      channel.postMessage(null);
    } catch {
      // The committed selection stays known; a hint cannot undo its result.
    } finally {
      temporary?.close();
    }
  }

  private open(): Promise<IDBDatabase> {
    try {
      this.communication();
    } catch {
      return Promise.reject(new SessionStorageError());
    }
    if (this.database) return this.database;
    this.database = new Promise<IDBDatabase>((resolve, reject) => {
      let finished = false;
      const fail = () => {
        if (finished) return;
        finished = true;
        clearTimeout(timer);
        reject(new SessionStorageError());
      };
      const timer = setTimeout(fail, TIMEOUT);
      let request: IDBOpenDBRequest;
      try {
        request = indexedDB.open(DATABASE, 1);
      } catch {
        fail();
        return;
      }
      request.onupgradeneeded = () => {
        request.result.createObjectStore(STORE);
      };
      request.onerror = fail;
      request.onblocked = fail;
      request.onsuccess = () => {
        const database = request.result;
        if (finished) {
          database.close();
          return;
        }
        finished = true;
        clearTimeout(timer);
        database.onversionchange = () => {
          database.close();
          this.database = undefined;
        };
        resolve(database);
      };
    }).catch((error: unknown) => {
      this.database = undefined;
      throw error;
    });
    return this.database;
  }

  private async transaction<T>(
    mode: IDBTransactionMode,
    update: (value: unknown) => { result: T; replacement?: BrowserSession },
  ): Promise<T> {
    const database = await this.open();
    return new Promise((resolve, reject) => {
      let finished = false;
      let result: T;
      let failure: SessionStorageError | undefined;
      let transaction: IDBTransaction;
      try {
        transaction = database.transaction(STORE, mode);
      } catch {
        this.database = undefined;
        reject(new SessionStorageError());
        return;
      }
      const fail = () => {
        if (finished) return;
        finished = true;
        clearTimeout(timer);
        reject(failure ?? new SessionStorageError());
      };
      const timer = setTimeout(() => {
        try {
          transaction.abort();
        } catch {
          // A completed transaction may already be awaiting its completion event.
        }
        fail();
      }, TIMEOUT);
      transaction.onerror = fail;
      transaction.onabort = fail;
      transaction.oncomplete = () => {
        if (finished) return;
        finished = true;
        clearTimeout(timer);
        resolve(result);
      };
      const store = transaction.objectStore(STORE);
      const request = store.get(KEY);
      request.onsuccess = () => {
        try {
          const next = update(request.result);
          result = next.result;
          if (next.replacement) store.put(next.replacement, KEY);
        } catch (error) {
          if (error instanceof SessionStorageError) failure = error;
          transaction.abort();
        }
      };
    });
  }

  read(): Promise<BrowserSession | null> {
    return this.transaction("readonly", (value) => ({ result: parse(value) }));
  }

  /** A new, explicit login invalidates earlier attempts and in-flight reads. */
  async begin(operation: string): Promise<Attempt> {
    if (!UUID_V4.test(operation))
      return Promise.reject(new SessionStorageError());
    const result = await this.transaction("readwrite", (value) => {
      let current: BrowserSession | null = null;
      try {
        current = parse(value);
      } catch {
        // Only explicit reauthentication may replace a damaged local selection.
      }
      const next: BrowserSession = {
        version: 1,
        revision: revision(),
        session: current?.session ?? null,
        attempt: operation,
      };
      return {
        result: { operation, revision: next.revision },
        replacement: next,
      };
    });
    this.notify();
    return result;
  }

  async accept(expected: Attempt, result: Session): Promise<Selection | null> {
    if (!validSession(result)) return Promise.reject(new SessionStorageError());
    const selected = await this.transaction<Selection | null>(
      "readwrite",
      (value) => {
        const current = parse(value);
        if (!sameAttempt(current, expected)) return { result: null };
        const next: BrowserSession = {
          version: 1,
          revision: revision(),
          session: { id: result.id, expiresAt: result.expiresAt },
          attempt: null,
        };
        return {
          result: { id: result.id, revision: next.revision },
          replacement: next,
        };
      },
    );
    if (selected) this.notify();
    return selected;
  }

  async clear(expected: Selection): Promise<boolean> {
    return (await this.release(expected)) !== null;
  }

  /** Returns the exact revision committed by this compare-and-clear. */
  async release(expected: Selection): Promise<string | null> {
    const cleared = await this.transaction<string | null>("readwrite", (value) => {
      const current = parse(value);
      if (!current || !sameSelection(current, expected))
        return { result: null };
      const nextRevision = current.attempt ? current.revision : revision();
      return {
        result: nextRevision,
        replacement: {
          version: 1,
          // Removing the old session must preserve the active attempt's CAS.
          revision: nextRevision,
          session: null,
          attempt: current.attempt,
        },
      };
    });
    if (cleared) this.notify();
    return cleared;
  }

  async close(): Promise<void> {
    this.channel?.close();
    this.channel = undefined;
    const opening = this.database;
    this.database = undefined;
    if (opening) (await opening).close();
  }
}
