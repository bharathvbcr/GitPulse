import { get, writable } from "svelte/store";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { HostLoadPulse } from "../../stores/hostLoadStore";
import { BACKDROP_MOTION } from "./backdrops";
import {
  MOTION_ATTRIBUTE,
  MOTION_LAYER_CLASS,
  PULSE_EXPIRY_MS,
  QUIET_PULSES,
  installWallpaperMotion,
  motionPlan,
  renderMotion,
  wallpaperMotionState,
  type MotionPlan,
  type MotionSources,
} from "./wallpaperMotion";
import type { WallpaperState } from "./wallpaperStore";

// The unit environment has no DOM, so the layer is drawn onto a stand-in with
// just the surface wallpaperMotion.ts touches.

class FakeElement {
  children: FakeElement[] = [];
  parent: FakeElement | null = null;
  className = "";
  dataset: Record<string, string> = {};
  attrs = new Map<string, string>();
  props = new Map<string, string>();
  style = {
    backgroundImage: "",
    setProperty: (name: string, value: string) => void this.props.set(name, value),
  };
  muted = false;
  defaultMuted = false;
  loop = false;
  playsInline = false;
  autoplay = false;
  controls = true;
  disablePictureInPicture = false;
  poster = "";
  paused = true;
  playRejection: unknown = null;
  playCalls = 0;
  loads = 0;
  pauses = 0;
  classes = new Set<string>();
  classList = { contains: (name: string) => this.classes.has(name) };

  constructor(readonly tag: string) {}

  get src(): string {
    return this.attrs.get("src") ?? "";
  }
  set src(value: string) {
    this.attrs.set("src", value);
  }
  setAttribute(name: string, value: string) {
    this.attrs.set(name, value);
  }
  getAttribute(name: string) {
    return this.attrs.get(name) ?? null;
  }
  removeAttribute(name: string) {
    this.attrs.delete(name);
  }
  append(...nodes: FakeElement[]) {
    for (const node of nodes) {
      node.parent = this;
      this.children.push(node);
    }
  }
  prepend(node: FakeElement) {
    node.parent = this;
    this.children.unshift(node);
  }
  remove() {
    if (this.parent) this.parent.children = this.parent.children.filter((child) => child !== this);
    this.parent = null;
  }
  descendants(): FakeElement[] {
    return this.children.flatMap((child) => [child, ...child.descendants()]);
  }
  querySelectorAll(selector: string) {
    return this.descendants().filter((node) => node.tag === selector);
  }
  querySelector(selector: string) {
    return this.querySelectorAll(selector)[0] ?? null;
  }
  play() {
    this.playCalls += 1;
    if (this.playRejection !== null) return Promise.reject(this.playRejection);
    this.paused = false;
    return Promise.resolve();
  }
  pause() {
    this.pauses += 1;
    this.paused = true;
  }
  load() {
    this.loads += 1;
  }
}

class FakeMedia {
  matches = false;
  listeners = new Set<() => void>();
  addEventListener(_type: string, fn: () => void) {
    this.listeners.add(fn);
  }
  removeEventListener(_type: string, fn: () => void) {
    this.listeners.delete(fn);
  }
}

class FakeDocument {
  documentElement = new FakeElement("html");
  body: FakeElement | null = new FakeElement("body");
  visibilityState = "visible";
  listeners = new Map<string, Set<() => void>>();
  media = { motion: new FakeMedia(), transparency: new FakeMedia() };
  defaultView = {
    matchMedia: (query: string) => (query.includes("motion") ? this.media.motion : this.media.transparency),
  };
  createElement(tag: string) {
    return new FakeElement(tag);
  }
  querySelector(_selector: string) {
    return this.body?.children.find((child) => child.className === MOTION_LAYER_CLASS) ?? null;
  }
  addEventListener(type: string, fn: () => void) {
    if (!this.listeners.has(type)) this.listeners.set(type, new Set());
    this.listeners.get(type)?.add(fn);
  }
  removeEventListener(type: string, fn: () => void) {
    this.listeners.get(type)?.delete(fn);
  }
  fire(type: string) {
    for (const fn of this.listeners.get(type) ?? []) fn();
  }
  layer() {
    return this.querySelector("") as FakeElement | null;
  }
}

const asDoc = (doc: FakeDocument) => doc as unknown as Document;

const NONE: WallpaperState = { status: "none" };
const READY_VIDEO: WallpaperState = {
  status: "ready", url: "blob:still", name: "clip.mp4", width: 10, height: 10, videoUrl: "blob:video",
};

describe("renderMotion", () => {
  it("draws a built-in's textures as one layer first in body, with the root attribute set", () => {
    const doc = new FakeDocument();
    const plan = motionPlan("neural", NONE) as MotionPlan;
    renderMotion(asDoc(doc), plan);
    const layer = doc.layer();
    expect(layer?.getAttribute("aria-hidden")).toBe("true");
    expect(layer?.dataset.wallpaperMotion).toBe("neural");
    expect(layer?.children.length).toBe(BACKDROP_MOTION.neural.length);
    const first = layer?.children[0];
    expect(first?.className).toBe("gp-wp-drift");
    expect(first?.props.get("--gp-wp-dur")).toBe("90s");
    expect(first?.style.backgroundImage).toMatch(/^url\("/);
    const delayed = layer?.children.find((child) => child.props.has("--gp-wp-delay"));
    expect(delayed?.className).toMatch(/^gp-wp-accent gp-wp-/);
    expect(doc.documentElement.getAttribute(MOTION_ATTRIBUTE)).toBe("");
  });

  it("keeps the same layer while the plan's key holds, and rebuilds it when the key changes", () => {
    const doc = new FakeDocument();
    renderMotion(asDoc(doc), motionPlan("neural", NONE));
    const first = doc.layer();
    renderMotion(asDoc(doc), motionPlan("neural", NONE));
    expect(doc.layer()).toBe(first);
    renderMotion(asDoc(doc), motionPlan("reactor", NONE));
    expect(doc.layer()).not.toBe(first);
    expect(doc.layer()?.dataset.wallpaperMotion).toBe("reactor");
    expect(doc.body?.children).toHaveLength(1);
  });

  it("removes the layer and the root attribute when there is no plan", () => {
    const doc = new FakeDocument();
    renderMotion(asDoc(doc), motionPlan("neural", NONE));
    renderMotion(asDoc(doc), null);
    expect(doc.layer()).toBeNull();
    expect(doc.documentElement.getAttribute(MOTION_ATTRIBUTE)).toBeNull();
    // Nothing to remove is not an error.
    renderMotion(asDoc(doc), null);
  });

  it("draws nothing before the body exists", () => {
    const doc = new FakeDocument();
    doc.body = null;
    renderMotion(asDoc(doc), motionPlan("neural", NONE));
    expect(doc.documentElement.getAttribute(MOTION_ATTRIBUTE)).toBeNull();
  });

  it("builds a muted looping video with its poster and source, and starts it", () => {
    const doc = new FakeDocument();
    renderMotion(asDoc(doc), motionPlan("custom", READY_VIDEO));
    const layer = doc.layer();
    expect(layer?.dataset.wallpaperMotion).toBe("video");
    const video = layer?.querySelector("video") as FakeElement;
    expect([video.muted, video.loop, video.autoplay, video.playsInline, video.controls, video.disablePictureInPicture]).toEqual([true, true, true, true, false, true]);
    expect(video.poster).toBe("blob:still");
    expect(video.src).toBe("blob:video");
    expect(video.playCalls).toBe(1);
    // A running video is not started again by the next evaluation.
    renderMotion(asDoc(doc), motionPlan("custom", READY_VIDEO));
    expect(video.playCalls).toBe(1);
  });

  it("never gives a style or a src a URL that is not our own data: or blob:", () => {
    const doc = new FakeDocument();
    const hostile: MotionPlan = { kind: "video", key: "x", src: "https://evil.example/a.mp4", poster: 'blob:a"b' };
    renderMotion(asDoc(doc), hostile);
    const video = doc.layer()?.querySelector("video") as FakeElement;
    expect(video.src).toBe("");
    expect(video.poster).toBe("");
    const layers: MotionPlan = { kind: "builtin", key: "k", layers: [{ kind: "fade", url: "javascript:alert(1)", seconds: 1, delay: 0 }] };
    renderMotion(asDoc(doc), layers);
    expect(doc.layer()?.children).toHaveLength(0);
  });

  it("releases the video's decoder when its layer goes, and rebuilds for a different source", () => {
    const doc = new FakeDocument();
    renderMotion(asDoc(doc), motionPlan("custom", READY_VIDEO));
    const first = doc.layer()?.querySelector("video") as FakeElement;
    renderMotion(asDoc(doc), motionPlan("custom", { ...READY_VIDEO, videoUrl: "blob:other" }));
    expect(first.pauses).toBe(1);
    expect(first.loads).toBe(1);
    expect(first.getAttribute("src")).toBeNull();
    const second = doc.layer()?.querySelector("video") as FakeElement;
    expect(second).not.toBe(first);
    renderMotion(asDoc(doc), null);
    expect(second.pauses).toBe(1);
    expect(second.loads).toBe(1);
  });

  it("reports a refused play by its source, and ignores an aborted one", async () => {
    const refused = vi.fn();
    const doc = new FakeDocument();
    const original = doc.createElement.bind(doc);
    doc.createElement = (tag: string) => {
      const el = original(tag);
      el.playRejection = new DOMException("no", "NotAllowedError");
      return el;
    };
    renderMotion(asDoc(doc), motionPlan("custom", READY_VIDEO), refused);
    await Promise.resolve();
    await Promise.resolve();
    expect(refused).toHaveBeenCalledExactlyOnceWith("blob:video");

    const aborted = vi.fn();
    const doc2 = new FakeDocument();
    const original2 = doc2.createElement.bind(doc2);
    doc2.createElement = (tag: string) => {
      const el = original2(tag);
      el.playRejection = new DOMException("removed", "AbortError");
      return el;
    };
    renderMotion(asDoc(doc2), motionPlan("custom", READY_VIDEO), aborted);
    await Promise.resolve();
    await Promise.resolve();
    expect(aborted).not.toHaveBeenCalled();
  });
});

describe("installWallpaperMotion", () => {
  const pulse = (over: Partial<HostLoadPulse> = {}) =>
    ({ enabled: true, state: "fresh", power: "ac", thermal: "nominal", pressure: "normal", compressor_thrash: false, load_per_core: 0.1, ...over }) as unknown as HostLoadPulse;

  function setup(over: { backdrop?: string; custom?: WallpaperState } = {}) {
    const doc = new FakeDocument();
    doc.documentElement.classes.add("macos");
    const prefs = writable({ backdrop: over.backdrop ?? "neural", wallpaperMotion: true, reduceMotion: false });
    const platform = writable({ native_material: true });
    const custom = writable<WallpaperState>(over.custom ?? NONE);
    const pulses = writable<HostLoadPulse | null>(null);
    let time = 1_000;
    const startPulse = vi.fn();
    const sources = { prefs, platform, custom, pulse: pulses, startPulse, doc: asDoc(doc), now: () => time } as unknown as MotionSources;
    const stop = installWallpaperMotion(sources);
    return { doc, prefs, platform, custom, pulses, startPulse, stop, advance: (ms: number) => { time += ms; } };
  }

  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("does nothing without a document", () => {
    const stop = installWallpaperMotion({ doc: null } as unknown as MotionSources);
    expect(stop).toBeTypeOf("function");
    stop();
  });

  it("asks for the sampler once movement is wanted, and moves only after enough quiet pulses", () => {
    const t = setup();
    expect(t.startPulse).toHaveBeenCalled();
    expect(get(wallpaperMotionState).reason).toBe("unknown");
    for (let i = 0; i < QUIET_PULSES - 1; i += 1) t.pulses.set(pulse());
    expect(get(wallpaperMotionState).reason).toBe("settling");
    expect(t.doc.layer()).toBeNull();
    t.pulses.set(pulse());
    expect(get(wallpaperMotionState)).toMatchObject({ play: true, reason: "playing" });
    expect(t.doc.layer()?.dataset.wallpaperMotion).toBe("neural");
    expect(t.doc.documentElement.getAttribute(MOTION_ATTRIBUTE)).toBe("");
    t.stop();
    expect(t.doc.layer()).toBeNull();
  });

  it("stops at once on battery, and when the window is hidden, reduced, or the app's Reduce Motion is on", () => {
    const t = setup();
    for (let i = 0; i < QUIET_PULSES; i += 1) t.pulses.set(pulse());
    expect(get(wallpaperMotionState).play).toBe(true);

    t.doc.visibilityState = "hidden";
    t.doc.fire("visibilitychange");
    expect(get(wallpaperMotionState).reason).toBe("hidden");
    expect(t.doc.layer()).toBeNull();
    t.doc.visibilityState = "visible";
    t.doc.fire("visibilitychange");
    expect(get(wallpaperMotionState).play).toBe(true);

    t.doc.media.motion.matches = true;
    for (const fn of t.doc.media.motion.listeners) fn();
    expect(get(wallpaperMotionState).reason).toBe("reduce-motion");
    t.doc.media.motion.matches = false;
    t.doc.media.transparency.matches = true;
    for (const fn of t.doc.media.transparency.listeners) fn();
    expect(get(wallpaperMotionState).reason).toBe("reduce-transparency");
    t.doc.media.transparency.matches = false;

    t.prefs.update((p) => ({ ...p, reduceMotion: true }));
    expect(get(wallpaperMotionState).reason).toBe("reduce-motion");
    t.prefs.update((p) => ({ ...p, reduceMotion: false }));

    t.pulses.set(pulse({ power: "battery" }));
    expect(get(wallpaperMotionState).reason).toBe("battery");
    expect(t.doc.layer()).toBeNull();
    t.stop();
  });

  it("lets a pulse that stops arriving expire, through its own re-check", async () => {
    // The re-check runs on the browser's adaptive timer, which needs a window and document to exist.
    vi.stubGlobal("window", {
      setTimeout: (handler: () => void, ms: number) => setTimeout(handler, ms),
      clearTimeout: (handle: number) => clearTimeout(handle),
      addEventListener() {},
      removeEventListener() {},
    });
    vi.stubGlobal("document", { visibilityState: "visible", hasFocus: () => true, addEventListener() {}, removeEventListener() {} });
    const t = setup();
    for (let i = 0; i < QUIET_PULSES; i += 1) t.pulses.set(pulse());
    expect(get(wallpaperMotionState).play).toBe(true);
    t.advance(PULSE_EXPIRY_MS + 1_000);
    await vi.advanceTimersByTimeAsync(PULSE_EXPIRY_MS + 6_000);
    expect(get(wallpaperMotionState).reason).toBe("unknown");
    expect(t.doc.layer()).toBeNull();
    t.stop();
    vi.unstubAllGlobals();
  });

  it("falls back to the first frame once the window refuses to play the video", async () => {
    const t = setup({ backdrop: "custom", custom: READY_VIDEO });
    const original = t.doc.createElement.bind(t.doc);
    t.doc.createElement = (tag: string) => {
      const el = original(tag);
      el.playRejection = new DOMException("blocked", "NotSupportedError");
      return el;
    };
    for (let i = 0; i < QUIET_PULSES; i += 1) t.pulses.set(pulse());
    await vi.advanceTimersByTimeAsync(0);
    expect(get(wallpaperMotionState).reason).toBe("refused");
    expect(t.doc.layer()).toBeNull();
    t.stop();
  });

  it("does not start the sampler in a window that draws no wallpaper", () => {
    const doc = new FakeDocument();
    const startPulse = vi.fn();
    const stop = installWallpaperMotion({
      prefs: writable({ backdrop: "neural", wallpaperMotion: true, reduceMotion: false }),
      platform: writable({ native_material: true }),
      custom: writable<WallpaperState>(NONE),
      pulse: writable<HostLoadPulse | null>(null),
      startPulse,
      doc: asDoc(doc),
    } as unknown as MotionSources);
    expect(startPulse).not.toHaveBeenCalled();
    expect(get(wallpaperMotionState).reason).toBe("host");
    stop();
  });

  it("re-evaluates when the root's class or motion attributes change", () => {
    let observed: (() => void) | null = null;
    const disconnect = vi.fn();
    vi.stubGlobal("MutationObserver", class {
      constructor(fn: () => void) { observed = fn; }
      observe() {}
      disconnect = disconnect;
    });
    try {
      const t = setup();
      for (let i = 0; i < QUIET_PULSES; i += 1) t.pulses.set(pulse());
      expect(get(wallpaperMotionState).play).toBe(true);
      t.doc.documentElement.setAttribute("data-motion", "reduced");
      (observed as (() => void) | null)?.();
      expect(get(wallpaperMotionState).reason).toBe("reduce-motion");
      t.stop();
      expect(disconnect).toHaveBeenCalled();
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("calls a listener with the current state at once and stops calling it after it unsubscribes", () => {
    const seen: string[] = [];
    const off = wallpaperMotionState.subscribe((state) => seen.push(state.reason));
    expect(seen).toHaveLength(1);
    const t = setup();
    expect(seen.length).toBeGreaterThan(1);
    off();
    const count = seen.length;
    t.prefs.update((p) => ({ ...p, wallpaperMotion: false }));
    expect(seen).toHaveLength(count);
    t.stop();
  });
});
