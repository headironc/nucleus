import type { Platform, Reply } from "./platform";

export type Request = {
  method: "GET" | "POST" | "DELETE";
  path: string;
  operation: string;
  token?: string;
  input?: unknown;
};
export const requestTimeout = 35000;
export type Exchange = { promise: Promise<Reply | undefined>; readonly pending: boolean; cancel(): void };

export function apiOrigin(value: string): string {
  if (!/^https:\/\/[a-z0-9](?:[a-z0-9.-]*[a-z0-9])?(?::[1-9][0-9]{0,4})?$/.test(value)) {
    throw new Error("An exact HTTPS API origin is required");
  }
  return value;
}

/** Aborting ends the local wait; it does not establish the server write outcome. */
export class Transport {
  constructor(private platform: Platform, private origin: string) {
    apiOrigin(origin);
  }

  send(request: Request): Exchange {
    let finish: (reply: Reply | undefined) => void = () => {};
    let settled = false;
    const promise = new Promise<Reply | undefined>(resolve => {
      finish = reply => {
        if (settled) return;
        settled = true;
        resolve(reply);
      };
    });
    const header: Record<string, string> = {
      "Operation-Id": request.operation,
      Accept: "application/json",
    };
    if (request.token) header.Authorization = `Bearer ${request.token}`;
    if (request.input !== undefined) header["Content-Type"] = "application/json";
    let task: { abort(): void } | undefined;
    try {
      task = this.platform.request({
        url: `${this.origin}${request.path}`,
        method: request.method,
        header,
        ...(request.input === undefined ? {} : { data: JSON.stringify(request.input) }),
        dataType: "text",
        responseType: "text",
        timeout: requestTimeout,
        success: finish,
        fail: () => finish(undefined),
      });
    } catch { finish(undefined); }
    return {
      promise,
      get pending() { return !settled; },
      cancel() {
        if (settled) return;
        finish(undefined);
        try { task?.abort(); } catch { /* The generation guard still rejects late replies. */ }
      },
    };
  }
}
