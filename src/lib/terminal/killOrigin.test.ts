import { describe, expect, it } from "vitest";
import { protectedSkipOf } from "./killOrigin";

describe("protectedSkipOf", () => {
  it("names the session the backend spared", () => {
    const text = "session term-4-a is protected and was not closed by batch_close; close its tab directly to stop it";
    expect(protectedSkipOf(text)).toBe("term-4-a");
    expect(protectedSkipOf(new Error(text))).toBe("term-4-a");
  });
  it("is null for every other failure", () => {
    expect(protectedSkipOf("native unavailable")).toBeNull();
    expect(protectedSkipOf(undefined)).toBeNull();
    expect(protectedSkipOf({ message: "x" })).toBeNull();
  });
});
