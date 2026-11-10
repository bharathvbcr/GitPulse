import { describe, expect, it } from "vitest";
import { get } from "svelte/store";
import {
  DEFAULT_PANEL_MODES,
  PANEL_MODE_STORAGE_KEY,
  createPanelModeStore,
  parsePanelModes,
} from "./panelMode";

function memoryStorage(initial: Record<string, string> = {}) {
  const data = { ...initial };
  return {
    data,
    getItem: (key: string) => data[key] ?? null,
    setItem: (key: string, value: string) => {
      data[key] = value;
    },
  };
}

describe("panel modes", () => {
  it("defaults both panels to docked until the owner decides otherwise", () => {
    expect(DEFAULT_PANEL_MODES).toEqual({ sidebar: "docked", dock: "docked" });
    expect(parsePanelModes(null)).toEqual(DEFAULT_PANEL_MODES);
  });

  it("falls back per field and never throws on hostile input", () => {
    expect(parsePanelModes("{nope")).toEqual(DEFAULT_PANEL_MODES);
    expect(parsePanelModes("[]")).toEqual(DEFAULT_PANEL_MODES);
    expect(parsePanelModes('{"sidebar":"overlay","dock":"floaty"}')).toEqual({ sidebar: "overlay", dock: "docked" });
    expect(parsePanelModes('{"sidebar":3}')).toEqual(DEFAULT_PANEL_MODES);
  });

  it("persists a toggle and reads it back on the next start", () => {
    const storage = memoryStorage();
    const store = createPanelModeStore(storage);
    store.toggle("sidebar");
    expect(get(store)).toEqual({ sidebar: "overlay", dock: "docked" });
    expect(JSON.parse(storage.data[PANEL_MODE_STORAGE_KEY])).toEqual({ sidebar: "overlay", dock: "docked" });
    expect(get(createPanelModeStore(storage)).sidebar).toBe("overlay");
    store.toggle("sidebar");
    expect(get(store).sidebar).toBe("docked");
  });

  it("does not write when set to the mode it already has", () => {
    const storage = memoryStorage();
    const store = createPanelModeStore(storage);
    store.set("dock", "docked");
    expect(storage.data[PANEL_MODE_STORAGE_KEY]).toBeUndefined();
  });

  it("keeps the choice in memory when storage throws", () => {
    const store = createPanelModeStore({
      getItem: () => null,
      setItem: () => {
        throw new Error("quota");
      },
    });
    store.toggle("dock");
    expect(get(store).dock).toBe("overlay");
  });
});
