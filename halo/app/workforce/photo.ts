import { inspectImage } from "../avatar/format";

const MAX_BYTES = 8 * 1024 * 1024;
const MAX_SIDE = 4096;
const MAX_PIXELS = 16_000_000;

export type PhotoError = "size" | "format" | "animation" | "dimensions" | "crop" | "damaged" | "unavailable";
export type PhotoFrame = Readonly<{ width: number; height: number }>;
export type PhotoCrop = Readonly<{ zoom: number; horizontal: number; vertical: number }>;
export type PhotoRectangle = Readonly<{ x: number; y: number; width: number; height: number }>;
export const INITIAL_CROP: PhotoCrop = { zoom: 1, horizontal: 0.5, vertical: 0.5 };

/** Coordinates use the decoded, orientation-corrected image shown to its owner. */
export function photoRectangle(image: PhotoFrame, frame: PhotoFrame, crop: PhotoCrop): PhotoRectangle {
  if (![image.width, image.height, frame.width, frame.height].every((value) => Number.isFinite(value) && value > 0)
    || !Number.isFinite(crop.zoom) || crop.zoom < 1 || crop.zoom > 4
    || ![crop.horizontal, crop.vertical].every((value) => Number.isFinite(value) && value >= 0 && value <= 1))
    throw new Error("invalid_photo_crop");
  const ratio = frame.width / frame.height;
  const fittedWidth = Math.min(image.width, image.height * ratio) / crop.zoom;
  const width = Math.floor(fittedWidth);
  const height = Math.floor(fittedWidth / ratio);
  return { x: Math.floor((image.width - width) * crop.horizontal), y: Math.floor((image.height - height) * crop.vertical), width, height };
}

type DecodedPhoto = PhotoFrame & {
  draw(context: CanvasRenderingContext2D, rectangle: PhotoRectangle, output: PhotoFrame): void;
  close(): void;
};
export type ConfirmedPhoto = Readonly<{ file: File; width: number; height: number }>;
export type PhotoState =
  | { kind: "empty" }
  | { kind: "reading" }
  | { kind: "error"; error: PhotoError }
  | { kind: "editing"; name: string; image: PhotoFrame; frame: PhotoFrame; crop: PhotoCrop; confirmed: ConfirmedPhoto | null; preparing: boolean };

type Dependencies = {
  decode(blob: Blob): Promise<DecodedPhoto>;
  encode(image: DecodedPhoto, rectangle: PhotoRectangle, output: PhotoFrame): Promise<ConfirmedPhoto>;
};

async function decode(blob: Blob): Promise<DecodedPhoto> {
  const bitmap = await createImageBitmap(blob, { imageOrientation: "from-image" });
  return {
    width: bitmap.width, height: bitmap.height,
    draw(context, source, output) {
      context.drawImage(bitmap, source.x, source.y, source.width, source.height, 0, 0, output.width, output.height);
    },
    close: () => bitmap.close(),
  };
}

async function encode(image: DecodedPhoto, rectangle: PhotoRectangle, output: PhotoFrame): Promise<ConfirmedPhoto> {
  const canvas = document.createElement("canvas");
  canvas.width = output.width;
  canvas.height = output.height;
  try {
    const context = canvas.getContext("2d");
    if (!context) throw new Error("photo_canvas_unavailable");
    context.imageSmoothingQuality = "high";
    image.draw(context, rectangle, canvas);
    const blob = await new Promise<Blob>((resolve, reject) => canvas.toBlob((result) => result ? resolve(result) : reject(new Error("photo_encode_failed")), "image/png"));
    return { file: new File([blob], "application-photo.png", { type: "image/png" }), width: canvas.width, height: canvas.height };
  } finally {
    canvas.width = 0;
    canvas.height = 0;
  }
}

/** One in-memory photo, released on replacement, hiding, or departure. No upload or persistence. */
export class ApplicationPhoto {
  private state: PhotoState = { kind: "empty" };
  private readonly listeners = new Set<() => void>();
  private decoded?: DecodedPhoto;
  private generation = 0;
  private closed = false;
  constructor(private readonly dependencies: Dependencies = { decode, encode }) {}
  snapshot = () => this.state;
  subscribe = (listener: () => void) => { this.listeners.add(listener); return () => this.listeners.delete(listener); };

  async choose(file: File, frame: PhotoFrame): Promise<void> {
    if (this.closed) return;
    photoRectangle({ width: 1, height: 1 }, frame, INITIAL_CROP);
    const generation = ++this.generation;
    this.release();
    this.show({ kind: "reading" });
    let image: DecodedPhoto | undefined;
    try {
      if (file.size > MAX_BYTES) return this.fail(generation, "size");
      const bytes = new Uint8Array(await file.arrayBuffer());
      if (!this.active(generation)) return;
      if (bytes.length > MAX_BYTES) return this.fail(generation, "size");
      const facts = inspectImage(bytes);
      if (facts.kind === "unsupported" || (file.type !== "" && file.type !== facts.format)) return this.fail(generation, "format");
      if (facts.kind === "invalid") return this.fail(generation, "damaged");
      if (facts.animated) return this.fail(generation, "animation");
      if (!this.dimensions(facts)) return this.fail(generation, "dimensions");
      image = await this.dependencies.decode(new Blob([bytes], { type: facts.format }));
      if (!this.active(generation)) { image.close(); return; }
      const same = image.width === facts.width && image.height === facts.height;
      const rotated = facts.orientation && image.width === facts.height && image.height === facts.width;
      if (!this.dimensions(image) || (!same && !rotated)) {
        image.close(); image = undefined;
        return this.fail(generation, "damaged");
      }
      this.decoded = image;
      image = undefined;
      this.show({ kind: "editing", name: file.name, image: { width: this.decoded.width, height: this.decoded.height }, frame, crop: INITIAL_CROP, confirmed: null, preparing: false });
    } catch {
      image?.close();
      this.fail(generation, "damaged");
    }
  }

  adjust(crop: PhotoCrop) {
    const state = this.state;
    if (state.kind !== "editing" || this.closed) return;
    photoRectangle(state.image, state.frame, crop);
    this.generation++;
    this.show({ ...state, crop: { ...crop }, confirmed: null, preparing: false });
  }

  reframe(frame: PhotoFrame) {
    const state = this.state;
    if (state.kind !== "editing" || (state.frame.width === frame.width && state.frame.height === frame.height)) return;
    photoRectangle(state.image, frame, INITIAL_CROP);
    this.generation++;
    this.show({ ...state, frame, crop: INITIAL_CROP, confirmed: null, preparing: false });
  }

  draw(context: CanvasRenderingContext2D, output: PhotoFrame) {
    const state = this.state;
    if (state.kind !== "editing" || !this.decoded) return;
    const rectangle = photoRectangle(state.image, state.frame, state.crop);
    if (rectangle.width < 1 || rectangle.height < 1) return this.fail(this.generation, "crop");
    const scale = Math.min(output.width / rectangle.width, output.height / rectangle.height);
    const contained = { width: rectangle.width * scale, height: rectangle.height * scale };
    context.clearRect(0, 0, output.width, output.height);
    context.save();
    try {
      context.translate((output.width - contained.width) / 2, (output.height - contained.height) / 2);
      this.decoded.draw(context, rectangle, contained);
    } finally { context.restore(); }
  }

  async confirm(): Promise<void> {
    const state = this.state;
    const image = this.decoded;
    if (state.kind !== "editing" || !image || state.preparing || this.closed) return;
    const generation = ++this.generation;
    this.show({ ...state, preparing: true, confirmed: null });
    try {
      const rectangle = photoRectangle(state.image, state.frame, state.crop);
      if (rectangle.width < 1 || rectangle.height < 1) return this.fail(generation, "crop");
      const output = { width: rectangle.width, height: rectangle.height };
      const result = await this.dependencies.encode(image, rectangle, output);
      if (!this.active(generation)) return;
      if (result.file.size > MAX_BYTES) return this.fail(generation, "size");
      this.show({ ...state, preparing: false, confirmed: result });
    } catch { this.fail(generation, "unavailable"); }
  }

  previewFailed() { this.fail(this.generation, "unavailable"); }
  clear() {
    if (this.closed) return;
    this.generation++;
    this.release();
    this.show({ kind: "empty" });
  }
  close() {
    this.clear();
    this.closed = true;
    this.listeners.clear();
  }
  private dimensions(image: PhotoFrame) {
    return image.width >= 1 && image.height >= 1 && image.width <= MAX_SIDE && image.height <= MAX_SIDE && image.width * image.height <= MAX_PIXELS;
  }
  private active(generation: number) { return !this.closed && generation === this.generation; }
  private fail(generation: number, error: PhotoError) {
    if (!this.active(generation)) return;
    this.generation++;
    this.release();
    this.show({ kind: "error", error });
  }
  private release() { const image = this.decoded; this.decoded = undefined; image?.close(); }
  private show(state: PhotoState) { this.state = state; for (const listener of this.listeners) listener(); }
}
