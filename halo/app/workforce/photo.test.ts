import { describe, expect, it, vi } from "vitest";
import { ApplicationPhoto, photoRectangle } from "./photo";

// A container accepted by the shared inspector; decode is independently controlled here.
function png(width: number, height: number) {
  const bytes = new Uint8Array(57);
  bytes.set([137, 80, 78, 71, 13, 10, 26, 10]);
  const view = new DataView(bytes.buffer);
  view.setUint32(8, 13);
  bytes.set([73, 72, 68, 82], 12);
  view.setUint32(16, width);
  view.setUint32(20, height);
  bytes.set([73, 68, 65, 84], 37);
  bytes.set([73, 69, 78, 68], 49);
  return new File([bytes], "photo.png", { type: "image/png" });
}

const frame = { width: 3, height: 4 };
function fixture() {
  const image = { width: 800, height: 600, draw: vi.fn(), close: vi.fn() };
  const decode = vi.fn(async () => image);
  const confirmed = { file: png(450, 600), width: 450, height: 600 };
  const encode = vi.fn(async () => confirmed);
  return {
    photo: new ApplicationPhoto({ decode, encode }),
    image,
    decode,
    encode,
    confirmed,
  };
}

describe("explicit application photo crop", () => {
  it("contains portrait and landscape crops without stretching or exposing pixels outside the image", () => {
    expect(
      photoRectangle({ width: 800, height: 600 }, frame, {
        zoom: 1,
        horizontal: 0.5,
        vertical: 0.5,
      }),
    ).toEqual({ x: 175, y: 0, width: 450, height: 600 });
    expect(
      photoRectangle(
        { width: 600, height: 800 },
        { width: 4, height: 3 },
        { zoom: 2, horizontal: 1, vertical: 1 },
      ),
    ).toEqual({ x: 300, y: 575, width: 300, height: 225 });
    expect(() =>
      photoRectangle({ width: 1, height: 1 }, frame, {
        zoom: 0,
        horizontal: 0.5,
        vertical: 0.5,
      }),
    ).toThrow();
  });

  it("does not confirm a chosen image until the owner confirms; changing its crop clears that confirmation", async () => {
    const { photo, encode, confirmed } = fixture();
    await photo.choose(png(800, 600), frame);
    expect(photo.snapshot()).toMatchObject({
      kind: "editing",
      confirmed: null,
    });
    expect(encode).not.toHaveBeenCalled();
    await photo.confirm();
    expect(photo.snapshot()).toMatchObject({ confirmed });
    photo.adjust({ zoom: 2, horizontal: 0, vertical: 1 });
    expect(photo.snapshot()).toMatchObject({ confirmed: null });
  });

  it("releases the original and confirmed material on clear; a late encode cannot restore it", async () => {
    const { photo, image, encode, confirmed } = fixture();
    let finish!: (value: typeof confirmed) => void;
    encode.mockImplementation(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    await photo.choose(png(800, 600), frame);
    const pending = photo.confirm();
    photo.clear();
    finish(confirmed);
    await pending;
    expect(photo.snapshot()).toEqual({ kind: "empty" });
    expect(image.close).toHaveBeenCalledOnce();
  });

  it("closes a late decode after departure without restoring a private photo", async () => {
    const { photo, image, decode } = fixture();
    let finish!: (value: typeof image) => void;
    decode.mockImplementation(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    const pending = photo.choose(png(800, 600), frame);
    await vi.waitFor(() => expect(decode).toHaveBeenCalledOnce());
    photo.close();
    finish(image);
    await pending;
    expect(photo.snapshot()).toEqual({ kind: "empty" });
    expect(image.close).toHaveBeenCalledOnce();
  });

  it("cannot confirm a late encoding after the displayed preview has failed", async () => {
    const { photo, encode, confirmed } = fixture();
    let finish!: (value: typeof confirmed) => void;
    encode.mockImplementation(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    await photo.choose(png(800, 600), frame);
    const pending = photo.confirm();
    photo.previewFailed();
    finish(confirmed);
    await pending;
    expect(photo.snapshot()).toEqual({ kind: "error", error: "unavailable" });
  });

  it("invalidates crop confirmation when the template photo frame changes", async () => {
    const { photo } = fixture();
    await photo.choose(png(800, 600), frame);
    await photo.confirm();
    photo.reframe({ width: 4, height: 3 });
    expect(photo.snapshot()).toMatchObject({
      kind: "editing",
      frame: { width: 4, height: 3 },
      confirmed: null,
    });
  });

  it.each([
    [4097, 1],
    [4096, 4096],
    [0, 600],
  ])("rejects dimensions %i × %i before decoding", async (width, height) => {
    const { photo, decode } = fixture();
    await photo.choose(png(width, height), frame);
    expect(photo.snapshot()).toEqual({ kind: "error", error: "dimensions" });
    expect(decode).not.toHaveBeenCalled();
  });

  it("rejects an oversized input and mismatched declared media type before decoding", async () => {
    const { photo, decode } = fixture();
    await photo.choose(
      new File([new Uint8Array(8 * 1024 * 1024 + 1)], "large.png", {
        type: "image/png",
      }),
      frame,
    );
    expect(photo.snapshot()).toEqual({ kind: "error", error: "size" });
    await photo.choose(
      new File([await png(800, 600).arrayBuffer()], "fake.jpg", {
        type: "image/jpeg",
      }),
      frame,
    );
    expect(photo.snapshot()).toEqual({ kind: "error", error: "format" });
    expect(decode).not.toHaveBeenCalled();
  });
});
