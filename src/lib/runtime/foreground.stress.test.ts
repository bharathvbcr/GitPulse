import { afterEach, describe, expect, it } from "vitest";
import {
  addForegroundListener,
  bindForegroundChanges,
  isBackgroundDocument,
  noteForegroundFocus,
  readBackgroundDocument,
  resetForegroundFocus,
  type ForegroundDocument,
} from "./foreground";

function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), a | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

class CountTarget extends EventTarget {
  adds = 0;
  removes = 0;
  override addEventListener(...args: Parameters<EventTarget["addEventListener"]>): void {
    this.adds += 1;
    super.addEventListener(...args);
  }
  override removeEventListener(...args: Parameters<EventTarget["removeEventListener"]>): void {
    this.removes += 1;
    super.removeEventListener(...args);
  }
}

describe("foreground predicate", () => {
  afterEach(() => resetForegroundFocus());

  it("treats a missing document as foreground and a failed focus check as background", () => {
    expect(isBackgroundDocument(null)).toBe(false);
    expect(isBackgroundDocument(undefined)).toBe(false);
    expect(readBackgroundDocument()).toBe(false);
    const thrown: ForegroundDocument = { hasFocus: () => { throw new Error("denied"); } };
    expect(isBackgroundDocument(thrown)).toBe(true);
    expect(isBackgroundDocument({})).toBe(false);
    expect(isBackgroundDocument({ hasFocus: () => true })).toBe(false);
    expect(isBackgroundDocument({ hasFocus: () => false })).toBe(true);
    expect(isBackgroundDocument({ hidden: true, hasFocus: () => true })).toBe(true);
    expect(isBackgroundDocument({ visibilityState: "hidden", hasFocus: () => true })).toBe(true);
  });

  it("lets a focus event win over a stale hasFocus reading", () => {
    const doc = new EventTarget() as EventTarget & ForegroundDocument;
    doc.hidden = false;
    doc.visibilityState = "visible";
    doc.hasFocus = () => true;
    const frame = new EventTarget();
    let calls = 0;
    const unbind = bindForegroundChanges(doc, frame, () => { calls += 1; });
    frame.dispatchEvent(new Event("blur"));
    expect(calls).toBe(1);
    expect(isBackgroundDocument(doc)).toBe(true);
    doc.hasFocus = () => false;
    frame.dispatchEvent(new Event("focus"));
    expect(calls).toBe(2);
    expect(isBackgroundDocument(doc)).toBe(false);
    doc.hasFocus = () => { throw new Error("stale"); };
    expect(isBackgroundDocument(doc)).toBe(false);
    unbind();
    unbind();
    frame.dispatchEvent(new Event("blur"));
    expect(calls).toBe(2);
  });

  it("holds the matrix across 20,000 hostile documents", () => {
    const rand = mulberry32(0xca5e11ce);
    const hiddenValues = [true, false, 0, 1, "yes", null, undefined, Number.NaN];
    const visibilityValues = ["hidden", "visible", "", "prerender", undefined];
    for (let i = 0; i < 20_000; i += 1) {
      resetForegroundFocus();
      const hidden = hiddenValues[Math.floor(rand() * hiddenValues.length)];
      const visibilityState = visibilityValues[Math.floor(rand() * visibilityValues.length)];
      const mode = Math.floor(rand() * 5);
      const doc: ForegroundDocument = {};
      if (hidden !== undefined) doc.hidden = hidden as boolean;
      if (visibilityState !== undefined) doc.visibilityState = visibilityState;
      if (mode === 0) doc.hasFocus = () => { throw new Error("denied"); };
      else if (mode === 1) doc.hasFocus = () => true;
      else if (mode === 2) doc.hasFocus = () => false;
      else if (mode === 4) doc.hasFocus = () => 1 as unknown as boolean;
      const override = rand();
      if (override < 0.33) noteForegroundFocus(false);
      else if (override < 0.66) noteForegroundFocus(true);
      let background = true;
      expect(() => { background = isBackgroundDocument(doc); }, `step ${i}`).not.toThrow();
      const concealed = doc.hidden === true || doc.visibilityState === "hidden";
      if (concealed) expect(background, `hidden step ${i}`).toBe(true);
      else if (override < 0.33) expect(background, `blur override step ${i}`).toBe(true);
      else if (override < 0.66) expect(background, `focus override step ${i}`).toBe(false);
      else if (mode === 0 || mode === 2 || mode === 4) expect(background, `unfocused step ${i}`).toBe(true);
      else expect(background, `focused step ${i}`).toBe(false);
    }
  });

  it("does not subscribe the same listener twice", () => {
    const doc = new CountTarget();
    const frame = new CountTarget();
    const listener = () => {};
    addForegroundListener(doc, frame, "focus", listener);
    addForegroundListener(doc, frame, "focus", listener);
    expect(frame.adds).toBe(1);
    expect(doc.adds).toBe(0);
  });

  it("binds and unbinds without leaking listeners", () => {
    const doc = new CountTarget();
    const frame = new CountTarget();
    const listener = () => {};
    for (let i = 0; i < 1_000; i += 1) {
      const unbind = bindForegroundChanges(doc, frame, listener);
      unbind();
      unbind();
    }
    expect(doc.adds).toBe(1_000);
    expect(doc.removes).toBe(1_000);
    expect(frame.adds).toBe(2_000);
    expect(frame.removes).toBe(2_000);
  });
});
