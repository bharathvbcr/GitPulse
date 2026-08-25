import { describe, expect, it } from "vitest";
import { BRANCH_PALETTE, getBranchColor } from "./Palette";

describe("getBranchColor", () => {
  it("cycles through the palette", () => {
    expect(getBranchColor(0)).toBe(BRANCH_PALETTE[0]);
    expect(getBranchColor(BRANCH_PALETTE.length)).toBe(BRANCH_PALETTE[0]);
    expect(getBranchColor(5)).toBe(BRANCH_PALETTE[5]);
  });
});
