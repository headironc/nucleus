import {
  applicationResource,
  sameApplicationContent,
  type ApplicationResult,
  type ApplicationSnapshot,
  type ApplicationTemplate,
  type PhotoMetadata,
} from "../protocol/applications";
import type { PhotoFrame } from "./photo";

type Template = ApplicationTemplate["template"];
export type ArtworkError = "font" | "image" | "layout";
export type ArtworkResult<T> =
  ApplicationResult<T> | { kind: "artwork-error"; error: ArtworkError };
const FIELDS = ["name", "number", "department", "title"] as const;

function areaValid(area: Template["layout"]["photo"], template: Template) {
  const layout = template.layout;
  return (
    [
      area.xUm,
      area.yUm,
      area.widthUm,
      area.heightUm,
      layout.widthUm,
      layout.heightUm,
    ].every(Number.isFinite) &&
    area.xUm >= 0 &&
    area.yUm >= 0 &&
    area.widthUm > 0 &&
    area.heightUm > 0 &&
    area.xUm + area.widthUm <= layout.widthUm &&
    area.yUm + area.heightUm <= layout.heightUm
  );
}

/** Owns only the current fixed background and its exact font bytes. */
export class TemplateArtwork {
  private closed = false;
  constructor(
    readonly template: Template,
    private readonly background: ImageBitmap,
    private readonly font: FontFace,
  ) {}
  close() {
    if (this.closed) return;
    this.closed = true;
    this.background.close();
    document.fonts.delete(this.font);
  }
  backdrop = (context: CanvasRenderingContext2D, output: PhotoFrame) => {
    if (this.closed) return;
    const layout = this.template.layout,
      slot = layout.photo;
    context.save();
    try {
      const scale = output.width / slot.widthUm;
      context.scale(scale, scale);
      context.drawImage(
        this.background,
        -slot.xUm,
        -slot.yUm,
        layout.widthUm,
        layout.heightUm,
      );
    } finally {
      context.restore();
    }
  };

  async preview(
    snapshot: ApplicationSnapshot,
    photo: PhotoMetadata,
    blob: Blob,
  ): Promise<ArtworkResult<ApplicationArtwork>> {
    if (
      this.closed ||
      !sameApplicationContent(snapshot.template, this.template) ||
      snapshot.photoId !== photo.photoId ||
      photo.mediaType !== "image/webp" ||
      blob.type !== photo.mediaType
    )
      return { kind: "artwork-error", error: "layout" };
    let bitmap: ImageBitmap | undefined;
    try {
      bitmap = await createImageBitmap(blob);
      if (
        this.closed ||
        bitmap.width !== photo.width ||
        bitmap.height !== photo.height ||
        bitmap.width < 1 ||
        bitmap.height < 1
      )
        throw new Error("invalid_application_photo");
      const artwork = new ApplicationArtwork(
        this,
        snapshot,
        bitmap,
        this.background,
        this.font.family,
      );
      artwork.validate();
      return { kind: "ok", value: artwork };
    } catch {
      bitmap?.close();
      return { kind: "artwork-error", error: "layout" };
    }
  }
  available() {
    return !this.closed;
  }
}

/** A complete preview uses its stored lines and one uniform photo scale. */
export class ApplicationArtwork {
  private closed = false;
  constructor(
    private readonly owner: TemplateArtwork,
    readonly snapshot: ApplicationSnapshot,
    private readonly photo: ImageBitmap,
    private readonly background: ImageBitmap,
    private readonly family: string,
  ) {}
  close() {
    if (!this.closed) {
      this.closed = true;
      this.photo.close();
    }
  }
  private fields() {
    return {
      name: this.snapshot.name,
      number: this.snapshot.printedNumber,
      department: this.snapshot.department.printed,
      title: this.snapshot.printedTitle,
    };
  }
  private lines(context: CanvasRenderingContext2D, draw: boolean) {
    const template = this.snapshot.template;
    const values = this.fields();
    for (const key of FIELDS) {
      const spec = template.layout[key],
        area = spec.area;
      const bytes = new TextEncoder().encode(values[key]);
      let end = 0;
      context.font = `${spec.fontSizeUm}px "${this.family}"`;
      context.textBaseline = "alphabetic";
      context.textAlign = "left";
      for (const line of this.snapshot.text[key]) {
        if (
          !Number.isSafeInteger(line.start) ||
          !Number.isSafeInteger(line.end) ||
          line.start !== end ||
          line.end <= line.start ||
          line.end > bytes.length ||
          ![line.xUm, line.baselineUm, line.widthUm].every(Number.isFinite) ||
          line.widthUm < 0
        )
          throw new Error("invalid_application_lines");
        const text = new TextDecoder("utf-8", { fatal: true }).decode(
          bytes.subarray(line.start, line.end),
        );
        end = line.end;
        context.direction = line.direction;
        const metrics = context.measureText(text);
        const tolerance = 0.000001;
        if (
          line.xUm - metrics.actualBoundingBoxLeft < area.xUm - tolerance ||
          line.xUm + metrics.actualBoundingBoxRight >
            area.xUm + area.widthUm + tolerance ||
          line.baselineUm - metrics.actualBoundingBoxAscent <
            area.yUm - tolerance ||
          line.baselineUm + metrics.actualBoundingBoxDescent >
            area.yUm + area.heightUm + tolerance ||
          metrics.width > area.widthUm + tolerance
        )
          throw new Error("application_text_overflow");
        if (draw) context.fillText(text, line.xUm, line.baselineUm);
      }
      if (end !== bytes.length || !bytes.length)
        throw new Error("incomplete_application_text");
    }
  }
  validate() {
    const canvas = document.createElement("canvas");
    try {
      const context = canvas.getContext("2d");
      if (!context) throw new Error("application_canvas_unavailable");
      this.lines(context, false);
    } finally {
      canvas.width = 0;
      canvas.height = 0;
    }
  }
  paint(canvas: HTMLCanvasElement, width: number) {
    if (this.closed || !this.owner.available())
      throw new Error("application_artwork_released");
    const layout = this.snapshot.template.layout;
    const scale = Math.max(1, width) / layout.widthUm;
    canvas.width = Math.max(1, Math.round(width));
    canvas.height = Math.ceil(layout.heightUm * scale);
    const context = canvas.getContext("2d");
    if (!context) throw new Error("application_canvas_unavailable");
    context.setTransform(scale, 0, 0, scale, 0, 0);
    context.drawImage(this.background, 0, 0, layout.widthUm, layout.heightUm);
    const slot = layout.photo;
    const photoScale = Math.min(
      slot.widthUm / this.photo.width,
      slot.heightUm / this.photo.height,
    );
    const photoWidth = this.photo.width * photoScale,
      photoHeight = this.photo.height * photoScale;
    context.drawImage(
      this.photo,
      slot.xUm + (slot.widthUm - photoWidth) / 2,
      slot.yUm + (slot.heightUm - photoHeight) / 2,
      photoWidth,
      photoHeight,
    );
    context.fillStyle = "#000000";
    this.lines(context, true);
  }
}

export async function loadTemplateArtwork(
  session: string,
  template: Template,
  signal: AbortSignal,
): Promise<ArtworkResult<TemplateArtwork>> {
  if (
    !areaValid(template.layout.photo, template) ||
    FIELDS.some(
      (key) =>
        !areaValid(template.layout[key].area, template) ||
        template.layout[key].fontSizeUm <= 0 ||
        template.layout[key].lineHeightUm <= 0,
    )
  )
    return { kind: "artwork-error", error: "layout" };
  const [background, font] = await Promise.all([
    applicationResource(session, template.backgroundId, signal),
    applicationResource(session, template.fontId, signal),
  ]);
  if (background.kind !== "ok") return background;
  if (font.kind !== "ok") return font;
  if (
    background.value.mediaType !== "image/webp" ||
    !["font/ttf", "font/otf"].includes(font.value.mediaType)
  )
    return { kind: "untrusted" };
  let bitmap: ImageBitmap | undefined;
  let face: FontFace | undefined;
  try {
    bitmap = await createImageBitmap(background.value.blob);
    face = new FontFace(
      `Application_${crypto.randomUUID().replaceAll("-", "")}`,
      await font.value.blob.arrayBuffer(),
    );
    await face.load();
    if (signal.aborted) throw new Error("application_artwork_cancelled");
    document.fonts.add(face);
    return { kind: "ok", value: new TemplateArtwork(template, bitmap, face) };
  } catch {
    bitmap?.close();
    if (face) document.fonts.delete(face);
    return { kind: "artwork-error", error: "font" };
  }
}
