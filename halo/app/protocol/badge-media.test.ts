import { expect, it, vi } from "vitest";
import { readBadgeBytes } from "./badge-media";

function response(
  chunks: Uint8Array[],
  headers: Record<string, string> = {},
  declared = String(chunks.reduce((size, part) => size + part.byteLength, 0)),
) {
  const cancel = vi.fn();
  let sent = 0;
  const stream = new ReadableStream<Uint8Array>({
    pull(controller) {
      if (sent < chunks.length) controller.enqueue(chunks[sent++]);
      else controller.close();
    },
    cancel,
  });
  const value = new Response(stream, {
    headers: {
      "Content-Type": "image/webp",
      "Content-Length": declared,
      ...headers,
    },
  });
  return { value, cancel, stream };
}

it("reads the exact declared chunks and preserves their bytes", async () => {
  const { value, stream } = response([
    new Uint8Array([82, 73, 70, 70]),
    new Uint8Array([1, 2, 3, 4]),
  ]);
  const result = await readBadgeBytes(value, new AbortController().signal);
  expect(result.kind).toBe("ok");
  if (result.kind !== "ok") throw new Error("Expected bounded bytes");
  expect(result.value.type).toBe("image/webp");
  expect(new Uint8Array(await result.value.arrayBuffer())).toEqual(
    new Uint8Array([82, 73, 70, 70, 1, 2, 3, 4]),
  );
  expect(stream.locked).toBe(false);
});

it("does not consume a body after its page scope was aborted", async () => {
  const { value, stream, cancel } = response([new Uint8Array([1])]);
  const controller = new AbortController();
  controller.abort();
  const reader = vi.spyOn(stream, "getReader");
  expect(await readBadgeBytes(value, controller.signal)).toEqual({
    kind: "disconnected",
  });
  expect(reader).not.toHaveBeenCalled();
  expect(cancel).toHaveBeenCalledOnce();
  expect(stream.locked).toBe(false);
});

it("rejects a failed HTTP status even with plausible image headers", async () => {
  const cancel = vi.fn();
  const stream = new ReadableStream<Uint8Array>({ cancel });
  const reader = vi.spyOn(stream, "getReader");
  const value = new Response(stream, {
    status: 503,
    headers: { "Content-Type": "image/webp", "Content-Length": "1" },
  });
  expect(await readBadgeBytes(value, new AbortController().signal)).toEqual({
    kind: "untrusted",
  });
  expect(reader).not.toHaveBeenCalled();
  expect(cancel).toHaveBeenCalledOnce();
});

it("rejects a missing body despite a positive declared length", async () => {
  const value = new Response(null, {
    headers: { "Content-Type": "image/webp", "Content-Length": "1" },
  });
  expect(await readBadgeBytes(value, new AbortController().signal)).toEqual({
    kind: "untrusted",
  });
});

it.each([undefined, "", "0", "-1", "1.5", "NaN", "16777217"])(
  "rejects declared length %s before consuming the body",
  async (length) => {
    const { value, stream, cancel } = response([new Uint8Array([1])]);
    if (length === undefined) value.headers.delete("Content-Length");
    else value.headers.set("Content-Length", length);
    const reader = vi.spyOn(stream, "getReader");
    expect(await readBadgeBytes(value, new AbortController().signal)).toEqual({
      kind: "untrusted",
    });
    expect(reader).not.toHaveBeenCalled();
    expect(cancel).toHaveBeenCalledOnce();
    expect(stream.locked).toBe(false);
  },
);

it("accepts the full 16 MiB contract boundary without collecting an extra byte", async () => {
  const { value, stream } = response([
    new Uint8Array(8 * 1024 * 1024),
    new Uint8Array(8 * 1024 * 1024),
  ]);
  const result = await readBadgeBytes(value, new AbortController().signal);
  expect(result.kind === "ok" && result.value.size).toBe(16 * 1024 * 1024);
  expect(stream.locked).toBe(false);
});

it.each(["image/png", "application/json", "application/octet-stream"])(
  "rejects a successful response of type %s",
  async (type) => {
    const { value, stream } = response([new Uint8Array([1])], {
      "Content-Type": type,
    });
    const reader = vi.spyOn(stream, "getReader");
    expect(await readBadgeBytes(value, new AbortController().signal)).toEqual({
      kind: "untrusted",
    });
    expect(reader).not.toHaveBeenCalled();
    expect(stream.locked).toBe(false);
  },
);

it.each(["3", "5"])(
  "rejects a declared length %s that differs from actual bytes",
  async (length) => {
    const { value, stream } = response(
      [new Uint8Array([1, 2, 3, 4])],
      {},
      length,
    );
    expect(await readBadgeBytes(value, new AbortController().signal)).toEqual({
      kind: "untrusted",
    });
    expect(stream.locked).toBe(false);
  },
);

it("cancels a chunked response as soon as it exceeds the agreed limit", async () => {
  const { value, stream, cancel } = response(
    [new Uint8Array(16 * 1024 * 1024), new Uint8Array([1])],
    {},
    "16777216",
  );
  expect(await readBadgeBytes(value, new AbortController().signal)).toEqual({
    kind: "untrusted",
  });
  expect(cancel).toHaveBeenCalledOnce();
  expect(stream.locked).toBe(false);
});

it("aborts a stalled body and releases its reader", async () => {
  let started!: () => void;
  const reading = new Promise<void>((resolve) => {
    started = resolve;
  });
  const cancel = vi.fn();
  const stream = new ReadableStream<Uint8Array>({ pull: started, cancel });
  const controller = new AbortController();
  const pending = readBadgeBytes(
    new Response(stream, {
      headers: { "Content-Type": "image/webp", "Content-Length": "1" },
    }),
    controller.signal,
  );
  await reading;
  controller.abort();
  expect(await pending).toEqual({ kind: "disconnected" });
  expect(cancel).toHaveBeenCalledOnce();
  expect(stream.locked).toBe(false);
});

it("releases a reader after a failed network stream", async () => {
  const stream = new ReadableStream<Uint8Array>({
    pull(controller) {
      controller.error(new Error("Controlled read failure"));
    },
  });
  const value = new Response(stream, {
    headers: { "Content-Type": "image/webp", "Content-Length": "1" },
  });
  expect(await readBadgeBytes(value, new AbortController().signal)).toEqual({
    kind: "disconnected",
  });
  expect(stream.locked).toBe(false);
});
