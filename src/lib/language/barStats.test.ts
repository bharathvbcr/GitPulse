import { describe, expect, it } from "vitest";
import {
  describeLanguageMix,
  pickLanguageBarStats,
  type LanguageMixInput,
  type RepoLanguageStat,
} from "./barStats";

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

  it("orders and sorts languages per percentage descending", () => {
    const stats = [
      stat("HTML", 40, "markup"),
      stat("TypeScript", 35),
      stat("CSS", 15, "markup"),
      stat("Rust", 10),
    ];
    const picked = pickLanguageBarStats(stats, 6);
    expect(picked.map((s) => s.language)).toEqual(["HTML", "TypeScript", "CSS", "Rust"]);
    expect(picked.map((s) => s.percentage)).toEqual([40, 35, 15, 10]);
  });

  it("sorts a mixed programming and data mix by percentage with Other last", () => {
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
    expect(picked.map((s) => s.language)).toEqual([
      "JSON",
      "Markdown",
      "TOML",
      "Svelte",
      "TypeScript",
      "Rust",
      "Other",
    ]);
    expect(picked.map((s) => s.percentage)).toEqual([40, 20, 10, 5, 3, 1, 21]);
    const named = picked.filter((s) => s.language !== "Other");
    for (let i = 1; i < named.length; i++) {
      expect(named[i - 1].percentage).toBeGreaterThanOrEqual(named[i].percentage);
    }
    expect(picked.at(-1)?.language).toBe("Other");
  });

  it("breaks equal percentages alphabetically by language name", () => {
    const picked = pickLanguageBarStats(
      [stat("Rust", 20), stat("Go", 20), stat("CSS", 20, "markup")],
      6,
    );
    expect(picked.map((s) => s.language)).toEqual(["CSS", "Go", "Rust"]);
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
    expect(picked.map((s) => s.language)).toEqual(["Rust", "JSON", "Markdown"]);
    expect(picked.map((s) => s.percentage)).toEqual([50, 25, 15]);
  });

  it("places a merged language by its summed percentage", () => {
    const picked = pickLanguageBarStats(
      [
        stat("JSON", 25, "data"),
        stat("Rust", 30),
        stat("JSON", 25, "data"),
        stat("Markdown", 15, "prose"),
      ],
      6,
    );
    expect(picked.map((s) => s.language)).toEqual(["JSON", "Rust", "Markdown"]);
    expect(picked.map((s) => s.percentage)).toEqual([50, 30, 15]);
    expect(picked[0].code_lines).toBe(50);
    expect(picked[0].file_count).toBe(2);
  });
});

function snapshot(overrides: Partial<LanguageMixInput> = {}): LanguageMixInput {
  return {
    value: {
      stats: [stat("Rust", 70), stat("TypeScript", 30)],
      truncated: false,
      scanned_files: 120,
      candidate_files: 120,
    },
    state: "ready",
    stale: null,
    ...overrides,
  };
}

describe("describeLanguageMix", () => {
  it("leads with the dominant programming language", () => {
    const mix = describeLanguageMix(snapshot());
    expect(mix.dominant?.language).toBe("Rust");
    expect(mix.partial).toBe(false);
    expect(mix.partialNotice).toBeNull();
    expect(mix.failed).toBe(false);
  });

  it("names the highest-percentage language as dominant, including markup", () => {
    const mix = describeLanguageMix(
      snapshot({
        value: {
          stats: [
            stat("HTML", 40, "markup"),
            stat("TypeScript", 35),
            stat("CSS", 15, "markup"),
            stat("Rust", 10),
          ],
          truncated: false,
          scanned_files: 40,
          candidate_files: 40,
        },
      }),
    );
    expect(mix.dominant?.language).toBe("HTML");
    expect(mix.dominant?.percentage).toBe(40);
    expect(mix.stats.map((s) => s.language)).toEqual(["HTML", "TypeScript", "CSS", "Rust"]);
  });

  it("never leads with the Other aggregate when a real language is present", () => {
    // "Other" sorts last out of pickLanguageBarStats, but a one-language
    // repository past the cap can put it first; leading with it would name
    // the repository after a bucket.
    const mix = describeLanguageMix(
      snapshot({
        value: {
          stats: [stat("JSON", 60, "data"), stat("Rust", 40)],
          truncated: false,
          scanned_files: 10,
          candidate_files: 10,
        },
      }),
    );
    expect(mix.dominant?.language).not.toBe("Other");
    expect(mix.dominant?.language).toBe("JSON");
    expect(mix.dominant?.percentage).toBe(60);
  });

  it("marks a capped scan partial and names what it counted", () => {
    const mix = describeLanguageMix(
      snapshot({
        value: {
          stats: [stat("Rust", 70)],
          truncated: true,
          scanned_files: 800,
          candidate_files: 10_000,
        },
      }),
    );
    expect(mix.partial).toBe(true);
    expect(mix.partialNotice).toBe("Partial scan: 800 of 10000 files counted");
  });

  it("marks a stale reading partial even when the scan itself completed", () => {
    // The other half of the honesty rule: a complete scan of a repository
    // that has since moved is still a floor, and rendering it as a total is
    // the same lie with a later timestamp.
    const mix = describeLanguageMix(snapshot({ stale: "repository-changed" }));
    expect(mix.partial).toBe(true);
    expect(mix.partialNotice).toContain("changed since this scan");
  });

  it("prefers the truncation notice when a reading is both capped and stale", () => {
    const mix = describeLanguageMix(
      snapshot({
        value: {
          stats: [stat("Rust", 70)],
          truncated: true,
          scanned_files: 5,
          candidate_files: 50,
        },
        stale: "repository-changed",
      }),
    );
    expect(mix.partialNotice).toContain("Partial scan");
  });

  it("reports a failure only when nothing survives to show", () => {
    expect(describeLanguageMix(snapshot({ value: null, state: "failed" })).failed).toBe(true);
    // A refresh that failed over a previous good value keeps the value and
    // reports it as stale, not as a failure with nothing behind it.
    const kept = describeLanguageMix(snapshot({ state: "failed", stale: "refresh-failed" }));
    expect(kept.failed).toBe(false);
    expect(kept.partial).toBe(true);
  });

  it("renders nothing rather than guessing when no scan has landed", () => {
    const mix = describeLanguageMix(snapshot({ value: null, state: "loading" }));
    expect(mix.stats).toEqual([]);
    expect(mix.dominant).toBeNull();
    expect(mix.partial).toBe(false);
    expect(mix.failed).toBe(false);
  });

  it("survives a malformed report instead of throwing into the status bar", () => {
    const mix = describeLanguageMix({
      value: { stats: null as never, truncated: false, scanned_files: 0, candidate_files: 0 },
      state: "ready",
      stale: null,
    });
    expect(mix.stats).toEqual([]);
    expect(mix.dominant).toBeNull();
  });
});
