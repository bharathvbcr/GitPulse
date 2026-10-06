import { readFileSync } from "node:fs";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  isBackgroundDocument,
  isHiddenDocument,
  readHiddenDocument,
  resetForegroundFocus,
  whenDocumentShown,
} from "./foreground";

/** A document stand-in whose visibility a test flips and announces. */
function fakeDocument(hidden: boolean) {
  const listeners = new Set<() => void>();
  const doc = {
    hidden,
    visibilityState: hidden ? "hidden" : "visible",
    hasFocus: () => false,
    addEventListener: (type: string, fn: () => void) => {
      if (type === "visibilitychange") listeners.add(fn);
    },
    removeEventListener: (type: string, fn: () => void) => {
      if (type === "visibilitychange") listeners.delete(fn);
    },
    set(next: boolean) {
      doc.hidden = next;
      doc.visibilityState = next ? "hidden" : "visible";
      for (const fn of [...listeners]) fn();
    },
    listenerCount: () => listeners.size,
  };
  return doc;
}

afterEach(() => {
  vi.unstubAllGlobals();
  resetForegroundFocus();
});

describe("hidden, as opposed to merely unfocused", () => {
  it("is false for an unfocused but visible window, which background still counts", () => {
    const doc = { hidden: false, visibilityState: "visible", hasFocus: () => false };
    expect(isBackgroundDocument(doc)).toBe(true);
    expect(isHiddenDocument(doc)).toBe(false);
  });

  it("is true for either spelling of hidden, and false with no document", () => {
    expect(isHiddenDocument({ hidden: true })).toBe(true);
    expect(isHiddenDocument({ visibilityState: "hidden" })).toBe(true);
    expect(isHiddenDocument(null)).toBe(false);
    expect(isHiddenDocument(undefined)).toBe(false);
    expect(readHiddenDocument(), "node has no DOM").toBe(false);
  });
});

describe("whenDocumentShown", () => {
  it("resolves at once with no DOM, or when already shown", async () => {
    await expect(whenDocumentShown()).resolves.toBeUndefined();
    vi.stubGlobal("document", fakeDocument(false));
    await expect(whenDocumentShown()).resolves.toBeUndefined();
  });

  it("waits through changes that leave it hidden, resolves on show, and unsubscribes", async () => {
    const doc = fakeDocument(true);
    vi.stubGlobal("document", doc);
    let shown = false;
    const waiting = whenDocumentShown().then(() => {
      shown = true;
    });
    await Promise.resolve();
    expect(shown).toBe(false);
    doc.set(true);
    await Promise.resolve();
    expect(shown, "still hidden").toBe(false);
    doc.set(false);
    await waiting;
    expect(shown).toBe(true);
    expect(doc.listenerCount(), "no listener left behind").toBe(0);
  });
});

describe("hidden-window checks have one owner", () => {
  const read = (path: string) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");
  it.each(["repos/watcherRefresh.ts", "ipc/invoke.ts", "stores/repoStore.ts"])(
    "%s does not read visibility itself",
    (path) => {
      const source = read(path);
      expect(source).not.toMatch(/document\.hidden/);
      expect(source).not.toMatch(/visibilityState\s*[!=]==/);
    },
  );
});
