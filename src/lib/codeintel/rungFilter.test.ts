import { describe, expect, it } from "vitest";
import {
  rungHistogramLine,
  rungParam,
  shouldShowRungControl,
} from "./rungFilter";

describe("rungFilter", () => {
  it("omits min_rung when 'all' is selected", () => {
    expect(rungParam("all")).toBeUndefined();
    expect(rungParam("deterministic")).toBe("deterministic");
  });

  it("suppresses the control when layered impact is active", () => {
    expect(shouldShowRungControl(true)).toBe(false);
    expect(shouldShowRungControl(false)).toBe(true);
  });

  it("formats filtered_out as what you did not see", () => {
    const line = rungHistogramLine({
      deterministic: 2,
      high: 5,
      speculative: 9,
      filtered_out: 7,
    });
    expect(line).toContain("filtered out by min_rung");
    expect(line).toContain("2 deterministic");
    expect(line).toContain("7 filtered out");
  });
});
