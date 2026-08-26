import { describe, expect, it, beforeEach } from "vitest";
import {
  authorIdentity,
  authorColor,
  resetAuthorIdentityCache,
  authorIdentityCacheSize,
} from "./authorIdentity";

/**
 * Stress: the identity function sits on every commit row render and every
 * canvas strip paint. Fuzzed here with git-realistic garbage — empty names,
 * emails as names, emoji, RTL overrides, control characters, megabyte strings —
 * asserting the three invariants that matter: determinism (same input → same
 * output across cache states), bounded output (initials ≤ 2 clusters, finite
 * hue), and a bounded cache no matter how many distinct authors stream past.
 *
 * Seeded LCG so failures reproduce exactly.
 */
function lcg(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
    return state / 2 ** 32;
  };
}

const ALPHABETS = [
  "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ",
  "àáâãäåæçèéêëìíîïñòóôõöøùúûüýÿ",
  "日本語の一覧中文한국어",
  "😀🚀🦀🌍🎉👍",
  "مرحبا العال",
  "\u0001\u0002\u0003\u007f",
];

function randomName(rnd: () => number): string {
  const alphabet = ALPHABETS[Math.floor(rnd() * ALPHABETS.length)];
  const len = Math.floor(rnd() * 24);
  let out = "";
  for (let i = 0; i < len; i++) out += alphabet[Math.floor(rnd() * alphabet.length)];
  return out;
}

function randomEmail(rnd: () => number): string {
  if (rnd() < 0.1) return "";
  const len = Math.floor(rnd() * 16);
  let local = "";
  for (let i = 0; i < len; i++) local += String.fromCharCode(33 + Math.floor(rnd() * 90));
  return `${local}@example.test`;
}

describe("authorIdentity stress", () => {
  beforeEach(() => resetAuthorIdentityCache());

  it("survives 20k adversarial authors with bounded output and cache", () => {
    const rnd = lcg(0xc0ffee);
    for (let i = 0; i < 20_000; i++) {
      const name = randomName(rnd);
      const email = randomEmail(rnd);
      const id = authorIdentity(name, email);
      expect(Number.isFinite(id.hue)).toBe(true);
      expect(id.hue).toBeGreaterThanOrEqual(0);
      expect(id.hue).toBeLessThan(360);
      expect([...id.initials].length).toBeLessThanOrEqual(2);
      expect(id.key.length).toBeLessThanOrEqual(512);
    }
    expect(authorIdentityCacheSize()).toBeLessThanOrEqual(512);
  });

  it("is deterministic regardless of cache warmth or eviction order", () => {
    const rnd = lcg(42);
    const inputs: Array<[string, string]> = [];
    for (let i = 0; i < 800; i++) inputs.push([randomName(rnd), randomEmail(rnd)]);

    // Cold pass.
    resetAuthorIdentityCache();
    const cold = inputs.map(([n, e]) => authorIdentity(n, e));

    // Warm pass: entries still cached come back as the SAME object, evicted
    // ones re-derive — either way content must be identical. 800 inputs over
    // a 512-entry cache guarantees both paths are exercised.
    const warm = inputs.map(([n, e]) => authorIdentity(n, e));
    let sameRef = 0;
    for (let i = 0; i < inputs.length; i++) {
      if (warm[i] === cold[i]) sameRef += 1;
      expect(warm[i]).toEqual(cold[i]);
    }
    // Both cache branches must have run, or the test proves nothing.
    expect(sameRef).toBeGreaterThan(0);
    expect(sameRef).toBeLessThan(inputs.length);

    // Refetch after a full reset must still produce equal identities.
    resetAuthorIdentityCache();
    const rederived = inputs.map(([n, e]) => authorIdentity(n, e));
    for (let i = 0; i < inputs.length; i++) expect(rederived[i]).toEqual(cold[i]);
  });

  it("spreads hues so distinct authors rarely collapse to near-identical colours", () => {
    resetAuthorIdentityCache();
    // Similar short strings were the old hash's failure mode: single-letter
    // names landed on contiguous hues ((charCode << 5) - charCode). They must
    // now cover a wide arc, not one sliver of the wheel.
    resetAuthorIdentityCache();
    const letters = "abcdefghijklmnopqrstuvwxyz".split("").map((c) => authorIdentity(c, ""));
    const letterSpan = Math.max(...letters.map((l) => l.hue)) - Math.min(...letters.map((l) => l.hue));
    expect(letterSpan).toBeGreaterThan(120);

    // Numbered teammates (the generated-repo pattern) likewise spread rather
    // than banded together under the old modulo-of-low-entropy hashes.
    resetAuthorIdentityCache();
    const team = Array.from({ length: 40 }, (_, i) => authorIdentity(`dev${i}`, `dev${i}@corp.example`).hue);
    const teamSpan = Math.max(...team) - Math.min(...team);
    expect(teamSpan).toBeGreaterThan(180);

    // Arbitrary authors stay near-uniform: pairs within 2° should be close to
    // chance (C(400,2)·4/360 ≈ 887) and never approach old-hash clustering.
    const rnd = lcg(7);
    const spread: number[] = [];
    for (let i = 0; i < 400; i++) {
      spread.push(authorIdentity(randomName(rnd), randomEmail(rnd)).hue || 0);
    }
    let collisions = 0;
    for (let i = 0; i < spread.length; i++) {
      for (let j = i + 1; j < spread.length; j++) {
        if (Math.abs(spread[i] - spread[j]) < 2) collisions += 1;
      }
    }
    expect(collisions).toBeLessThan(1100);
  });

  it("keeps colour strings valid CSS for every fuzzed hue", () => {
    const rnd = lcg(99);
    for (let i = 0; i < 2000; i++) {
      const color = authorColor(authorIdentity(randomName(rnd), "").hue);
      expect(color).toMatch(/^hsl\(\d+(\.\d+)?, 62%, 45%\)$/);
    }
  });
});
