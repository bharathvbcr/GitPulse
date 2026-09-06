import { describe, expect, it } from "vitest";
import {
  filterPathsByFileQuery,
  globToRegExp,
  matchesFileQuery,
  parseFileQuery,
} from "./fileQuery";

const paths = ["src/lib/main.ts", "src/App.svelte", "README.md", "docs/guide.md"];

describe("parseFileQuery", () => {
  it("treats empty and whitespace as match-all", () => {
    expect(parseFileQuery("").kind).toBe("all");
    expect(parseFileQuery("   ").kind).toBe("all");
  });

  it("parses substring, glob, regex, and fuzzy forms", () => {
    expect(parseFileQuery("Main").kind).toBe("substring");
    expect(parseFileQuery("Main").needle).toBe("main");
    expect(parseFileQuery("*.ts").kind).toBe("glob");
    expect(parseFileQuery("src/**/*.md").kind).toBe("glob");
    expect(parseFileQuery("/App\\./").kind).toBe("regex");
    expect(parseFileQuery("~sml").kind).toBe("fuzzy");
  });

  it("strips is: and ext: tokens from the path pattern", () => {
    const query = parseFileQuery("is:staged ext:ts src/**");
    expect(query.status).toBe("staged");
    expect(query.ext).toBe(".ts");
    expect(query.kind).toBe("glob");
    expect(query.needle).toBe("src/**");
  });

  it("fails closed on invalid or oversized regex", () => {
    expect(parseFileQuery("/(unclosed/").error).toBe("Invalid regular expression");
    const huge = `/${"a".repeat(250)}/`;
    expect(parseFileQuery(huge).error).toMatch(/longer than/);
    expect(filterPathsByFileQuery(paths, parseFileQuery("/(unclosed/")).paths).toEqual([]);
  });

  /**
   * A quantified group wrapping an unbounded quantifier backtracks
   * exponentially, and a JavaScript regex is not interruptible: once `test`
   * has started, no timeout or worker cancellation can shorten it. The only
   * thing that keeps the window responsive is refusing to compile it — the
   * same call `lineSearch` already makes for the diff and file-viewer search
   * boxes. Measured before this guard existed: ONE 45-character path took
   * 1.4 s for `(a+)+$` and 2.7 s for `(\w+)+$`, against a module whose own
   * contract is 100,000 paths.
   */
  it("refuses catastrophically backtracking patterns instead of hanging", () => {
    for (const pattern of ["/(a+)+$/", "/(\\w+)+$/", "/([a-z]+)*$/", "/(\\d{2,})*$/"]) {
      const query = parseFileQuery(pattern);
      expect(query.error ?? "<compiled, not refused>").toMatch(/backtrack/i);
      expect(query.regex, `${pattern} must not compile`).toBeNull();
    }
  });

  it("refuses them fast enough that the explorer never blocks", () => {
    // The shape that hangs: a long run the pattern can consume many ways,
    // ending in something it cannot match, so the engine explores every split.
    const hostile = Array.from(
      { length: 50 },
      (_, i) => `src/lib/components/${"a".repeat(24)}${i}.ts`,
    );
    const query = parseFileQuery("/(a+)+$/");
    const started = performance.now();
    expect(filterPathsByFileQuery(hostile, query).paths).toEqual([]);
    // Generous by three orders of magnitude against the measured 70 s, so it
    // stays honest under machine load rather than flapping on a busy box.
    expect(performance.now() - started).toBeLessThan(1_000);
  });

  /**
   * The budget is the backstop behind the static refusal, because that
   * refusal cannot be complete — it is an approximation measured on V8 while
   * the app ships on JavaScriptCore. These pin that the backstop exists, is
   * honest about having fired, and does not fire on ordinary work.
   */
  it("stops a long scan at its budget and reports the result as partial", () => {
    const many = Array.from({ length: 200_000 }, (_, i) => `src/lib/file${i}.ts`);
    const started = Date.now();
    const result = filterPathsByFileQuery(many, parseFileQuery("~fl"), { maxMillis: 5 });
    expect(Date.now() - started).toBeLessThan(3_000);
    expect(result.truncated).toBe(true);
    expect(result.paths.length).toBeLessThan(many.length);
  });

  it("does not report truncation when the whole list was scanned", () => {
    const result = filterPathsByFileQuery(paths, parseFileQuery("~ts"));
    expect(result.truncated).toBe(false);
  });

  it("keeps match-all cheap: no budget scan when nothing narrows", () => {
    const many = Array.from({ length: 100_000 }, (_, i) => `f${i}.ts`);
    const result = filterPathsByFileQuery(many, parseFileQuery(""), { maxMillis: 1 });
    expect(result.truncated).toBe(false);
    expect(result.paths).toHaveLength(many.length);
  });

  it("still compiles ordinary regexes, including safe nesting", () => {
    expect(parseFileQuery("/App\\./").error).toBeNull();
    expect(parseFileQuery("/(foo|bar)+/").error).toBeNull();
    expect(parseFileQuery("/(\\d+)/").error).toBeNull();
    const query = parseFileQuery("/main\\.ts$/");
    expect(filterPathsByFileQuery(paths, query).paths).toEqual(["src/lib/main.ts"]);
  });
});

describe("matchesFileQuery", () => {
  it("matches substring on basename or full path", () => {
    const query = parseFileQuery("MAIN");
    expect(paths.filter((p) => matchesFileQuery(p, query))).toEqual(["src/lib/main.ts"]);
  });

  it("matches globs against path and basename", () => {
    const ts = parseFileQuery("*.ts");
    expect(paths.filter((p) => matchesFileQuery(p, ts))).toEqual(["src/lib/main.ts"]);
    const nested = parseFileQuery("src/**/*.svelte");
    expect(paths.filter((p) => matchesFileQuery(p, nested))).toEqual(["src/App.svelte"]);
  });

  it("applies ext: even when the path pattern is empty", () => {
    const query = parseFileQuery("ext:md");
    expect(filterPathsByFileQuery(paths, query).paths).toEqual(["README.md", "docs/guide.md"]);
  });

  it("fuzzy-matches subsequence queries", () => {
    const query = parseFileQuery("~sma");
    expect(matchesFileQuery("src/lib/main.ts", query)).toBe(true);
    expect(matchesFileQuery("README.md", query)).toBe(false);
  });
});

describe("globToRegExp", () => {
  it("treats character classes as literals", () => {
    const { regex } = globToRegExp("[ab].ts");
    expect(regex?.test("[ab].ts")).toBe(true);
    expect(regex?.test("a.ts")).toBe(false);
  });

  it("does not let a single star cross directories", () => {
    const { regex } = globToRegExp("src/*.ts");
    expect(regex?.test("src/main.ts")).toBe(true);
    expect(regex?.test("src/lib/main.ts")).toBe(false);
  });

  it("matches exactly one non-separator character for ?", () => {
    const { regex } = globToRegExp("src/?.ts");
    expect(regex?.test("src/a.ts")).toBe(true);
    expect(regex?.test("src/ab.ts")).toBe(false);
    expect(regex?.test("src/.ts")).toBe(false);
    // `?` must not swallow a separator, for the same reason a single `*` does not.
    expect(globToRegExp("src?lib")?.regex?.test("src/lib")).toBe(false);
  });

  it("refuses a glob longer than the pattern cap instead of compiling it", () => {
    const { regex, error } = globToRegExp(`${"*".repeat(201)}.ts`);
    expect(regex).toBeNull();
    expect(error).toBe("Invalid glob");
  });

  /**
   * Globs are built by escaping every metacharacter, so a hostile glob cannot
   * become a hostile regex. This pins that property rather than trusting the
   * comment that claims it.
   */
  it("cannot be talked into emitting an unbounded nested quantifier", () => {
    for (const glob of ["(a+)+", "[a-z]*", "**/(x+)+*", "(a|a)*"]) {
      const { regex } = globToRegExp(glob);
      if (!regex) continue;
      const started = performance.now();
      regex.test("a".repeat(40) + "!");
      expect(performance.now() - started, glob).toBeLessThan(100);
    }
  });
});
