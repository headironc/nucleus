export type Reply = {
  statusCode: number;
  header: Record<string, string>;
  data: string | ArrayBuffer | unknown;
};

/** Only the native capabilities used by this consumer. */
export interface Platform {
  getRandomValues(options: {
    length: number;
    success(result: { randomValues: ArrayBuffer }): void;
    fail(): void;
  }): void;
  getStorageSync(key: string): unknown;
  setStorageSync(key: string, value: unknown): void;
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
  }): { abort(): void };
}

export function random(platform: Platform, length: number): Promise<Uint8Array> {
  return new Promise((resolve, reject) => {
    if (typeof platform.getRandomValues !== "function") {
      reject(new Error("Secure random unavailable"));
      return;
    }
    platform.getRandomValues({
      length,
      success({ randomValues }) {
        if (!(randomValues instanceof ArrayBuffer) || randomValues.byteLength !== length) {
          reject(new Error("Invalid random bytes"));
        } else resolve(new Uint8Array(randomValues));
      },
      fail() { reject(new Error("Secure random unavailable")); },
    });
  });
}

export async function operation(platform: Platform): Promise<string> {
  const bytes = await random(platform, 16);
  bytes[6] = (bytes[6] & 15) | 64;
  bytes[8] = (bytes[8] & 63) | 128;
  const text = Array.from(bytes, value => value.toString(16).padStart(2, "0")).join("");
  return `${text.slice(0, 8)}-${text.slice(8, 12)}-${text.slice(12, 16)}-${text.slice(16, 20)}-${text.slice(20)}`;
}

export async function secret(platform: Platform): Promise<string> {
  const bytes = await random(platform, 32);
  const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
  let result = "";
  for (let offset = 0; offset < bytes.length; offset += 3) {
    const value = (bytes[offset] << 16) | ((bytes[offset + 1] ?? 0) << 8) | (bytes[offset + 2] ?? 0);
    result += alphabet[(value >>> 18) & 63] + alphabet[(value >>> 12) & 63];
    if (offset + 1 < bytes.length) result += alphabet[(value >>> 6) & 63];
    if (offset + 2 < bytes.length) result += alphabet[value & 63];
  }
  return result;
}
