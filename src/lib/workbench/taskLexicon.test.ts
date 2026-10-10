import { describe, expect, it } from "vitest";
import { CONCEPTS, editDistance, fold, stem, synonymsOf, typoBudget, words } from "./taskLexicon";

describe("words", () => {
  it("folds case, width and accents, and splits code identifiers", () => {
    expect(words("fixOAuth2_login-Flow")).toEqual(["fix", "o", "auth", "2", "login", "flow"]);
    expect(words("Café ＡＰＩ")).toEqual(["cafe", "api"]);
    expect(words("HTTPServer parseJSON")).toEqual(["http", "server", "parse", "json"]);
    expect(fold("Ünïcödé")).toBe("unicode");
  });

  it("is bounded and total", () => {
    expect(words("a ".repeat(1000)).length).toBe(256);
    expect(words(undefined as unknown as string)).toEqual([]);
    expect(words("")).toEqual([]);
    expect(words("—…!!")).toEqual([]);
  });
});

describe("stem", () => {
  it("joins the common forms of a word", () => {
    for (const [a, b] of [["crashes", "crash"], ["tests", "test"], ["running", "run"], ["stopped", "stop"], ["dependencies", "dependency"], ["issues", "issue"], ["logging", "log"]]) {
      expect(stem(a), `${a} ~ ${b}`).toBe(stem(b));
    }
  });

  it("leaves short words, numbers and -ss/-us/-is words alone", () => {
    expect(stem("bus")).toBe("bus");
    expect(stem("status")).toBe("status");
    expect(stem("analysis")).toBe("analysis");
    expect(stem("process")).toBe("process");
    expect(stem("v2s")).toBe("v2s");
  });
});

describe("concepts", () => {
  it("relates words in a group both ways and never to themselves", () => {
    expect(synonymsOf(stem("login")).has(stem("auth"))).toBe(true);
    expect(synonymsOf(stem("auth")).has(stem("login"))).toBe(true);
    expect(synonymsOf(stem("slow")).has(stem("latency"))).toBe(true);
    expect(synonymsOf(stem("login")).has(stem("login"))).toBe(false);
    expect(synonymsOf("zzzz").size).toBe(0);
  });

  it("keeps every group lower-case, unique and more than one word", () => {
    for (const group of CONCEPTS) {
      expect(group.length).toBeGreaterThan(1);
      expect(new Set(group).size).toBe(group.length);
      for (const word of group) expect(word).toBe(fold(word));
    }
  });
});

describe("editDistance", () => {
  it("counts a swap as one edit and gives up past the budget", () => {
    expect(editDistance("crash", "crahs", 2)).toBe(1);
    expect(editDistance("crash", "crush", 2)).toBe(1);
    expect(editDistance("crash", "cash", 2)).toBe(1);
    expect(editDistance("crash", "crash", 2)).toBe(0);
    expect(editDistance("crash", "zebra", 1)).toBe(2);
    expect(editDistance("a", "abcdef", 2)).toBe(3);
  });

  it("grants typos by length", () => {
    expect(typoBudget(4)).toBe(0);
    expect(typoBudget(5)).toBe(1);
    expect(typoBudget(9)).toBe(2);
  });
});
