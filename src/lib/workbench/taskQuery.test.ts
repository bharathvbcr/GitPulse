import { describe, expect, it } from "vitest";
import { emptyFacet } from "./taskOrganize";
import {
  facetToQuery,
  hasFreeText,
  isEmptyQuery,
  mergeLegacyFacet,
  parseTaskQuery,
  partAtCaret,
  qualifierState,
  queryChips,
  removePart,
  replacePart,
  serverWord,
  isState,
  setIsState,
  setQualifier,
  splitQuery,
  suggest,
} from "./taskQuery";

describe("parseTaskQuery", () => {
  it("reads words as clauses that must all match", () => {
    const parsed = parseTaskQuery("Login crash");
    expect(parsed.clauses).toEqual([["login"], ["crash"]]);
    expect(hasFreeText(parsed)).toBe(true);
  });

  it("joins words around OR into one clause", () => {
    expect(parseTaskQuery("login OR signin crash").clauses).toEqual([["login", "signin"], ["crash"]]);
    expect(parseTaskQuery("OR crash").clauses).toEqual([["crash"]]);
    expect(parseTaskQuery("crash OR").clauses).toEqual([["crash"]]);
  });

  it("reads phrases, negations and stopwords", () => {
    const parsed = parseTaskQuery('the "Sign In" -flaky -"won\'t fix"');
    expect(parsed.phrases).toEqual(["sign in"]);
    expect(parsed.excluded).toEqual(["flaky"]);
    expect(parsed.excludedPhrases).toEqual(["won t fix"]);
    expect(parsed.clauses).toEqual([]);
    expect(parseTaskQuery("the").clauses).toEqual([["the"]]);
  });

  it("reads qualifiers with aliases, comma lists, quotes and negation", () => {
    const parsed = parseTaskQuery('repo:gitpulse,manvi type:bug -owner:@pat label:"needs design" prio:p0,high status:"in progress" is:due-soon');
    expect(parsed.qualifiers).toEqual([
      { key: "repo", values: ["gitpulse", "manvi"], negated: false },
      { key: "kind", values: ["bug"], negated: false },
      { key: "owner", values: ["@pat"], negated: true },
      { key: "label", values: ["needs design"], negated: false },
      { key: "priority", values: ["0", "1"], negated: false },
      { key: "status", values: ["in_progress"], negated: false },
      { key: "is", values: ["soon"], negated: false },
    ]);
    expect(parsed.clauses).toEqual([]);
    expect(isEmptyQuery(parsed)).toBe(false);
    expect(hasFreeText(parsed)).toBe(false);
  });

  it("says what it did not understand instead of silently filtering nothing", () => {
    const parsed = parseTaskQuery("priority:huge status:nope is:weird label: todo:fix https://x.y");
    expect(parsed.qualifiers).toEqual([]);
    expect(parsed.warnings.join("\n")).toMatch(/Unknown priority "huge"/);
    expect(parsed.warnings.join("\n")).toMatch(/Unknown status "nope"/);
    expect(parsed.warnings.join("\n")).toMatch(/Unknown state "is:weird"/);
    expect(parsed.warnings.join("\n")).toMatch(/"label:" has no value/);
    expect(parsed.warnings.join("\n")).toMatch(/"todo:" is not a filter/);
    expect(parsed.warnings.join("\n")).not.toMatch(/https/);
    expect(parsed.clauses).toEqual([["todo"], ["fix"], ["https"], ["x"], ["y"]]);
  });

  it("is bounded and never throws on odd input", () => {
    const long = parseTaskQuery("w ".repeat(400));
    expect(long.clauses.length).toBeLessThanOrEqual(32);
    expect(long.warnings.join(" ")).toMatch(/first 512 characters|first 32/);
    for (const text of ['"', '-"', "-", ":", "-:", "a:", '"unterminated phrase', "\u0000\u{1F600}", "OR OR OR"]) {
      expect(() => parseTaskQuery(text)).not.toThrow();
    }
    expect(isEmptyQuery(parseTaskQuery("   "))).toBe(true);
    expect(isEmptyQuery(parseTaskQuery("!!! ---"))).toBe(true);
  });
});

describe("splitQuery", () => {
  it("keeps quoted runs together and records offsets", () => {
    expect(splitQuery('a label:"x y"  -"p q" b').map((p) => p.raw)).toEqual(["a", 'label:"x y"', '-"p q"', "b"]);
    expect(splitQuery("  ab cd")[1]).toEqual({ raw: "cd", start: 5, end: 7 });
  });
});

describe("serverWord", () => {
  it("sends the longest required word, trimmed to the prefix its forms share", () => {
    expect(serverWord(parseTaskQuery("fix login crash"))).toBe("login");
    expect(serverWord(parseTaskQuery("upgrade dependencies"))).toBe("dependenc");
    expect(serverWord(parseTaskQuery('"memory leak"'))).toBe("memory");
  });

  it("does not send a word only one side of an OR needs, nor a filter", () => {
    expect(serverWord(parseTaskQuery("login OR signin"))).toBeNull();
    expect(serverWord(parseTaskQuery("label:ui"))).toBeNull();
    expect(serverWord(parseTaskQuery("a"))).toBeNull();
  });
});

describe("editing the query from the filters", () => {
  it("replaces every qualifier of a key and keeps the rest as typed", () => {
    expect(setQualifier("crash label:a -label:b  owner:x", "label", "needs design")).toBe('crash owner:x label:"needs design"');
    expect(setQualifier("crash label:a", "label", null)).toBe("crash");
    expect(setQualifier("crash tag:a", "label", "b")).toBe("crash label:b");
  });

  it("reports one value, several, or none per key", () => {
    expect(qualifierState(parseTaskQuery("label:a"), "label")).toEqual({ kind: "one", value: "a" });
    expect(qualifierState(parseTaskQuery("label:a,b"), "label")).toEqual({ kind: "many" });
    expect(qualifierState(parseTaskQuery("-label:a"), "label")).toEqual({ kind: "many" });
    expect(qualifierState(parseTaskQuery("crash"), "label")).toEqual({ kind: "all" });
  });

  it("names chips for filters, phrases and exclusions, each removable", () => {
    const text = 'crash label:ui -flaky "sign in"';
    const chips = queryChips(text);
    expect(chips.map((c) => c.label)).toEqual(["label: ui", "flaky", "“sign in”"]);
    expect(chips[1].negated).toBe(true);
    expect(removePart(text, chips[0].index)).toBe('crash -flaky "sign in"');
  });

  it("finds the part under the caret and replaces it", () => {
    const at = partAtCaret("crash label:u", 13);
    expect(at?.prefix).toBe("label:u");
    expect(replacePart("crash label:u", at!.index, "label:ui")).toEqual({ text: "crash label:ui ", caret: 15 });
    expect(replacePart("la crash", 0, "label:")).toEqual({ text: "label: crash", caret: 6 });
    expect(replacePart("label:u crash", 0, "label:ui")).toEqual({ text: "label:ui crash", caret: 9 });
    expect(partAtCaret("crash ", 6)).toBeNull();
  });
});

describe("legacy facets", () => {
  it("fold into the query a saved view is applied with", () => {
    expect(facetToQuery(emptyFacet())).toBe("");
    expect(facetToQuery({ priority: 0, kind: "bug", owner: "", label: "needs design", due: "overdue" })).toBe('priority:urgent kind:bug is:unassigned label:"needs design" is:overdue');
    expect(facetToQuery({ ...emptyFacet(), owner: "Pat", due: "none" })).toBe("owner:Pat is:no-due");
    expect(mergeLegacyFacet("crash kind:bug", { ...emptyFacet(), kind: "bug", due: "soon" })).toBe("crash kind:bug is:soon");
    expect(mergeLegacyFacet("crash", null)).toBe("crash");
  });
});

describe("is: groups", () => {
  it("replaces only the states of one group", () => {
    expect(setIsState("crash is:unassigned is:overdue", ["overdue", "soon", "due", "no-due"], "soon")).toBe("crash is:unassigned is:soon");
    expect(setIsState("is:late,unassigned", ["overdue", "soon", "due", "no-due"], null)).toBe("is:late,unassigned");
    expect(setIsState("is:late", ["overdue", "soon", "due", "no-due"], null)).toBe("");
    expect(isState(parseTaskQuery("is:unassigned is:late"), ["overdue", "soon"])).toEqual({ kind: "one", value: "overdue" });
    expect(isState(parseTaskQuery("-is:late"), ["overdue"])).toEqual({ kind: "many" });
  });
});

describe("suggest", () => {
  const sources = { label: [{ value: "ui", count: 3 }, { value: "a11y", count: 1 }, { value: "needs design", count: 2 }] };

  it("completes qualifier names from two letters", () => {
    expect(suggest("la", sources).map((s) => s.insert)).toEqual(["label:"]);
    expect(suggest("-re", sources).map((s) => s.insert)).toEqual(["-repo:"]);
    expect(suggest("l", sources)).toEqual([]);
    expect(suggest("label", sources)).toEqual([]);
    expect(suggest("login", sources)).toEqual([]);
  });

  it("completes values, prefix matches first, after the last comma, quoting as needed", () => {
    expect(suggest("label:", sources).map((s) => s.insert)).toEqual(["label:ui", "label:a11y", 'label:"needs design"']);
    expect(suggest("tag:ne", sources)[0]).toEqual({ label: "label:needs design", detail: "2 tasks", insert: 'tag:"needs design"' });
    expect(suggest("label:ui,a", sources).map((s) => s.insert)).toEqual(["label:ui,a11y"]);
    expect(suggest("label:ui,d", sources).map((s) => s.insert)).toEqual(['label:ui,"needs design"']);
    expect(suggest("label:ui", sources)).toEqual([]);
  });

  it("offers fixed vocabularies without loaded tasks", () => {
    expect(suggest("status:in", {}).map((s) => s.insert)).toEqual(["status:inbox", "status:in_progress"]);
    expect(suggest("is:un", {}).map((s) => s.insert)).toEqual(["is:unassigned", "is:unlabeled"]);
    expect(suggest("priority:", {}).length).toBe(4);
    expect(suggest("nope:x", {})).toEqual([]);
  });
});
