import { describe, it, expect, afterEach, vi } from "vitest";
import { get } from "svelte/store";
import {
  SECTION_STORAGE_KEY,
  STORAGE_KEY,
  createLayoutStore,
  loadLayout,
  loadSections,
  saveSections,
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
    expect(elapsedMs).toBeLessThan(100);
    expect(({} as Record<string, unknown>).polluted).toBeUndefined();

    // Every accepted width must round-trip through JSON.stringify unchanged.
    for (let i = 0; i < 500; i += 1) {
      const layout = loadLayout(inputs[i]);
      expect(loadLayout(JSON.stringify(layout))).toEqual(layout);
    }
  });

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
});
