import { describe, it, expect, afterEach, vi } from "vitest";
import { SIDEBAR_VIEWS, sidebarViewForKey } from "./layoutStore";
import { STRESS_TIMEOUT_MS, expectWithinBudget } from "../__tests__/perfBudget";
import { get } from "svelte/store";
import {
  SECTION_STORAGE_KEY,
  SIDEBAR_NAV_STORAGE_KEY,
  STORAGE_KEY,
  createLayoutStore,
  createSidebarNavStore,
  doubleClickFollows,
  loadLayout,
  loadSections,
  loadSidebarNav,
  resolveFileOpen,
  SIDEBAR_DOUBLE_CLICK_WINDOW_MS,
  saveSections,
  saveSidebarNav,
} from "./layoutStore";
import {
  SIDEBAR_DEFAULT_WIDTH,
  SIDEBAR_MAX_WIDTH,
  SIDEBAR_MIN_WIDTH,
} from "./metrics";

/**
 * Installs a fake window.localStorage backed by a plain record (node env has
 * no storage), mirroring themeStore.persistence.test's installGlobals.
 */
function installStorage(backing: Record<string, string>) {
  const fake = {
    getItem: (key: string) => (key in backing ? backing[key] : null),
    setItem: (key: string, value: string) => {
      backing[key] = value;
    },
    removeItem: (key: string) => {
      delete backing[key];
    },
    clear: () => {
      for (const key of Object.keys(backing)) delete backing[key];
    },
  };
  const original = globalThis.window;
  Object.defineProperty(globalThis, "window", { value: { localStorage: fake }, configurable: true });
  return () => {
    if (original === undefined) {
      delete (globalThis as Record<string, unknown>).window;
    } else {
      Object.defineProperty(globalThis, "window", { value: original, configurable: true });
    }
  };
}

afterEach(() => {
  vi.restoreAllMocks();
});

/** A store built against whatever storage installStorage installed. */
function createFreshStore() {
  return createLayoutStore();
}

describe("loadLayout", () => {
  it("returns defaults for null, empty, and non-JSON blobs", () => {
    for (const raw of [null, "", "   ", "{not json", "[1,2]", '"str"', "123", "true", "null"]) {
      expect(loadLayout(raw)).toEqual({ width: SIDEBAR_DEFAULT_WIDTH, collapsed: false });
    }
  });

  it("parses valid layouts", () => {
    expect(loadLayout('{"width":420,"collapsed":false}')).toEqual({ width: 420, collapsed: false });
    expect(loadLayout('{"width":560,"collapsed":true}')).toEqual({ width: SIDEBAR_MAX_WIDTH, collapsed: true });
  });

  it("clamps out-of-range widths into the supported range", () => {
    expect(loadLayout('{"width":-50,"collapsed":false}').width).toBe(SIDEBAR_MIN_WIDTH);
    expect(loadLayout('{"width":99999}').width).toBe(SIDEBAR_MAX_WIDTH);
    expect(loadLayout('{"width":0}').width).toBe(SIDEBAR_MIN_WIDTH);
  });

  it("falls back field-wise so one bad field never discards the other", () => {
    // Valid width + garbage collapsed keeps the width.
    expect(loadLayout('{"width":480,"collapsed":"yes"}')).toEqual({
      width: 480,
      collapsed: false,
    });
    // Garbage width + valid collapsed keeps the collapsed flag.
    expect(loadLayout('{"width":"480","collapsed":true}')).toEqual({
      width: SIDEBAR_DEFAULT_WIDTH,
      collapsed: true,
    });
    expect(loadLayout('{"width":{"nested":1},"collapsed":false}').collapsed).toBe(false);
    expect(loadLayout('{"width":null,"collapsed":null}')).toEqual({
      width: SIDEBAR_DEFAULT_WIDTH,
      collapsed: false,
    });
  });

  it("never pollutes Object.prototype via hostile keys", () => {
    const result = loadLayout('{"__proto__":{"polluted":1},"width":400}');
    expect(result.width).toBe(400);
    expect(({} as Record<string, unknown>).polluted).toBeUndefined();
  });

  it("treats array blobs and scalar JSON as garbage", () => {
    expect(loadLayout('[400,true]')).toEqual({ width: SIDEBAR_DEFAULT_WIDTH, collapsed: false });
    expect(loadLayout("400")).toEqual({ width: SIDEBAR_DEFAULT_WIDTH, collapsed: false });
  });
});

describe("layoutStore", () => {
  it("starts at defaults when storage is empty", () => {
    const restore = installStorage({});
    try {
      const store = createFreshStore();
      expect(get(store)).toEqual({ width: SIDEBAR_DEFAULT_WIDTH, collapsed: false });
    } finally {
      restore();
    }
  });

  it("hydrates from previously persisted state", () => {
    const restore = installStorage({ [STORAGE_KEY]: '{"width":500,"collapsed":true}' });
    try {
      const store = createFreshStore();
      expect(get(store)).toEqual({ width: 500, collapsed: true });
    } finally {
      restore();
    }
  });

  it("setWidth clamps and persists; NaN fails closed to the default", () => {
    const backing: Record<string, string> = {};
    const restore = installStorage(backing);
    try {
      const store = createFreshStore();
      store.setWidth(50);
      expect(get(store).width).toBe(SIDEBAR_MIN_WIDTH);

      store.setWidth(1200);
      expect(get(store).width).toBe(SIDEBAR_MAX_WIDTH);

      store.setWidth(Number.NaN);
      expect(get(store).width).toBe(SIDEBAR_DEFAULT_WIDTH);

      store.setWidth(442);
      expect(get(store).width).toBe(442);
      expect(JSON.parse(backing[STORAGE_KEY])).toEqual({ width: 442, collapsed: false });
    } finally {
      restore();
    }
  });

  it("toggleCollapsed flips and persists across instances", () => {
    const backing: Record<string, string> = {};
    const restore = installStorage(backing);
    try {
      const store = createFreshStore();
      store.toggleCollapsed();
      expect(get(store).collapsed).toBe(true);
      store.toggleCollapsed();
      expect(get(store).collapsed).toBe(false);
      expect(JSON.parse(backing[STORAGE_KEY]).collapsed).toBe(false);

      const reopened = createFreshStore();
      expect(get(reopened).collapsed).toBe(false);
    } finally {
      restore();
    }
  });

  it("reset restores defaults and writes them", () => {
    const backing: Record<string, string> = {};
    const restore = installStorage(backing);
    try {
      const store = createFreshStore();
      store.setWidth(520);
      store.toggleCollapsed();
      store.reset();
      expect(get(store)).toEqual({ width: SIDEBAR_DEFAULT_WIDTH, collapsed: false });
      expect(JSON.parse(backing[STORAGE_KEY])).toEqual({
        width: SIDEBAR_DEFAULT_WIDTH,
        collapsed: false,
      });
    } finally {
      restore();
    }
  });
});

describe("loadSections / saveSections", () => {
  it("defaults both sections open for missing or hostile input", () => {
    for (const raw of [null, "", "{{{", "[true,false]", '"x"', "7", '{"staged":1}', '{"unstaged":"no"}']) {
      expect(loadSections(raw)).toEqual({ staged: false, unstaged: false });
    }
  });

  it("parses valid section flags field-wise", () => {
    expect(loadSections('{"staged":true,"unstaged":false}')).toEqual({ staged: true, unstaged: false });
    expect(loadSections('{"staged":true}')).toEqual({ staged: true, unstaged: false });
    expect(loadSections('{"unstaged":true,"staged":"garbage"}')).toEqual({ staged: false, unstaged: true });
  });

  it("round-trips through saveSections with an explicit storage", () => {
    const backing: Record<string, string> = {};
    saveSections({ staged: true, unstaged: true }, {
      setItem: (key, value) => {
        backing[key] = value;
      },
    });
    expect(backing[SECTION_STORAGE_KEY]).toBe('{"staged":true,"unstaged":true}');
    expect(loadSections(backing[SECTION_STORAGE_KEY])).toEqual({ staged: true, unstaged: true });
  });

  it("saveSections no-ops when storage is unavailable", () => {
    expect(() => saveSections({ staged: true, unstaged: false }, null)).not.toThrow();
  });

  it("swallows quota failures instead of throwing", () => {
    expect(() =>
      saveSections({ staged: false, unstaged: false }, {
        setItem: () => {
          throw new Error("QuotaExceededError");
        },
      }),
    ).not.toThrow();
  });
});

/* --- Fuzz / stress -------------------------------------------------------- */

/** Deterministic LCG so failures reproduce; Math.random would flake reports. */
function makeRng(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (state * 1664525 + 1013904223) >>> 0;
    return state / 0xffffffff;
  };
}

const GARBAGE_ATOMS = [
  "{", "}", "[", "]", ",", ":", '"', "'", "\\", "width", "collapsed",
  "null", "true", "false", "0", "-1", "1e999", "NaN", "__proto__",
  "constructor", "prototype", "\\u0000", " ", "\n\t", "é", "🙂",
];

function randomGarbage(rng: () => number): string {
  const pieces: string[] = [];
  const length = 1 + Math.floor(rng() * 40);
  for (let i = 0; i < length; i += 1) {
    pieces.push(GARBAGE_ATOMS[Math.floor(rng() * GARBAGE_ATOMS.length)]);
  }
  return pieces.join("");
}

/** Semi-structured payloads: right shape, wrong field types. */
function randomHostileObject(rng: () => number): string {
  const widthPool: unknown[] = [
    rng() * 2000 - 500, Math.round(rng() * 10000), "400", null, true,
    { nested: rng() }, [rng()], Number.MAX_SAFE_INTEGER + 0.5,
  ];
  const collapsedPool: unknown[] = [rng() > 0.5, 0, 1, "true", null, {}, [], undefined];
  return JSON.stringify({
    width: widthPool[Math.floor(rng() * widthPool.length)],
    collapsed: collapsedPool[Math.floor(rng() * collapsedPool.length)],
    [`key${Math.floor(rng() * 10)}`]: randomGarbage(rng),
  });
}

function isSchemaValid(layout: ReturnType<typeof loadLayout>): boolean {
  return (
    Number.isFinite(layout.width) &&
    layout.width >= SIDEBAR_MIN_WIDTH &&
    layout.width <= SIDEBAR_MAX_WIDTH &&
    typeof layout.collapsed === "boolean"
  );
}

describe("loadLayout fuzz/stress", () => {
  it("survives 10k random garbage strings without throwing, always schema-valid, under 100ms", () => {
    const rng = makeRng(0x5eed_2026);
    const inputs: string[] = [];
    for (let i = 0; i < 10_000; i += 1) {
      // Mix raw noise with structured-but-hostile JSON objects.
      inputs.push(i % 3 === 0 ? randomHostileObject(rng) : randomGarbage(rng));
    }
    inputs.push('{"__proto__":{"polluted":1}}');
    inputs.push('{"constructor":{"prototype":{"polluted":1}}}');

    // Timed pass measures the parser alone; the correctness sweep below runs
    // untimed because per-iteration expect() costs dwarf the parse itself.
    const start = performance.now();
    for (const raw of inputs) loadLayout(raw);
    const elapsedMs = performance.now() - start;

    for (const raw of inputs) {
      let result: ReturnType<typeof loadLayout> | undefined;
      expect(() => {
        result = loadLayout(raw);
      }).not.toThrow();
      expect(isSchemaValid(result!)).toBe(true);
    }

    expect(inputs.length).toBeGreaterThanOrEqual(10_000);
    expectWithinBudget(elapsedMs, 300, "loadLayout over 10k garbage strings");
    expect(({} as Record<string, unknown>).polluted).toBeUndefined();

    // Every accepted width must round-trip through JSON.stringify unchanged.
    for (let i = 0; i < 500; i += 1) {
      const layout = loadLayout(inputs[i]);
      expect(loadLayout(JSON.stringify(layout))).toEqual(layout);
    }
  }, STRESS_TIMEOUT_MS);

  it("fuzzed sections parser also never throws", () => {
    const rng = makeRng(0xbeef_cafe);
    for (let i = 0; i < 2_000; i += 1) {
      const raw = i % 2 === 0
        ? JSON.stringify({ staged: randomGarbage(rng), unstaged: rng() > 0.5 })
        : randomGarbage(rng);
      const sections = loadSections(raw.slice(0, 300));
      expect(typeof sections.staged).toBe("boolean");
      expect(typeof sections.unstaged).toBe("boolean");
    }
  });

  it("persists sidebar navigation view and explicit file open target safely without guessing", () => {
    for (const raw of [null, "", "{bad", "[]", "42", '{"view":"bogus","openTarget":"guess"}']) {
      // "auto" is the default now; this test encoded the old default, "code".
      expect(loadSidebarNav(raw)).toEqual({ view: "git", openTarget: "auto" });
    }
    expect(loadSidebarNav('{"view":"files","openTarget":"split"}')).toEqual({
      view: "files",
      openTarget: "split",
    });
    expect(loadSidebarNav('{"view":"files","openTarget":"tab"}')).toEqual({
      view: "files",
      openTarget: "tab",
    });

    const backing: Record<string, string> = {};
    const restore = installStorage(backing);
    try {
      saveSidebarNav({ view: "files", openTarget: "tab" });
      expect(JSON.parse(backing[SIDEBAR_NAV_STORAGE_KEY])).toEqual({
        v: 2,
        view: "files",
        openTarget: "tab",
      });
      const navStore = createSidebarNavStore();
      expect(get(navStore)).toEqual({ view: "files", openTarget: "tab" });
      navStore.setOpenTarget("split");
      navStore.setView("git");
      expect(get(navStore)).toEqual({ view: "git", openTarget: "split" });
      navStore.reset();
      expect(get(navStore)).toEqual({ view: "git", openTarget: "auto" });
    } finally {
      restore();
    }
  });
});

describe("resolveFileOpen: a double-click follows its own clicks", () => {
  const base = { path: "a.ts", now: 10_000, defaultTarget: "code" as const, terminalActive: false };

  it("resolves a plain click from the target and modifier keys", () => {
    expect(resolveFileOpen({ ...base, pin: false, lastOpen: null })).toBe("code");
    expect(resolveFileOpen({ ...base, pin: false, lastOpen: null, altKey: true })).toBe("split");
  });

  it("keeps an Alt-double-click a split instead of pinning in Code", () => {
    const lastOpen = { path: "a.ts", action: "split" as const, at: 9_800 };
    // The double-click itself carries no modifiers: its clicks consumed them.
    expect(resolveFileOpen({ ...base, pin: true, lastOpen })).toBe("split");
  });

  it("re-resolves when the last click was another file, too old, or in the future", () => {
    expect(resolveFileOpen({ ...base, pin: true, lastOpen: { path: "b.ts", action: "split", at: 9_900 } })).toBe("code");
    expect(
      resolveFileOpen({ ...base, pin: true, lastOpen: { path: "a.ts", action: "split", at: base.now - SIDEBAR_DOUBLE_CLICK_WINDOW_MS - 1 } }),
    ).toBe("code");
    expect(resolveFileOpen({ ...base, pin: true, lastOpen: { path: "a.ts", action: "split", at: base.now + 1 } })).toBe("code");
  });

  it("never lets a single click reuse the last action", () => {
    expect(resolveFileOpen({ ...base, pin: false, lastOpen: { path: "a.ts", action: "split", at: 9_900 } })).toBe("code");
  });
});

describe("doubleClickFollows", () => {
  it("joins a double-click to its own click within the window only", () => {
    const last = { path: "a.ts", at: 1_000 };
    expect(doubleClickFollows(last, "a.ts", 1_000)).toBe(true);
    expect(doubleClickFollows(last, "a.ts", 1_000 + SIDEBAR_DOUBLE_CLICK_WINDOW_MS)).toBe(true);
    expect(doubleClickFollows(last, "a.ts", 1_001 + SIDEBAR_DOUBLE_CLICK_WINDOW_MS)).toBe(false);
    expect(doubleClickFollows(last, "a.ts", 999)).toBe(false);
    expect(doubleClickFollows(last, "b.ts", 1_100)).toBe(false);
    expect(doubleClickFollows(null, "a.ts", 1_100)).toBe(false);
  });
});

describe("sidebarViewForKey: arrow keys move the Git | Explorer tabs", () => {
  it("lists the views in the order the switcher draws them", () => {
    expect(SIDEBAR_VIEWS).toEqual(["git", "files"]);
  });

  it("moves with the arrows, wrapping, and jumps with Home and End", () => {
    expect(sidebarViewForKey("git", "ArrowRight")).toBe("files");
    expect(sidebarViewForKey("files", "ArrowRight")).toBe("git");
    expect(sidebarViewForKey("git", "ArrowLeft")).toBe("files");
    expect(sidebarViewForKey("files", "ArrowLeft")).toBe("git");
    expect(sidebarViewForKey("files", "Home")).toBe("git");
    expect(sidebarViewForKey("git", "End")).toBe("files");
  });

  it("leaves every other key to the caller", () => {
    for (const key of ["Enter", " ", "Tab", "ArrowDown", "ArrowUp", "a"]) {
      expect(sidebarViewForKey("git", key)).toBeNull();
    }
  });
});

describe("resolveFileOpen: the one decision table", () => {
  const base = { path: "a.ts", pin: false, lastOpen: null, now: 10_000 };

  it("explicit targets and modifiers, ported from the two resolvers it replaced", () => {
    // [defaultTarget, altKey, metaOrCtrlKey] -> where it opens. Alt wins over
    // Cmd/Ctrl; Cmd/Ctrl swaps code <-> terminal (code -> tab, else -> code).
    // An explicit target ignores whether the terminal is active.
    const table: Array<["code" | "tab" | "split", boolean, boolean, string]> = [
      ["code", false, false, "code"],
      ["tab", false, false, "tab"],
      ["split", false, false, "split"],
      ["code", true, false, "split"],
      ["tab", true, false, "split"],
      ["split", true, false, "split"],
      ["code", false, true, "tab"],
      ["tab", false, true, "code"],
      ["split", false, true, "code"],
      ["code", true, true, "split"],
      ["tab", true, true, "split"],
      ["split", true, true, "split"],
    ];
    for (const [defaultTarget, altKey, metaOrCtrlKey, expected] of table) {
      for (const terminalActive of [false, true]) {
        expect(resolveFileOpen({ ...base, defaultTarget, altKey, metaOrCtrlKey, terminalActive })).toBe(expected);
      }
    }
  });

  it("auto: a terminal split while the terminal is active, Code otherwise", () => {
    const table: Array<[boolean, boolean, boolean, string]> = [
      // [terminalActive, altKey, metaOrCtrlKey] -> where it opens
      [false, false, false, "code"],
      [true, false, false, "split"],
      [false, true, false, "split"],
      [true, true, false, "split"],
      // Cmd/Ctrl swaps the decided base, so it is never a no-op under auto.
      [false, false, true, "tab"],
      [true, false, true, "code"],
      [false, true, true, "split"],
      [true, true, true, "split"],
    ];
    for (const [terminalActive, altKey, metaOrCtrlKey, expected] of table) {
      expect(resolveFileOpen({ ...base, defaultTarget: "auto", terminalActive, altKey, metaOrCtrlKey })).toBe(expected);
    }
  });

  it("a double-click follows its click even when it carries modifiers, whatever the setting", () => {
    const lastOpen = { path: "a.ts", action: "split" as const, at: 5_000 };
    for (const defaultTarget of ["auto", "code", "tab", "split"] as const) {
      expect(
        resolveFileOpen({ path: "a.ts", pin: true, lastOpen, now: 5_100, defaultTarget, terminalActive: false, metaOrCtrlKey: true }),
      ).toBe("split");
    }
  });

  it("a double-click follows at exactly the window edge and not one ms later", () => {
    const at = 5_000;
    const lastOpen = { path: "a.ts", action: "tab" as const, at };
    const follow = { path: "a.ts", pin: true, lastOpen, defaultTarget: "code" as const, terminalActive: false };
    expect(resolveFileOpen({ ...follow, now: at + SIDEBAR_DOUBLE_CLICK_WINDOW_MS })).toBe("tab");
    expect(resolveFileOpen({ ...follow, now: at + SIDEBAR_DOUBLE_CLICK_WINDOW_MS + 1 })).toBe("code");
  });
});

describe("loadSidebarNav: the versioned open target", () => {
  it("reads an unversioned blob's explicit tab and split, and anything else as auto", () => {
    expect(loadSidebarNav('{"view":"files"}')).toEqual({ view: "files", openTarget: "auto" });
    // The old default was written on every commit, so an unversioned "code" is
    // not a choice.
    expect(loadSidebarNav('{"view":"git","openTarget":"code"}')).toEqual({ view: "git", openTarget: "auto" });
    expect(loadSidebarNav('{"view":"git","openTarget":"tab"}')).toEqual({ view: "git", openTarget: "tab" });
    expect(loadSidebarNav('{"view":"git","openTarget":"split"}')).toEqual({ view: "git", openTarget: "split" });
    expect(loadSidebarNav('{"view":"git","openTarget":"auto"}')).toEqual({ view: "git", openTarget: "auto" });
    expect(loadSidebarNav('{"view":"git","openTarget":"nope"}')).toEqual({ view: "git", openTarget: "auto" });
  });

  it("keeps every choice, code included, in a version-2 blob; an unknown one is auto", () => {
    for (const openTarget of ["auto", "code", "tab", "split"]) {
      expect(loadSidebarNav(JSON.stringify({ v: 2, view: "files", openTarget }))).toEqual({ view: "files", openTarget });
    }
    expect(loadSidebarNav('{"v":2,"view":"git","openTarget":"nope"}')).toEqual({ view: "git", openTarget: "auto" });
    expect(loadSidebarNav('{"v":3,"view":"git","openTarget":"code"}')).toEqual({ view: "git", openTarget: "auto" });
  });

  it("an explicit code survives a save and a reload instead of migrating to auto again", () => {
    const backing: Record<string, string> = {};
    const restore = installStorage(backing);
    try {
      createSidebarNavStore().setOpenTarget("code");
      expect(JSON.parse(backing[SIDEBAR_NAV_STORAGE_KEY])).toEqual({ v: 2, view: "git", openTarget: "code" });
      expect(get(createSidebarNavStore())).toEqual({ view: "git", openTarget: "code" });
      // A view switch re-commits the target; it must stay as chosen.
      const again = createSidebarNavStore();
      again.setView("files");
      expect(get(createSidebarNavStore())).toEqual({ view: "files", openTarget: "code" });
    } finally {
      restore();
    }
  });

  it("migrates a stored legacy code to auto on load, then persists the migration only on a commit", () => {
    const backing: Record<string, string> = {
      [SIDEBAR_NAV_STORAGE_KEY]: '{"view":"files","openTarget":"code"}',
    };
    const restore = installStorage(backing);
    try {
      const store = createSidebarNavStore();
      expect(get(store)).toEqual({ view: "files", openTarget: "auto" });
      store.setView("git");
      expect(JSON.parse(backing[SIDEBAR_NAV_STORAGE_KEY])).toEqual({ v: 2, view: "git", openTarget: "auto" });
    } finally {
      restore();
    }
  });
});
