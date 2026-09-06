import { describe, it, expect } from "vitest";
import { get } from "svelte/store";
import { createThemeStore } from "../themeStore";

/**
 * Installs the minimum globals `applyWithTransition` needs to take the View
 * Transitions branch. `matchMedia` has to answer per query: a stub that
 * returns one `matches` for everything reports reduced motion and the
 * crossfade is skipped, so the branch under test never runs. localStorage is
 * deliberately left absent — the store already degrades to "system".
 */
function installTransitionGlobals(startViewTransition: (update: () => void) => unknown) {
  const classes = new Set<string>();
  const fakeWindow = {
    matchMedia: (query: string) => ({
      matches: query.includes("prefers-color-scheme: dark"),
      addEventListener: () => {},
      removeEventListener: () => {},
    }),
  };
  const fakeDocument = {
    documentElement: {
      classList: {
        toggle: (name: string, force: boolean) => {
          if (force) classes.add(name);
          else classes.delete(name);
        },
      },
    },
    startViewTransition,
  };
  const originals = { window: globalThis.window, document: globalThis.document };
  Object.defineProperty(globalThis, "window", { value: fakeWindow, configurable: true });
  Object.defineProperty(globalThis, "document", { value: fakeDocument, configurable: true });
  return {
    classes,
    restore: () => {
      for (const [key, value] of Object.entries(originals)) {
        if (value === undefined) delete (globalThis as Record<string, unknown>)[key];
        else Object.defineProperty(globalThis, key, { value, configurable: true });
      }
    },
  };
}

/** Records anything Node reports as an unhandled rejection while `run` executes. */
async function collectUnhandledRejections(run: () => void): Promise<unknown[]> {
  const seen: unknown[] = [];
  const onUnhandled = (reason: unknown) => seen.push(reason);
  process.on("unhandledRejection", onUnhandled);
  try {
    run();
    // Node reports unhandled rejections after the microtask queue drains; two
    // macrotask hops leave no doubt that the check has run.
    await new Promise((resolve) => setImmediate(resolve));
    await new Promise((resolve) => setImmediate(resolve));
  } finally {
    process.off("unhandledRejection", onUnhandled);
  }
  return seen;
}

describe("themeStore view transitions", () => {
  it("handles the ready rejection when a flip supersedes an in-flight crossfade", async () => {
    // A superseded transition rejects `ready` — WebKit and Chromium both report
    // InvalidStateError. `finished` resolves, so catching that one would catch
    // nothing.
    const abort = () => ({
      ready: Promise.reject(
        Object.assign(new Error("Transition was aborted because of invalid state"), {
          name: "InvalidStateError",
        }),
      ),
      finished: Promise.resolve(),
      updateCallbackDone: Promise.resolve(),
      skipTransition: () => {},
    });
    const env = installTransitionGlobals((update) => {
      update();
      return abort();
    });
    try {
      const store = createThemeStore();
      const unhandled = await collectUnhandledRejections(() => {
        store.setTheme("light");
        store.setTheme("dark");
      });

      expect(unhandled).toEqual([]);
      // The crossfade was skipped, but the theme itself still landed.
      expect(get(store)).toBe("dark");
      expect([...env.classes]).toEqual(["dark"]);
    } finally {
      env.restore();
    }
  });

  it("still applies the theme when the transition rejects on every flip", async () => {
    // A window that is not being rendered (minimised, occluded) rejects `ready`
    // for every transition, not just superseded ones.
    const env = installTransitionGlobals((update) => {
      update();
      return {
        ready: Promise.reject(new Error("Transition was aborted because of invalid state")),
        finished: Promise.resolve(),
        updateCallbackDone: Promise.resolve(),
        skipTransition: () => {},
      };
    });
    try {
      const store = createThemeStore();
      const unhandled = await collectUnhandledRejections(() => {
        store.setTheme("light");
      });

      expect(unhandled).toEqual([]);
      expect(get(store)).toBe("light");
      expect([...env.classes]).toEqual(["light"]);
    } finally {
      env.restore();
    }
  });
});
