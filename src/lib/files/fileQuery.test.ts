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
    expect(filterPathsByFileQuery(paths, parseFileQuery("/(unclosed/"))).toEqual([]);
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
    expect(filterPathsByFileQuery(paths, query)).toEqual(["README.md", "docs/guide.md"]);
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
});
