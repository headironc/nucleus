export const MAX_BADGE_BYTES = 16 * 1024 * 1024;

type BadgeBytes =
  { kind: "ok"; value: Blob } | { kind: "untrusted" | "disconnected" };

/** Rejects unbounded bodies before reading and checks the exact encoded size. */
export async function readBadgeBytes(
  response: Response,
  signal: AbortSignal,
): Promise<BadgeBytes> {
  const mediaType = response.headers
    .get("content-type")
    ?.split(";")[0]
    .trim()
    .toLowerCase();
  const declared = response.headers.get("content-length");
  const length = declared && /^\d+$/.test(declared) ? Number(declared) : 0;
  if (
    signal.aborted ||
    response.status !== 200 ||
    mediaType !== "image/webp" ||
    !Number.isSafeInteger(length) ||
    length <= 0 ||
    length > MAX_BADGE_BYTES
  ) {
    await response.body?.cancel().catch(() => {});
    return { kind: signal.aborted ? "disconnected" : "untrusted" };
  }
  const reader = response.body?.getReader();
  if (!reader) return { kind: "untrusted" };
  const abort = () => void reader.cancel().catch(() => {});
  signal.addEventListener("abort", abort, { once: true });
  let complete = false;
  try {
    const bytes = new Uint8Array(length);
    let size = 0;
    for (;;) {
      if (signal.aborted) return { kind: "disconnected" };
      const item = await reader.read();
      if (signal.aborted) return { kind: "disconnected" };
      if (item.done) break;
      if (item.value.byteLength > length - size) return { kind: "untrusted" };
      bytes.set(item.value, size);
      size += item.value.byteLength;
    }
    if (size !== length) return { kind: "untrusted" };
    complete = true;
    return { kind: "ok", value: new Blob([bytes], { type: "image/webp" }) };
  } catch {
    return { kind: "disconnected" };
  } finally {
    signal.removeEventListener("abort", abort);
    if (!complete) await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}
