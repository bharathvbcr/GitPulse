import { describe, expect, it } from "vitest";
import { WORK_TIMEOUT_MS, workTimeout } from "./request";

describe("workTimeout", () => {
  it("returns the default when a value is invalid", () => {
    expect(workTimeout()).toBe(WORK_TIMEOUT_MS);
    expect(workTimeout(NaN)).toBe(WORK_TIMEOUT_MS);
    expect(workTimeout(0)).toBe(WORK_TIMEOUT_MS);
    expect(workTimeout(-1)).toBe(WORK_TIMEOUT_MS);
  });

  it("caps long timeouts and enforces a positive lower bound", () => {
    expect(workTimeout(50_000)).toBe(WORK_TIMEOUT_MS);
    expect(workTimeout(0.5)).toBe(1);
  });

  it("keeps valid values as-is when already in range", () => {
    expect(workTimeout(5_000)).toBe(5_000);
  });
});
