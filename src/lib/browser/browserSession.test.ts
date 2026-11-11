import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

type Listener = (event: { payload: unknown }) => void;
let pageListener: Listener | null = null;
const listen = vi.fn(async (channel: string, listener: Listener) => {
  if (channel === "browser-pane") pageListener = listener;
  return () => {};
});
vi.mock("@tauri-apps/api/event", () => ({ listen: (...args: [string, Listener]) => listen(...args) }));

const invoke = vi.fn(async () => undefined);
vi.mock("../ipc/invoke", () => ({ invoke: (...args: unknown[]) => invoke(...(args as [])) }));

const {
  DEFAULT_ADDRESS,
  browserIpc,
  isOccluded,
  lastAddress,
  normalizeAddress,
  onPageEvents,
  overlapsRect,
  paneShown,
  parsePageEvent,
  rememberAddress,
  sameBounds,
  samplePoints,
  toWindowPoints,
} = await import("./browserSession");

describe("page events from the backend", () => {
  it("reads every kind the backend sends", () => {
    expect(parsePageEvent({ id: "t1", kind: "loading", url: "http://localhost:3000/" })).toEqual({ id: "t1", kind: "loading", url: "http://localhost:3000/" });
    expect(parsePageEvent({ id: "t1", kind: "loaded", url: "http://localhost:3000/a" })).toEqual({ id: "t1", kind: "loaded", url: "http://localhost:3000/a" });
    expect(parsePageEvent({ id: "t1", kind: "title", title: "Home" })).toEqual({ id: "t1", kind: "title", title: "Home" });
    expect(parsePageEvent({ id: "t1", kind: "refused", url: "https://example.com/" })).toEqual({ id: "t1", kind: "refused", url: "https://example.com/" });
  });

  it.each([
    ["nothing", undefined],
    ["a string", "loaded"],
    ["no id", { kind: "loaded", url: "x" }],
    ["an empty id", { id: "", kind: "loaded", url: "x" }],
    ["an unknown kind", { id: "t1", kind: "crashed", url: "x" }],
    ["a load with no url", { id: "t1", kind: "loaded" }],
    ["a title that is not text", { id: "t1", kind: "title", title: 7 }],
  ])("ignores %s", (_label, payload) => {
    expect(parsePageEvent(payload)).toBeNull();
  });

  it("caps page text, which the page controls", () => {
    const parsed = parsePageEvent({ id: "t1", kind: "title", title: "x".repeat(10_000) });
    expect(parsed?.kind === "title" && parsed.title.length).toBe(2048);
  });

  it("hands each session only its own events, through one listener", async () => {
    const seenA: unknown[] = [];
    const seenB: unknown[] = [];
    const stopA = onPageEvents("a", (e) => seenA.push(e));
    onPageEvents("b", (e) => seenB.push(e));
    await Promise.resolve();
    expect(listen).toHaveBeenCalledTimes(1);
    pageListener?.({ payload: { id: "a", kind: "title", title: "A" } });
    pageListener?.({ payload: { id: "b", kind: "title", title: "B" } });
    pageListener?.({ payload: { id: "c", kind: "title", title: "nobody" } });
    pageListener?.({ payload: { id: "a", kind: "bogus" } });
    expect(seenA).toEqual([{ id: "a", kind: "title", title: "A" }]);
    expect(seenB).toEqual([{ id: "b", kind: "title", title: "B" }]);
    stopA();
    pageListener?.({ payload: { id: "a", kind: "title", title: "after" } });
    expect(seenA).toHaveLength(1);
  });
});

describe("the commands", () => {
  it("names each command and its arguments as the backend registers them", async () => {
    const bounds = { x: 1, y: 2, width: 3, height: 4 };
    await browserIpc.open("t1", "http://localhost:3000/", bounds);
    await browserIpc.setBounds("t1", bounds);
    await browserIpc.setVisible("t1", false);
    await browserIpc.navigate("t1", "http://localhost:3000/b");
    await browserIpc.go("t1", "back");
    await browserIpc.close("t1");
    await browserIpc.snapshot("t1");
    expect(invoke.mock.calls).toEqual([
      ["cmd_browser_open", { id: "t1", url: "http://localhost:3000/", bounds }],
      ["cmd_browser_set_bounds", { id: "t1", bounds }],
      ["cmd_browser_set_visible", { id: "t1", visible: false }],
      ["cmd_browser_navigate", { id: "t1", url: "http://localhost:3000/b" }],
      ["cmd_browser_go", { id: "t1", to: "back" }],
      ["cmd_browser_close", { id: "t1" }],
      ["cmd_browser_snapshot", { id: "t1" }],
    ]);
  });
});

describe("the address bar", () => {
  it.each([
    ["3000", "http://localhost:3000"],
    ["5173/app?x=1", "http://localhost:5173/app?x=1"],
    ["localhost:5173", "http://localhost:5173"],
    ["LOCALHOST", "http://LOCALHOST"],
    ["127.0.0.1:8080/x", "http://127.0.0.1:8080/x"],
    ["[::1]:4000", "http://[::1]:4000"],
    ["  http://localhost:3000/  ", "http://localhost:3000/"],
    // Not shortened forms: passed on for the backend to refuse.
    ["example.com", "example.com"],
    ["https://example.com/", "https://example.com/"],
    ["localhost.evil.com", "localhost.evil.com"],
    ["123456", "123456"],
    ["", ""],
  ])("reads %j as %j", (typed, url) => {
    expect(normalizeAddress(typed)).toBe(url);
  });

  describe("the last address", () => {
    const store = new Map<string, string>();
    beforeEach(() => {
      store.clear();
      vi.stubGlobal("localStorage", {
        getItem: (k: string) => store.get(k) ?? null,
        setItem: (k: string, v: string) => void store.set(k, v),
      });
    });
    afterEach(() => vi.unstubAllGlobals());

    it("is remembered and offered next time", () => {
      expect(lastAddress()).toBe(DEFAULT_ADDRESS);
      rememberAddress("http://localhost:5173/");
      expect(lastAddress()).toBe("http://localhost:5173/");
    });

    it("never stores or offers an oversized one", () => {
      rememberAddress(`http://localhost/${"x".repeat(5000)}`);
      expect(lastAddress()).toBe(DEFAULT_ADDRESS);
      store.set("gitpulse.browser.lastAddress", "y".repeat(5000));
      expect(lastAddress()).toBe(DEFAULT_ADDRESS);
    });

    it("works without storage", () => {
      vi.stubGlobal("localStorage", {
        getItem: () => {
          throw new Error("denied");
        },
        setItem: () => {
          throw new Error("denied");
        },
      });
      expect(lastAddress()).toBe(DEFAULT_ADDRESS);
      expect(() => rememberAddress("http://localhost:1/")).not.toThrow();
    });
  });
});

describe("placement", () => {
  it("scales CSS pixels to window points by the page zoom in effect", () => {
    const rect = { left: 10, top: 20, width: 300, height: 200 };
    expect(toWindowPoints(rect, 1)).toEqual({ x: 10, y: 20, width: 300, height: 200 });
    expect(toWindowPoints(rect, 1.25)).toEqual({ x: 12.5, y: 25, width: 375, height: 250 });
    // A zoom that is not a real, positive number is treated as none.
    for (const zoom of [0, -1, Number.NaN, Number.POSITIVE_INFINITY]) {
      expect(toWindowPoints(rect, zoom)).toEqual({ x: 10, y: 20, width: 300, height: 200 });
    }
    expect(toWindowPoints({ left: 0, top: 0, width: -5, height: -5 }, 1)).toEqual({ x: 0, y: 0, width: 0, height: 0 });
  });

  it("treats sub-point jitter as no move", () => {
    const a = { x: 10, y: 10, width: 100, height: 100 };
    expect(sameBounds(a, { ...a, x: 10.4 })).toBe(true);
    expect(sameBounds(a, { ...a, width: 100.6 })).toBe(false);
    expect(sameBounds(null, null)).toBe(true);
    expect(sameBounds(a, null)).toBe(false);
  });

  it("samples a grid at most 64px apart, inset from the border, at most 10 a side", () => {
    const small = samplePoints({ left: 0, top: 0, width: 100, height: 60 });
    expect(small).toContainEqual({ x: 3, y: 3 });
    expect(small).toContainEqual({ x: 97, y: 57 });
    expect(small).toContainEqual({ x: 50, y: 3 });
    const page = { left: 10, top: 20, width: 600, height: 400 };
    const grid = samplePoints(page);
    const xs = [...new Set(grid.map((p) => p.x))].sort((a, b) => a - b);
    const ys = [...new Set(grid.map((p) => p.y))].sort((a, b) => a - b);
    // At most 64px apart, or a ninth of the sampled span on a larger page.
    expect(xs.every((x, i) => i === 0 || x - xs[i - 1] <= Math.max(64, 594 / 9) + 1e-9)).toBe(true);
    expect(ys.every((y, i) => i === 0 || y - ys[i - 1] <= 64)).toBe(true);
    expect(xs[0]).toBe(13);
    expect(xs.at(-1)).toBe(607);
    // A 200px menu tucked 8px inside the top-right corner falls on the grid.
    const menu = { left: 10 + 600 - 208, top: 20 + 8, width: 200, height: 220 };
    expect(grid.some((p) => p.x >= menu.left && p.x <= menu.left + menu.width && p.y >= menu.top && p.y <= menu.top + menu.height)).toBe(true);
    // A huge page is sampled more coarsely, never with more than 100 points.
    expect(samplePoints({ left: 0, top: 0, width: 5000, height: 3000 })).toHaveLength(100);
    expect(samplePoints({ left: 0, top: 0, width: 4, height: 100 })).toEqual([]);
  });

  it("counts any overlap by a real box, and ignores a sliver", () => {
    const pane = { left: 0, top: 0, width: 100, height: 100 };
    expect(overlapsRect(pane, { left: 90, top: 90, width: 20, height: 20 })).toBe(true);
    expect(overlapsRect(pane, { left: 100, top: 0, width: 20, height: 20 })).toBe(false);
    expect(overlapsRect(pane, { left: -30, top: 10, width: 20, height: 20 })).toBe(false);
    expect(overlapsRect(pane, { left: 50, top: 50, width: 1, height: 1 })).toBe(false);
    expect(overlapsRect(pane, { left: 50, top: 50, width: 0, height: 40 })).toBe(false);
  });

  describe("occlusion", () => {
    const host = { contains: (el: unknown) => el === child } as unknown as Element;
    const child = {} as Element;
    const overlay = {} as Element;
    const points = [{ x: 1, y: 1 }, { x: 2, y: 2 }];

    it("is clear when the area or what is inside it is on top everywhere", () => {
      expect(isOccluded(host, points, () => host)).toBe(false);
      expect(isOccluded(host, points, (x) => (x === 1 ? child : host))).toBe(false);
      // Off screen: nothing to be drawn over.
      expect(isOccluded(host, points, () => null)).toBe(false);
    });

    it("is covered when anything else is on top at any point", () => {
      expect(isOccluded(host, points, (x) => (x === 2 ? overlay : host))).toBe(true);
    });
  });

  it("shows the view only when opened, on screen, uncovered and of real size", () => {
    const bounds = { x: 0, y: 0, width: 10, height: 10 };
    const base = { opened: true, onscreen: true, occluded: false, bounds };
    expect(paneShown(base)).toBe(true);
    expect(paneShown({ ...base, opened: false })).toBe(false);
    expect(paneShown({ ...base, onscreen: false })).toBe(false);
    expect(paneShown({ ...base, occluded: true })).toBe(false);
    expect(paneShown({ ...base, bounds: null })).toBe(false);
    expect(paneShown({ ...base, bounds: { ...bounds, width: 0.5 } })).toBe(false);
  });
});
