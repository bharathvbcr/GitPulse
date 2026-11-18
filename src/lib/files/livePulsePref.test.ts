import { describe, expect, it } from "vitest";
import { get } from "svelte/store";
import {
  createLivePulsePrefStore,
  LIVE_PULSE_STORAGE_KEY,
  readLivePulsePrefs,
  writeLivePulsePrefs,
} from "./livePulsePref";

function memoryStorage(initial: Record<string, string> = {}) {
  const data = new Map(Object.entries(initial));
  return {
    data,
    getItem: (key: string) => data.get(key) ?? null,
    setItem: (key: string, value: string) => void data.set(key, value),
  };
}

describe("Live pulse visibility preference", () => {
  it("defaults to shown with no storage, nothing stored, or garbage stored", () => {
    expect(readLivePulsePrefs(null)).toEqual({ visible: true });
    expect(readLivePulsePrefs(memoryStorage())).toEqual({ visible: true });
    for (const raw of ["not json", "[]", "null", "42", '{"visible":"no"}']) {
      expect(readLivePulsePrefs(memoryStorage({ [LIVE_PULSE_STORAGE_KEY]: raw }))).toEqual({ visible: true });
    }
  });

  it("survives a throwing storage on read and on write", () => {
    const throwing = {
      getItem: () => {
        throw new Error("denied");
      },
      setItem: () => {
        throw new Error("quota");
      },
    };
    expect(readLivePulsePrefs(throwing)).toEqual({ visible: true });
    expect(() => writeLivePulsePrefs({ visible: false }, throwing)).not.toThrow();
  });

  it("remembers a hidden Live pulse for the next store, which is the next launch", () => {
    const storage = memoryStorage();
    const first = createLivePulsePrefStore(storage);
    first.setVisible(false);
    expect(get(first).visible).toBe(false);
    expect(storage.data.get(LIVE_PULSE_STORAGE_KEY)).toBe('{"visible":false}');

    const relaunched = createLivePulsePrefStore(storage);
    expect(get(relaunched).visible).toBe(false);
    relaunched.setVisible(true);
    expect(get(createLivePulsePrefStore(storage)).visible).toBe(true);
  });

  it("does not write when the value is unchanged", () => {
    const storage = memoryStorage();
    const store = createLivePulsePrefStore(storage);
    store.setVisible(true);
    expect(storage.data.has(LIVE_PULSE_STORAGE_KEY)).toBe(false);
  });
});
