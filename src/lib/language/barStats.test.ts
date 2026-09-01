import { describe, expect, it } from "vitest";
import { pickLanguageBarStats, type RepoLanguageStat } from "./barStats";

function stat(
  language: string,
  percentage: number,
  category = "programming",
): RepoLanguageStat {
  return { language, color_hex: "#000", category, percentage, code_lines: percentage, file_count: 1 };
}

describe("pickLanguageBarStats", () => {
  it("keeps Rust among programming languages even when data langs dominate raw order", () => {
    const stats = [
      stat("JSON", 40, "data"),
      stat("Markdown", 20, "prose"),
      stat("TOML", 10, "data"),
      stat("YAML", 8, "data"),
      stat("CSS", 7, "markup"),
      stat("HTML", 6, "markup"),
      stat("Svelte", 5),
      stat("TypeScript", 3),
      stat("Rust", 1),
    ];
    const picked = pickLanguageBarStats(stats, 6);
    expect(picked.map((s) => s.language)).toContain("Rust");
    expect(picked.map((s) => s.language)).toContain("Other");
    expect(picked.find((s) => s.language === "Rust")?.category).toBe("programming");
  });

  it("does not invent Other when everything fits", () => {
    const stats = [stat("Rust", 70), stat("Svelte", 30)];
    expect(pickLanguageBarStats(stats).map((s) => s.language)).toEqual(["Rust", "Svelte"]);
  });

  it("lists folded languages on Other and omits the field when there is no fold", () => {
    const stats = [
      stat("JSON", 40, "data"),
      stat("Markdown", 20, "prose"),
      stat("TOML", 10, "data"),
      stat("YAML", 8, "data"),
      stat("CSS", 7, "markup"),
      stat("HTML", 6, "markup"),
      stat("Svelte", 5),
      stat("TypeScript", 3),
      stat("Rust", 1),
    ];
    const picked = pickLanguageBarStats(stats, 6);
    const other = picked.find((s) => s.language === "Other");
    expect(other).toBeDefined();
    expect(new Set(other?.other_languages)).toEqual(new Set(["YAML", "CSS", "HTML"]));
    for (const entry of picked) {
      if (entry.language !== "Other") expect(entry.other_languages).toBeUndefined();
    }

    const noFold = pickLanguageBarStats([stat("Rust", 70), stat("Svelte", 30)]);
    expect(noFold.every((s) => s.other_languages === undefined)).toBe(true);
  });
});

describe("pickLanguageBarStats hardening", () => {
  it("clamps negative maxShown to zero and folds everything into Other", () => {
    const picked = pickLanguageBarStats([stat("Rust", 60), stat("JSON", 40, "data")], -3);
    expect(picked.map((s) => s.language)).toEqual(["Other"]);
    expect(picked[0].percentage).toBe(100);
  });

  it("treats maxShown of 0 as show-nothing", () => {
    const picked = pickLanguageBarStats([stat("Rust", 100)], 0);
    expect(picked.map((s) => s.language)).toEqual(["Other"]);
  });

  it("floors fractional maxShown", () => {
    const picked = pickLanguageBarStats(
      [stat("Rust", 40), stat("Go", 30), stat("JSON", 30, "data")],
      2.9,
    );
    expect(picked.map((s) => s.language)).toEqual(["Rust", "Go", "Other"]);
  });

  it("treats non-finite maxShown as zero", () => {
    const picked = pickLanguageBarStats([stat("Rust", 100)], Number.NaN);
    expect(picked.map((s) => s.language)).toEqual(["Other"]);
  });

  it("drops NaN percentages from the Other rollup instead of poisoning widths", () => {
    const poisoned = stat("JSON", Number.NaN, "data");
    const picked = pickLanguageBarStats([poisoned, stat("Rust", 50)], 1);
    const other = picked.find((s) => s.language === "Other");
    expect(other?.percentage ?? 0).not.toBeNaN();
    for (const entry of picked) {
      expect(Number.isFinite(entry.percentage)).toBe(true);
    }
  });

  it("aggregates duplicate language names before capping", () => {
    const picked = pickLanguageBarStats(
      [stat("Rust", 30), stat("Rust", 20), stat("JSON", 25, "data"), stat("Markdown", 15, "prose")],
      6,
    );
    const rustRows = picked.filter((s) => s.language === "Rust");
    expect(rustRows).toHaveLength(1);
    expect(rustRows[0].percentage).toBeCloseTo(50);
    expect(rustRows[0].code_lines).toBe(50);
    expect(rustRows[0].file_count).toBe(2);
  });
});
