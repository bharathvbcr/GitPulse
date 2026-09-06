import { describe, expect, it } from "vitest";
import { foldFleetLanguages, mergeLanguageStats } from "./languages";
import type { FleetLanguageStat } from "./types";

function language(
  name: string,
  lines: number,
  percentage: number,
  category = "programming",
): FleetLanguageStat {
  return {
    language: name,
    color_hex: "#123456",
    category,
    code_lines: lines,
    file_count: 2,
    percentage,
  };
}

describe("foldFleetLanguages", () => {
  it("renders an absent breakdown as absent, never as one grey band", () => {
    // A single placeholder segment reads like a measurement of something.
    expect(foldFleetLanguages([])).toEqual([]);
  });

  it("goes through the same fold the language bar uses", () => {
    // Seven languages against a cap of six: the tail folds into "Other" with
    // the names it swallowed, exactly as `pickLanguageBarStats` does for the
    // status bar. A second fold here would eventually disagree with that one.
    const many = Array.from({ length: 7 }, (_, i) =>
      language(`Lang${i}`, (7 - i) * 100, (7 - i) * 2),
    );
    const folded = foldFleetLanguages(many);
    const other = folded.find((stat) => stat.language === "Other");
    expect(other, "the tail folds rather than being dropped").toBeTruthy();
    expect(other?.other_languages).toContain("Lang6");
  });

  it("honours a smaller cap for the narrow in-row bar", () => {
    const many = Array.from({ length: 6 }, (_, i) => language(`Lang${i}`, 100, 10));
    expect(foldFleetLanguages(many, 2).length).toBeLessThanOrEqual(3);
  });
});

describe("mergeLanguageStats", () => {
  it("recomputes shares from summed lines rather than averaging percentages", () => {
    const merged = mergeLanguageStats([
      [language("Rust", 200, 100)],
      [language("TypeScript", 200_000, 100)],
    ]);
    expect(merged.totalLines).toBe(200_200);
    expect(merged.stats[0].language).toBe("TypeScript");
    expect(merged.stats[0].percentage).toBeCloseTo(99.9, 1);
    expect(merged.counted).toBe(2);
  });

  it("adds the same language across repositories into one row", () => {
    const merged = mergeLanguageStats([
      [language("Go", 100, 50), language("Rust", 100, 50)],
      [language("Go", 300, 100)],
    ]);
    const go = merged.stats.find((stat) => stat.language === "Go");
    expect(go?.code_lines).toBe(400);
    expect(go?.file_count).toBe(4);
    expect(merged.stats[0].language, "largest first").toBe("Go");
  });

  it("does not count a repository that contributed nothing", () => {
    const merged = mergeLanguageStats([[], [language("Go", 10, 100)], []]);
    expect(merged.counted).toBe(1);
  });

  it("survives a non-finite line count instead of poisoning the total", () => {
    // One bad row must stay one bad row. A NaN total renders as a broken bar
    // across the whole fleet, which is a worse lie than the missing count.
    const merged = mergeLanguageStats([
      [language("Go", Number.NaN, 50), language("Rust", 100, 50)],
    ]);
    expect(Number.isFinite(merged.totalLines)).toBe(true);
    expect(merged.totalLines).toBe(100);
    expect(merged.stats.every((stat) => Number.isFinite(stat.percentage))).toBe(true);
  });

  it("yields zero shares rather than dividing by zero on an all-empty mix", () => {
    const merged = mergeLanguageStats([[language("Go", 0, 0)]]);
    expect(merged.totalLines).toBe(0);
    expect(merged.stats[0].percentage).toBe(0);
  });

  it("leaves the rows it was handed untouched", () => {
    const original = language("Go", 100, 50);
    mergeLanguageStats([[original], [language("Go", 100, 50)]]);
    expect(original.code_lines).toBe(100);
    expect(original.percentage).toBe(50);
  });
});
