import { describe, expect, it } from "vitest";
import { asRecord, isRecord } from "./record";

describe("isRecord / asRecord", () => {
  it("accepts plain objects and rejects null, arrays and primitives", () => {
    const obj = { a: 1 };
    expect(isRecord(obj)).toBe(true);
    expect(isRecord({})).toBe(true);
    for (const bad of [null, undefined, [], [1], "x", 0, true, () => {}]) expect(isRecord(bad)).toBe(false);
    expect(asRecord(obj)).toBe(obj);
    expect(asRecord([])).toBeNull();
    expect(asRecord(null)).toBeNull();
  });
});
