import { describe, expect, it } from "vitest";
import { get } from "svelte/store";
import {
  clampExplorerSidebarWidth,
  createTerminalExplorerSidebarStore,
  EXPLORER_SIDEBAR_STORAGE_KEY,
  normalizeExplorerOpenTarget,
  readExplorerSidebarPrefs,
  resolveExplorerFileAction,
  TERMINAL_EXPLORER_WIDTH_DEFAULT,
  TERMINAL_EXPLORER_WIDTH_MAX,
  TERMINAL_EXPLORER_WIDTH_MIN,
  writeExplorerSidebarPrefs,
} from "./explorerSidebar";
import { resolveSidebarFileAction } from "../sidebar/layoutStore";

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

  it("normalizes open target to tab by default so file clicks never guess a 3rd split pane", () => {
    expect(normalizeExplorerOpenTarget(undefined)).toBe("tab");
    expect(normalizeExplorerOpenTarget(null)).toBe("tab");
    expect(normalizeExplorerOpenTarget("bogus")).toBe("tab");
    expect(normalizeExplorerOpenTarget("tab")).toBe("tab");
    expect(normalizeExplorerOpenTarget("split")).toBe("split");
    expect(normalizeExplorerOpenTarget("code")).toBe("code");
  });

  it("routes file clicks deterministically from user preference and modifier keys", () => {
    // Default "tab" mode: opens a standalone tab without forcing a 3rd split pane,
    // or updates an already-visible file pane in place when one is on screen.
    expect(
      resolveExplorerFileAction({
        defaultTarget: "tab",
        hasVisibleFilePane: false,
      }),
    ).toBe("open-tab");
    expect(
      resolveExplorerFileAction({
        defaultTarget: "tab",
        hasVisibleFilePane: true,
      }),
    ).toBe("update-visible-pane");

    // Explicit "split" mode: opens side-by-side in split.
    expect(
      resolveExplorerFileAction({
        defaultTarget: "split",
        hasVisibleFilePane: false,
      }),
    ).toBe("open-split");

    // Explicit "code" mode: opens in full Code view.
    expect(
      resolveExplorerFileAction({
        defaultTarget: "code",
        hasVisibleFilePane: true,
      }),
    ).toBe("open-code");

    // Modifier overrides: Alt+Click always splits; Cmd/Ctrl+Click opens in Code view.
    expect(
      resolveExplorerFileAction({
        defaultTarget: "tab",
        altKey: true,
        hasVisibleFilePane: false,
      }),
    ).toBe("open-split");
    expect(
      resolveExplorerFileAction({
        defaultTarget: "split",
        metaOrCtrlKey: true,
        hasVisibleFilePane: true,
      }),
    ).toBe("open-code");
  });

  it("gives modifier clicks the same meaning as the app sidebar's explorer", () => {
    // One rule for both explorers (`resolveSidebarFileAction`): Alt wins over
    // ⌘/Ctrl, and ⌘/Ctrl+Click opens the surface the default does not — so
    // with Code as the default it reaches the terminal instead of repeating Code.
    expect(
      resolveExplorerFileAction({
        defaultTarget: "split",
        metaOrCtrlKey: true,
        altKey: true,
        hasVisibleFilePane: true,
      }),
    ).toBe("open-split");
    expect(
      resolveExplorerFileAction({
        defaultTarget: "code",
        metaOrCtrlKey: true,
        hasVisibleFilePane: false,
      }),
    ).toBe("open-tab");
    expect(
      resolveExplorerFileAction({
        defaultTarget: "code",
        metaOrCtrlKey: true,
        hasVisibleFilePane: true,
      }),
    ).toBe("update-visible-pane");
    for (const defaultTarget of ["tab", "split", "code"] as const) {
      for (const altKey of [false, true]) {
        for (const metaOrCtrlKey of [false, true]) {
          const sidebar = resolveSidebarFileAction({ defaultTarget, altKey, metaOrCtrlKey });
          const terminal = resolveExplorerFileAction({
            defaultTarget,
            altKey,
            metaOrCtrlKey,
            hasVisibleFilePane: false,
          });
          expect(terminal).toBe(sidebar === "code" ? "open-code" : sidebar === "split" ? "open-split" : "open-tab");
        }
      }
    }
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
      target: "tab",
      width: TERMINAL_EXPLORER_WIDTH_DEFAULT,
    });

    writeExplorerSidebarPrefs(
      { open: true, target: "split", width: 310 },
      storage,
    );
    expect(bag.has(EXPLORER_SIDEBAR_STORAGE_KEY)).toBe(true);
    expect(readExplorerSidebarPrefs(storage)).toEqual({
      open: true,
      target: "split",
      width: 310,
    });

    // Corrupt JSON falls back cleanly to safe defaults
    bag.set(EXPLORER_SIDEBAR_STORAGE_KEY, "{not-json");
    expect(readExplorerSidebarPrefs(storage)).toEqual({
      open: false,
      target: "tab",
      width: TERMINAL_EXPLORER_WIDTH_DEFAULT,
    });

    // Out-of-bounds width and unknown target are sanitized on read
    bag.set(
      EXPLORER_SIDEBAR_STORAGE_KEY,
      JSON.stringify({ open: "yes", target: "magic", width: -50 }),
    );
    expect(readExplorerSidebarPrefs(storage)).toEqual({
      open: false,
      target: "tab",
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
    expect(() => writeExplorerSidebarPrefs({ open: true, target: "tab", width: 240 }, broken)).not.toThrow();
    expect(readExplorerSidebarPrefs(broken)).toEqual({
      open: false,
      target: "tab",
      width: TERMINAL_EXPLORER_WIDTH_DEFAULT,
    });

    const store = createTerminalExplorerSidebarStore(storage);
    store.setOpen(true);
    store.setTarget("code");
    store.setWidth(999);
    expect(get(store)).toEqual({
      open: true,
      target: "code",
      width: TERMINAL_EXPLORER_WIDTH_MAX,
    });
    expect(readExplorerSidebarPrefs(storage)).toEqual({
      open: true,
      target: "code",
      width: TERMINAL_EXPLORER_WIDTH_MAX,
    });
  });
});
