import { describe, expect, it } from "vitest";
import {
  DEFAULT_MAX_COMMITS,
  LOAD_MORE_STEP,
  MAX_LOAD_COMMITS,
  nextLoadLimit,
} from "../graphLimits";

describe("nextLoadLimit", () => {
  it("steps up from the default by one page", () => {
    expect(nextLoadLimit(DEFAULT_MAX_COMMITS)).toBe(DEFAULT_MAX_COMMITS + LOAD_MORE_STEP);
  });

  it("clamps the final step at the ceiling", () => {
    expect(nextLoadLimit(MAX_LOAD_COMMITS - 1)).toBe(MAX_LOAD_COMMITS);
  });

  it("returns null once everything loadable is loaded", () => {
    expect(nextLoadLimit(MAX_LOAD_COMMITS)).toBeNull();
    expect(nextLoadLimit(MAX_LOAD_COMMITS + 5_000)).toBeNull();
  });
});
