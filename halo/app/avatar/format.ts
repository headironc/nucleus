export type ImageFormat = "image/jpeg" | "image/png" | "image/webp";

export type ImageInspection =
  | {
      readonly kind: "valid";
      readonly format: ImageFormat;
      readonly width: number;
      readonly height: number;
      readonly animated: boolean;
      readonly orientation: boolean;
    }
  | { readonly kind: "invalid"; readonly format: ImageFormat }
  | { readonly kind: "unsupported" };

const INVALID_PNG: ImageInspection = { kind: "invalid", format: "image/png" };
const INVALID_JPEG: ImageInspection = { kind: "invalid", format: "image/jpeg" };
const INVALID_WEBP: ImageInspection = { kind: "invalid", format: "image/webp" };

function ascii(bytes: Uint8Array, offset: number, length: number): string {
  if (offset < 0 || offset + length > bytes.length) return "";
  let value = "";
  for (let index = offset; index < offset + length; index++)
    value += String.fromCharCode(bytes[index]);
  return value;
}

function readUint32BE(bytes: Uint8Array, offset: number): number {
  return (
    bytes[offset] * 0x1000000 +
    ((bytes[offset + 1] << 16) | (bytes[offset + 2] << 8) | bytes[offset + 3])
  );
}

function readUint32LE(bytes: Uint8Array, offset: number): number {
  return (
    bytes[offset] +
    bytes[offset + 1] * 0x100 +
    bytes[offset + 2] * 0x10000 +
    bytes[offset + 3] * 0x1000000
  );
}

function readUint24LE(bytes: Uint8Array, offset: number): number {
  return (
    bytes[offset] + bytes[offset + 1] * 0x100 + bytes[offset + 2] * 0x10000
  );
}

function readUint16LE(bytes: Uint8Array, offset: number): number {
  return bytes[offset] + bytes[offset + 1] * 0x100;
}

function writeUint32LE(bytes: Uint8Array, offset: number, value: number) {
  bytes[offset] = value & 0xff;
  bytes[offset + 1] = (value >>> 8) & 0xff;
  bytes[offset + 2] = (value >>> 16) & 0xff;
  bytes[offset + 3] = (value >>> 24) & 0xff;
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

function signature(bytes: Uint8Array): ImageFormat | undefined {
  if (
    bytes.length >= 8 &&
    bytes[0] === 0x89 &&
    bytes[1] === 0x50 &&
    bytes[2] === 0x4e &&
    bytes[3] === 0x47 &&
    bytes[4] === 0x0d &&
    bytes[5] === 0x0a &&
    bytes[6] === 0x1a &&
    bytes[7] === 0x0a
  )
    return "image/png";
  if (
    bytes.length >= 3 &&
    bytes[0] === 0xff &&
    bytes[1] === 0xd8 &&
    bytes[2] === 0xff
  )
    return "image/jpeg";
  if (
    bytes.length >= 12 &&
    ascii(bytes, 0, 4) === "RIFF" &&
    ascii(bytes, 8, 4) === "WEBP"
  )
    return "image/webp";
  return undefined;
}

function inspectPng(bytes: Uint8Array): ImageInspection {
  let offset = 8;
  let width = 0;
  let height = 0;
  let header = false;
  let data = false;
  let end = false;
  let animated = false;
  let orientation = false;
  while (offset + 12 <= bytes.length) {
    const length = readUint32BE(bytes, offset);
    if (length > bytes.length - offset - 12) return INVALID_PNG;
    const type = ascii(bytes, offset + 4, 4);
    if (!header) {
      if (type !== "IHDR" || length !== 13) return INVALID_PNG;
      width = readUint32BE(bytes, offset + 8);
      height = readUint32BE(bytes, offset + 12);
      header = true;
    }
    if (type === "acTL") animated = true;
    if (type === "eXIf") orientation = true;
    if (type === "IDAT") data = true;
    offset += 12 + length;
    if (type === "IEND") {
      end = true;
      break;
    }
  }
  if (!header || !data || !end) return INVALID_PNG;
  return {
    kind: "valid",
    format: "image/png",
    width,
    height,
    animated,
    orientation,
  };
}

function isJpegFrame(marker: number): boolean {
  return (
    marker >= 0xc0 &&
    marker <= 0xcf &&
    marker !== 0xc4 &&
    marker !== 0xc8 &&
    marker !== 0xcc
  );
}

function inspectJpeg(bytes: Uint8Array): ImageInspection {
  let offset = 2;
  let frame = false;
  let scan = false;
  let orientation = false;
  let width = 0;
  let height = 0;
  while (offset + 2 <= bytes.length) {
    if (bytes[offset] !== 0xff) return INVALID_JPEG;
    let next = offset + 1;
    while (next < bytes.length && bytes[next] === 0xff) next++;
    if (next >= bytes.length) return INVALID_JPEG;
    const marker = bytes[next];
    offset = next + 1;
    if (marker === 0xd9)
      return scan && frame
        ? {
            kind: "valid",
            format: "image/jpeg",
            width,
            height,
            animated: false,
            orientation,
          }
        : INVALID_JPEG;
    if (marker === 0xd8 || marker === 0x00) return INVALID_JPEG;
    if (marker === 0x01 || (marker >= 0xd0 && marker <= 0xd7)) continue;
    if (offset + 2 > bytes.length) return INVALID_JPEG;
    const length = (bytes[offset] << 8) | bytes[offset + 1];
    if (length < 2 || length > bytes.length - offset) return INVALID_JPEG;
    const segmentEnd = offset + length;
    if (isJpegFrame(marker)) {
      if (length < 7) return INVALID_JPEG;
      height = (bytes[offset + 3] << 8) | bytes[offset + 4];
      width = (bytes[offset + 5] << 8) | bytes[offset + 6];
      frame = true;
    }
    if (
      marker === 0xe1 &&
      length >= 8 &&
      ascii(bytes, offset + 2, 6) === "Exif\u0000\u0000"
    )
      orientation = true;
    if (marker === 0xda) {
      scan = true;
      offset = segmentEnd;
      while (offset < bytes.length) {
        if (bytes[offset] !== 0xff) {
          offset++;
          continue;
        }
        let probe = offset + 1;
        while (probe < bytes.length && bytes[probe] === 0xff) probe++;
        if (probe >= bytes.length) return INVALID_JPEG;
        const following = bytes[probe];
        if (
          following === 0x00 ||
          (following >= 0xd0 && following <= 0xd7)
        ) {
          offset = probe + 1;
          continue;
        }
        break;
      }
      if (offset >= bytes.length) return INVALID_JPEG;
      continue;
    }
    offset = segmentEnd;
  }
  return INVALID_JPEG;
}

function inspectWebp(bytes: Uint8Array): ImageInspection {
  if (readUint32LE(bytes, 4) !== bytes.length - 8) return INVALID_WEBP;
  let offset = 12;
  let width = -1;
  let height = -1;
  let animated = false;
  let orientation = false;
  while (offset < bytes.length) {
    if (offset + 8 > bytes.length) return INVALID_WEBP;
    const type = ascii(bytes, offset, 4);
    const length = readUint32LE(bytes, offset + 4);
    const data = offset + 8;
    const end = data + length;
    if (end > bytes.length) return INVALID_WEBP;
    if (type === "VP8X") {
      if (length < 10) return INVALID_WEBP;
      const flags = bytes[data];
      if ((flags & 0x02) !== 0) animated = true;
      if ((flags & 0x08) !== 0) orientation = true;
      width = 1 + readUint24LE(bytes, data + 4);
      height = 1 + readUint24LE(bytes, data + 7);
    } else if (type === "VP8 ") {
      if (length < 10) return INVALID_WEBP;
      if (
        bytes[data + 3] !== 0x9d ||
        bytes[data + 4] !== 0x01 ||
        bytes[data + 5] !== 0x2a
      )
        return INVALID_WEBP;
      if (width < 0) {
        width = readUint16LE(bytes, data + 6) & 0x3fff;
        height = readUint16LE(bytes, data + 8) & 0x3fff;
      }
    } else if (type === "VP8L") {
      if (length < 5 || bytes[data] !== 0x2f) return INVALID_WEBP;
      if (width < 0) {
        const bits = readUint32LE(bytes, data + 1);
        width = 1 + (bits & 0x3fff);
        height = 1 + ((bits >>> 14) & 0x3fff);
      }
    } else if (type === "ANIM" || type === "ANMF") animated = true;
    else if (type === "EXIF") orientation = true;
    offset = end + (length & 1);
  }
  if (offset !== bytes.length || width < 0 || height < 0) return INVALID_WEBP;
  return {
    kind: "valid",
    format: "image/webp",
    width,
    height,
    animated,
    orientation,
  };
}

/**
 * Read only the encoded container facts needed by the local candidate check.
 * A recognised signature with broken framing is invalid; other bytes are an
 * unsupported format rather than a corrupt image of a supported one.
 */
export function inspectImage(bytes: Uint8Array): ImageInspection {
  switch (signature(bytes)) {
    case "image/png":
      return inspectPng(bytes);
    case "image/jpeg":
      return inspectJpeg(bytes);
    case "image/webp":
      return inspectWebp(bytes);
    default:
      return { kind: "unsupported" };
  }
}

/**
 * Copy only the orientation metadata away from the preview material. The
 * original upload bytes are never changed. Without metadata the original
 * array is returned so the caller can decode it directly.
 */
export function withoutOrientation(
  bytes: Uint8Array<ArrayBuffer>,
  format: ImageFormat,
): Uint8Array<ArrayBuffer> | undefined {
  switch (format) {
    case "image/png":
      return stripPng(bytes);
    case "image/jpeg":
      return stripJpeg(bytes);
    case "image/webp":
      return stripWebp(bytes);
  }
}

function stripPng(
  bytes: Uint8Array<ArrayBuffer>,
): Uint8Array<ArrayBuffer> | undefined {
  const parts: Uint8Array[] = [bytes.subarray(0, 8)];
  let offset = 8;
  let removed = false;
  while (offset + 12 <= bytes.length) {
    const length = readUint32BE(bytes, offset);
    if (length > bytes.length - offset - 12) return undefined;
    const type = ascii(bytes, offset + 4, 4);
    const end = offset + 12 + length;
    if (type === "eXIf") removed = true;
    else parts.push(bytes.subarray(offset, end));
    offset = end;
    if (type === "IEND") {
      parts.push(bytes.subarray(offset));
      break;
    }
  }
  return removed ? concat(parts) : bytes;
}

function stripJpeg(
  bytes: Uint8Array<ArrayBuffer>,
): Uint8Array<ArrayBuffer> | undefined {
  const parts: Uint8Array[] = [bytes.subarray(0, 2)];
  let offset = 2;
  let removed = false;
  while (offset + 2 <= bytes.length) {
    const start = offset;
    if (bytes[offset] !== 0xff) return undefined;
    let next = offset + 1;
    while (next < bytes.length && bytes[next] === 0xff) next++;
    if (next >= bytes.length) return undefined;
    const marker = bytes[next];
    const markerEnd = next + 1;
    if (marker === 0xd9) {
      parts.push(bytes.subarray(start));
      return removed ? concat(parts) : bytes;
    }
    if (marker === 0xd8 || marker === 0x00) return undefined;
    if (marker === 0x01 || (marker >= 0xd0 && marker <= 0xd7)) {
      parts.push(bytes.subarray(start, markerEnd));
      offset = markerEnd;
      continue;
    }
    if (markerEnd + 2 > bytes.length) return undefined;
    const length = (bytes[markerEnd] << 8) | bytes[markerEnd + 1];
    if (length < 2 || length > bytes.length - markerEnd) return undefined;
    const segmentEnd = markerEnd + length;
    const exif =
      marker === 0xe1 &&
      length >= 8 &&
      ascii(bytes, markerEnd + 2, 6) === "Exif\u0000\u0000";
    if (exif) removed = true;
    else parts.push(bytes.subarray(start, segmentEnd));
    offset = segmentEnd;
    if (marker !== 0xda) continue;
    const entropy = offset;
    while (offset < bytes.length) {
      if (bytes[offset] !== 0xff) {
        offset++;
        continue;
      }
      let probe = offset + 1;
      while (probe < bytes.length && bytes[probe] === 0xff) probe++;
      if (probe >= bytes.length) return undefined;
      const following = bytes[probe];
      if (following === 0x00 || (following >= 0xd0 && following <= 0xd7)) {
        offset = probe + 1;
        continue;
      }
      break;
    }
    if (offset >= bytes.length) return undefined;
    parts.push(bytes.subarray(entropy, offset));
  }
  return undefined;
}

function stripWebp(
  bytes: Uint8Array<ArrayBuffer>,
): Uint8Array<ArrayBuffer> | undefined {
  const parts: Uint8Array[] = [];
  let offset = 12;
  let removed = false;
  while (offset < bytes.length) {
    if (offset + 8 > bytes.length) return undefined;
    const type = ascii(bytes, offset, 4);
    const length = readUint32LE(bytes, offset + 4);
    const data = offset + 8;
    const end = data + length;
    if (end > bytes.length) return undefined;
    if (type === "EXIF") removed = true;
    else if (type === "VP8X") {
      const chunk = bytes.slice(offset, end);
      if ((chunk[8] & 0x08) !== 0) {
        chunk[8] &= ~0x08;
        removed = true;
      }
      parts.push(chunk);
    } else parts.push(bytes.subarray(offset, end));
    if (type !== "EXIF" && (length & 1) !== 0) {
      if (end + 1 > bytes.length) return undefined;
      parts.push(bytes.subarray(end, end + 1));
    }
    offset = end + (length & 1);
  }
  if (offset !== bytes.length || !removed) return bytes;
  let total = 12;
  for (const part of parts) total += part.length;
  const result = new Uint8Array(total);
  result.set(bytes.subarray(0, 4), 0);
  result.set(bytes.subarray(8, 12), 8);
  writeUint32LE(result, 4, total - 8);
  let at = 12;
  for (const part of parts) {
    result.set(part, at);
    at += part.length;
  }
  return result;
}
