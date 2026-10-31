import { describe, expect, it, vi } from "vitest";
import { clearLegacyMarker } from "./marker";

const key = "halo:affiliation";
describe("retired affiliation markers", () => {
  it.each(["broken", { version: 1, session: "old" }, null])(
    "removes only its own history key: %j",
    (marker) => {
      const other = { preserved: true };
      const history = {
        state: { idx: 3, key: "route", usr: other, [key]: marker },
        replaceState: vi.fn(),
      };
      const removeItem = vi.fn();
      clearLegacyMarker(
        () => ({ removeItem }),
        () => history,
      );
      expect(removeItem).toHaveBeenCalledExactlyOnceWith(key);
      expect(history.replaceState).toHaveBeenCalledExactlyOnceWith(
        { idx: 3, key: "route", usr: other },
        "",
      );
      expect(history.state[key]).toBe(marker);
    },
  );
  it("cleans history even when storage access fails", () => {
    const replaceState = vi.fn();
    expect(() =>
      clearLegacyMarker(
        () => {
          throw Error("denied");
        },
        () => ({ state: { idx: 2, [key]: "bad" }, replaceState }),
      ),
    ).not.toThrow();
    expect(replaceState).toHaveBeenCalledExactlyOnceWith({ idx: 2 }, "");
  });
  it("cleans storage even when history access fails", () => {
    const removeItem = vi.fn();
    expect(() =>
      clearLegacyMarker(
        () => ({ removeItem }),
        () => {
          throw Error("denied");
        },
      ),
    ).not.toThrow();
    expect(removeItem).toHaveBeenCalledExactlyOnceWith(key);
  });
  it("does not replace unrelated history", () => {
    const replaceState = vi.fn();
    clearLegacyMarker(
      () => ({ removeItem: vi.fn() }),
      () => ({ state: { idx: 2, usr: { value: "kept" } }, replaceState }),
    );
    expect(replaceState).not.toHaveBeenCalled();
  });
});
