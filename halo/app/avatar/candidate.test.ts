import { describe, expect, it, vi } from "vitest";
import { AvatarSelection } from "./candidate";

const MAX_BYTES = 2 * 1024 * 1024;

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

function uint32BE(value: number): Uint8Array<ArrayBuffer> {
  return Uint8Array.of(
    (value >>> 24) & 0xff,
    (value >>> 16) & 0xff,
    (value >>> 8) & 0xff,
    value & 0xff,
  );
}

function chunk(type: string, data: Uint8Array): Uint8Array<ArrayBuffer> {
  return concat([
    uint32BE(data.length),
    ascii(type),
    data,
    Uint8Array.of(0, 0, 0, 0),
  ]);
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
    Uint8Array.of(0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a),
    chunk("IHDR", header),
    ...extra,
    chunk("IDAT", Uint8Array.of(0)),
    chunk("IEND", new Uint8Array()),
  ]);
}

function file(
  bytes: Uint8Array<ArrayBuffer>,
  type = "image/png",
  name = "avatar.png",
): File {
  return new File([bytes], name, { type });
}

type Dependencies = NonNullable<
  ConstructorParameters<typeof AvatarSelection>[0]
>;
type Decoded = {
  width: number;
  height: number;
  draw(context: CanvasRenderingContext2D): void;
  close(): void;
};

function setup(overrides: Partial<Dependencies> = {}) {
  const resources: { closed: boolean; draws: number }[] = [];
  const selection = new AvatarSelection({
    decode: async () => {
      const resource = { closed: false, draws: 0 };
      resources.push(resource);
      return {
        width: 64,
        height: 64,
        draw: () => {
          resource.draws++;
        },
        close: () => {
          resource.closed = true;
        },
      };
    },
    ...overrides,
  });
  return { selection, resources };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((done, fail) => {
    resolve = done;
    reject = fail;
  });
  return { promise, resolve, reject };
}

const flush = () => new Promise<void>((resolve) => setTimeout(resolve, 0));
const context = {} as CanvasRenderingContext2D;

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

describe("local avatar candidate", () => {
  it("releases an unpaintable candidate without letting an old failure clear a new one", async () => {
    const fixture = setup();
    await fixture.selection.choose(file(png()));
    const first = fixture.selection.snapshot();
    if (first.kind !== "ready") throw new Error("expected first candidate");
    await fixture.selection.choose(file(png()));
    const second = fixture.selection.snapshot();
    if (second.kind !== "ready") throw new Error("expected second candidate");
    fixture.selection.previewFailed(first.candidate);
    expect(fixture.selection.snapshot()).toBe(second);
    expect(fixture.resources[1].closed).toBe(false);
    fixture.selection.previewFailed(second.candidate);
    expect(fixture.resources[1].closed).toBe(true);
    expect(fixture.selection.snapshot()).toEqual({
      kind: "error",
      error: "preview-unavailable",
    });
  });

  it("keeps the original bytes and decodes a metadata-free preview copy", async () => {
    const exif = chunk("eXIf", Uint8Array.of(0x49, 0x49, 0x2a, 6));
    const bytes = png(64, 64, [exif]);
    const original = bytes.slice();
    let decoded: Uint8Array | undefined;
    let closed = false;
    const fixture = setup({
      decode: async (blob) => {
        decoded = new Uint8Array(await blob.arrayBuffer());
        return {
          width: 64,
          height: 64,
          draw: () => {},
          close: () => {
            closed = true;
          },
        };
      },
    });
    const material = file(bytes);

    await fixture.selection.choose(material);

    expect(fixture.selection.snapshot()).toMatchObject({
      kind: "ready",
      candidate: {
        mediaType: "image/png",
        width: 64,
        height: 64,
        name: "avatar.png",
      },
    });
    expect(closed).toBe(false);
    expect(bytes).toEqual(original);
    expect(new Uint8Array(await material.arrayBuffer())).toEqual(original);
    expect(decoded).toBeDefined();
    expect(contains(decoded!, ascii("eXIf"))).toBe(false);
    expect(contains(decoded!, ascii("IDAT"))).toBe(true);

    fixture.selection.clear();
    expect(closed).toBe(true);
  });

  it("resolves an empty supplied type from the inspected bytes", async () => {
    const fixture = setup({
      decode: async () => ({
        width: 32,
        height: 32,
        draw: () => {},
        close: () => {},
      }),
    });
    await fixture.selection.choose(file(png(32, 32), ""));
    expect(fixture.selection.snapshot()).toMatchObject({
      kind: "ready",
      candidate: { mediaType: "image/png", width: 32, height: 32 },
    });
  });

  it("rejects a conflicting supported type before decoding", async () => {
    let decoded = 0;
    const fixture = setup({
      decode: async () => {
        decoded++;
        return { width: 64, height: 64, draw: () => {}, close: () => {} };
      },
    });
    await fixture.selection.choose(file(png(), "image/jpeg", "avatar.jpg"));
    expect(fixture.selection.snapshot()).toEqual({
      kind: "error",
      error: "format",
    });
    expect(decoded).toBe(0);
  });

  it("rejects unsupported bytes and unsupported supplied types", async () => {
    let decoded = 0;
    const fixture = setup({
      decode: async () => {
        decoded++;
        return { width: 64, height: 64, draw: () => {}, close: () => {} };
      },
    });
    await fixture.selection.choose(file(ascii("GIF89a"), ""));
    expect(fixture.selection.snapshot()).toEqual({
      kind: "error",
      error: "format",
    });
    await fixture.selection.choose(file(ascii("GIF89a"), "image/gif"));
    expect(fixture.selection.snapshot()).toEqual({
      kind: "error",
      error: "format",
    });
    expect(decoded).toBe(0);
  });

  it("rejects oversized material without reading or decoding it", async () => {
    let decoded = 0;
    const fixture = setup({
      decode: async () => {
        decoded++;
        return { width: 64, height: 64, draw: () => {}, close: () => {} };
      },
    });
    await fixture.selection.choose(
      file(new Uint8Array(MAX_BYTES + 1)),
    );
    expect(fixture.selection.snapshot()).toEqual({
      kind: "error",
      error: "size",
    });
    expect(decoded).toBe(0);
  });

  it("rejects animation, non-square and out-of-range encoded dimensions", async () => {
    let decoded = 0;
    const fixture = setup({
      decode: async () => {
        decoded++;
        return { width: 64, height: 64, draw: () => {}, close: () => {} };
      },
    });
    await fixture.selection.choose(
      file(
        png(64, 64, [
          chunk("acTL", Uint8Array.of(0, 0, 0, 2, 0, 0, 0, 0)),
        ]),
      ),
    );
    expect(fixture.selection.snapshot()).toEqual({
      kind: "error",
      error: "animation",
    });
    await fixture.selection.choose(file(png(64, 32)));
    expect(fixture.selection.snapshot()).toEqual({
      kind: "error",
      error: "dimensions",
    });
    await fixture.selection.choose(file(png(2048, 2048)));
    expect(fixture.selection.snapshot()).toEqual({
      kind: "error",
      error: "dimensions",
    });
    expect(decoded).toBe(0);
  });

  it("rejects a supported signature with broken framing as damaged", async () => {
    const fixture = setup();
    const bytes = png();
    await fixture.selection.choose(
      file(bytes.subarray(0, bytes.length - 4)),
    );
    expect(fixture.selection.snapshot()).toEqual({
      kind: "error",
      error: "damaged",
    });
    expect(fixture.resources).toEqual([]);
  });

  it("does not expose a candidate until decoding completes", async () => {
    const gate = deferred<Decoded>();
    const fixture = setup({ decode: () => gate.promise });
    const run = fixture.selection.choose(file(png()));
    await flush();
    expect(fixture.selection.snapshot()).toEqual({ kind: "reading" });
    gate.resolve({ width: 64, height: 64, draw: () => {}, close: () => {} });
    await run;
    expect(fixture.selection.snapshot().kind).toBe("ready");
  });

  it("closes a decoded preview whose size differs from the encoded size", async () => {
    let closed = false;
    const fixture = setup({
      decode: async () => ({
        width: 32,
        height: 32,
        draw: () => {},
        close: () => {
          closed = true;
        },
      }),
    });
    await fixture.selection.choose(file(png()));
    expect(fixture.selection.snapshot()).toEqual({
      kind: "error",
      error: "damaged",
    });
    expect(closed).toBe(true);
  });

  it("reports a decoder failure without exposing a candidate", async () => {
    let decoded = 0;
    const fixture = setup({
      decode: async () => {
        decoded++;
        throw new Error("corrupt pixels");
      },
    });
    await fixture.selection.choose(file(png()));
    expect(fixture.selection.snapshot()).toEqual({
      kind: "error",
      error: "damaged",
    });
    expect(decoded).toBe(1);
  });

  it("reports an unavailable local decoder separately from corrupt pixels", async () => {
    vi.stubGlobal("createImageBitmap", undefined);
    try {
      const selection = new AvatarSelection();
      await selection.choose(file(png()));
      expect(selection.snapshot()).toEqual({
        kind: "error",
        error: "preview-unavailable",
      });
      selection.close();
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("closes a late decode after a newer selection and keeps the later preview", async () => {
    const first = deferred<Decoded>();
    const second = deferred<Decoded>();
    const started = deferred<void>();
    let calls = 0;
    let firstClosed = false;
    let secondClosed = false;
    const fixture = setup({
      decode: () => {
        calls++;
        if (calls === 1) {
          started.resolve();
          return first.promise;
        }
        return second.promise;
      },
    });

    const firstRun = fixture.selection.choose(
      file(png(), "image/png", "first.png"),
    );
    await started.promise;
    const secondRun = fixture.selection.choose(
      file(png(), "image/png", "second.png"),
    );
    await flush();
    second.resolve({
      width: 64,
      height: 64,
      draw: () => {},
      close: () => {
        secondClosed = true;
      },
    });
    await secondRun;
    const selected = fixture.selection.snapshot();
    if (selected.kind !== "ready") throw new Error("expected later candidate");
    expect(selected.candidate.name).toBe("second.png");

    first.resolve({
      width: 64,
      height: 64,
      draw: () => {},
      close: () => {
        firstClosed = true;
      },
    });
    await firstRun;

    expect(firstClosed).toBe(true);
    expect(secondClosed).toBe(false);
    expect(fixture.selection.snapshot()).toBe(selected);
    fixture.selection.clear();
    expect(secondClosed).toBe(true);
  });

  it("stops a superseded candidate from painting over a newer one", async () => {
    let calls = 0;
    let firstDraws = 0;
    let secondDraws = 0;
    let firstClosed = false;
    let secondClosed = false;
    const fixture = setup({
      decode: async () => {
        const current = ++calls;
        return {
          width: 64,
          height: 64,
          draw: () => {
            if (current === 1) firstDraws++;
            else secondDraws++;
          },
          close: () => {
            if (current === 1) firstClosed = true;
            else secondClosed = true;
          },
        };
      },
    });

    await fixture.selection.choose(
      file(png(), "image/png", "first.png"),
    );
    const first = fixture.selection.snapshot();
    if (first.kind !== "ready") throw new Error("expected first candidate");
    first.candidate.draw(context);
    expect(firstDraws).toBe(1);

    await fixture.selection.choose(
      file(png(), "image/png", "second.png"),
    );
    const second = fixture.selection.snapshot();
    if (second.kind !== "ready") throw new Error("expected second candidate");
    expect(firstClosed).toBe(true);

    first.candidate.draw(context);
    expect(firstDraws).toBe(1);
    second.candidate.draw(context);
    expect(secondDraws).toBe(1);

    fixture.selection.clear();
    expect(secondClosed).toBe(true);
    second.candidate.draw(context);
    expect(secondDraws).toBe(1);
  });

  it("clears while decoding and closes the late result", async () => {
    const gate = deferred<Decoded>();
    let closed = false;
    const fixture = setup({ decode: () => gate.promise });
    const run = fixture.selection.choose(file(png()));
    await flush();
    fixture.selection.clear();
    expect(fixture.selection.snapshot()).toEqual({ kind: "empty" });

    gate.resolve({
      width: 64,
      height: 64,
      draw: () => {},
      close: () => {
        closed = true;
      },
    });
    await run;

    expect(closed).toBe(true);
    expect(fixture.selection.snapshot()).toEqual({ kind: "empty" });
  });

  it("closes while decoding and ignores work that completes later", async () => {
    const gate = deferred<Decoded>();
    let closed = false;
    const fixture = setup({ decode: () => gate.promise });
    const run = fixture.selection.choose(file(png()));
    await flush();
    fixture.selection.close();
    expect(fixture.selection.snapshot()).toEqual({ kind: "empty" });
    gate.resolve({
      width: 64,
      height: 64,
      draw: () => {},
      close: () => {
        closed = true;
      },
    });
    await run;
    expect(closed).toBe(true);
    expect(fixture.selection.snapshot()).toEqual({ kind: "empty" });
  });
});
