import { describe, expect, it } from "vitest";
import { countHidden, countWrapped } from "./wrappedChildren";

const at = (...tops: number[]) => tops.map((offsetTop) => ({ offsetTop }));

describe("countWrapped", () => {
  it("counts nothing for an empty strip or one that fits on its line", () => {
    expect(countWrapped([])).toBe(0);
    expect(countWrapped(at(3, 3, 3))).toBe(0);
  });

  it("counts every child that flowed below the first line", () => {
    expect(countWrapped(at(3, 3, 23, 23, 43))).toBe(3);
  });

  it("treats a rounding pixel as the same line", () => {
    expect(countWrapped(at(3, 4, 3))).toBe(0);
  });
});

describe("countHidden", () => {
  it("is the wrapped count while the first child fits", () => {
    expect(countHidden(at(3, 3, 23), false)).toBe(1);
    expect(countHidden(at(3, 3, 3), false)).toBe(0);
  });

  it("hides every child when even the first does not fit", () => {
    expect(countHidden(at(3, 23, 43), true)).toBe(3);
    expect(countHidden(at(3), true)).toBe(1);
  });

  it("counts nothing for an empty strip", () => {
    expect(countHidden([], true)).toBe(0);
  });
});
