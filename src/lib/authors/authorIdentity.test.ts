import { describe, expect, it, beforeEach } from "vitest";
import {
  authorIdentity,
  authorColor,
  resetAuthorIdentityCache,
  authorIdentityCacheSize,
} from "./authorIdentity";

describe("authorIdentity", () => {
  beforeEach(() => resetAuthorIdentityCache());

  it("derives two-token initials from display name", () => {
    expect(authorIdentity("Ada Lovelace", "ada@example.com").initials).toBe("AL");
  });

  it("derives one initial for single-token names", () => {
    expect(authorIdentity("ada", "ada@example.com").initials).toBe("A");
  });

  it("prefers email over name for the hue key so renamed authors keep their colour", () => {
    const before = authorIdentity("Grace B. Hopper", "grace@navy.mil");
    const after = authorIdentity("Rear Admiral Hopper", "grace@navy.mil");
    expect(after.hue).toBe(before.hue);
    expect(after.initials).toBe("RH");
  });

  it("falls back to the name when the email is missing", () => {
    const byName = authorIdentity("Ada Lovelace", "");
    const again = authorIdentity("Ada Lovelace", null);
    expect(again).toEqual(byName);
  });

  it("uses the email local part when only an email is available", () => {
    expect(authorIdentity("", "ada.lovelace@example.com").initials).toBe("AL");
  });

  it("returns ? for empty, whitespace and non-string inputs", () => {
    expect(authorIdentity("", "").initials).toBe("?");
    expect(authorIdentity(null, undefined).initials).toBe("?");
    expect(authorIdentity("   ", "   ").initials).toBe("?");
    // Punctuation-only tokens carry no identity.
    expect(authorIdentity("- . -", "..@..").initials).toBe("?");
  });

  it("never produces NaN or out-of-range hues for adversarial keys", () => {
    const cases = [
      ["", ""],
      [null, null],
      ["\u0000\u0000", "\ud83d\ude00"],
      ["x".repeat(10_000), "y".repeat(10_000)],
      ["🚀", "rocket@example.com"],
      ["مرحبا", "rtl@example.com"],
      ["日本語", "cjk@example.com"],
    ] as const;
    for (const [name, email] of cases) {
      const id = authorIdentity(name as string | null, email as string | null);
      expect(Number.isFinite(id.hue)).toBe(true);
      expect(id.hue).toBeGreaterThanOrEqual(0);
      expect(id.hue).toBeLessThan(360);
      expect(id.initials.length).toBeLessThanOrEqual(2);
    }
  });

  it("handles lone surrogates without throwing", () => {
    const id = authorIdentity("\uD800", "surrogate@example.com");
    expect(typeof id.initials).toBe("string");
    expect(Number.isFinite(id.hue)).toBe(true);
  });

  it("is deterministic across calls with equal inputs", () => {
    const a = authorIdentity("Linus Torvalds", "torvalds@kernel.org");
    const b = authorIdentity("Linus Torvalds", "torvalds@kernel.org");
    expect(a).toEqual(b);
  });

  it("distinguishes different authors sharing a display name", () => {
    const a = authorIdentity("Pat", "pat@a.example");
    const b = authorIdentity("Pat", "pat@b.example");
    expect(a.key).not.toBe(b.key);
    // Golden-angle spread makes small hash deltas land far apart in hue.
    expect(Math.abs(a.hue - b.hue)).toBeGreaterThan(1);
  });

  it("caps the identity cache and keeps serving correct results past the cap", () => {
    for (let i = 0; i < 600; i++) {
      authorIdentity(`author-${i}`, `a${i}@example.com`);
    }
    expect(authorIdentityCacheSize()).toBeLessThanOrEqual(512);
    expect(authorIdentity("author-599", "a599@example.com").key).toBe("a599@example.com");
  });
});

describe("authorColor", () => {
  it("wraps out-of-range hues into the colour wheel", () => {
    expect(authorColor(-30)).toBe(authorColor(330));
    expect(authorColor(400)).toBe(authorColor(40));
  });

  it("returns a safe colour for non-finite input instead of poisoning CSS", () => {
    expect(authorColor(Number.NaN)).toMatch(/^hsl\(\d+,/);
    expect(authorColor(Number.POSITIVE_INFINITY)).toMatch(/^hsl\(\d+,/);
  });
});
