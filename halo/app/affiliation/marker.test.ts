import { describe, expect, it, vi } from "vitest";
import { browserMarker as createBrowserMarker, type MarkerStore } from "./marker";

const session = "019354fd-933c-7e6a-8912-345678901234";
const other = "019354fd-933c-7e6a-8912-345678901235";
const key = "halo:affiliation";

function historyEntry() {
  return {
    state: { idx: 3, key: "original-entry" } as Record<string, unknown>,
    replaceState(state: Record<string, unknown>) { this.state = state; },
  };
}

function browserMarker(open: Parameters<typeof createBrowserMarker>[0], history = historyEntry()) {
  return createBrowserMarker(open, () => history);
}

function storage(initial?: string) {
  const values = new Map<string, string>();
  if (initial !== undefined) values.set(key, initial);
  return {
    values,
    getItem: vi.fn((name: string) => values.get(name) ?? null),
    setItem: vi.fn((name: string, value: string) => {
      values.set(name, value);
    }),
    removeItem: vi.fn((name: string) => {
      values.delete(name);
    }),
  };
}

function marker(store = storage()): [MarkerStore, ReturnType<typeof storage>] {
  return [browserMarker(() => store), store];
}

describe("affiliation marker", () => {
  it("stores only a version and the owning session", () => {
    const [store, backing] = marker();
    expect(store.read(session)).toBe("absent");
    expect(store.mark(session)).toBe(true);
    expect(backing.setItem).toHaveBeenCalledWith(
      key,
      `{"version":1,"session":"${session}"}`,
    );
    expect(backing.values.get(key)).not.toMatch(/affiliation|党员|群众|value|name/);
    expect(store.read(session)).toBe("present");
  });

  it("ignores another tab's session without erasing its protection", () => {
    const backing = storage(`{"version":1,"session":"${other}"}`);
    const store = browserMarker(() => backing);
    expect(store.read(session)).toBe("absent");
    expect(store.read(other)).toBe("present");
    expect(backing.removeItem).not.toHaveBeenCalled();
  });

  it.each([
    "",
    "not-json",
    "[]",
    "null",
    `{"version":1,"session":"${session}","choice":"01"}`,
    `{"version":2,"session":"${session}"}`,
    `{"version":1,"session":"not-a-uuid"}`,
    `{"version":1}`,
    `{"version":"1","session":"${session}"}`,
  ])("fails closed on corrupt material (%s)", (raw) => {
    const [store] = marker(storage(raw));
    expect(store.read(session)).toBe("corrupt");
  });

  it("reports denied access and refuses to claim a save", () => {
    const denied = browserMarker(() => {
      throw new Error("denied");
    });
    expect(denied.read(session)).toBe("denied");
    expect(denied.mark(session)).toBe(false);
    denied.clear(session);

    const backing = storage();
    const store = browserMarker(() => ({
      getItem: vi.fn(() => {
        throw new Error("denied");
      }),
      setItem: backing.setItem,
      removeItem: backing.removeItem,
    }));
    expect(store.read(session)).toBe("denied");
    expect(store.mark(session)).toBe(false);
  });

  it("requires the exact saved marker to read back before returning true", () => {
    const backing = storage();
    const store = browserMarker(() => ({
      getItem: vi.fn(() => "another value"),
      setItem: backing.setItem,
      removeItem: backing.removeItem,
    }));
    expect(store.mark(session)).toBe(false);
    expect(store.mark("")).toBe(false);
  });

  it("clears the marker and leaves corruption closed when clearing fails", () => {
    const backing = storage(`{"version":1,"session":"${session}"}`);
    const store = browserMarker(() => backing);
    store.clear(session);
    expect(backing.removeItem).toHaveBeenCalledWith(key);
    expect(store.read(session)).toBe("absent");

    const failed = browserMarker(() => ({
      getItem: backing.getItem,
      setItem: backing.setItem,
      removeItem: () => {
        throw new Error("denied");
      },
    }));
    failed.clear(session);
  });

  it("does not reopen a submitted history entry after its tab marker disappears", () => {
    const backing = storage();
    const history = historyEntry();
    const store = browserMarker(() => backing, history);
    expect(store.mark(session)).toBe(true);
    expect(history.state).toEqual({ idx: 3, key: "original-entry", [key]: `{"version":1,"session":"${session}"}` });
    backing.values.clear();
    const restored = browserMarker(() => backing, history);
    expect(restored.read(session)).toBe("corrupt");
    expect(restored.read(other)).toBe("absent");
  });

  it("requires the submitted history marker to save and read back", () => {
    const backing = storage();
    const history = historyEntry();
    history.replaceState = () => {};
    expect(browserMarker(() => backing, history).mark(session)).toBe(false);
    expect(backing.values.get(key)).toBeDefined();
  });

  it("clears only the completed session and preserves router history state", () => {
    const backing = storage();
    const history = historyEntry();
    const store = browserMarker(() => backing, history);
    expect(store.mark(session)).toBe(true);
    store.clear(other);
    expect(store.read(session)).toBe("present");
    store.clear(session);
    expect(store.read(session)).toBe("absent");
    expect(history.state).toEqual({ idx: 3, key: "original-entry" });
  });
});
