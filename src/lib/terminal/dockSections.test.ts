import { describe, expect, it } from "vitest";
import {
  DOCK_SECTIONS,
  DOCK_SECTION_STORAGE_KEY,
  createDockSectionStore,
  dockSectionChordLabel,
  dockSectionForChord,
  dockStatus,
  normalizeDockSection,
} from "./dockSections";
import { get } from "svelte/store";

const key = { code: "", key: "", altKey: false, ctrlKey: false, metaKey: false, shiftKey: false };

describe("dock sections", () => {
  it("lists the four sections in switcher order", () => {
    expect(DOCK_SECTIONS.map((entry) => entry.id)).toEqual(["shell", "agents", "files", "console"]);
    for (const entry of DOCK_SECTIONS) {
      expect(entry.label.length).toBeGreaterThan(0);
      expect(entry.hint.length).toBeGreaterThan(0);
    }
  });

  it("falls back to Shell for anything that is not a section", () => {
    expect(normalizeDockSection("files")).toBe("files");
    expect(normalizeDockSection("agents")).toBe("agents");
    expect(normalizeDockSection("console")).toBe("console");
    expect(normalizeDockSection("bogus")).toBe("shell");
    expect(normalizeDockSection(null)).toBe("shell");
    expect(normalizeDockSection(undefined)).toBe("shell");
  });
});

describe("dockSectionForChord", () => {
  it("answers Option+Shift+Command+1-4 on macOS, by physical key", () => {
    // Option and Shift rewrite `key` (Option+Shift+1 types "⁄"), so `code` decides.
    const mac = { ...key, altKey: true, metaKey: true, shiftKey: true };
    expect(dockSectionForChord({ ...mac, code: "Digit1", key: "⁄" }, "macos")).toBe("shell");
    expect(dockSectionForChord({ ...mac, code: "Digit2" }, "macos")).toBe("agents");
    expect(dockSectionForChord({ ...mac, code: "Digit3" }, "macos")).toBe("files");
    expect(dockSectionForChord({ ...mac, code: "Digit4" }, "macos")).toBe("console");
  });

  it("answers Ctrl+Alt+Shift+1-4 on other hosts", () => {
    const pc = { ...key, altKey: true, ctrlKey: true, shiftKey: true };
    for (const os of ["windows", "linux"]) {
      expect(dockSectionForChord({ ...pc, code: "Digit1", key: "!" }, os)).toBe("shell");
      expect(dockSectionForChord({ ...pc, code: "Digit4" }, os)).toBe("console");
    }
  });

  it("leaves the split-pane jump chord (no Shift) to the split tree", () => {
    // ⌥⌘1–8 / Ctrl+Alt+1–8 focus split pane N (`terminalSplitChord`).
    expect(dockSectionForChord({ ...key, altKey: true, metaKey: true, code: "Digit1" }, "macos")).toBeNull();
    expect(dockSectionForChord({ ...key, altKey: true, ctrlKey: true, code: "Digit1" }, "linux")).toBeNull();
  });

  it("ignores digits outside 1-4, a missing code and chords missing a modifier", () => {
    const pc = { ...key, altKey: true, ctrlKey: true, shiftKey: true };
    expect(dockSectionForChord({ ...pc, code: "Digit5" }, "linux")).toBeNull();
    expect(dockSectionForChord({ ...pc, code: "Digit0" }, "linux")).toBeNull();
    expect(dockSectionForChord({ ...pc, code: "", key: "1" }, "linux")).toBeNull();
    expect(dockSectionForChord({ ...key, ctrlKey: true, shiftKey: true, code: "Digit1" }, "linux")).toBeNull();
    expect(dockSectionForChord({ ...key, altKey: true, shiftKey: true, code: "Digit1" }, "linux")).toBeNull();
  });

  it("leaves the wrong host's modifier alone", () => {
    const shift = { ...key, altKey: true, shiftKey: true };
    expect(dockSectionForChord({ ...shift, metaKey: true, code: "Digit1" }, "linux")).toBeNull();
    expect(dockSectionForChord({ ...shift, ctrlKey: true, code: "Digit1" }, "macos")).toBeNull();
    expect(dockSectionForChord({ ...shift, ctrlKey: true, metaKey: true, code: "Digit1" }, "macos")).toBeNull();
  });
});

describe("dockSectionChordLabel", () => {
  it("spells the chord for the host", () => {
    expect(dockSectionChordLabel("shell", "macos")).toBe("⌥⇧⌘1");
    expect(dockSectionChordLabel("console", "macos")).toBe("⌥⇧⌘4");
    expect(dockSectionChordLabel("agents", "linux")).toBe("Ctrl+Alt+Shift+2");
    expect(dockSectionChordLabel("files", "windows")).toBe("Ctrl+Alt+Shift+3");
  });
});

describe("dock section store", () => {
  function memory(initial?: string) {
    const data = new Map<string, string>();
    if (initial !== undefined) data.set(DOCK_SECTION_STORAGE_KEY, initial);
    return {
      data,
      getItem: (k: string) => data.get(k) ?? null,
      setItem: (k: string, v: string) => void data.set(k, v),
    };
  }

  it("starts at Shell and remembers the last choice", () => {
    const storage = memory();
    const store = createDockSectionStore(storage);
    expect(get(store)).toBe("shell");
    store.set("files");
    expect(get(store)).toBe("files");
    expect(storage.data.get(DOCK_SECTION_STORAGE_KEY)).toBe("files");
    expect(get(createDockSectionStore(storage))).toBe("files");
  });

  it("ignores a stored value that is not a section", () => {
    expect(get(createDockSectionStore(memory("nonsense")))).toBe("shell");
  });

  it("works without storage", () => {
    const store = createDockSectionStore(null);
    store.set("agents");
    expect(get(store)).toBe("agents");
    const throwing = {
      getItem: () => {
        throw new Error("denied");
      },
      setItem: () => {
        throw new Error("denied");
      },
    };
    const guarded = createDockSectionStore(throwing);
    expect(get(guarded)).toBe("shell");
    expect(() => guarded.set("console")).not.toThrow();
    expect(get(guarded)).toBe("console");
  });
});

describe("dockStatus", () => {
  const clean = { ok: true, overlapping_files: 0, shared_worktree_files: 0 };

  it("is empty when nothing is wrong", () => {
    expect(dockStatus({ conflictedCount: 0, collision: clean })).toEqual({ primary: null, others: [] });
    expect(dockStatus({ conflictedCount: 0, collision: null })).toEqual({ primary: null, others: [] });
  });

  it("puts conflicts before collisions before an unverified check", () => {
    const both = dockStatus({ conflictedCount: 2, collision: { ...clean, overlapping_files: 3 } });
    expect(both.primary).toMatchObject({ kind: "conflict", tone: "danger", label: "2 conflicts" });
    expect(both.others.map((entry) => entry.kind)).toEqual(["collision"]);

    const collisionOnly = dockStatus({ conflictedCount: 0, collision: { ...clean, overlapping_files: 1 } });
    expect(collisionOnly.primary).toMatchObject({ kind: "collision", tone: "warn", label: "1 cross-worktree collision" });

    const unverified = dockStatus({ conflictedCount: 0, collision: { ok: false, overlapping_files: 0, shared_worktree_files: 0 } });
    expect(unverified.primary).toMatchObject({ kind: "unverified", tone: "neutral", label: "Collisions unverified" });
  });

  it("singularises and prefers overlapping over shared dirty files", () => {
    expect(dockStatus({ conflictedCount: 1, collision: null }).primary?.label).toBe("1 conflict");
    const shared = dockStatus({ conflictedCount: 0, collision: { ...clean, shared_worktree_files: 4 } });
    expect(shared.primary?.label).toBe("4 shared dirty files");
    const mixed = dockStatus({ conflictedCount: 0, collision: { ...clean, overlapping_files: 2, shared_worktree_files: 4 } });
    expect(mixed.primary?.label).toBe("2 cross-worktree collisions");
    expect(mixed.others).toEqual([]);
  });

  it("a failed collision check hides the file counts it could not verify", () => {
    const result = dockStatus({ conflictedCount: 1, collision: { ok: false, overlapping_files: 5, shared_worktree_files: 5 } });
    expect(result.primary?.kind).toBe("conflict");
    expect(result.others.map((entry) => entry.kind)).toEqual(["unverified"]);
  });
});
