import { describe, expect, it } from "vitest";
import { wallClockBudget } from "../../testing/wallClock";
import {
  BANNED_UI_TERMS,
  CROSS_APP_TERMS,
  GLOSSARY,
  RETIRED_VERBS,
  RETIRED_VERB_TERMS,
  VERBS,
  VOICE_TERMS,
  findBannedTerms,
  type BannedTerm,
} from "./vocabulary";

/** All three lists: the rule-14 banned terms, the retired verbs and the voice rules. */
const EVERY_LIST: readonly BannedTerm[] = [...BANNED_UI_TERMS, ...RETIRED_VERB_TERMS, ...VOICE_TERMS];

/**
 * Every banned term must come with text it matches and text it must not.
 * The table is keyed by `term`, and a test below fails when a term is added to
 * the list without an entry here, so a pattern cannot ship unexercised.
 */
const EXAMPLES: Record<string, { hit: string[]; miss: string[] }> = {
  "code graph": { hit: ["Open the code graph", "Code Graph"], miss: ["Open the code map", "codegraph"] },
  "repo map": { hit: ["Rebuild the repo map"], miss: ["Rebuild the code map", "repository map"] },
  codeintel: { hit: ["codeintel is stale", "Codeintel"], miss: ["codeintelStore", "code_intel"] },
  devmap: {
    hit: ["Run devmap build", "devmap is stale", "(devmap)", "Install devmap."],
    miss: ["DevMap", "devmap/status.json", "path/devmap", ".devmap", "devmapper", "devmap-cli", "mydevmap"],
  },
  MANVI: { hit: ["MANVI blocked this", "Ask MANVI."], miss: ["Manvi blocked this", "manvi", "MANVIA"] },
  harness: {
    hit: ["The harness failed", "Harness", "harness.", "harnesses are off"],
    miss: ["harness/foo.html", "/harness", "harness.html", "test-harness", "harnessed", "my_harness"],
  },
  sidecar: { hit: ["Restart the sidecar", "Sidecars"], miss: ["sidecarless", "side car"] },
  posture: { hit: ["Host posture", "Posture"], miss: ["posturing", "composture"] },
  attempt: { hit: ["Attempt 2", "3 attempts"], miss: ["attempted", "attempting", "run 2"] },
  lane: { hit: ["Merge this lane", "Lanes"], miss: ["plane", "planet", "airplane"] },
  "linked checkout": {
    hit: ["Remove the linked checkout", "another checkout"],
    miss: ["Checkout branch", "Check out main", "checkout", "linked worktree"],
  },
  ticket: { hit: ["Open the ticket", "Tickets"], miss: ["Open the task", "ticketing"] },
  repo: { hit: ["Open repo", "Add repos"], miss: ["Open repository", "reporting", "repository"] },
  project: { hit: ["Open project", "Projects"], miss: ["Open repository", "projection", "projected"] },
  Preferences: { hit: ["Preferences", "Open Preferences…"], miss: ["Settings", "your preferences", "preferences"] },
  Options: { hit: ["Options", "Show Options"], miss: ["Settings", "options", "Optional"] },
  "min rung": { hit: ["min rung 2", "Minimum rung", "min rungs"], miss: ["minimum", "min height"] },
  rung: { hit: ["rung 3", "Rungs"], miss: ["strung", "sprung"] },
  "ledger record": { hit: ["3 ledger records", "Ledger record"], miss: ["ledger", "record", "the ledger entry"] },
  "generation N": { hit: ["generation 12", "Generation #4"], miss: ["code generation", "generation", "next generation of tools"] },
  is_fresh: { hit: ["is_fresh is false", "(is_fresh)"], miss: ["is fresh", "isFresh", "is-fresh"] },
  "snake_case identifier": {
    hit: ["failed: in_progress", "min_rung", "a_b_c"],
    miss: ["In progress", "snake-case", "camelCase", "SCREAMING_CASE", "_leading", "trailing_"],
  },
  successfully: { hit: ["Saved successfully", "Successfully merged"], miss: ["success", "Saved"] },
  Oops: { hit: ["Oops, that failed", "oops"], miss: ["hoops", "Loops"] },
  "could not": {
    hit: ["The log could not be read", "Could not save", "COULD NOT RUN"],
    miss: ["The log couldn't be read", "Couldn't save", "could nothing", "discourse could notably"],
  },
  please: { hit: ["Please wait", "Wait, please."], miss: ["pleasure", "displease"] },
  "exclamation mark": {
    hit: ["Done!", "Saved! ", 'Welcome!"', "Nice (really)! Yes"],
    miss: ["a !== b", "{#if !open}", "a != b", "!important", "Done.", "border-red-500/60! text-x", "'text-red-600!'", "bg-warn!"],
  },
  "British spelling": {
    hit: ["Change the colour", "behaviour", "Customise", "initialising", "synchronisation", "centre"],
    miss: ["Change the color", "behavior", "Customize", "initializing", "synchronization", "center", "colorful"],
  },
  Reload: { hit: ["Reload", "reloading the page", "Reloaded"], miss: ["Refresh", "preload", "unloaded"] },
  Recheck: { hit: ["Recheck", "rechecking"], miss: ["Rescan", "check", "re-check box"] },
  Retry: { hit: ["Retry", "retrying", "Retries left", "retried"], miss: ["Try again", "retryLabel", "entry"] },
  Launch: { hit: ["Launch agent", "launched", "Launching…"], miss: ["Run agent", "Open", "launchpad"] },
  Organise: { hit: ["Organise files", "organising"], miss: ["Organize files", "organizing", "organ"] },
};

function patternFor(term: string): BannedTerm {
  const found = EVERY_LIST.find((banned) => banned.term === term);
  if (found === undefined) throw new Error(`no banned term named ${term}`);
  return found;
}

describe("BANNED_UI_TERMS contract", () => {
  it("is exactly the UI_RULES §3 banned line plus the glossary's never-say column", () => {
    expect(BANNED_UI_TERMS.map((banned) => banned.term).sort()).toEqual(
      [
        "MANVI",
        "Options",
        "Oops",
        "Preferences",
        "attempt",
        "code graph",
        "codeintel",
        "could not",
        "devmap",
        "generation N",
        "harness",
        "is_fresh",
        "lane",
        "ledger record",
        "linked checkout",
        "min rung",
        "posture",
        "project",
        "repo",
        "repo map",
        "rung",
        "sidecar",
        "snake_case identifier",
        "successfully",
        "ticket",
      ].sort(),
    );
  });

  it("keeps the retired verbs and the voice rules out of the rule-14 list, in lists of their own", () => {
    expect(RETIRED_VERB_TERMS.map((t) => t.term)).toEqual(RETIRED_VERBS.map((v) => v.verb));
    expect(VOICE_TERMS.map((t) => t.term)).toEqual(["please", "exclamation mark", "British spelling"]);
    const rule14 = new Set(BANNED_UI_TERMS.map((t) => t.term));
    for (const other of [...RETIRED_VERB_TERMS, ...VOICE_TERMS]) expect(rule14.has(other.term), other.term).toBe(false);
  });

  it("has the shape lane L consumes: term, RegExp pattern and a use for every entry", () => {
    expect(BANNED_UI_TERMS.length).toBeGreaterThan(20);
    for (const banned of EVERY_LIST) {
      expect(typeof banned.term).toBe("string");
      expect(banned.term.length).toBeGreaterThan(0);
      expect(banned.pattern).toBeInstanceOf(RegExp);
      expect(banned.use.trim().length).toBeGreaterThan(0);
      if (banned.note !== undefined) expect(banned.note.trim().length).toBeGreaterThan(0);
    }
  });

  it("has unique terms", () => {
    const terms = EVERY_LIST.map((banned) => banned.term);
    expect(new Set(terms).size).toBe(terms.length);
  });

  it("has no global or sticky pattern, so test() is stateless and entries are shareable", () => {
    for (const banned of EVERY_LIST) {
      expect(banned.pattern.global, banned.term).toBe(false);
      expect(banned.pattern.sticky, banned.term).toBe(false);
    }
  });

  it("is a readonly list at the type level and is not mutated by a lookup", () => {
    const before = BANNED_UI_TERMS.length;
    findBannedTerms("harness MANVI");
    findBannedTerms("harness MANVI");
    expect(BANNED_UI_TERMS.length).toBe(before);
  });

  it("has an examples entry for every term, and none for a term that does not exist", () => {
    const terms = EVERY_LIST.map((banned) => banned.term).sort();
    expect(Object.keys(EXAMPLES).sort()).toEqual(terms);
  });
});

describe("every pattern has a positive and a negative example", () => {
  for (const banned of EVERY_LIST) {
    const examples = EXAMPLES[banned.term];
    it(`${banned.term}: matches its hits and none of its misses`, () => {
      expect(examples, `examples for ${banned.term}`).toBeDefined();
      expect(examples.hit.length).toBeGreaterThan(0);
      expect(examples.miss.length).toBeGreaterThan(0);
      for (const text of examples.hit) {
        expect(banned.pattern.test(text), `${banned.term} should match ${JSON.stringify(text)}`).toBe(true);
      }
      for (const text of examples.miss) {
        expect(banned.pattern.test(text), `${banned.term} should not match ${JSON.stringify(text)}`).toBe(false);
      }
    });
  }
});

describe("case sensitivity where case is the point", () => {
  it("bans MANVI and accepts Manvi in every position", () => {
    const manvi = patternFor("MANVI");
    expect(manvi.pattern.test("MANVI")).toBe(true);
    expect(manvi.pattern.test("Manvi")).toBe(false);
    expect(manvi.pattern.test("Manvi policy")).toBe(false);
    expect(manvi.pattern.test("the MANVI policy")).toBe(true);
    expect(findBannedTerms("Manvi policy blocked this.")).toEqual([]);
  });

  it("bans lowercase devmap and leaves DevMap, the CLI's product name", () => {
    expect(findBannedTerms("DevMap").map((b) => b.term)).toEqual([]);
    expect(findBannedTerms("Install DevMap in Settings").map((b) => b.term)).toEqual([]);
    expect(findBannedTerms("devmap").map((b) => b.term)).toEqual(["devmap"]);
  });

  it("matches the capitalized Preferences and Options titles but not lowercase prose", () => {
    expect(patternFor("Preferences").pattern.test("Preferences")).toBe(true);
    expect(patternFor("Preferences").pattern.test("preferences")).toBe(false);
    expect(patternFor("Options").pattern.test("options")).toBe(false);
  });

  it("matches the case-insensitive terms in any case", () => {
    for (const text of ["SIDECAR", "Sidecar", "sidecar"]) {
      expect(patternFor("sidecar").pattern.test(text)).toBe(true);
    }
    for (const text of ["POSTURE", "Posture", "posture"]) {
      expect(patternFor("posture").pattern.test(text)).toBe(true);
    }
  });
});

describe("harness is banned as a UI noun only, and the lint can only approximate that", () => {
  const harness = patternFor("harness").pattern;

  it("skips path-shaped uses", () => {
    for (const text of ["harness/foo.html", "open harness/foo.html", "/Users/x/harness", "harness.html", "scripts/harness-runner.mjs", "test-harness"]) {
      expect(harness.test(text), text).toBe(false);
    }
  });

  it("documents in its note that it cannot tell a noun from a verb", () => {
    expect(patternFor("harness").note).toMatch(/noun/);
    // Known false positive, kept visible on purpose: the verb is flagged too.
    expect(harness.test("We harness the cache")).toBe(true);
  });

  it("flags the noun at the end of a sentence and before punctuation", () => {
    expect(harness.test("The harness.")).toBe(true);
    expect(harness.test("(harness)")).toBe(true);
    expect(harness.test("harness, then run")).toBe(true);
  });
});

describe("findBannedTerms", () => {
  it("returns nothing for clean copy, the empty string and whitespace", () => {
    expect(findBannedTerms("Couldn't fetch origin. Try again.", EVERY_LIST)).toEqual([]);
    expect(findBannedTerms("")).toEqual([]);
    expect(findBannedTerms("   \n\t")).toEqual([]);
  });

  it("returns every distinct term that matches, in list order", () => {
    const hits = findBannedTerms("Oops, the sidecar posture failed in_progress").map((b) => b.term);
    expect(hits).toEqual(expect.arrayContaining(["sidecar", "posture", "Oops", "snake_case identifier"]));
    expect(hits).toEqual([...hits].sort((a, b) => indexOfTerm(a) - indexOfTerm(b)));
  });

  it("is stateless across repeated calls", () => {
    for (let i = 0; i < 5; i += 1) {
      expect(findBannedTerms("MANVI").map((b) => b.term)).toEqual(["MANVI"]);
    }
  });

  it("handles unicode and very long input without hanging", () => {
    expect(findBannedTerms("日本語のテキスト 🚀 émoji — ünïcode")).toEqual([]);
    const long = `${"a".repeat(200_000)} harness ${"b ".repeat(50_000)}`;
    const started = Date.now();
    expect(findBannedTerms(long).map((b) => b.term)).toContain("harness");
    expect(Date.now() - started).toBeLessThan(wallClockBudget(2000));
  });

  it("does not ban verdict: it is Manvi's own word", () => {
    expect(findBannedTerms("The verdict is allow.")).toEqual([]);
    expect(BANNED_UI_TERMS.some((banned) => /verdict/i.test(banned.term))).toBe(false);
    const verdict = CROSS_APP_TERMS.find((entry) => entry.term === "verdict");
    expect(verdict?.owner).toBe("Manvi");
  });
});

function indexOfTerm(term: string): number {
  return BANNED_UI_TERMS.findIndex((banned) => banned.term === term);
}

describe("glossary and verbs", () => {
  it("every glossary 'never' word is caught by some banned pattern", () => {
    for (const entry of GLOSSARY) {
      for (const never of entry.never) {
        expect(findBannedTerms(never).length, `${entry.concept}: ${never}`).toBeGreaterThan(0);
      }
    }
  });

  it("no glossary 'say' word is itself banned", () => {
    for (const entry of GLOSSARY) {
      expect(findBannedTerms(entry.say), `${entry.concept}: ${entry.say}`).toEqual([]);
    }
  });

  it("has one entry per concept", () => {
    const concepts = GLOSSARY.map((entry) => entry.concept);
    expect(new Set(concepts).size).toBe(concepts.length);
  });

  it("defines the sixteen verbs, each with a one-line meaning and no banned word", () => {
    expect(VERBS.map((entry) => entry.verb)).toEqual([
      "Refresh",
      "Fetch",
      "Rescan",
      "Rebuild",
      "Try again",
      "Delete",
      "Discard",
      "Remove",
      "Archive",
      "Clear",
      "Dismiss",
      "Open",
      "Show",
      "Hide",
      "Run",
      "Stop",
    ]);
    for (const entry of VERBS) {
      expect(entry.meaning.trim().length).toBeGreaterThan(0);
      expect(entry.meaning).not.toContain("\n");
      expect(findBannedTerms(entry.verb, EVERY_LIST)).toEqual([]);
    }
  });

  it("retires exactly Reload, Recheck, Retry, Launch and Organise, each banned and each with a live replacement", () => {
    expect(RETIRED_VERBS.map((entry) => entry.verb)).toEqual(["Reload", "Recheck", "Retry", "Launch", "Organise"]);
    for (const retired of RETIRED_VERBS) {
      expect(findBannedTerms(retired.verb, RETIRED_VERB_TERMS).map((b) => b.term), retired.verb).toContain(retired.verb);
      expect(retired.use.trim().length).toBeGreaterThan(0);
    }
  });

  it("never lists a retired verb as a live one", () => {
    const live = new Set(VERBS.map((entry) => entry.verb));
    for (const retired of RETIRED_VERBS) expect(live.has(retired.verb)).toBe(false);
  });
});
