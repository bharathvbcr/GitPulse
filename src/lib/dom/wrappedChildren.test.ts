import { describe, expect, it } from "vitest";
import { countWrapped } from "./wrappedChildren";

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
