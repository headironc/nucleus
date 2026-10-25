import {
  inspectImage,
  withoutOrientation,
  type ImageFormat,
} from "./format";

const MAX_BYTES = 2 * 1024 * 1024;
const MAX_SIDE = 1024;
const SUPPORTED = new Set<string>(["image/png", "image/jpeg", "image/webp"]);
const EMPTY: AvatarSelectionState = { kind: "empty" };

export type AvatarSelectionError =
  | "size"
  | "format"
  | "dimensions"
  | "animation"
  | "damaged"
  | "preview-unavailable";

export type AvatarCandidate = Readonly<{
  /** Original upload material. It stays in memory and is never persisted. */
  readonly file: File;
  /**
   * Paints the decoded local preview. It is not upload material and becomes a
   * no-op as soon as this candidate is superseded.
   */
  readonly draw: (context: CanvasRenderingContext2D) => void;
  readonly mediaType: ImageFormat;
  readonly width: number;
  readonly height: number;
  readonly name: string;
}>;

export type AvatarSelectionState =
  | { readonly kind: "empty" }
  | { readonly kind: "reading" }
  | { readonly kind: "ready"; readonly candidate: AvatarCandidate }
  | { readonly kind: "error"; readonly error: AvatarSelectionError };

type Decoded = Readonly<{
  width: number;
  height: number;
  draw(context: CanvasRenderingContext2D): void;
  close(): void;
}>;

type Dependencies = Readonly<{
  decode?: (blob: Blob) => Promise<Decoded>;
}>;

class PreviewUnavailable extends Error {}

function closeDecoded(decoded: Decoded | undefined) {
  if (!decoded) return;
  try {
    decoded.close();
  } catch {
    // A failed release cannot restore a superseded preview.
  }
}

function decodeImage(blob: Blob): Promise<Decoded> {
  if (typeof createImageBitmap !== "function")
    return Promise.reject(new PreviewUnavailable());
  return createImageBitmap(blob).then((bitmap) => ({
    width: bitmap.width,
    height: bitmap.height,
    draw: (context) => context.drawImage(bitmap, 0, 0),
    close: () => bitmap.close(),
  }));
}

/**
 * Owns the one local avatar candidate. Original bytes and the decoded preview
 * stay in this instance; a newer choice, an error, clear or close releases the
 * previous preview before another callback can replace it.
 */
export class AvatarSelection {
  private state: AvatarSelectionState = EMPTY;
  private readonly listeners = new Set<() => void>();
  private readonly decode: (blob: Blob) => Promise<Decoded>;
  private generation = 0;
  private closed = false;
  private decoded: Decoded | undefined;

  constructor(dependencies: Dependencies = {}) {
    this.decode = dependencies.decode ?? decodeImage;
  }

  snapshot = (): AvatarSelectionState => this.state;
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  async choose(file: File): Promise<void> {
    if (this.closed) return;
    const generation = ++this.generation;
    this.release();
    this.show({ kind: "reading" });
    let decoded: Decoded | undefined;
    try {
      if (file.size > MAX_BYTES) {
        this.fail(generation, "size");
        return;
      }
      if (file.type !== "" && !SUPPORTED.has(file.type)) {
        this.fail(generation, "format");
        return;
      }
      let buffer: ArrayBuffer;
      try {
        buffer = await file.arrayBuffer();
      } catch {
        this.fail(generation, "preview-unavailable");
        return;
      }
      if (!this.active(generation)) return;
      const bytes = new Uint8Array(buffer);
      if (bytes.length > MAX_BYTES) {
        this.fail(generation, "size");
        return;
      }
      const inspection = inspectImage(bytes);
      if (inspection.kind === "unsupported") {
        this.fail(generation, "format");
        return;
      }
      if (file.type !== "" && file.type !== inspection.format) {
        this.fail(generation, "format");
        return;
      }
      if (inspection.kind === "invalid") {
        this.fail(generation, "damaged");
        return;
      }
      if (inspection.animated) {
        this.fail(generation, "animation");
        return;
      }
      if (
        inspection.width !== inspection.height ||
        inspection.width < 1 ||
        inspection.width > MAX_SIDE
      ) {
        this.fail(generation, "dimensions");
        return;
      }
      const preview = withoutOrientation(bytes, inspection.format);
      if (!preview) {
        this.fail(generation, "damaged");
        return;
      }
      const blob = new Blob([preview], { type: inspection.format });
      try {
        decoded = await this.decode(blob);
      } catch (error) {
        this.fail(
          generation,
          error instanceof PreviewUnavailable
            ? "preview-unavailable"
            : "damaged",
        );
        return;
      }
      if (!this.active(generation)) {
        closeDecoded(decoded);
        return;
      }
      if (
        decoded.width !== inspection.width ||
        decoded.height !== inspection.height
      ) {
        closeDecoded(decoded);
        decoded = undefined;
        this.fail(generation, "damaged");
        return;
      }
      const owned = decoded;
      this.decoded = owned;
      decoded = undefined;
      this.show({
        kind: "ready",
        candidate: {
          file,
          draw: (context) => {
            if (this.decoded !== owned) return;
            owned.draw(context);
          },
          mediaType: inspection.format,
          width: inspection.width,
          height: inspection.height,
          name: file.name,
        },
      });
    } catch {
      closeDecoded(decoded);
      this.fail(generation, "damaged");
    }
  }

  previewFailed(candidate: AvatarCandidate) {
    if (this.state.kind !== "ready" || this.state.candidate !== candidate) return;
    this.fail(this.generation, "preview-unavailable");
  }

  clear() {
    if (this.closed) return;
    this.generation++;
    this.release();
    this.show(EMPTY);
  }

  close() {
    if (this.closed) return;
    this.closed = true;
    this.generation++;
    this.release();
    this.listeners.clear();
    this.state = EMPTY;
  }

  private active(generation: number): boolean {
    return !this.closed && generation === this.generation;
  }

  private fail(generation: number, error: AvatarSelectionError) {
    if (!this.active(generation)) return;
    this.release();
    this.show({ kind: "error", error });
  }

  private release() {
    const decoded = this.decoded;
    this.decoded = undefined;
    closeDecoded(decoded);
  }

  private show(state: AvatarSelectionState) {
    this.state = state;
    for (const listener of [...this.listeners]) listener();
  }
}
