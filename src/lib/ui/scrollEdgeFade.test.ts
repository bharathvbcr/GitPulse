import { beforeEach, describe, expect, it, vi } from "vitest";
import type { OverflowHint } from "../dom/overflowHint";

const observed = vi.hoisted(() => ({
  deliver: null as null | ((hint: OverflowHint) => void),
  stop: vi.fn(),
}));

vi.mock("../dom/overflowHint", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../dom/overflowHint")>()),
  observeOverflow: (_el: unknown, _axis: unknown, onChange: (hint: OverflowHint) => void) => {
    observed.deliver = onChange;
    return observed.stop;
  },
}));

import { scrollEdgeFade } from "./scrollEdgeFade";

function body(): HTMLElement {
  return { dataset: {} as DOMStringMap } as unknown as HTMLElement;
}

describe("scrollEdgeFade", () => {
  beforeEach(() => {
    observed.deliver = null;
    observed.stop.mockClear();
  });

  it("fades the edge that still hides content and clears it at the end of the scroll", () => {
    const el = body();
    scrollEdgeFade(el);
    observed.deliver?.({ canScroll: true, showStart: false, showEnd: true });
    expect(el.dataset.gpFade).toBe("y-end");
    observed.deliver?.({ canScroll: true, showStart: true, showEnd: true });
    expect(el.dataset.gpFade).toBe("y-both");
    // Scrolled to the end: nothing is hidden below, so the last row must not
    // sit under a mask.
    observed.deliver?.({ canScroll: true, showStart: true, showEnd: false });
    expect(el.dataset.gpFade).toBe("y-start");
  });

  it("leaves a body that fits unmasked", () => {
    const el = body();
    scrollEdgeFade(el);
    observed.deliver?.({ canScroll: false, showStart: false, showEnd: false });
    expect(el.dataset.gpFade).toBeUndefined();
  });

  it("stops observing and removes its mask when the dialog goes", () => {
    const el = body();
    const action = scrollEdgeFade(el);
    observed.deliver?.({ canScroll: true, showStart: false, showEnd: true });
    action.destroy();
    expect(observed.stop).toHaveBeenCalledOnce();
    expect(el.dataset.gpFade).toBeUndefined();
  });
});
