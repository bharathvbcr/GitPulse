import { describe, expect, it } from "vitest";
import { formatDate, formatRelativeTime, shortHash } from "./format";

describe("formatRelativeTime extra edges", () => {
  it("is deterministic when `now` is supplied", () => {
    const ts = 1_700_000_000;
    expect(formatRelativeTime(ts, 1_700_000_000)).toBe("just now");
    expect(formatRelativeTime(ts, 1_700_000_000)).toBe(formatRelativeTime(ts, 1_700_000_000));
    // Wall clock must not leak in when `now` is explicit.
    expect(formatRelativeTime(ts, 1_700_003_600)).toBe("1h ago");
    expect(formatRelativeTime(ts, 1_700_000_060)).toBe("1m ago");
  });

  it("returns empty for NaN timestamps instead of throwing", () => {
    // NaN is falsy, so the !timestampSec guard catches it before any math.
    expect(formatRelativeTime(Number.NaN, 1_700_000_000)).toBe("");
    expect(() => formatRelativeTime(Number.NaN)).not.toThrow();
  });
});

describe("formatDate extra edges", () => {
  it("returns empty for NaN and negative-zero epochs", () => {
    expect(formatDate(Number.NaN)).toBe("");
    expect(formatDate(-0)).toBe("");
  });

  it("renders a real date string just above the falsy boundary", () => {
    expect(typeof formatDate(1)).toBe("string");
    expect(formatDate(1).length).toBeGreaterThan(0);
  });
});

describe("shortHash extra edges", () => {
  it("returns empty at len 0", () => {
    expect(shortHash("0123456789", 0)).toBe("");
  });

  it("pins negative-length slice semantics (truncates from the end)", () => {
    // Defensible-but-surprising: hash.slice(0, -3) drops the LAST three
    // chars rather than guarding. Callers pass positive literals; pinned as
    // contract so a future guard change is deliberate.
    expect(shortHash("abcdefgh", -3)).toBe("abcde");
    expect(shortHash("abcdefgh", -100)).toBe("");
  });

  it("passes through when len exceeds the hash length", () => {
    expect(shortHash("abc", 10)).toBe("abc");
  });

  it("throws TypeError for non-string truthy input (no typeof guard)", () => {
    // Pinned actual behavior: numbers/objects reach .slice and explode.
    const shortHashAny = shortHash as unknown as (hash: unknown, len?: number) => string;
    expect(() => shortHashAny(42)).toThrow(TypeError);
    expect(() => shortHashAny({})).toThrow(TypeError);
    expect(() => shortHashAny(true)).toThrow(TypeError);
  });

  it("fails soft for non-string FALSY input", () => {
    const shortHashAny = shortHash as unknown as (hash: unknown) => string;
    expect(shortHashAny(NaN)).toBe("");
    expect(shortHashAny(false)).toBe("");
  });
});
