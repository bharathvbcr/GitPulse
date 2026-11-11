import { describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import { memoryStorage } from "../../repos/persist";
import { DEFAULT_BRANCH_ROW_LAYOUT } from "../../sidebar/metrics";
import { BRANCH_ROW_LAYOUT_REV, migrateBranchRowLayout } from "../interfaceStore";

const KEY = "gitpulse_interface_prefs";

/**
 * A store module loaded fresh over a storage that holds `stored` (nothing
 * when `undefined`), the way the app meets a preference written by an older
 * build. `run` gets the store and the storage; the window stub is removed
 * afterwards so a failure cannot leak into the next file.
 */
async function withStored<T>(
  stored: Record<string, unknown> | undefined,
  run: (
    store: (typeof import("../interfaceStore"))["interfaceStore"],
    storage: ReturnType<typeof memoryStorage>,
  ) => Promise<T> | T,
): Promise<T> {
  const restore = Object.getOwnPropertyDescriptor(globalThis, "window");
  const storage = memoryStorage(stored === undefined ? {} : { [KEY]: JSON.stringify(stored) });
  try {
    Object.defineProperty(globalThis, "window", {
      value: { localStorage: storage },
      configurable: true,
      writable: true,
    });
    vi.resetModules();
    const { interfaceStore } = await import("../interfaceStore");
    return await run(interfaceStore, storage);
  } finally {
    if (restore) Object.defineProperty(globalThis, "window", restore);
    else Reflect.deleteProperty(globalThis, "window");
    vi.resetModules();
  }
}

const layoutOf = (stored: Record<string, unknown> | undefined) =>
  withStored(stored, (store) => get(store).branchRowLayout);

describe("branch row layout default", () => {
  it("is three lines on a fresh install, with the revision already current", async () => {
    expect(DEFAULT_BRANCH_ROW_LAYOUT).toBe("three-line");
    expect(await layoutOf(undefined)).toBe("three-line");
    const rev = await withStored(undefined, (store) => get(store).branchRowLayoutRev);
    expect(rev).toBe(BRANCH_ROW_LAYOUT_REV);
  });

  it("is three lines when a stored blob has no layout at all", async () => {
    expect(await layoutOf({ accent: "teal" })).toBe("three-line");
  });

  it("is three lines again after a reset", async () => {
    const after = await withStored({ branchRowLayout: "one-line", branchRowLayoutRev: 1 }, (store) => {
      store.reset();
      return get(store).branchRowLayout;
    });
    expect(after).toBe("three-line");
  });
});

describe("migrateBranchRowLayout", () => {
  it("moves a two-line saved before the revision to three lines", () => {
    expect(migrateBranchRowLayout("two-line", undefined)).toBe("three-line");
    expect(migrateBranchRowLayout("two-line", 0)).toBe("three-line");
  });

  it("treats a revision it cannot read as not yet migrated", () => {
    for (const rev of ["1", Number.NaN, -1, true, null, {}, [1]]) {
      expect(migrateBranchRowLayout("two-line", rev)).toBe("three-line");
    }
  });

  it("keeps a two-line chosen at or after the revision", () => {
    expect(migrateBranchRowLayout("two-line", BRANCH_ROW_LAYOUT_REV)).toBe("two-line");
    // A newer build's revision is also "already reconciled".
    expect(migrateBranchRowLayout("two-line", BRANCH_ROW_LAYOUT_REV + 1)).toBe("two-line");
  });

  it("never touches one-line or three-line", () => {
    for (const rev of [undefined, 0, BRANCH_ROW_LAYOUT_REV]) {
      expect(migrateBranchRowLayout("one-line", rev)).toBe("one-line");
      expect(migrateBranchRowLayout("three-line", rev)).toBe("three-line");
    }
  });

  it("falls back to the default for anything that is not a layout", () => {
    for (const bad of ["", "four-line", "THREE-LINE", 3, null, undefined, {}, ["two-line"]]) {
      expect(migrateBranchRowLayout(bad, BRANCH_ROW_LAYOUT_REV)).toBe("three-line");
      expect(migrateBranchRowLayout(bad, undefined)).toBe("three-line");
    }
  });
});

describe("branch row layout read from storage", () => {
  it("migrates a stored two-line, which was the old default, to three lines", async () => {
    expect(await layoutOf({ branchRowLayout: "two-line" })).toBe("three-line");
  });

  it("leaves a stored one-line alone", async () => {
    expect(await layoutOf({ branchRowLayout: "one-line" })).toBe("one-line");
  });

  it("keeps a stored three-line", async () => {
    expect(await layoutOf({ branchRowLayout: "three-line" })).toBe("three-line");
  });

  it("falls back to three lines for a garbage layout", async () => {
    for (const bad of ["four-line", "", 2, null, {}, true]) {
      expect(await layoutOf({ branchRowLayout: bad })).toBe("three-line");
    }
  });

  it("keeps a two-line that was saved with the current revision", async () => {
    expect(
      await layoutOf({ branchRowLayout: "two-line", branchRowLayoutRev: BRANCH_ROW_LAYOUT_REV }),
    ).toBe("two-line");
  });

  it("migrates once: a later deliberate two-line survives a restart", async () => {
    // The old build's blob: two-line, no revision.
    const restore = Object.getOwnPropertyDescriptor(globalThis, "window");
    const storage = memoryStorage({ [KEY]: JSON.stringify({ branchRowLayout: "two-line" }) });
    try {
      Object.defineProperty(globalThis, "window", {
        value: { localStorage: storage },
        configurable: true,
        writable: true,
      });
      vi.resetModules();
      const first = (await import("../interfaceStore")).interfaceStore;
      expect(get(first).branchRowLayout).toBe("three-line");

      // Reading wrote nothing, so the migration repeats until a save — and
      // is harmless to repeat.
      expect(JSON.parse(storage.getItem(KEY) ?? "{}").branchRowLayoutRev).toBeUndefined();

      // The reader picks two lines on purpose. The save carries the revision.
      first.setBranchRowLayout("two-line");
      first.flush();
      expect(JSON.parse(storage.getItem(KEY) ?? "{}")).toMatchObject({
        branchRowLayout: "two-line",
        branchRowLayoutRev: BRANCH_ROW_LAYOUT_REV,
      });

      vi.resetModules();
      const second = (await import("../interfaceStore")).interfaceStore;
      expect(get(second).branchRowLayout).toBe("two-line");
    } finally {
      if (restore) Object.defineProperty(globalThis, "window", restore);
      else Reflect.deleteProperty(globalThis, "window");
      vi.resetModules();
    }
  });

  it("writes the revision with any later save, so an unrelated change settles the migration", async () => {
    const saved = await withStored({ branchRowLayout: "two-line" }, (store, storage) => {
      store.setTimestampStyle("absolute");
      store.flush();
      return JSON.parse(storage.getItem(KEY) ?? "{}") as Record<string, unknown>;
    });
    expect(saved).toMatchObject({
      branchRowLayout: "three-line",
      branchRowLayoutRev: BRANCH_ROW_LAYOUT_REV,
    });
  });
});
