import { describe, expect, it } from "vitest";
import { pickLanguageBarStats, type LanguageStat } from "./barStats";

function stat(
  language: string,
  percentage: number,
  category = "programming",
): LanguageStat {
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
