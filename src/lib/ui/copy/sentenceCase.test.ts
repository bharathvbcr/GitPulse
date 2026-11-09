import { describe, expect, it } from "vitest";
import { wallClockBudget } from "../../testing/wallClock";
import { AMBIGUOUS_PROPER_NOUNS, KNOWN_ACRONYMS, PROPER_NOUNS, isSentenceCase, toSentenceCase } from "./sentenceCase";

describe("toSentenceCase", () => {
  it("fixes Title Case and shouting-free mixed case", () => {
    expect(toSentenceCase("Create New Branch")).toBe("Create new branch");
    expect(toSentenceCase("Open In Finder")).toBe("Open in Finder");
    expect(toSentenceCase("rebuild code map")).toBe("Rebuild code map");
    expect(toSentenceCase("Pull-Request Review")).toBe("Pull-request review");
  });

  it("leaves a label that is already correct untouched", () => {
    for (const label of ["Try again", "Couldn't check secrets", "Rebuild code map", "Delete branch", "3 files changed", "Open in Finder"]) {
      expect(toSentenceCase(label), label).toBe(label);
      expect(isSentenceCase(label), label).toBe(true);
    }
  });

  it("keeps every listed proper noun, and repairs one typed in the wrong case", () => {
    for (const noun of PROPER_NOUNS) {
      expect(toSentenceCase(`Open ${noun} now`), noun).toBe(`Open ${noun} now`);
      expect(toSentenceCase(`${noun} panel`), noun).toBe(`${noun} panel`);
      if (!AMBIGUOUS_PROPER_NOUNS.includes(noun)) {
        expect(toSentenceCase(`Open ${noun.toLowerCase()} now`), noun).toBe(`Open ${noun} now`);
      }
    }
    expect(toSentenceCase("Sign in to GITHUB")).toBe("Sign in to GitHub");
    expect(toSentenceCase("Manvi Policy")).toBe("Manvi policy");
    expect(toSentenceCase("Run With Claude Code")).toBe("Run with Claude Code");
    expect(toSentenceCase("Open System Settings")).toBe("Open System Settings");
    expect(toSentenceCase("macOS settings")).toBe("macOS settings");
  });

  it("keeps Windows and Git when capitalized but never repairs them from lowercase (ordinary words and commands)", () => {
    expect(AMBIGUOUS_PROPER_NOUNS).toEqual(["Windows", "Git"]);
    expect(toSentenceCase("Close all windows")).toBe("Close all windows");
    expect(toSentenceCase("Run git status")).toBe("Run git status");
    expect(toSentenceCase("Open terminal windows")).toBe("Open terminal windows");
    expect(isSentenceCase("Close all windows")).toBe(true);
    expect(isSentenceCase("Run git status")).toBe(true);
    expect(toSentenceCase("Sign in on Windows")).toBe("Sign in on Windows");
    expect(toSentenceCase("Install Git")).toBe("Install Git");
    // The documented cost: Title Case on these two words is not caught.
    expect(toSentenceCase("Close All Windows")).toBe("Close all Windows");
    // A label that starts with one is still capitalized.
    expect(toSentenceCase("windows settings")).toBe("Windows Settings");
    expect(toSentenceCase("git status")).toBe("Git status");
  });

  it("keeps all-caps acronyms, in any position", () => {
    expect(toSentenceCase("Open PR In Browser")).toBe("Open PR in browser");
    expect(toSentenceCase("CI Status")).toBe("CI status");
    expect(toSentenceCase("Copy URL")).toBe("Copy URL");
    expect(toSentenceCase("Show AI Summary")).toBe("Show AI summary");
    expect(toSentenceCase("API Keys")).toBe("API keys");
    expect(toSentenceCase("Copy Commit ID")).toBe("Copy commit ID");
    expect(toSentenceCase("S3 bucket")).toBe("S3 bucket");
  });

  it("passes an acronym-only label straight through", () => {
    for (const label of ["PR", "CI", "URL", "ID", "AI", "API", "PR CI", "MCP"]) {
      expect(toSentenceCase(label), label).toBe(label);
      expect(isSentenceCase(label), label).toBe(true);
    }
  });

  it("keeps names with an internal capital", () => {
    expect(toSentenceCase("Sign in with OAuth")).toBe("Sign in with OAuth");
    expect(toSentenceCase("Open In iTerm")).toBe("Open in iTerm");
    expect(toSentenceCase("Show PRs")).toBe("Show PRs");
    expect(toSentenceCase("Use TypeScript")).toBe("Use TypeScript");
  });

  it("repairs shouting but keeps known acronyms inside it", () => {
    expect(toSentenceCase("DELETE ALL")).toBe("Delete all");
    expect(toSentenceCase("OPEN PR")).toBe("OPEN PR");
    expect(toSentenceCase("DELETE ALL PR")).toBe("Delete all PR");
    expect(toSentenceCase("REFRESH CI STATUS")).toBe("Refresh CI status");
    expect(toSentenceCase("README")).toBe("README");
    expect(toSentenceCase("Open README")).toBe("Open README");
  });

  it("keeps keyboard shortcuts verbatim", () => {
    expect(toSentenceCase("Open File (⌘K)")).toBe("Open file (⌘K)");
    expect(toSentenceCase("Save All  ⌘⇧S")).toBe("Save all  ⌘⇧S");
    expect(toSentenceCase("Command Palette Ctrl+Shift+P")).toBe("Command palette Ctrl+Shift+P");
    expect(toSentenceCase("Toggle Sidebar (Cmd+B)")).toBe("Toggle sidebar (Cmd+B)");
    expect(toSentenceCase("Cmd+K Opens Palette")).toBe("Cmd+K opens palette");
    expect(isSentenceCase("Open file (⌘K)")).toBe(true);
  });

  it("keeps a lone capital letter that is not the first word, except the article A", () => {
    expect(toSentenceCase("Press K To Search")).toBe("Press K to search");
    expect(toSentenceCase("Create A New Branch")).toBe("Create a new branch");
    expect(toSentenceCase("a new branch")).toBe("A new branch");
    expect(toSentenceCase("I Agree")).toBe("I agree");
  });

  it("handles the empty string, whitespace and punctuation-only labels", () => {
    expect(toSentenceCase("")).toBe("");
    expect(isSentenceCase("")).toBe(true);
    expect(toSentenceCase("   ")).toBe("   ");
    expect(toSentenceCase("…")).toBe("…");
    expect(toSentenceCase("—")).toBe("—");
    expect(toSentenceCase("123")).toBe("123");
  });

  it("preserves surrounding whitespace, punctuation and the ellipsis character", () => {
    expect(toSentenceCase("  Rename Branch…  ")).toBe("  Rename branch…  ");
    expect(toSentenceCase("Fetching…")).toBe("Fetching…");
    expect(toSentenceCase("Loading Branches...")).toBe("Loading branches...");
    expect(toSentenceCase("Delete Branch?")).toBe("Delete branch?");
    expect(toSentenceCase('Rename "Old Name"')).toBe('Rename "old name"');
  });

  it("capitalizes the first letter after leading digits and punctuation", () => {
    expect(toSentenceCase("3 Files Changed")).toBe("3 files changed");
    expect(toSentenceCase("(Optional) Add Note")).toBe("(Optional) add note");
    expect(toSentenceCase("1st Run")).toBe("1st run");
  });

  it("handles unicode: accents, non-Latin scripts and emoji", () => {
    expect(toSentenceCase("Ünïcode Étiquette")).toBe("Ünïcode étiquette");
    expect(toSentenceCase("éclair")).toBe("Éclair");
    expect(toSentenceCase("日本語 テキスト")).toBe("日本語 テキスト");
    expect(toSentenceCase("Run 🚀 Now")).toBe("Run 🚀 now");
    expect(toSentenceCase("ßeta Test")).toBe("ßeta test");
    expect(toSentenceCase("Don’t Show Again")).toBe("Don’t show again");
  });

  it("is idempotent on a spread of inputs", () => {
    const samples = [
      "Create New Branch",
      "DELETE ALL",
      "OPEN PR",
      "Open File (⌘K)",
      "Run With Claude Code",
      "3 Files Changed",
      "Create A New Branch",
      "pull-request review",
      "",
      "   ",
      "Ünïcode Étiquette",
    ];
    for (const sample of samples) {
      const once = toSentenceCase(sample);
      expect(toSentenceCase(once), sample).toBe(once);
    }
  });

  it("handles a very long label in linear time", () => {
    const label = `Open ${"Word ".repeat(40_000)}`;
    const started = Date.now();
    const result = toSentenceCase(label);
    expect(Date.now() - started).toBeLessThan(wallClockBudget(2000));
    expect(result.startsWith("Open word word")).toBe(true);
  });

  it("accepts a caller's noun list and treats it as a replacement", () => {
    expect(toSentenceCase("Open Kingfisher Report", [...PROPER_NOUNS, "Kingfisher"])).toBe("Open Kingfisher report");
    expect(toSentenceCase("Open Kingfisher Report")).toBe("Open kingfisher report");
    expect(toSentenceCase("Open Finder", [])).toBe("Open finder");
  });

  it("rejects a blank proper noun, which would match everywhere", () => {
    expect(() => toSentenceCase("x", ["GitHub", "  "])).toThrow(RangeError);
    expect(() => toSentenceCase("x", [""])).toThrow(RangeError);
  });

  it("does not treat regex metacharacters in a noun as syntax", () => {
    expect(toSentenceCase("Use C++ Compiler", ["C++ Compiler"])).toBe("Use C++ Compiler");
    expect(toSentenceCase("Open (beta) Mode", ["(beta) Mode"])).toBe("Open (beta) Mode");
  });

  it("does not match a multi-word noun inside a larger word", () => {
    expect(toSentenceCase("Reclaude Codex", ["Claude Code"])).toBe("Reclaude codex");
  });
});

describe("isSentenceCase", () => {
  it("rejects Title Case and shouting and accepts sentence case", () => {
    expect(isSentenceCase("Create New Branch")).toBe(false);
    expect(isSentenceCase("DELETE ALL")).toBe(false);
    expect(isSentenceCase("Create new branch")).toBe(true);
    expect(isSentenceCase("create new branch")).toBe(false);
  });

  it("rejects a proper noun typed in the wrong case", () => {
    expect(isSentenceCase("Open github")).toBe(false);
    expect(isSentenceCase("Open GitHub")).toBe(true);
  });

  it("agrees with toSentenceCase for every proper noun and acronym on its own", () => {
    for (const noun of PROPER_NOUNS) expect(isSentenceCase(noun), noun).toBe(true);
    for (const acronym of KNOWN_ACRONYMS) expect(isSentenceCase(acronym), acronym).toBe(true);
  });
});

describe("the exported lists", () => {
  it("name every proper noun the brief lists", () => {
    for (const noun of ["GitPulse", "GitHub", "Manvi", "DevMap", "Git", "macOS", "Windows", "Linux", "Claude", "Codex", "Firebase", "Tauri", "MCP"]) {
      expect(PROPER_NOUNS).toContain(noun);
    }
  });

  it("name every acronym the brief lists", () => {
    for (const acronym of ["PR", "CI", "URL", "ID", "AI", "API"]) expect(KNOWN_ACRONYMS).toContain(acronym);
  });

  it("have no duplicates (case-insensitively) and no blank entries", () => {
    const lowered = PROPER_NOUNS.map((noun) => noun.toLowerCase());
    expect(new Set(lowered).size).toBe(lowered.length);
    expect(PROPER_NOUNS.every((noun) => noun.trim().length > 0)).toBe(true);
    expect(new Set(KNOWN_ACRONYMS).size).toBe(KNOWN_ACRONYMS.length);
  });
});
