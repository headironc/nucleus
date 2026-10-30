import { describe, expect, it } from "vitest";
import { inspectImage, withoutOrientation } from "./format";

function ascii(value: string): Uint8Array<ArrayBuffer> {
  const bytes = new Uint8Array(value.length);
  for (let index = 0; index < value.length; index++)
    bytes[index] = value.charCodeAt(index);
  return bytes;
}

function concat(parts: Uint8Array[]): Uint8Array<ArrayBuffer> {
  let length = 0;
  for (const part of parts) length += part.length;
  const result = new Uint8Array(length);
  let offset = 0;
  for (const part of parts) {
    result.set(part, offset);
    offset += part.length;
  }
  return result;
}

function uint16BE(value: number): Uint8Array<ArrayBuffer> {
  return Uint8Array.of(value >>> 8, value & 0xff);
}

function uint32BE(value: number): Uint8Array<ArrayBuffer> {
  return Uint8Array.of(
    (value >>> 24) & 0xff,
    (value >>> 16) & 0xff,
    (value >>> 8) & 0xff,
    value & 0xff,
  );
}

function uint32LE(value: number): Uint8Array<ArrayBuffer> {
  return Uint8Array.of(
    value & 0xff,
    (value >>> 8) & 0xff,
    (value >>> 16) & 0xff,
    (value >>> 24) & 0xff,
  );
}

function uint24LE(value: number): Uint8Array<ArrayBuffer> {
  return Uint8Array.of(
    value & 0xff,
    (value >>> 8) & 0xff,
    (value >>> 16) & 0xff,
  );
}

function contains(bytes: Uint8Array, marker: Uint8Array): boolean {
  for (let offset = 0; offset + marker.length <= bytes.length; offset++) {
    let found = true;
    for (let index = 0; index < marker.length; index++) {
      if (bytes[offset + index] !== marker[index]) {
        found = false;
        break;
      }
    }
    if (found) return true;
  }
  return false;
}

const PNG_SIGNATURE = Uint8Array.of(
  0x89,
  0x50,
  0x4e,
  0x47,
  0x0d,
  0x0a,
  0x1a,
  0x0a,
);

function pngChunk(type: string, data: Uint8Array): Uint8Array<ArrayBuffer> {
  return concat([uint32BE(data.length), ascii(type), data, Uint8Array.of(0, 0, 0, 0)]);
}

function png(
  width = 64,
  height = 64,
  extra: Uint8Array[] = [],
): Uint8Array<ArrayBuffer> {
  const header = concat([
    uint32BE(width),
    uint32BE(height),
    Uint8Array.of(8, 6, 0, 0, 0),
  ]);
  return concat([
    PNG_SIGNATURE,
    pngChunk("IHDR", header),
    ...extra,
    pngChunk("IDAT", Uint8Array.of(0)),
    pngChunk("IEND", new Uint8Array()),
  ]);
}

function jpegSegment(
  marker: number,
  payload: Uint8Array,
): Uint8Array<ArrayBuffer> {
  return concat([
    Uint8Array.of(0xff, marker),
    uint16BE(payload.length + 2),
    payload,
  ]);
}

function jpeg({
  width = 64,
  height = 64,
  segments = [] as Uint8Array[],
  end = true,
} = {}): Uint8Array<ArrayBuffer> {
  const frame = concat([
    Uint8Array.of(0xff, 0xc0),
    uint16BE(17),
    Uint8Array.of(8),
    uint16BE(height),
    uint16BE(width),
    Uint8Array.of(3, 1, 0x11, 0, 2, 0x11, 1, 3, 0x11, 1),
  ]);
  const scan = Uint8Array.of(
    0xff,
    0xda,
    0x00,
    0x0c,
    3,
    1,
    0,
    2,
    0x11,
    3,
    0x11,
    0,
    0x3f,
    0,
  );
  return concat([
    Uint8Array.of(0xff, 0xd8),
    ...segments,
    frame,
    scan,
    Uint8Array.of(1, 2, 3),
    end ? Uint8Array.of(0xff, 0xd9) : new Uint8Array(),
  ]);
}

function webpChunk(type: string, data: Uint8Array): Uint8Array<ArrayBuffer> {
  return concat([
    ascii(type),
    uint32LE(data.length),
    data,
    data.length % 2 === 0 ? new Uint8Array() : Uint8Array.of(0),
  ]);
}

function vp8x(
  flags: number,
  width = 64,
  height = 64,
): Uint8Array<ArrayBuffer> {
  return webpChunk(
    "VP8X",
    concat([
      Uint8Array.of(flags, 0, 0, 0),
      uint24LE(width - 1),
      uint24LE(height - 1),
    ]),
  );
}

function vp8l(width = 64, height = 64): Uint8Array<ArrayBuffer> {
  const bits = (width - 1) | ((height - 1) << 14);
  return webpChunk("VP8L", concat([Uint8Array.of(0x2f), uint32LE(bits)]));
}

function webp(chunks: Uint8Array[]): Uint8Array<ArrayBuffer> {
  const body = concat([ascii("WEBP"), ...chunks]);
  return concat([ascii("RIFF"), uint32LE(body.length), body]);
}

function webpChunks(bytes: Uint8Array): { type: string; data: Uint8Array }[] {
  const chunks: { type: string; data: Uint8Array }[] = [];
  let offset = 12;
  while (offset + 8 <= bytes.length) {
    const type = String.fromCharCode(
      bytes[offset],
      bytes[offset + 1],
      bytes[offset + 2],
      bytes[offset + 3],
    );
    const length =
      bytes[offset + 4] +
      bytes[offset + 5] * 0x100 +
      bytes[offset + 6] * 0x10000 +
      bytes[offset + 7] * 0x1000000;
    chunks.push({ type, data: bytes.subarray(offset + 8, offset + 8 + length) });
    offset += 8 + length + (length & 1);
  }
  return chunks;
}

describe("encoded avatar containers", () => {
  it("accepts a real lossy WebP with its frame tag before the start code", () => {
    // Pillow 11.3/libwebp encoded a 16x16 red/blue image at quality 80.
    // RFC 6386 section 9.1 places the start code after the three-byte frame tag.
    const bytes = Buffer.from(
      "UklGRkAAAABXRUJQVlA4IDQAAACQAQCdASoQABAAAUAmJaACdLoAA5gA/vij//8QI//mwP/0uD8Ef+lwf/pcDh//S4GaAAAA",
      "base64",
    );
    expect(inspectImage(bytes)).toEqual({
      kind: "valid",
      format: "image/webp",
      width: 16,
      height: 16,
      animated: false,
      orientation: false,
    });
  });

  it("reads square PNG dimensions and marks eXIf orientation metadata", () => {
    const bytes = png(96, 96, [
      pngChunk("eXIf", Uint8Array.of(0x49, 0x49, 0x2a, 0)),
    ]);
    expect(inspectImage(bytes)).toEqual({
      kind: "valid",
      format: "image/png",
      width: 96,
      height: 96,
      animated: false,
      orientation: true,
    });
  });

  it("strips PNG eXIf without changing the original or other chunks", () => {
    const exif = pngChunk("eXIf", Uint8Array.of(0x49, 0x49, 0x2a, 0));
    const bytes = png(64, 64, [exif]);
    const original = bytes.slice();
    const preview = withoutOrientation(bytes, "image/png");
    expect(preview).toBeDefined();
    expect(bytes).toEqual(original);
    expect(preview).not.toBe(bytes);
    expect(contains(preview!, ascii("eXIf"))).toBe(false);
    expect(contains(preview!, ascii("IHDR"))).toBe(true);
    expect(contains(preview!, ascii("IDAT"))).toBe(true);
    expect(preview!.length).toBe(bytes.length - exif.length);
  });

  it("returns the original PNG bytes when no orientation metadata exists", () => {
    const bytes = png();
    expect(withoutOrientation(bytes, "image/png")).toBe(bytes);
  });

  it("rejects a PNG with a truncated chunk walk", () => {
    const bytes = png();
    expect(inspectImage(bytes.subarray(0, bytes.length - 4))).toEqual({
      kind: "invalid",
      format: "image/png",
    });
    const broken = bytes.slice();
    broken[8] = 0x7f;
    broken[9] = 0xff;
    broken[10] = 0xff;
    broken[11] = 0xff;
    expect(inspectImage(broken)).toEqual({
      kind: "invalid",
      format: "image/png",
    });
  });

  it("detects PNG animation control chunks", () => {
    const bytes = png(64, 64, [
      pngChunk("acTL", Uint8Array.of(0, 0, 0, 2, 0, 0, 0, 0)),
    ]);
    expect(inspectImage(bytes)).toMatchObject({
      kind: "valid",
      animated: true,
      orientation: false,
    });
  });

  it("reads JPEG dimensions and EXIF orientation", () => {
    const exif = jpegSegment(
      0xe1,
      concat([ascii("Exif\0\0"), Uint8Array.of(0x49, 0x49, 0x2a, 0)]),
    );
    const bytes = jpeg({ width: 32, height: 32, segments: [exif] });
    expect(inspectImage(bytes)).toEqual({
      kind: "valid",
      format: "image/jpeg",
      width: 32,
      height: 32,
      animated: false,
      orientation: true,
    });
  });

  it("strips only the JPEG EXIF segment and preserves XMP and pixels", () => {
    const exif = jpegSegment(
      0xe1,
      concat([ascii("Exif\0\0"), Uint8Array.of(0x49, 0x49, 0x2a, 0)]),
    );
    const xmp = jpegSegment(0xe1, concat([ascii("XMP\0"), Uint8Array.of(7)]));
    const bytes = jpeg({ segments: [exif, xmp] });
    const original = bytes.slice();
    const preview = withoutOrientation(bytes, "image/jpeg");
    expect(preview).toBeDefined();
    expect(bytes).toEqual(original);
    expect(contains(preview!, ascii("Exif\0\0"))).toBe(false);
    expect(contains(preview!, ascii("XMP\0"))).toBe(true);
    expect(preview!.length).toBe(bytes.length - exif.length);
  });

  it("rejects truncated JPEG framing and a missing end marker", () => {
    const bytes = jpeg();
    const broken = bytes.slice();
    broken[4] = 0xff;
    broken[5] = 0xff;
    expect(inspectImage(broken)).toEqual({
      kind: "invalid",
      format: "image/jpeg",
    });
    expect(inspectImage(jpeg({ end: false }))).toEqual({
      kind: "invalid",
      format: "image/jpeg",
    });
  });

  it("reads WebP canvas dimensions and EXIF flags", () => {
    const bytes = webp([vp8x(0x08, 128, 128), vp8l(128, 128)]);
    expect(inspectImage(bytes)).toEqual({
      kind: "valid",
      format: "image/webp",
      width: 128,
      height: 128,
      animated: false,
      orientation: true,
    });
    expect(inspectImage(webp([vp8l(40, 40)]))).toMatchObject({
      kind: "valid",
      width: 40,
      height: 40,
      orientation: false,
    });
  });

  it("strips WebP EXIF, clears its flag and keeps the RIFF length coherent", () => {
    const exif = webpChunk("EXIF", Uint8Array.of(0x49, 0x49, 0x2a));
    const xmp = webpChunk("XMP ", Uint8Array.of(7));
    const bytes = webp([vp8x(0x08, 64, 64), vp8l(64, 64), exif, xmp]);
    const original = bytes.slice();
    const preview = withoutOrientation(bytes, "image/webp");
    expect(preview).toBeDefined();
    expect(bytes).toEqual(original);
    expect(preview!.length).toBe(bytes.length - exif.length);
    expect(
      preview![4] +
        preview![5] * 0x100 +
        preview![6] * 0x10000 +
        preview![7] * 0x1000000,
    ).toBe(preview!.length - 8);
    const chunks = webpChunks(preview!);
    expect(chunks.map((chunk) => chunk.type)).toEqual(["VP8X", "VP8L", "XMP "]);
    expect(chunks[0].data[0] & 0x08).toBe(0);
  });

  it("detects WebP animation flags and chunks", () => {
    expect(inspectImage(webp([vp8x(0x02, 64, 64), vp8l()]))).toMatchObject({
      kind: "valid",
      animated: true,
    });
    expect(
      inspectImage(
        webp([
          vp8x(0, 64, 64),
          vp8l(),
          webpChunk("ANIM", new Uint8Array(6)),
        ]),
      ),
    ).toMatchObject({ kind: "valid", animated: true });
  });

  it("rejects an incoherent WebP RIFF length", () => {
    const bytes = webp([vp8l()]);
    const broken = bytes.slice();
    broken[4] = 0xff;
    expect(inspectImage(broken)).toEqual({
      kind: "invalid",
      format: "image/webp",
    });
  });

  it("distinguishes unsupported bytes from a damaged supported image", () => {
    expect(inspectImage(ascii("GIF89a"))).toEqual({ kind: "unsupported" });
    const bytes = png();
    expect(inspectImage(bytes.subarray(0, 20))).toEqual({
      kind: "invalid",
      format: "image/png",
    });
  });
});
