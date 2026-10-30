/**
 * One non-sensitive tab/history marker records only that this tab may have
 * sent a first-affiliation PUT for one session. It never stores the value,
 * display name, request body, digest or completion result.
 */
const key = "halo:affiliation";
const version = 1;
const sessionPattern =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

export type MarkerState = "absent" | "present" | "corrupt" | "denied";

type Store = Pick<Storage, "getItem" | "setItem" | "removeItem">;
type Entry = Pick<History, "state" | "replaceState">;

export type MarkerStore = {
  read(session: string): MarkerState;
  mark(session: string): boolean;
  clear(session: string): void;
};

function serialized(session: string): string {
  return JSON.stringify({ version, session });
}

function inspect(raw: string, session: string): MarkerState {
  let value: unknown;
  try {
    value = JSON.parse(raw);
  } catch {
    return "corrupt";
  }
  if (!value || typeof value !== "object" || Array.isArray(value))
    return "corrupt";
  const keys = Object.keys(value).sort();
  if (
    keys.length !== 2 ||
    keys[0] !== "session" ||
    keys[1] !== "version" ||
    !("version" in value) ||
    value.version !== version ||
    !("session" in value) ||
    typeof value.session !== "string" ||
    !sessionPattern.test(value.session)
  )
    return "corrupt";
  // Another session or account is ignored, not cleared, so a late owner tab
  // cannot erase the marker that protects its own ambiguity.
  return value.session === session ? "present" : "absent";
}

/** Bind the marker contract to a storage that may itself be unavailable. */
export function browserMarker(open: () => Store, entry: () => Entry): MarkerStore {
  const store = (): Store | undefined => {
    try {
      return open();
    } catch {
      return undefined;
    }
  };
  return {
    read(session) {
      const storage = store();
      if (!storage) return "denied";
      try {
        const historical = entry().state?.[key];
        const historyState = historical === undefined
          ? "absent"
          : typeof historical === "string"
            ? inspect(historical, session)
            : "corrupt";
        if (historyState === "corrupt") return "corrupt";
        const raw = storage.getItem(key);
        const stored = raw === null ? "absent" : inspect(raw, session);
        // A submitted history entry remains closed if its tab marker was
        // removed or replaced. Absence is not evidence of a new operation.
        return historyState === "present" && stored === "absent"
          ? "corrupt"
          : stored;
      } catch {
        return "denied";
      }
    },
    mark(session) {
      const storage = store();
      if (!storage) return false;
      const exact = serialized(session);
      try {
        storage.setItem(key, exact);
        if (storage.getItem(key) !== exact) return false;
        const history = entry();
        history.replaceState({ ...history.state, [key]: exact }, "");
        // Both independently retained markers precede the first PUT.
        return history.state?.[key] === exact;
      } catch {
        return false;
      }
    },
    clear(session) {
      try {
        const storage = store();
        const raw = storage?.getItem(key);
        if (raw && inspect(raw, session) === "present") storage?.removeItem(key);
        const history = entry();
        if (history.state?.[key] === serialized(session)) {
          const state = { ...history.state };
          delete state[key];
          history.replaceState(state, "");
        }
      } catch {
        // A failed clear leaves a fail-closed marker, never a fresh form.
      }
    },
  };
}
