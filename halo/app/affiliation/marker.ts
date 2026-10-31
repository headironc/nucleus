const key = "halo:affiliation";
type Store = Pick<Storage, "removeItem">;
type Entry = Pick<History, "state" | "replaceState">;

/** Retire only this page's old markers; cleanup never gates a state read or write. */
export function clearLegacyMarker(open: () => Store, entry: () => Entry): void {
  try {
    open().removeItem(key);
  } catch {
    // The retired marker has no authority even when storage is unavailable.
  }
  try {
    const history = entry();
    if (
      history.state &&
      typeof history.state === "object" &&
      Object.hasOwn(history.state, key)
    ) {
      const state = { ...history.state };
      delete state[key];
      history.replaceState(state, "");
    }
  } catch {
    // Other router state stays intact; a failed cleanup does not lock the page.
  }
}
