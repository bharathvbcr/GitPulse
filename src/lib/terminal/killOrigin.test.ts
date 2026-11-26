import { describe, expect, it } from "vitest";
import { ProtectedSkip, protectedSkipOf, settleKill } from "./killOrigin";

describe("protectedSkipOf", () => {
  it("names the session the backend spared, by the typed skip", () => {
    expect(protectedSkipOf(new ProtectedSkip("term-4-a"))).toBe("term-4-a");
  });
  it("never recognises a refusal by its wording", () => {
    const text = "session term-4-a is protected and was not closed by batch_close; close its tab directly to stop it";
    expect(protectedSkipOf(text)).toBeNull();
    expect(protectedSkipOf(new Error(text))).toBeNull();
  });
  it("is null for every other failure", () => {
    expect(protectedSkipOf("native unavailable")).toBeNull();
    expect(protectedSkipOf(undefined)).toBeNull();
    expect(protectedSkipOf({ message: "x" })).toBeNull();
  });
});

describe("settleKill", () => {
  it("returns for a close that ran", () => {
    expect(() => settleKill("term-1", "killed")).not.toThrow();
  });
  it("throws the typed skip for a protected session", () => {
    let caught: unknown;
    try { settleKill("term-1", "skipped_protected"); } catch (error) { caught = error; }
    expect(protectedSkipOf(caught)).toBe("term-1");
  });
});
