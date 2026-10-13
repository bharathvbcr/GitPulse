import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { installWindowStatePersistence, type WindowStateStorage } from "./windowState";

/** The Tauri window API, as one object the test can drive and inspect. */
const win = {
  setSize: vi.fn(async () => {}),
  setPosition: vi.fn(async () => {}),
  maximize: vi.fn(async () => {}),
  outerSize: vi.fn(async () => ({ width: 1300, height: 900 })),
  outerPosition: vi.fn(async () => ({ x: 40, y: 50 })),
  isMaximized: vi.fn(async () => false),
  onResized: vi.fn(async (_handler: () => void) => unlistenResize),
  onMoved: vi.fn(async (_handler: () => void) => unlistenMove),
};
const unlistenResize = vi.fn();
const unlistenMove = vi.fn();
const availableMonitors = vi.fn(async () => [
  { position: { x: 0, y: 0 }, size: { width: 1920, height: 1080 } },
]);

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => win,
  availableMonitors: () => availableMonitors(),
}));
vi.mock("@tauri-apps/api/dpi", () => ({
  PhysicalSize: class {
    constructor(
      readonly width: number,
      readonly height: number,
    ) {}
  },
  PhysicalPosition: class {
    constructor(
      readonly x: number,
      readonly y: number,
    ) {}
  },
}));

function memoryStorage(initial: Record<string, string> = {}): WindowStateStorage & { data: Map<string, string> } {
  const data = new Map(Object.entries(initial));
  return {
    data,
    getItem: (key) => data.get(key) ?? null,
    setItem: (key, value) => void data.set(key, value),
  };
}

const KEY = "gitpulse_window_state";
const SAVED = JSON.stringify({ x: 100, y: 120, width: 1280, height: 850, maximized: false });

beforeEach(() => {
  vi.useFakeTimers();
  for (const fn of Object.values(win)) fn.mockClear();
  unlistenResize.mockClear();
  unlistenMove.mockClear();
  availableMonitors.mockClear();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("installWindowStatePersistence", () => {
  it("does nothing outside Tauri, or with nowhere to store", async () => {
    const outside = await installWindowStatePersistence(false, memoryStorage());
    outside();
    const noStorage = await installWindowStatePersistence(true, null);
    noStorage();
    expect(availableMonitors).not.toHaveBeenCalled();
    expect(win.onResized).not.toHaveBeenCalled();
  });

  it("restores the saved size and position, then listens for changes", async () => {
    const dispose = await installWindowStatePersistence(true, memoryStorage({ [KEY]: SAVED }));
    expect(win.setSize).toHaveBeenCalledWith(expect.objectContaining({ width: 1280, height: 850 }));
    expect(win.setPosition).toHaveBeenCalledWith(expect.objectContaining({ x: 100, y: 120 }));
    expect(win.maximize).not.toHaveBeenCalled();
    expect(win.onResized).toHaveBeenCalledTimes(1);
    expect(win.onMoved).toHaveBeenCalledTimes(1);
    dispose();
  });

  it("maximizes only after the size and position have landed", async () => {
    const order: string[] = [];
    win.setSize.mockImplementationOnce(async () => void order.push("size"));
    win.setPosition.mockImplementationOnce(async () => void order.push("position"));
    win.maximize.mockImplementationOnce(async () => void order.push("maximize"));
    const stored = JSON.stringify({ x: 100, y: 120, width: 1280, height: 850, maximized: true });
    const dispose = await installWindowStatePersistence(true, memoryStorage({ [KEY]: stored }));
    expect(order).toEqual(["size", "position", "maximize"]);
    dispose();
  });

  it("opens at the configured default when nothing was saved", async () => {
    const dispose = await installWindowStatePersistence(true, memoryStorage());
    expect(win.setSize).not.toHaveBeenCalled();
    expect(win.setPosition).not.toHaveBeenCalled();
    dispose();
  });

  it("pulls a rectangle saved on a monitor that is gone back onto one that exists", async () => {
    const offscreen = JSON.stringify({ x: 9000, y: 9000, width: 1280, height: 850, maximized: false });
    const dispose = await installWindowStatePersistence(true, memoryStorage({ [KEY]: offscreen }));
    const position = (win.setPosition.mock.calls as unknown as [{ x: number; y: number }][])[0]?.[0] as { x: number; y: number };
    expect(position.x).toBeLessThan(1920);
    expect(position.y).toBeLessThan(1080);
    dispose();
  });

  it("saves once where a drag ends, not on every move event", async () => {
    const storage = memoryStorage();
    const dispose = await installWindowStatePersistence(true, storage, 400);
    const onMoved = win.onMoved.mock.calls[0]![0] as () => void;
    onMoved();
    onMoved();
    onMoved();
    await vi.advanceTimersByTimeAsync(399);
    expect(storage.data.has(KEY)).toBe(false);
    await vi.advanceTimersByTimeAsync(2);
    expect(JSON.parse(storage.data.get(KEY)!)).toEqual({ x: 40, y: 50, width: 1300, height: 900, maximized: false });
    expect(win.outerSize).toHaveBeenCalledTimes(1);
    dispose();
  });

  it("captures once more on dispose so a quit inside the debounce window keeps the last move", async () => {
    const storage = memoryStorage();
    const dispose = await installWindowStatePersistence(true, storage, 400);
    (win.onResized.mock.calls[0]![0] as () => void)();
    dispose();
    expect(unlistenResize).toHaveBeenCalledTimes(1);
    expect(unlistenMove).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(0);
    expect(storage.data.has(KEY)).toBe(true);
    // The pending debounced capture was cancelled, so there is exactly one.
    expect(win.outerSize).toHaveBeenCalledTimes(1);
  });

  it("survives the window disappearing mid-capture", async () => {
    const storage = memoryStorage();
    win.outerSize.mockRejectedValueOnce(new Error("window closed"));
    const dispose = await installWindowStatePersistence(true, storage, 10);
    (win.onMoved.mock.calls[0]![0] as () => void)();
    await vi.advanceTimersByTimeAsync(20);
    expect(storage.data.has(KEY)).toBe(false);
    dispose();
  });

  it("falls back to a no-op disposer when the window API refuses", async () => {
    availableMonitors.mockRejectedValueOnce(new Error("no permission"));
    const dispose = await installWindowStatePersistence(true, memoryStorage({ [KEY]: SAVED }));
    expect(() => dispose()).not.toThrow();
    expect(win.setSize).not.toHaveBeenCalled();
  });
});
