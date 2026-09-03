import { describe, expect, it } from "vitest";
import { generatePulseSvgCard, type ExportCardOptions } from "./exportCard";

/** A fully measured card. Individual tests null out one field at a time. */
const MEASURED: ExportCardOptions = {
  repoName: "my-cool-project",
  totalCommits: 1420,
  activeDays: 214,
  windowStart: "2024-03-12",
  windowEnd: "2026-09-02",
  totalLoc: 86204,
  busFactor: 3,
  halfLifeDays: 95,
  conventionalPct: 88,
  signedPct: 75,
  generatedDate: "2026-09-03",
};

function card(overrides: Partial<ExportCardOptions> = {}): string {
  return generatePulseSvgCard({ ...MEASURED, ...overrides });
}

/** Text content of every element carrying `className`, in document order. */
function textOf(svg: string, className: string): string[] {
  const matches = svg.matchAll(new RegExp(`class="${className}"[^>]*>(.*?)</text>`, "g"));
  return [...matches].map((m) => m[1].replace(/<[^>]*>/g, ""));
}

describe("generatePulseSvgCard structure", () => {
  it("produces a standalone SVG element", () => {
    const svg = card();
    expect(svg.startsWith("<svg")).toBe(true);
    expect(svg.endsWith("</svg>")).toBe(true);
    expect(svg).toContain('xmlns="http://www.w3.org/2000/svg"');
    expect(svg).toContain('viewBox="0 0 820 504"');
  });

  it("carries an accessible name and a full text description", () => {
    const svg = card();
    expect(svg).toContain('role="img"');
    expect(svg).toContain("<title>my-cool-project repository pulse</title>");
    const desc = /<desc>(.*?)<\/desc>/s.exec(svg)?.[1] ?? "";
    for (const label of [
      "COMMITS",
      "CONVENTIONAL COMMITS",
      "SIGNED COMMITS",
      "LINES OF CODE",
      "CODE HALF-LIFE",
      "BUS FACTOR",
    ]) {
      expect(desc).toContain(label);
    }
    expect(desc).toContain("86,204");
  });

  it("emits pure ASCII so no renderer has to guess an encoding", () => {
    const svg = card({ repoName: "café-résumé", authorScope: "Ünal Öztürk" });
    // eslint-disable-next-line no-control-regex
    expect(/^[\x20-\x7E\n]*$/.test(svg)).toBe(true);
    expect(svg).toContain("&#8212;"); // em dash, as a character reference
  });

  it("scopes its stylesheet so inlining it cannot restyle the host document", () => {
    const style = /<style>(.*?)<\/style>/s.exec(card())?.[1] ?? "";
    expect(style.length).toBeGreaterThan(0);
    const selectors = [...style.matchAll(/^\s*([^{]+)\{/gm)].map((m) => m[1].trim());
    expect(selectors.length).toBeGreaterThan(0);
    for (const selector of selectors) {
      expect(selector.startsWith("svg.gp-pulse-card")).toBe(true);
    }
  });

  it("avoids the CSS properties standalone rasterisers ignore", () => {
    const svg = card();
    const style = /<style>(.*?)<\/style>/s.exec(svg)?.[1] ?? "";
    expect(style).not.toContain("text-transform");
    expect(style).not.toMatch(/\brx\s*:/);
    // Corner radii are attributes, which every renderer honours.
    expect(svg).toContain('rx="10"');
  });

  it("escapes repository names instead of interpolating them as markup", () => {
    const svg = card({ repoName: '<script>alert("x")</script>' });
    expect(svg).not.toContain("<script>");
    expect(svg).toContain("&lt;script&gt;");
  });

  it("renders every tile label exactly once", () => {
    const labels = textOf(card(), "gp-label");
    expect(labels).toEqual([
      "COMMITS",
      "CONVENTIONAL COMMITS",
      "SIGNED COMMITS",
      "LINES OF CODE",
      "CODE HALF-LIFE",
      "BUS FACTOR",
    ]);
  });

  it("groups the commit-log tiles apart from the working-tree tiles", () => {
    const groups = textOf(card(), "gp-group");
    expect(groups[0]).toBe("FROM THE COMMIT LOG");
    expect(groups[1]).toContain("GIT BLAME AND WORKING TREE");
  });
});

describe("generatePulseSvgCard honesty", () => {
  const cases: ReadonlyArray<{
    field: keyof ExportCardOptions;
    label: string;
    reason: string;
  }> = [
    { field: "totalLoc", label: "LINES OF CODE", reason: "not a count of zero" },
    { field: "busFactor", label: "BUS FACTOR", reason: "not a bus factor of zero" },
    { field: "halfLifeDays", label: "CODE HALF-LIFE", reason: "not an age of zero" },
    { field: "conventionalPct", label: "CONVENTIONAL COMMITS", reason: "nothing to measure" },
    { field: "signedPct", label: "SIGNED COMMITS", reason: "nothing to measure" },
  ];

  for (const { field, label, reason } of cases) {
    it(`renders ${label} as an em dash, never a zero, when it was not measured`, () => {
      const svg = card({ [field]: null } as Partial<ExportCardOptions>);
      const values = textOf(svg, "gp-value");
      const index = textOf(svg, "gp-label").indexOf(label);
      expect(index).toBeGreaterThanOrEqual(0);
      expect(values[index]).toBe("&#8212;");
      expect(values[index]).not.toBe("0");
      expect(svg.replace(/\s+/g, " ")).toContain(reason);
    });
  }

  it("omits the active-day sentence rather than inventing a day count", () => {
    const meaning = textOf(card({ activeDays: null }), "gp-mean").join(" ");
    expect(meaning).toContain("Commits in scope, merges included.");
    expect(meaning).not.toContain("active day");
    expect(meaning).not.toContain("0 active");
  });

  it("dims an unmeasured value so it does not read as a measurement", () => {
    const measuredFill = /class="gp-value"[^>]*fill="([^"]+)"/.exec(card())?.[1];
    const unmeasuredFill = /class="gp-value"[^>]*fill="([^"]+)"[^>]*>&#8212;/.exec(
      card({ totalLoc: null }),
    )?.[1];
    expect(unmeasuredFill).toBeDefined();
    expect(unmeasuredFill).not.toBe(measuredFill);
  });

  it("marks an unmeasured metric with an UNSCANNED chip", () => {
    expect(card({ busFactor: null })).toContain("UNSCANNED");
    expect(card()).not.toContain("UNSCANNED");
  });

  it("states the honesty rule in the footer", () => {
    expect(card().replace(/\s+/g, " ")).toContain("scan did not run shows &#8212;, never 0");
  });

  it("says on the commits tile itself that the scan was capped", () => {
    const svg = card({ truncated: true });
    expect(svg).toContain("CAPPED");
    expect(textOf(svg, "gp-mean").join(" ")).toContain("Older history was not scanned.");
    expect(textOf(svg, "gp-scope")[0]).toContain("Most recent 1,420 commits");
  });

  it("says on the lines-of-code tile that a capped language scan is a floor", () => {
    const svg = card({ locPartial: true });
    expect(svg).toContain("PARTIAL");
    expect(textOf(svg, "gp-mean").join(" ")).toContain("this is a floor");
  });

  it("prefers the partial-blame warning over a reassuring bus factor rating", () => {
    expect(card({ busFactor: 5, blamePartial: true })).not.toContain("HEALTHY");
    expect(card({ busFactor: 5, blamePartial: true })).toContain("PARTIAL");
  });
});

describe("generatePulseSvgCard readability", () => {
  it("defines each metric in plain language on its own tile", () => {
    const meaning = textOf(card(), "gp-mean").join(" ");
    expect(meaning).toContain("merges included");
    expect(meaning).toContain("Blank lines and comments excluded");
    expect(meaning).toContain("Half the live code was last touched within the past 95 days.");
    expect(meaning).toContain("own half the surviving lines. Higher is safer.");
    expect(meaning).toContain("Conventional Commits");
    expect(meaning).toContain("good signature");
  });

  it("keeps every wrapped line inside the tile", () => {
    const svg = card({
      repoName: "x".repeat(200),
      totalCommits: 12_345_678,
      activeDays: 9_876_543,
      busFactor: 1234,
      truncated: true,
    });
    for (const line of textOf(svg, "gp-mean")) {
      expect(line.length).toBeLessThanOrEqual(34);
    }
  });

  it("ellipsises copy that cannot fit rather than overflowing the tile", () => {
    const svg = card({ activeDays: 999_999_999_999, truncated: true });
    const lines = textOf(svg, "gp-mean");
    expect(lines.some((line) => line.endsWith("&#8230;"))).toBe(true);
    for (const line of lines) {
      expect(line.replace(/&#\d+;/g, "*").length).toBeLessThanOrEqual(34);
    }
  });

  it("never lets a tile spill past three explanation lines", () => {
    const svg = card({ truncated: true, activeDays: 1_000_000 });
    const perTile = svg.split('<g transform="translate(').slice(1);
    for (const tile of perTile) {
      expect((tile.match(/class="gp-mean"/g) ?? []).length).toBeLessThanOrEqual(3);
    }
  });

  it("truncates a repository name rather than letting it run off the card", () => {
    const name = textOf(card({ repoName: "n".repeat(120) }), "gp-name")[0];
    // One character reference stands for one glyph when measuring the fit.
    expect(name.replace(/&#\d+;/g, "*").length).toBeLessThanOrEqual(44);
    expect(name.endsWith("&#8230;")).toBe(true);
  });

  it("shrinks the headline so long counts stay inside their tile", () => {
    const small = /class="gp-value" font-size="(\d+)"/.exec(card({ totalCommits: 42 }))?.[1];
    const large = /class="gp-value" font-size="(\d+)"/.exec(
      card({ totalCommits: 123_456_789 }),
    )?.[1];
    expect(Number(small)).toBe(28);
    expect(Number(large)).toBeLessThan(28);
  });

  it("agrees with the app on the bus factor rating", () => {
    expect(card({ busFactor: 1 })).toContain("CRITICAL");
    expect(card({ busFactor: 2 })).toContain("MODERATE");
    expect(card({ busFactor: 3 })).toContain("HEALTHY");
  });

  it("uses singular units where the number is one", () => {
    const svg = card({ busFactor: 1, halfLifeDays: 1, totalCommits: 1, activeDays: 1 });
    const units = textOf(svg, "gp-unit");
    expect(units).toContain("contributor");
    expect(units).toContain("day");
    expect(textOf(svg, "gp-scope")[0]).toContain("1 commit ");
    expect(textOf(svg, "gp-mean").join(" ")).toContain("Spread over 1 active day.");
    expect(textOf(svg, "gp-mean").join(" ")).toContain("within the past 1 day.");
    expect(textOf(svg, "gp-mean").join(" ")).not.toContain("1 days");
  });

  it("groups thousands the same way regardless of host locale", () => {
    expect(textOf(card(), "gp-value")).toContain("86,204");
  });

  it("clamps a percentage into the range a percentage can occupy", () => {
    expect(textOf(card({ signedPct: 140 }), "gp-value")).toContain("100%");
    expect(textOf(card({ signedPct: -5 }), "gp-value")).toContain("0%");
  });
});

describe("generatePulseSvgCard scope line", () => {
  it("names the window, its dates and the author scope", () => {
    const scope = textOf(card(), "gp-scope")[0];
    expect(scope).toContain("1,420 commits");
    expect(scope).toContain("12 Mar 2024");
    expect(scope).toContain("2 Sep 2026");
    expect(scope).toContain("All contributors");
  });

  it("names the author when the window was narrowed to one", () => {
    expect(textOf(card({ authorScope: "Ada Lovelace" }), "gp-scope")[0]).toContain(
      "Author: Ada Lovelace",
    );
  });

  it("collapses a single-day window to one date", () => {
    const scope = textOf(card({ windowStart: "2026-09-02", windowEnd: "2026-09-02" }), "gp-scope")[0];
    expect(scope).toContain("2 Sep 2026");
    expect(scope).not.toContain("&#8211;"); // no en dash range
  });

  it("omits the range when no commit dates are known", () => {
    const scope = textOf(card({ windowStart: null, windowEnd: null }), "gp-scope")[0];
    expect(scope).toBe("1,420 commits  &#183;  All contributors");
  });

  it("reads dates as written rather than shifting them through UTC", () => {
    const scope = textOf(card({ windowStart: "2026-01-01", windowEnd: "2026-01-01" }), "gp-scope")[0];
    expect(scope).toContain("1 Jan 2026");
    expect(scope).not.toContain("Dec 2025");
  });

  it("ignores a malformed date instead of printing NaN", () => {
    const scope = textOf(card({ windowStart: "not-a-date", windowEnd: "2026-13-45" }), "gp-scope")[0];
    expect(scope).not.toContain("NaN");
    expect(scope).not.toContain("undefined");
  });

  it("stamps the generation date, defaulting to today", () => {
    expect(textOf(card(), "gp-stamp")[0]).toBe("Generated 3 Sep 2026");
    const today = new Date().toISOString().slice(0, 10);
    const { generatedDate: _omitted, ...rest } = MEASURED;
    expect(generatePulseSvgCard(rest)).toContain(`Generated ${Number(today.slice(8, 10))} `);
  });

  it("falls back to the raw stamp when the caller supplies an unparseable one", () => {
    expect(textOf(card({ generatedDate: "yesterday" }), "gp-stamp")[0]).toBe("Generated yesterday");
  });

  it("falls back to a placeholder name for a blank repository name", () => {
    expect(textOf(card({ repoName: "   " }), "gp-name")[0]).toBe("repository");
  });
});
