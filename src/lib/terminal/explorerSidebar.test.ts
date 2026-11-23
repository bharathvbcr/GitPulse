import { describe, expect, it } from "vitest";
import { get } from "svelte/store";
import {
  clampExplorerSidebarWidth,
  createTerminalExplorerSidebarStore,
  EXPLORER_SIDEBAR_STORAGE_KEY,
  readExplorerSidebarPrefs,
  TERMINAL_EXPLORER_WIDTH_DEFAULT,
  TERMINAL_EXPLORER_WIDTH_MAX,
  TERMINAL_EXPLORER_WIDTH_MIN,
  writeExplorerSidebarPrefs,
} from "./explorerSidebar";

// Where a clicked file opens is no longer this store's: it is the app-wide
// setting, decided by `resolveFileOpen` (layoutStore.test.ts holds its table,
// ported from the `resolveExplorerFileAction` tests that lived here).
describe("terminal explorer sidebar model", () => {
  it("clamps sidebar width and handles non-finite inputs safely", () => {
    expect(clampExplorerSidebarWidth(TERMINAL_EXPLORER_WIDTH_DEFAULT)).toBe(
      TERMINAL_EXPLORER_WIDTH_DEFAULT,
    );
    expect(clampExplorerSidebarWidth(20)).toBe(TERMINAL_EXPLORER_WIDTH_MIN);
    expect(clampExplorerSidebarWidth(9999)).toBe(TERMINAL_EXPLORER_WIDTH_MAX);
    expect(clampExplorerSidebarWidth(Number.NaN)).toBe(TERMINAL_EXPLORER_WIDTH_DEFAULT);
    expect(clampExplorerSidebarWidth(Number.POSITIVE_INFINITY)).toBe(
      TERMINAL_EXPLORER_WIDTH_DEFAULT,
    );
  });

  it("reads and writes preferences safely with malformed or unavailable storage", () => {
    const bag = new Map<string, string>();
    const storage = {
      getItem: (k: string) => bag.get(k) ?? null,
      setItem: (k: string, v: string) => {
        bag.set(k, v);
      },
    };

    expect(readExplorerSidebarPrefs(null)).toEqual({
      open: false,
      width: TERMINAL_EXPLORER_WIDTH_DEFAULT,
    });

    writeExplorerSidebarPrefs({ open: true, width: 310 }, storage);
    expect(bag.has(EXPLORER_SIDEBAR_STORAGE_KEY)).toBe(true);
    expect(readExplorerSidebarPrefs(storage)).toEqual({ open: true, width: 310 });

    // Corrupt JSON falls back cleanly to safe defaults
    bag.set(EXPLORER_SIDEBAR_STORAGE_KEY, "{not-json");
    expect(readExplorerSidebarPrefs(storage)).toEqual({
      open: false,
      width: TERMINAL_EXPLORER_WIDTH_DEFAULT,
    });

    // Out-of-bounds width is sanitized on read
    bag.set(EXPLORER_SIDEBAR_STORAGE_KEY, JSON.stringify({ open: "yes", width: -50 }));
    expect(readExplorerSidebarPrefs(storage)).toEqual({
      open: false,
      width: TERMINAL_EXPLORER_WIDTH_MIN,
    });

    // Throwing storage never crashes
    const broken = {
      getItem: () => {
        throw new Error("denied");
      },
      setItem: () => {
        throw new Error("quota");
      },
    };
    expect(() => writeExplorerSidebarPrefs({ open: true, width: 240 }, broken)).not.toThrow();
    expect(readExplorerSidebarPrefs(broken)).toEqual({
      open: false,
      width: TERMINAL_EXPLORER_WIDTH_DEFAULT,
    });

    const store = createTerminalExplorerSidebarStore(storage);
    store.setOpen(true);
    store.setWidth(999);
    expect(get(store)).toEqual({ open: true, width: TERMINAL_EXPLORER_WIDTH_MAX });
    expect(readExplorerSidebarPrefs(storage)).toEqual({ open: true, width: TERMINAL_EXPLORER_WIDTH_MAX });
  });

  it("drops the open target an older version stored here, and never writes one back", () => {
    const bag = new Map<string, string>([
      [EXPLORER_SIDEBAR_STORAGE_KEY, JSON.stringify({ open: true, target: "split", width: 300 })],
    ]);
    const storage = {
      getItem: (k: string) => bag.get(k) ?? null,
      setItem: (k: string, v: string) => {
        bag.set(k, v);
      },
    };
    expect(readExplorerSidebarPrefs(storage)).toEqual({ open: true, width: 300 });
    createTerminalExplorerSidebarStore(storage).setWidth(320);
    expect(JSON.parse(bag.get(EXPLORER_SIDEBAR_STORAGE_KEY) ?? "{}")).toEqual({ open: true, width: 320 });
  });
});
