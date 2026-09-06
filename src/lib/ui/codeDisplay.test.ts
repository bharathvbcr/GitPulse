import { describe, expect, it } from "vitest";
import {
  DEFAULT_TAB_WIDTH,
  DIFF_LAYOUTS,
  TAB_WIDTHS,
  applyTabWidth,
  isDiffLayout,
  isTabWidth,
} from "./codeDisplay";

class FakeStyle {
  readonly values = new Map<string, string>();
  setProperty(name: string, value: string) {
    this.values.set(name, value);
  }
}

describe("diff layout", () => {
  it("accepts exactly the two layouts the viewer implements", () => {
    for (const layout of DIFF_LAYOUTS) expect(isDiffLayout(layout)).toBe(true);
    expect(DIFF_LAYOUTS).toEqual(["unified", "split"]);
  });

  it.each([["side-by-side"], [""], [null], [undefined], [0], [{}]])(
    "rejects %p",
    (value) => {
      expect(isDiffLayout(value)).toBe(false);
    },
  );
});

describe("tab width", () => {
  it("defaults to the CSS initial value, so the setting is purely additive", () => {
    // A default of 2 or 4 would re-render every existing user's diffs the
    // first time they upgraded, which is not what adding an option means.
    expect(DEFAULT_TAB_WIDTH).toBe(8);
    expect(isTabWidth(DEFAULT_TAB_WIDTH)).toBe(true);
  });

  it("accepts only the offered widths", () => {
    for (const width of TAB_WIDTHS) expect(isTabWidth(width)).toBe(true);
    for (const value of [0, 1, 3, 16, -4, 4.5, "4", null, undefined]) {
      expect(isTabWidth(value), `accepted ${String(value)}`).toBe(false);
    }
  });

  it("publishes the width as the custom property app.css reads", () => {
    const style = new FakeStyle();
    applyTabWidth(2, { style });
    expect(style.values.get("--gp-tab-size")).toBe("2");
    applyTabWidth(4, { style });
    expect(style.values.get("--gp-tab-size")).toBe("4");
  });

  it("falls back rather than writing an arbitrary number into the stylesheet", () => {
    const style = new FakeStyle();
    applyTabWidth(37, { style });
    expect(style.values.get("--gp-tab-size")).toBe(String(DEFAULT_TAB_WIDTH));
    applyTabWidth(Number.NaN, { style });
    expect(style.values.get("--gp-tab-size")).toBe(String(DEFAULT_TAB_WIDTH));
  });

  it("is a no-op without a target instead of throwing", () => {
    expect(() => applyTabWidth(4, null)).not.toThrow();
  });
});
