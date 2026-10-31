import { afterEach, expect, it, vi } from "vitest";
import { BadgeImage } from "./badge-image";

// A real encoded 16 x 16 WebP; these unit tests stub the browser decoder.
const WEBP =
  "UklGRkAAAABXRUJQVlA4IDQAAACQAQCdASoQABAAAUAmJaACdLoAA5gA/vij//8QI//mwP/0uD8Ef+lwf/pcDh//S4GaAAAA";

function blob() {
  return new Blob([Buffer.from(WEBP, "base64")], { type: "image/webp" });
}

function setup() {
  const bitmap = { width: 16, height: 16, close: vi.fn() };
  const decode = vi.fn(async () => bitmap as unknown as ImageBitmap);
  vi.stubGlobal("createImageBitmap", decode);
  const create = vi.spyOn(URL, "createObjectURL").mockReturnValue("blob:badge");
  const revoke = vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => {});
  return { bitmap, decode, create, revoke };
}

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

it("owns the original stored bytes and releases the preview URL once", async () => {
  const { bitmap, create, revoke } = setup();
  const bytes = blob();
  const image = await BadgeImage.open(bytes, new AbortController().signal);
  expect(image).toMatchObject({ url: "blob:badge", width: 16, height: 16 });
  if (!image) throw new Error("Expected a stored image");
  expect(create).toHaveBeenCalledExactlyOnceWith(bytes);
  expect(bitmap.close).toHaveBeenCalledOnce();
  expect(revoke).not.toHaveBeenCalled();
  image.close();
  image.close();
  expect(revoke).toHaveBeenCalledExactlyOnceWith("blob:badge");
});

it("closes a late decoded bitmap without exposing a URL after concealment", async () => {
  const { bitmap, decode, create } = setup();
  let started!: () => void;
  const reading = new Promise<void>((resolve) => {
    started = resolve;
  });
  let finish!: (value: ImageBitmap) => void;
  decode.mockImplementation(() => {
    started();
    return new Promise<ImageBitmap>((resolve) => {
      finish = resolve;
    });
  });
  const controller = new AbortController();
  const pending = BadgeImage.open(blob(), controller.signal);
  await reading;
  controller.abort();
  finish(bitmap as unknown as ImageBitmap);
  expect(await pending).toBeUndefined();
  expect(bitmap.close).toHaveBeenCalledOnce();
  expect(create).not.toHaveBeenCalled();
});

it("rejects excessive encoded dimensions before asking the browser to decode", async () => {
  const { decode, create } = setup();
  const bytes = Buffer.from(WEBP, "base64");
  // VP8 frame width follows its frame tag and 0x9d012a start code.
  bytes.writeUInt16LE(4097, 26);
  expect(
    await BadgeImage.open(
      new Blob([bytes], { type: "image/webp" }),
      new AbortController().signal,
    ),
  ).toBeUndefined();
  expect(decode).not.toHaveBeenCalled();
  expect(create).not.toHaveBeenCalled();
});

it("rejects a decoded image whose dimensions disagree with its container", async () => {
  const { bitmap, create } = setup();
  bitmap.width = 32;
  expect(
    await BadgeImage.open(blob(), new AbortController().signal),
  ).toBeUndefined();
  expect(bitmap.close).toHaveBeenCalledOnce();
  expect(create).not.toHaveBeenCalled();
});

it("does not expose broken compressed image data", async () => {
  const { decode, create } = setup();
  decode.mockRejectedValue(new Error("Controlled decode failure"));
  expect(
    await BadgeImage.open(blob(), new AbortController().signal),
  ).toBeUndefined();
  expect(create).not.toHaveBeenCalled();
});
