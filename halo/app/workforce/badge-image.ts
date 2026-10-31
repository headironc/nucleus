import { inspectImage } from "../avatar/format";
import { MAX_BADGE_BYTES } from "../protocol/badge-media";
import {
  badgeResource,
  type ApplicationResult,
} from "../protocol/applications";

export async function loadBadgeImage(
  session: string,
  id: string,
  signal: AbortSignal,
): Promise<ApplicationResult<BadgeImage>> {
  const bytes = await badgeResource(session, id, signal);
  if (bytes.kind !== "ok") return bytes;
  const image = await BadgeImage.open(bytes.value, signal);
  return image
    ? { kind: "ok", value: image }
    : { kind: signal.aborted ? "disconnected" : "untrusted" };
}

/** Owns one stored image URL; the original encoded bytes are never repainted. */
export class BadgeImage {
  private closed = false;
  private constructor(
    readonly url: string,
    readonly width: number,
    readonly height: number,
  ) {}

  static async open(
    blob: Blob,
    signal: AbortSignal,
  ): Promise<BadgeImage | undefined> {
    if (
      signal.aborted ||
      blob.type !== "image/webp" ||
      blob.size < 1 ||
      blob.size > MAX_BADGE_BYTES
    )
      return;
    let bitmap: ImageBitmap | undefined;
    try {
      const image = inspectImage(new Uint8Array(await blob.arrayBuffer()));
      if (
        signal.aborted ||
        image.kind !== "valid" ||
        image.format !== "image/webp" ||
        image.animated ||
        image.orientation ||
        image.width < 1 ||
        image.height < 1 ||
        image.width > 4096 ||
        image.height > 4096 ||
        image.width * image.height > 4194304
      )
        return;
      bitmap = await createImageBitmap(blob);
      if (
        signal.aborted ||
        bitmap.width !== image.width ||
        bitmap.height !== image.height
      )
        return;
      return new BadgeImage(
        URL.createObjectURL(blob),
        image.width,
        image.height,
      );
    } catch {
      return;
    } finally {
      bitmap?.close();
    }
  }

  close() {
    if (this.closed) return;
    this.closed = true;
    URL.revokeObjectURL(this.url);
  }
}
