import { describe, expect, it } from "vitest";
import {
  matchesCommit,
  parseFilterQuery,
  queryNeedsServerFetch,
  type ParsedFilterQuery,
} from "./parseQuery";
import { PARSE_MEMO_CAP, parseFilterQueryCached, queryParser } from "./queryMemo";

// ---------------------------------------------------------------------------
// Adversarial stress for the filter query grammar. Every "pinned" expectation
// documents CURRENT behavior of parseQuery.ts (verified this branch); nothing
// here prescribes aspirational behavior.
// ---------------------------------------------------------------------------

describe("parseFilterQuery stress: extreme token sizes", () => {
  it("keeps a 10k-char token intact without throwing or stalling", () => {
    const needle = "x".repeat(10_000);
    const startedAt = performance.now();
    const parsed = parseFilterQuery(`author:ada ${needle} tail`);
    const elapsedMs = performance.now() - startedAt;
    expect(parsed.author).toBe("ada");
    expect(parsed.text).toContain(needle);
    expect(parsed.text.endsWith("tail")).toBe(true);
    expect(elapsedMs).toBeLessThan(2_000);
  });

  it("handles 1,000 tokens, preserving order and count", () => {
    const tokens = Array.from({ length: 1_000 }, (_, i) => (i % 2 === 0 ? `w${i}` : "author:a"));
    const parsed = parseFilterQuery(tokens.join(" "));
    // author:a repeated is fine; every other token lands in free text in order.
    expect(parsed.author).toBe("a");
    const freeTokens = tokens.filter((t) => t !== "author:a");
    expect(parsed.text.split(" ")).toEqual(freeTokens);
  });
});

describe("parseFilterQuery stress: quoting trap (characterization)", () => {
  it("silently embeds quote characters in the author needle", () => {
    // There is NO quoted-value grammar in parseQuery.ts. `author:"Ada Lovelace"`
    // splits on whitespace into two tokens: the author needle becomes `"ada`
    // (quote included!) and `lovelace"` falls into free text. No error, no
    // warning — a user typing the natural quoted form gets a filter that can
    // never match any real name. Pinned so a future quoting grammar changes
    // this deliberately.
    const parsed = parseFilterQuery('author:"Ada Lovelace" refactor');
    expect(parsed.author).toBe('"ada');
    expect(parsed.text).toBe('lovelace" refactor');
  });

  it("drops an empty key value silently instead of erroring", () => {
    // The actual "silent-empty" behavior: bare `author:` / `path:` / `sha:`
    // leave the field undefined and vanish from the query.
    const parsed = parseFilterQuery("author: path: sha:");
    expect(parsed.author).toBeUndefined();
    expect(parsed.path).toBeUndefined();
    expect(parsed.sha).toBeUndefined();
    expect(parsed.text).toBe("");
  });

  it("treats unknown-type trailing colons as literal free text", () => {
    // `fix:` alone is the commitType form; `fixup:` is NOT a conventional
    // type so it stays free text verbatim — including the colon.
    expect(parseFilterQuery("fix:").commitType).toBe("fix");
    const parsed = parseFilterQuery("fixup:");
    expect(parsed.commitType).toBeUndefined();
    expect(parsed.text).toBe("fixup:");
  });
});

describe("parseFilterQuery stress: pathspec magic attempts", () => {
  it("keeps path values as opaque literals — no glob/magic interpretation", () => {
    expect(parseFilterQuery("path:/etc/passwd").path).toBe("/etc/passwd");
    expect(parseFilterQuery("path:**/*.rs").path).toBe("**/*.rs");
    expect(parseFilterQuery("path:a/../b").path).toBe("a/../b");
    // Path magic prefixes are NOT stripped here; that is git's job downstream.
    expect(parseFilterQuery("path::(literal)x").path).toBe(":(literal)x");
  });

  it("diverges from queryNeedsServerFetch on bare 'path:' (characterization)", () => {
    // The parser drops an empty path value, but the server-fetch detector
    // still claims the query needs a git walk. Conservative over-fetch, not a
    // correctness lie — pinned as current contract.
    expect(parseFilterQuery("path:").path).toBeUndefined();
    expect(queryNeedsServerFetch("path:")).toBe(true);
    expect(queryNeedsServerFetch("path:x")).toBe(true);
    expect(queryNeedsServerFetch("author:x path:y")).toBe(true);
    expect(queryNeedsServerFetch("author:x")).toBe(false);
  });
});

describe("parseFilterQuery stress: nesting and repetition", () => {
  it("collapses author:author:author:x into one absurd-but-harmless needle", () => {
    const parsed = parseFilterQuery("author:author:author:x");
    expect(parsed.author).toBe("author:author:x");
    expect(matchesCommit(
      { id: "a", summary: "s", author_name: "someone", author_email: "e@x" },
      parsed
    )).toBe(false);
  });

  it("lets the LAST duplicate key win for each field", () => {
    const parsed = parseFilterQuery("author:a author:b sha:1 sha:2 type:fix type:feat rest");
    expect(parsed.author).toBe("b");
    expect(parsed.sha).toBe("2");
    expect(parsed.commitType).toBe("feat");
    expect(parsed.text).toBe("rest");
  });

  it("keeps non-conventional types as predicates, matching the backend filter", () => {
    // Parity contract with CommitFilter::parse: any non-empty type: value is
    // a commit-type predicate on BOTH sides of the IPC boundary.
    const parsed = parseFilterQuery("type:wip");
    expect(parsed.commitType).toBe("wip");
    expect(parsed.text).toBe("");
  });

  it("searches negative-looking tokens literally — no negation grammar", () => {
    const parsed = parseFilterQuery("-is:pr");
    expect(parsed.text).toBe("-is:pr");
    // Free text is one joined needle matched against the whole haystack.
    const row = { id: "abc", summary: "-is:pr should match literally", author_name: "a", author_email: "e" };
    expect(matchesCommit(row, parsed)).toBe(true);
    expect(matchesCommit({ ...row, summary: "clean pr" }, parsed)).toBe(false);
  });
});

describe("parseFilterQuery stress: unicode / RTL / zero-width", () => {
  it("preserves zero-width and bidi controls inside needles without throwing", () => {
    const zws = "\u200B";
    const rlo = "\u202E";
    const parsed = parseFilterQuery(`author:ad${zws}a ${rlo}spoof`);
    expect(parsed.author).toBe(`ad${zws}a`);
    expect(parsed.text).toBe(`${rlo}spoof`);
  });

  it("matches only rows whose haystack contains the exact code units", () => {
    const row = { id: "f00d", summary: "fix café\u200B menu", author_name: "José", author_email: "j@x" };
    expect(matchesCommit(row, parseFilterQuery("café"))).toBe(true);
    expect(matchesCommit(row, parseFilterQuery("cafe\u0301"))).toBe(false);
    expect(matchesCommit(row, parseFilterQuery("caf\u200Be"))).toBe(false);
    // NFC vs NFD never throw, they just answer faithfully.
    expect(() => matchesCommit(row, parseFilterQuery("\u0301"))).not.toThrow();
  });

  it("case-folds non-ASCII via toLowerCase without crashing on edge scripts", () => {
    const parsed = parseFilterQuery("İSTANBUL"); // dotted capital I
    expect(parsed.text).toBe("i̇stanbul");
  });
});

describe("parseFilterQuery stress: only-colons input", () => {
  it("routes 'a::b::c' wholly to free text", () => {
    const parsed = parseFilterQuery("a::b::c");
    expect(parsed.author).toBeUndefined();
    expect(parsed.path).toBeUndefined();
    expect(parsed.sha).toBeUndefined();
    expect(parsed.commitType).toBeUndefined();
    expect(parsed.text).toBe("a::b::c");
  });

  it("never throws on colon-dense garbage", () => {
    expect(() => parseFilterQuery(":::: : : :author::::path:")).not.toThrow();
  });
});

describe("matchesCommit stress: hostile fields against typed queries", () => {
  const baseRow = { id: "AbCdEf0123", summary: "feat(ui): ship", author_name: "Ada", author_email: "ada@x.dev" };

  function assertMatch(query: string, row: typeof baseRow, expected: boolean): void {
    const parsed: ParsedFilterQuery = parseFilterQuery(query);
    expect(matchesCommit(row, parsed)).toBe(expected);
  }

  it("prefix-matches shas after folding BOTH sides to lowercase", () => {
    assertMatch("sha:abc", baseRow, true);
    // The needle is lowercased during parse, so hostile casing still matches.
    assertMatch("sha:ABC", baseRow, true);
    assertMatch("sha:fff", baseRow, false);
  });

  it("requires conventional header shapes for type filters", () => {
    assertMatch("type:feat", baseRow, true);          // "feat("
    const scoped = { ...baseRow, summary: "feat: x" };
    assertMatch("type:feat", scoped, true);           // "feat:"
    const bad = { ...baseRow, summary: "feature: x" };
    assertMatch("type:feat", bad, false);             // prefix must be followed by : or (
  });

  it("ANDs every present clause", () => {
    assertMatch("author:ada type:feat ship", baseRow, true);
    assertMatch("author:ada type:fix ship", baseRow, false);
    assertMatch("author:nobody", baseRow, false);
  });

  it("survives empty/whitespace queries matching everything", () => {
    assertMatch("", baseRow, true);
    assertMatch("   ", baseRow, true);
  });
});

describe("parseFilterQueryCached memo interplay under alternation", () => {
  it("returns stable frozen results alternating A/B 500x with bounded cache growth", () => {
    const a = "author:ada feat gate";
    const b = "author:bob fix ship it";
    const missesBefore = queryParser.misses;

    let lastA: ParsedFilterQuery | null = null;
    let lastB: ParsedFilterQuery | null = null;
    for (let i = 0; i < 500; i += 1) {
      const pa = parseFilterQueryCached(a);
      const pb = parseFilterQueryCached(b);
      if (lastA) expect(pa).toBe(lastA); // same frozen instance, no drift
      if (lastB) expect(pb).toBe(lastB);
      expect(pa).toEqual(parseFilterQuery(a));
      expect(pb).toEqual(parseFilterQuery(b));
      lastA = pa;
      lastB = pb;
    }

    // Exactly two cold parses total, regardless of iteration count.
    expect(queryParser.misses - missesBefore).toBe(2);
    expect(queryParser.size).toBeLessThanOrEqual(PARSE_MEMO_CAP);
  });
});
