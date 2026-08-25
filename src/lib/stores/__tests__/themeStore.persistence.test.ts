import { describe, it, expect, afterEach, vi } from "vitest";
import { get } from "svelte/store";
import { createThemeStore } from "../themeStore";

interface MediaList {
  matches: boolean;
  listener: (() => void) | null;
}

function installGlobals(storage: Record<string, string>) {
  const media: MediaList = { matches: true, listener: null };
  const fakeWindow = {
    matchMedia: (_query: string) => ({
      get matches() {
        return media.matches;
      },
      addEventListener: (_type: string, cb: () => void) => {
        media.listener = cb;
      },
      removeEventListener: () => {},
      addListener: (cb: () => void) => {
        media.listener = cb;
      },
      removeListener: () => {},
    }),
  };
  const fakeDocument = {
    documentElement: { classList: { toggle: () => {} } },
    startViewTransition: undefined,
  };
  const originals = {
    window: globalThis.window,
    document: globalThis.document,
    localStorage: globalThis.localStorage,
  };
  Object.defineProperty(globalThis, "window", { value: fakeWindow, configurable: true });
  Object.defineProperty(globalThis, "document", { value: fakeDocument, configurable: true });
  Object.defineProperty(globalThis, "localStorage", {
    value: {
      getItem: (key: string) => (key in storage ? storage[key] : null),
      setItem: (key: string, value: string) => {
        storage[key] = value;
      },
      removeItem: (key: string) => {
        delete storage[key];
      },
    },
    configurable: true,
  });
  return {
    flipSystem: () => {
      media.matches = !media.matches;
      media.listener?.();
    },
    restore: () => {
      for (const [key, value] of Object.entries(originals)) {
        if (value === undefined) {
          delete (globalThis as Record<string, unknown>)[key];
        } else {
          Object.defineProperty(globalThis, key, { value, configurable: true });
        }
      }
    },
  };
}

afterEach(() => {
  vi.restoreAllMocks();
});

describe("themeStore persistence", () => {
  it("does not rewrite stored 'system' when the OS theme flips", () => {
    const backing: Record<string, string> = {};
    const env = installGlobals(backing);
    try {
      const store = createThemeStore();
      expect(store.preference()).toBe("system");
      expect(get(store)).toBe("dark");

      env.flipSystem();
      expect(get(store)).toBe("light");
      // The runtime class flipped without a single localStorage write.
      expect(Object.keys(backing)).toEqual([]);

      env.flipSystem();
      expect(get(store)).toBe("dark");
      expect(Object.keys(backing)).toEqual([]);
    } finally {
      env.restore();
    }
  });

  it("persists explicit selections only", () => {
    const backing: Record<string, string> = {};
    const env = installGlobals(backing);
    try {
      const store = createThemeStore();
      store.setTheme("light");
      expect(backing.gitpulse_theme_preference).toBe("light");

      env.flipSystem();
      // An explicit light preference ignores OS flips entirely.
      expect(get(store)).toBe("light");
      expect(backing.gitpulse_theme_preference).toBe("light");

      store.setPreference("system");
      expect(backing.gitpulse_theme_preference).toBe("system");
    } finally {
      env.restore();
    }
  });
});
