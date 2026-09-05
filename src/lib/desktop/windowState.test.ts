import { describe, expect, it } from "vitest";
import {
  clampToMonitors,
  MIN_WINDOW_HEIGHT,
  MIN_WINDOW_WIDTH,
  parseWindowState,
  readWindowState,
  shouldPersist,
  writeWindowState,
  type MonitorArea,
  type WindowRect,
} from "./windowState";

const LAPTOP: MonitorArea = { x: 0, y: 0, width: 1920, height: 1080 };
const EXTERNAL: MonitorArea = { x: 1920, y: 0, width: 2560, height: 1440 };

const rect = (over: Partial<WindowRect> = {}): WindowRect => ({
  x: 100,
  y: 100,
  width: 1280,
  height: 850,
  maximized: false,
  ...over,
});

function fakeStorage(initial?: string) {
  const map = new Map<string, string>();
  if (initial !== undefined) map.set("gitpulse_window_state", initial);
  return {
    map,
    getItem: (key: string) => map.get(key) ?? null,
    setItem: (key: string, value: string) => void map.set(key, value),
  };
}

describe("parseWindowState", () => {
  it("reads a complete rect", () => {
    expect(parseWindowState(JSON.stringify(rect()))).toEqual(rect());
  });

  it("rejects a rect with a non-finite field rather than half-restoring it", () => {
    // A NaN width is how an app opens invisible.
    for (const broken of [
      { ...rect(), width: Number.NaN },
      { ...rect(), x: Number.POSITIVE_INFINITY },
      { ...rect(), height: null },
      { ...rect(), y: "12" },
    ]) {
      expect(parseWindowState(JSON.stringify(broken))).toBeNull();
    }
  });

  it("rejects zero and negative extents", () => {
    expect(parseWindowState(JSON.stringify({ ...rect(), width: 0 }))).toBeNull();
    expect(parseWindowState(JSON.stringify({ ...rect(), height: -10 }))).toBeNull();
  });

  it("returns null for absent, empty or non-JSON input", () => {
    expect(parseWindowState(null)).toBeNull();
    expect(parseWindowState("")).toBeNull();
    expect(parseWindowState("{oops")).toBeNull();
    expect(parseWindowState("[]")).toBeNull();
    expect(parseWindowState("42")).toBeNull();
  });
});

describe("clampToMonitors", () => {
  it("keeps a rect that still lands on a monitor", () => {
    expect(clampToMonitors(rect(), [LAPTOP])).toEqual(rect());
  });

  it("keeps a rect on a secondary monitor", () => {
    const onExternal = rect({ x: 2000, y: 200 });
    expect(clampToMonitors(onExternal, [LAPTOP, EXTERNAL])).toEqual(onExternal);
  });

  it("recentres a window whose monitor was unplugged", () => {
    // The classic naive-restore failure: the saved rectangle describes a
    // display layout that no longer exists, and the window opens unreachable.
    const orphan = rect({ x: 2000, y: 200 });
    const restored = clampToMonitors(orphan, [LAPTOP]);
    expect(restored).not.toBeNull();
    expect(restored!.x).toBeGreaterThanOrEqual(LAPTOP.x);
    expect(restored!.x + restored!.width).toBeLessThanOrEqual(LAPTOP.x + LAPTOP.width);
  });

  it("recentres a window dragged almost entirely off-screen", () => {
    const barelyOn = rect({ x: 1900, y: 1060 });
    const restored = clampToMonitors(barelyOn, [LAPTOP]);
    expect(restored!.x).toBeLessThan(1900);
  });

  it("keeps the user's size when only the position was impossible", () => {
    const orphan = rect({ x: 5000, y: 5000, width: 1400, height: 900 });
    const restored = clampToMonitors(orphan, [LAPTOP]);
    expect(restored!.width).toBe(1400);
    expect(restored!.height).toBe(900);
  });

  it("never restores below the configured minimum size", () => {
    const tiny = rect({ width: 200, height: 150 });
    const restored = clampToMonitors(tiny, [LAPTOP]);
    expect(restored!.width).toBe(MIN_WINDOW_WIDTH);
    expect(restored!.height).toBe(MIN_WINDOW_HEIGHT);
  });

  it("shrinks to fit a monitor smaller than the saved window", () => {
    const small: MonitorArea = { x: 0, y: 0, width: 1024, height: 768 };
    const restored = clampToMonitors(rect({ x: 9000, y: 9000 }), [small]);
    expect(restored!.width).toBeLessThanOrEqual(small.width);
    expect(restored!.height).toBeLessThanOrEqual(small.height);
  });

  it("declines when there is no rect or no monitor", () => {
    expect(clampToMonitors(null, [LAPTOP])).toBeNull();
    expect(clampToMonitors(rect(), [])).toBeNull();
  });
});

describe("shouldPersist", () => {
  it("records an ordinary window", () => {
    expect(shouldPersist(rect())).toBe(true);
  });

  it("refuses a maximized rect as the restore size", () => {
    // Saving the screen-filling geometry is how un-maximize stops meaning
    // anything.
    expect(shouldPersist(rect({ maximized: true }))).toBe(false);
  });

  it("refuses a minimized window's degenerate geometry", () => {
    expect(shouldPersist(rect({ width: 0, height: 0 }))).toBe(false);
  });
});

describe("writeWindowState", () => {
  it("keeps the last normal geometry when the window is maximized", () => {
    const storage = fakeStorage(JSON.stringify(rect()));
    writeWindowState(storage, rect({ width: 3840, height: 2160, maximized: true }));
    const stored = readWindowState(storage);
    expect(stored).toEqual({ ...rect(), maximized: true });
  });

  it("records a normal resize", () => {
    const storage = fakeStorage();
    writeWindowState(storage, rect({ width: 1600, height: 1000 }));
    expect(readWindowState(storage)?.width).toBe(1600);
  });

  it("survives a storage that throws", () => {
    const hostile = {
      getItem: () => {
        throw new Error("blocked");
      },
      setItem: () => {
        throw new Error("quota");
      },
    };
    expect(() => writeWindowState(hostile, rect())).not.toThrow();
    expect(readWindowState(hostile)).toBeNull();
  });

  it("is a no-op with no storage at all", () => {
    expect(() => writeWindowState(null, rect())).not.toThrow();
    expect(readWindowState(null)).toBeNull();
  });
});
