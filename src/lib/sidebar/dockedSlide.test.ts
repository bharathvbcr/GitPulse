import { describe, expect, it, vi } from "vitest";
import { DURATION_MS, EASING, EXIT_EASE, EXIT_MS } from "../motion/tokens";
import { DOCKED_SLIDE_PROPERTIES, slideDocked } from "./dockedSlide";

const REDUCED = { matchMedia: () => ({ matches: true }) } as unknown as Pick<Window, "matchMedia">;
const FULL = { matchMedia: () => ({ matches: false }) } as unknown as Pick<Window, "matchMedia">;

interface Played {
  frames: Keyframe[];
  options: KeyframeAnimationOptions;
  animation: { onfinish: (() => void) | null; cancel: ReturnType<typeof vi.fn> };
}

function fakeElement(): { element: HTMLElement; played: Played[] } {
  const played: Played[] = [];
  const element = {
    animate: (frames: Keyframe[], options: KeyframeAnimationOptions) => {
      const animation = { onfinish: null as (() => void) | null, cancel: vi.fn() };
      played.push({ frames, options, animation });
      return animation as unknown as Animation;
    },
  } as unknown as HTMLElement;
  return { element, played };
}

describe("slideDocked", () => {
  it("animates only transform and opacity, for every slide", () => {
    for (const slide of ["in", "out", "settle"] as const) {
      const { element, played } = fakeElement();
      slideDocked(element, slide, 300, () => {}, FULL);
      const properties = new Set(played[0].frames.flatMap((frame) => Object.keys(frame)));
      for (const property of properties) {
        expect(DOCKED_SLIDE_PROPERTIES, `${slide}: ${property}`).toContain(property);
      }
      // No layout property, and not the `offset` keyframe key either.
      expect([...properties].some((property) => /^(width|height|margin|padding|top|left|right|bottom)/.test(property))).toBe(false);
    }
  });

  it("enters at the fast token and leaves at the exit duration, shorter than the entrance", () => {
    const into = fakeElement();
    slideDocked(into.element, "in", 300, () => {}, FULL);
    const out = fakeElement();
    slideDocked(out.element, "out", 300, () => {}, FULL);
    expect(into.played[0].options.duration).toBe(DURATION_MS.fast);
    expect(out.played[0].options.duration).toBe(EXIT_MS.fast);
    expect(EXIT_MS.fast).toBeLessThan(DURATION_MS.fast);
    expect(into.played[0].options.easing).toBe(EASING.effect);
    expect(out.played[0].options.easing).toBe(EXIT_EASE);
  });

  it("travels the open width less the rail's: in from away to home, out from home to away", () => {
    const into = fakeElement();
    slideDocked(into.element, "in", 300, () => {}, FULL);
    expect(into.played[0].frames[0]).toMatchObject({ transform: "translate3d(-300px, 0, 0)", opacity: 0 });
    expect(into.played[0].frames[1]).toMatchObject({ transform: "translate3d(0, 0, 0)", opacity: 1 });
    const out = fakeElement();
    slideDocked(out.element, "out", 300, () => {}, FULL);
    expect(out.played[0].frames[0]).toMatchObject({ transform: "translate3d(0, 0, 0)", opacity: 1 });
    expect(out.played[0].frames[1]).toMatchObject({ transform: "translate3d(-300px, 0, 0)", opacity: 0 });
  });

  it("never travels a negative or non-finite distance the wrong way", () => {
    const { element, played } = fakeElement();
    slideDocked(element, "in", -50, () => {}, FULL);
    expect(played[0].frames[0]).toMatchObject({ transform: "translate3d(0px, 0, 0)" });
  });

  it("does nothing under reduced motion except report done at once", () => {
    const { element, played } = fakeElement();
    const done = vi.fn();
    expect(slideDocked(element, "out", 300, done, REDUCED)).toBeNull();
    expect(done).toHaveBeenCalledTimes(1);
    expect(played).toHaveLength(0);
  });

  it("reports done when it finishes, then releases the held end frame after the caller's change", async () => {
    const { element, played } = fakeElement();
    const order: string[] = [];
    slideDocked(element, "out", 300, () => order.push("done"), FULL);
    played[0].animation.cancel.mockImplementation(() => order.push("cancel"));
    played[0].animation.onfinish?.();
    expect(order).toEqual(["done"]);
    await Promise.resolve();
    expect(order).toEqual(["done", "cancel"]);
  });

  it("does not report done for a slide its caller cancels", () => {
    const { element, played } = fakeElement();
    const done = vi.fn();
    slideDocked(element, "in", 300, done, FULL);
    played[0].animation.cancel();
    expect(done).not.toHaveBeenCalled();
  });

  it("falls back to done at once where Element.animate does not exist", () => {
    const done = vi.fn();
    expect(slideDocked({} as HTMLElement, "in", 300, done, FULL)).toBeNull();
    expect(done).toHaveBeenCalledTimes(1);
  });
});
