import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it, vi } from "vitest";
import { DURATION_MS } from "../motion/tokens";
import { SPLASH_ID, handOffSplash, type SplashElement } from "./Splash.svelte";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "Splash.svelte"), "utf8");

function plate(): SplashElement & { attributes: Record<string, string>; removed: boolean } {
  const element = {
    attributes: {} as Record<string, string>,
    removed: false,
    style: { transitionDuration: "" },
    setAttribute(name: string, value: string) {
      element.attributes[name] = value;
    },
    remove() {
      element.removed = true;
    },
  };
  return element;
}

describe("Splash hand-off", () => {
  it("fades the plate for the emphasis token, then removes it", () => {
    expect(DURATION_MS.emphasis).toBe(480);
    const el = plate();
    const scheduled: { callback: () => void; ms: number }[] = [];
    expect(handOffSplash(el, DURATION_MS.emphasis, (callback, ms) => scheduled.push({ callback, ms }))).toBe(true);
    expect(el.style.transitionDuration).toBe("480ms");
    expect(el.attributes["data-leaving"]).toBe("");
    expect(el.removed).toBe(false);
    expect(scheduled).toHaveLength(1);
    // Hands off within the 480 ms plus a frame's slack for the removal.
    expect(scheduled[0].ms).toBeLessThanOrEqual(DURATION_MS.emphasis + 60);
    scheduled[0].callback();
    expect(el.removed).toBe(true);
  });

  it("is a cut under reduced motion (0 ms): removed at once, nothing scheduled", () => {
    const el = plate();
    const schedule = vi.fn();
    expect(handOffSplash(el, 0, schedule)).toBe(true);
    expect(el.removed).toBe(true);
    expect(schedule).not.toHaveBeenCalled();
  });

  it("does nothing when the plate is already gone", () => {
    expect(handOffSplash(null, DURATION_MS.emphasis)).toBe(false);
  });

  it("finds the plate index.html paints, and waits on frames, not on data", () => {
    expect(SPLASH_ID).toBe("gp-boot");
    expect(source).toContain("requestAnimationFrame");
    expect(source).toContain('motionMs("emphasis")');
    expect(source).not.toMatch(/\bawait\b|invoke\(/);
  });
});
