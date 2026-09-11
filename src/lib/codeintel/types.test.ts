import { describe, expect, it } from "vitest";
import { parseCodeintelResponse, parseCodeintelStatus } from "./types";

describe("parseCodeintelResponse", () => {
  it("throws on null, arrays, and non-objects", () => {
    expect(() => parseCodeintelResponse(null)).toThrow(/no payload/);
    expect(() => parseCodeintelResponse(undefined)).toThrow(/no payload/);
    expect(() => parseCodeintelResponse([])).toThrow(/no payload/);
  });

  it("throws when available is true without items", () => {
    expect(() => parseCodeintelResponse({ available: true, truncated: false })).toThrow(
      /without items/,
    );
  });

  it("does not invent an empty success from a missing available flag", () => {
    expect(() => parseCodeintelResponse({})).toThrow(/omitted available/);
  });

  it("normalises an unavailable payload to an empty items list", () => {
    const parsed = parseCodeintelResponse<{ n: string }>({
      available: false,
      reason: "stale",
    });
    expect(parsed.available).toBe(false);
    expect(parsed.items).toEqual([]);
    expect(parsed.truncated).toBe(false);
  });
});

describe("parseCodeintelStatus", () => {
  it("throws on null", () => {
    expect(() => parseCodeintelStatus(null)).toThrow(/no payload/);
  });

  it("throws when available is true without db_path", () => {
    expect(() => parseCodeintelStatus({ available: true })).toThrow(/without db_path/);
  });

  it("accepts an unavailable status with an empty path", () => {
    const parsed = parseCodeintelStatus({ available: false, reason: "missing" });
    expect(parsed.available).toBe(false);
    expect(parsed.db_path).toBe("");
    expect(parsed.reason).toBe("missing");
  });
});
