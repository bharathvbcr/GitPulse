import { describe, expect, it } from "vitest";
import type { TaskCard } from "./client";
import {
  cardMatchesQuery,
  dueRangeFilter,
  isEmptyQuery,
  MAX_QUERY_TERMS,
  MAX_QUERY_TEXT,
  parseTaskQuery,
  queryChips,
  queryControlValue,
  queryHas,
  removeQueryTerm,
  replaceToken,
  setQueryControl,
  suggest,
  tokenAtCaret,
  toggleQueryTerm,
  type QueryContext,
} from "./taskQuery";
import { sanitizeSnapshot } from "../ui/taskBoardViews";

function card(over: Partial<TaskCard> = {}): TaskCard {
  return {
    id: "t1", revision: 1, updated_at: 1, title: "Crash on save", kind: "bug", status: "ready",
    priority: 1, severity: "High", owner: "Pat", due_at: 100, labels: ["ui", "Needs Review"],
    repository_ids: ["r1", "r2"], primary_repository_id: "r1", home_workspace_id: null, position: 1, archived: false, completed_at: null,
    ...over,
  };
}

const REPOS: Record<string, { name: string; identity_key: string }> = {
  r1: { name: "GitPulse", identity_key: "/Users/me/Code/devtools/GitPulse" },
  r2: { name: "Manvi", identity_key: "github.com/acme/manvi" },
};
const ctx: QueryContext = { repository: (id) => REPOS[id], nowSec: 50 };
const matches = (query: string, over: Partial<TaskCard> = {}, context = ctx) => cardMatchesQuery(card(over), parseTaskQuery(query), context);

describe("parseTaskQuery", () => {
  it("keeps filters out of the text the store searches", () => {
    // The defect: the whole box went to the store's full-text search, which
    // split `is:bug` into the words "is" and "bug" and matched neither type.
    const parsed = parseTaskQuery("is:bug crash priority:high");
    expect(parsed.text).toBe("crash");
    expect(parsed.terms.map((term) => [term.key, term.value])).toEqual([["kind", "bug"], ["priority", "1"]]);
    expect(parsed.problems).toEqual([]);
  });

  it("reads every alias of a key, case-insensitively", () => {
    for (const key of ["is", "type", "kind", "IS", "Type"]) expect(parseTaskQuery(`${key}:Bug`).terms[0]).toMatchObject({ key: "kind", value: "bug" });
    expect(parseTaskQuery("p:p0").terms[0]).toMatchObject({ key: "priority", value: "0" });
    expect(parseTaskQuery("prio:3").terms[0]).toMatchObject({ key: "priority", value: "3" });
    expect(parseTaskQuery("assignee:pat").terms[0].key).toBe("owner");
    expect(parseTaskQuery("tag:ci").terms[0].key).toBe("label");
    expect(parseTaskQuery("repository:gp").terms[0].key).toBe("repo");
    expect(parseTaskQuery("sev:high").terms[0].key).toBe("severity");
    expect(parseTaskQuery("state:done").terms[0].key).toBe("status");
  });

  it("resolves status spellings and refuses unknown ones out loud", () => {
    for (const spelling of ["in_progress", "in-progress", "In-Progress", "wip", "inprogress"]) {
      expect(parseTaskQuery(`status:${spelling}`).terms[0]?.value, spelling).toBe("in_progress");
    }
    expect(parseTaskQuery('status:"in progress"').terms[0]?.value).toBe("in_progress");
    expect(parseTaskQuery("status:open").terms[0]?.value).toBe("open");
    const bad = parseTaskQuery("status:shipped crash");
    expect(bad.terms).toEqual([]);
    expect(bad.text).toBe("crash");
    expect(bad.problems[0]).toMatch(/status:shipped is not a column/);
  });

  it("refuses an unreadable priority or due filter instead of searching for its words", () => {
    const parsed = parseTaskQuery("priority:huge due:tomorrow");
    expect(parsed.terms).toEqual([]);
    expect(parsed.text).toBe("");
    expect(parsed.problems).toHaveLength(2);
  });

  it("treats an unknown key as text, so URLs and times stay searchable", () => {
    const parsed = parseTaskQuery("https://example.com 10:30 foo:bar");
    expect(parsed.terms).toEqual([]);
    expect(parsed.text).toBe("https://example.com 10:30 foo:bar");
  });

  it("ignores a filter still being typed", () => {
    expect(parseTaskQuery("is:")).toEqual({ text: "", terms: [], problems: [] });
    expect(parseTaskQuery("-label:")).toEqual({ text: "", terms: [], problems: [] });
  });

  it("reads quoted values and quoted phrases", () => {
    const parsed = parseTaskQuery('label:"needs   review" "save dialog"');
    expect(parsed.terms[0]).toMatchObject({ key: "label", value: "needs review", raw: 'label:"needs   review"' });
    expect(parsed.text).toBe("save dialog");
    // An unclosed quote runs to the end rather than throwing or splitting.
    expect(parseTaskQuery('label:"needs review').terms[0]?.value).toBe("needs review");
  });

  it("lets a quoted filter be searched as words", () => {
    expect(parseTaskQuery('"is:bug"')).toMatchObject({ text: "is:bug", terms: [] });
  });

  it("never sends a negated word to the store, which would include it", () => {
    const parsed = parseTaskQuery("crash -flaky");
    expect(parsed.text).toBe("crash");
    expect(parsed.terms[0]).toMatchObject({ key: "title", value: "flaky", negated: true });
  });

  it("drops punctuation-only text that the store would turn into an empty match", () => {
    expect(parseTaskQuery("is:bug - !! ...").text).toBe("");
    expect(parseTaskQuery("- crash").text).toBe("crash");
  });

  it("bounds terms and text, and says what it dropped", () => {
    const many = Array.from({ length: MAX_QUERY_TERMS + 5 }, (_, i) => `label:l${i}`).join(" ");
    const parsed = parseTaskQuery(many);
    expect(parsed.terms).toHaveLength(MAX_QUERY_TERMS);
    expect(parsed.problems.at(-1)).toMatch(/5 more were ignored/);
    expect(parseTaskQuery("x".repeat(MAX_QUERY_TEXT * 3)).text.length).toBeLessThanOrEqual(MAX_QUERY_TEXT);
  });

  it("reads hostile input as an empty search", () => {
    for (const input of [null, undefined, 7, {}, [], Symbol("x")]) expect(parseTaskQuery(input)).toEqual({ text: "", terms: [], problems: [] });
  });
});

describe("cardMatchesQuery", () => {
  it("matches a type by name, case-insensitively, and a plural as its singular", () => {
    expect(matches("is:bug")).toBe(true);
    expect(matches("is:BUG")).toBe(true);
    expect(matches("is:bugs")).toBe(true);
    expect(matches("is:feature")).toBe(false);
    expect(matches("is:s", { kind: "" })).toBe(false);
  });

  it("widens within a key and narrows across keys", () => {
    expect(matches("is:feature is:bug")).toBe(true);
    expect(matches("is:bug owner:pat")).toBe(true);
    expect(matches("is:bug owner:ann")).toBe(false);
  });

  it("excludes with a leading minus", () => {
    expect(matches("-is:bug")).toBe(false);
    expect(matches("-is:feature")).toBe(true);
    expect(matches("-crash")).toBe(false);
    expect(matches("-unrelated")).toBe(true);
    // A negation is not an alternative: it holds alongside the positive one.
    expect(matches("is:bug -is:bug")).toBe(false);
  });

  it("matches priority by name or number", () => {
    expect(matches("priority:high")).toBe(true);
    expect(matches("priority:p1")).toBe(true);
    expect(matches("priority:urgent")).toBe(false);
    expect(matches("priority:urgent", { priority: 0 })).toBe(true);
  });

  it("reads medium as Normal, as the Markdown import does", () => {
    expect(matches("priority:medium", { priority: 2 })).toBe(true);
    expect(matches("priority:medium", { priority: 1 })).toBe(false);
    expect(parseTaskQuery("priority:medium").problems).toEqual([]);
  });

  it("matches status, and open as anything not done", () => {
    expect(matches("status:ready")).toBe(true);
    expect(matches("status:done")).toBe(false);
    expect(matches("status:open")).toBe(true);
    expect(matches("status:open", { status: "done" })).toBe(false);
  });

  it("matches owner, label and severity, and none for the empty ones", () => {
    expect(matches("owner:PAT")).toBe(true);
    expect(matches("owner:none")).toBe(false);
    expect(matches("owner:none", { owner: "  " })).toBe(true);
    expect(matches("owner:unassigned", { owner: null })).toBe(true);
    expect(matches('label:"needs review"')).toBe(true);
    expect(matches("label:none")).toBe(false);
    expect(matches("label:none", { labels: [" "] })).toBe(true);
    expect(matches("severity:high")).toBe(true);
    expect(matches("severity:none", { severity: null })).toBe(true);
  });

  it("matches a repository by name, path or id, across every linked repository", () => {
    expect(matches("repo:gitpulse")).toBe(true);
    expect(matches("repo:pulse")).toBe(true);
    expect(matches("repo:acme/manvi")).toBe(true);
    expect(matches("repo:r2")).toBe(true);
    expect(matches("repo:liquitask")).toBe(false);
    expect(matches("-repo:manvi")).toBe(false);
  });

  it("does not match every repository through the path parts they all share", () => {
    // Every local identity ends in `/.git` and most sit under /Users/<name>:
    // a substring match on the path made `repo:git` and `repo:users` match all.
    const local: QueryContext = { nowSec: 50, repository: (id) => ({
      r1: { name: "GitPulse", identity_key: "local:/Users/me/Code/GitPulse/.git" },
      r2: { name: "Manvi", identity_key: "local:/Users/me/Code/Manvi/.git" },
    } as Record<string, { name: string; identity_key: string }>)[id] };
    expect(matches("repo:git", { repository_ids: ["r2"] }, local)).toBe(false);
    expect(matches("repo:git", { repository_ids: ["r1"] }, local)).toBe(true);
    expect(matches("repo:co", { repository_ids: ["r2"] }, local)).toBe(false);
    // A whole path part still names it, and a path with a slash matches as typed.
    expect(matches("repo:code", { repository_ids: ["r2"] }, local)).toBe(true);
    expect(matches("repo:code/manvi", { repository_ids: ["r2"] }, local)).toBe(true);
    expect(matches("repo:acme", { repository_ids: ["r2"] })).toBe(true);
  });

  it("reads GitHub's is:open, is:closed, is:unassigned and is:overdue as the board's filters", () => {
    expect(parseTaskQuery("is:open").terms[0]).toMatchObject({ key: "status", value: "open" });
    expect(matches("is:open")).toBe(true);
    expect(matches("is:closed")).toBe(false);
    expect(matches("is:closed", { status: "done" })).toBe(true);
    expect(matches("is:done", { status: "done" })).toBe(true);
    expect(matches("is:unassigned", { owner: null })).toBe(true);
    expect(matches("is:unassigned")).toBe(false);
    expect(matches("is:overdue", {}, { ...ctx, nowSec: 200 })).toBe(true);
    expect(matches("-is:open", { status: "done" })).toBe(true);
  });

  it("matches only an exact id for a repository the catalog has not loaded", () => {
    const blind: QueryContext = { repository: () => undefined, nowSec: 50 };
    expect(matches("repo:gitpulse", {}, blind)).toBe(false);
    expect(matches("repo:r1", {}, blind)).toBe(true);
  });

  it("matches due states against the board's clock", () => {
    expect(matches("due:soon")).toBe(true);
    expect(matches("due:any")).toBe(true);
    expect(matches("due:overdue")).toBe(false);
    expect(matches("due:overdue", {}, { ...ctx, nowSec: 200 })).toBe(true);
    expect(matches("due:none", { due_at: null })).toBe(true);
    expect(matches("due:any", { due_at: 0 })).toBe(false);
    expect(matches("due:later", { due_at: 50 + 30 * 24 * 3600 })).toBe(true);
  });

  it("passes every card when the search has no filters", () => {
    expect(matches("")).toBe(true);
    expect(matches("crash")).toBe(true);
    expect(matches("priority:huge")).toBe(true);
  });
});

describe("removeQueryTerm and toggleQueryTerm", () => {
  it("removes exactly the chip's span and tidies the spacing", () => {
    const search = "crash  is:bug   owner:pat";
    const [kind] = parseTaskQuery(search).terms;
    expect(removeQueryTerm(search, kind)).toBe("crash owner:pat");
  });

  it("leaves the search alone when the span no longer holds the term", () => {
    const [kind] = parseTaskQuery("is:bug").terms;
    expect(removeQueryTerm("is:feature", kind)).toBe("is:feature");
  });

  it("adds a missing filter and removes every spelling of a present one", () => {
    expect(toggleQueryTerm("", "is:bug")).toBe("is:bug");
    expect(toggleQueryTerm("crash", "is:bug")).toBe("crash is:bug");
    expect(toggleQueryTerm("  type:Bug crash kind:bug ", "is:bug")).toBe("crash");
    expect(toggleQueryTerm("priority:p0", "priority:urgent")).toBe("");
  });

  it("does not confuse a negation with the filter it negates", () => {
    expect(toggleQueryTerm("-is:bug", "is:bug")).toBe("-is:bug is:bug");
    expect(queryHas(parseTaskQuery("-is:bug"), "is:bug")).toBe(false);
    expect(queryHas(parseTaskQuery("type:BUG"), "is:bug")).toBe(true);
  });

  it("refuses an unreadable filter and one that would overflow the store's limit", () => {
    expect(toggleQueryTerm("crash", "priority:huge")).toBe("crash");
    expect(toggleQueryTerm("crash", "nonsense")).toBe("crash");
    const full = "x".repeat(MAX_QUERY_TEXT - 2);
    expect(toggleQueryTerm(full, "is:bug")).toBe(full);
  });
});

describe("words, phrases, OR and comma lists", () => {
  it("reads words as clauses that must all match, folded for ranking", () => {
    const parsed = parseTaskQuery("Login crash");
    expect(parsed.words).toEqual({ clauses: [["login"], ["crash"]], phrases: [] });
    expect(parsed.text).toBe("Login crash");
    expect(isEmptyQuery(parsed)).toBe(false);
  });

  it("joins words around OR into one clause and keeps the alternatives away from the store", () => {
    const parsed = parseTaskQuery("login OR signin crash");
    expect(parsed.words?.clauses).toEqual([["login", "signin"], ["crash"]]);
    // A prefix-AND store would demand both alternatives.
    expect(parsed.text).toBe("crash");
    expect(parsed.problems).toEqual([]);
    expect(parseTaskQuery("bug | crash").words?.clauses).toEqual([["bug", "crash"]]);
    // Lower-case "or" is a word, and a stopword.
    expect(parseTaskQuery("login or signin").words?.clauses).toEqual([["login"], ["signin"]]);
  });

  it("reports an OR with nothing to join instead of dropping it silently", () => {
    for (const query of ["OR crash", "crash OR", "crash OR is:bug", "is:bug OR crash"]) {
      const parsed = parseTaskQuery(query);
      expect(parsed.problems.join(" "), query).toMatch(/OR joins two words/);
      expect(parsed.text.split(" "), query).toContain("crash");
    }
    // A doubled OR is reported once and the words still alternate.
    const doubled = parseTaskQuery("crash OR OR fix");
    expect(doubled.problems).toHaveLength(1);
    expect(doubled.words?.clauses).toEqual([["crash", "fix"]]);
  });

  it("splits an OR alternative of several words like the branch did: first word alternates, the rest are required", () => {
    expect(parseTaskQuery("crash OR dark-mode").words?.clauses).toEqual([["crash", "dark"], ["mode"]]);
  });

  it("reads quoted phrases for ranking and still sends their words to the store", () => {
    const parsed = parseTaskQuery('the "Sign In" crash');
    expect(parsed.words).toEqual({ clauses: [["crash"]], phrases: ["sign in"] });
    expect(parsed.text).toBe('the Sign In crash');
  });

  it("searches stopwords only when they are all there is", () => {
    expect(parseTaskQuery("the crash").words?.clauses).toEqual([["crash"]]);
    expect(parseTaskQuery("the").words?.clauses).toEqual([["the"]]);
  });

  it("reads a comma list as any of its values, one term per value sharing the token's span", () => {
    const parsed = parseTaskQuery("label:ui,ci,ui priority:urgent,p1");
    expect(parsed.terms.map((term) => [term.key, term.value, term.raw])).toEqual([
      ["label", "ui", "label:ui,ci,ui"], ["label", "ci", "label:ui,ci,ui"],
      ["priority", "0", "priority:urgent,p1"], ["priority", "1", "priority:urgent,p1"],
    ]);
    expect(parseTaskQuery('owner:"Smith, Jo"').terms.map((term) => term.value)).toEqual(["smith, jo"]);
    expect(matches("label:ci,ui")).toBe(true);
    expect(matches("label:ci,docs")).toBe(false);
    expect(matches("-label:ci,ui")).toBe(false);
    expect(matches("priority:urgent,high")).toBe(true);
  });

  it("applies the readable values of a list and reports the rest", () => {
    const parsed = parseTaskQuery("priority:high,huge");
    expect(parsed.terms.map((term) => term.value)).toEqual(["1"]);
    expect(parsed.problems[0]).toMatch(/priority:huge is not a priority/);
  });

  it("warns about a key that is not a filter, but not about a URL or a time", () => {
    expect(parseTaskQuery("todo:fix").problems[0]).toMatch(/todo: is not a filter/);
    expect(parseTaskQuery("https://x.y 10:30 note:").problems).toEqual([]);
  });

  it("is empty only with nothing to search or filter by", () => {
    for (const query of ["", "   ", "!!! ...", "is:", "OR"]) expect(isEmptyQuery(parseTaskQuery(query)), query).toBe(true);
    expect(isEmptyQuery(parseTaskQuery("is:bug"))).toBe(false);
    expect(isEmptyQuery(parseTaskQuery('"x"'))).toBe(false);
  });

  it("never throws on odd input", () => {
    for (const text of ['"', '-"', "-", ":", "-:", "a:", '"unterminated phrase', "\u0000\u{1F600}", "OR OR OR", "label:,,,", 'label:"', "|"]) {
      expect(() => parseTaskQuery(text), text).not.toThrow();
    }
  });
});

describe("chips and toggles over comma lists", () => {
  it("draws one chip per token", () => {
    expect(queryChips(parseTaskQuery("label:ui,ci is:bug")).map((term) => term.raw)).toEqual(["label:ui,ci", "is:bug"]);
  });

  it("toggles one value out of a list and keeps the rest as typed", () => {
    expect(toggleQueryTerm("crash is:bug,feature", "is:bug")).toBe("crash is:feature");
    expect(toggleQueryTerm("is:feature,bug,research", "type:bug")).toBe("is:feature,research");
    expect(toggleQueryTerm("is:bug,bugs", "is:bug")).toBe("is:bugs");
    expect(toggleQueryTerm("priority:huge,urgent", "priority:p0")).toBe("priority:huge");
    expect(toggleQueryTerm("label:ui,ci", "label:ui")).toBe("label:ci");
  });
});

describe("completions", () => {
  const sources = { label: ["ui", "a11y", "needs design"], kind: ["improvement"] };

  it("completes key names from two letters", () => {
    expect(suggest("la", sources).map((s) => s.insert)).toEqual(["label:"]);
    expect(suggest("-re", sources).map((s) => s.insert)).toEqual(["-repo:"]);
    expect(suggest("du", sources).map((s) => s.insert)).toEqual(["due:"]);
    expect(suggest("l", sources)).toEqual([]);
    expect(suggest("label", sources)).toEqual([]);
    expect(suggest("login", sources)).toEqual([]);
  });

  it("completes values, prefix matches first, after the last comma, quoting as needed", () => {
    expect(suggest("label:", sources).map((s) => s.insert)).toEqual(["label:ui", "label:a11y", 'label:"needs design"', "label:none"]);
    expect(suggest("tag:ne", sources)[0]).toEqual({ label: "tag:needs design", detail: "", insert: 'tag:"needs design"' });
    expect(suggest("label:ui,a", sources).map((s) => s.insert)).toEqual(["label:ui,a11y"]);
    expect(suggest("label:ui,d", sources).map((s) => s.insert)).toEqual(['label:ui,"needs design"']);
    expect(suggest("label:ui", sources)).toEqual([]);
  });

  it("offers fixed vocabularies and the board's own is: words without loaded tasks", () => {
    expect(suggest("status:in", {}).map((s) => s.insert)).toEqual(["status:inbox", "status:in_progress"]);
    expect(suggest("is:", sources).map((s) => s.insert)).toEqual(["is:open", "is:closed", "is:done", "is:unassigned", "is:overdue", "is:improvement"]);
    expect(suggest("priority:", {}).length).toBe(4);
    expect(suggest("due:", {}).map((s) => s.insert)).toEqual(["due:overdue", "due:soon", "due:later", "due:none", "due:any"]);
    expect(suggest("nope:x", {})).toEqual([]);
  });

  it("every completion parses as the filter it names", () => {
    for (const option of [...suggest("label:", sources), ...suggest("is:", sources), ...suggest("status:", {}), ...suggest("due:", {})]) {
      const parsed = parseTaskQuery(option.insert);
      expect(parsed.problems, option.insert).toEqual([]);
      expect(parsed.terms.length, option.insert).toBeGreaterThan(0);
    }
  });

  it("finds the token under the caret and replaces it", () => {
    const at = tokenAtCaret("crash label:u", 13);
    expect(at?.prefix).toBe("label:u");
    expect(replaceToken("crash label:u", at?.index ?? -1, "label:ui")).toEqual({ text: "crash label:ui ", caret: 15 });
    expect(replaceToken("la crash", 0, "label:")).toEqual({ text: "label: crash", caret: 6 });
    expect(replaceToken("label:u crash", 0, "label:ui")).toEqual({ text: "label:ui crash", caret: 9 });
    expect(tokenAtCaret("crash ", 6)).toBeNull();
  });
});

describe("due date ranges", () => {
  // Local midnight, as the board's own day boundaries are, so the expectations
  // hold in whatever zone the suite runs in.
  const at = (y: number, m: number, d: number, h = 0, min = 0) => Math.floor(new Date(y, m - 1, d, h, min).getTime() / 1000);
  const due = (dueAt: number | null) => ({ due_at: dueAt });

  it("matches a due time inside the range, both end days included", () => {
    const q = "due:2026-11-01..2026-11-30";
    expect(matches(q, due(at(2026, 11, 15, 12)))).toBe(true);
    expect(matches(q, due(at(2026, 11, 1, 0, 0)))).toBe(true);
    expect(matches(q, due(at(2026, 11, 30, 23, 59)))).toBe(true);
  });

  it("rejects a due time outside the range and a task with no due time", () => {
    const q = "due:2026-11-01..2026-11-30";
    expect(matches(q, due(at(2026, 10, 31, 23, 59)))).toBe(false);
    expect(matches(q, due(at(2026, 12, 1, 0, 0)))).toBe(false);
    expect(matches(q, due(null))).toBe(false);
    expect(matches(q, due(0))).toBe(false);
  });

  it("leaves an end open and excludes with a minus", () => {
    expect(matches("due:2026-11-01..", due(at(2030, 1, 1)))).toBe(true);
    expect(matches("due:2026-11-01..", due(at(2026, 10, 31, 23)))).toBe(false);
    expect(matches("due:..2026-11-30", due(at(2026, 1, 1)))).toBe(true);
    expect(matches("due:..2026-11-30", due(at(2026, 12, 1)))).toBe(false);
    expect(matches("-due:2026-11-01..2026-11-30", due(at(2026, 12, 5)))).toBe(true);
    expect(matches("-due:2026-11-01..2026-11-30", due(at(2026, 11, 5)))).toBe(false);
  });

  it("is one of several due alternatives, widening like any other list", () => {
    expect(matches("due:overdue,2026-11-01..2026-11-30", due(at(2026, 11, 15)), { ...ctx, nowSec: 5 })).toBe(true);
    expect(matches("due:overdue,2026-11-01..2026-11-30", due(at(2027, 3, 1)), { ...ctx, nowSec: 5 })).toBe(false);
  });

  it("reports a malformed range, applies nothing and keeps the words", () => {
    for (const bad of ["due:2026-11-30..2026-11-01", "due:2026-02-30..2026-03-01", "due:..", "due:2026-11-01...2026-11-30", "due:2026-11..2026-12", "due:2026-11-01..soon"]) {
      const parsed = parseTaskQuery(`crash ${bad}`);
      expect(parsed.terms, bad).toEqual([]);
      expect(parsed.text, bad).toBe("crash");
      expect(parsed.problems, bad).toHaveLength(1);
      expect(parsed.problems[0], bad).toMatch(/is not a date range/);
    }
    expect(parseTaskQuery("due:2026-11-30..2026-11-01").problems[0]).toMatch(/ends before it starts/);
    // An unreadable range filters nothing rather than emptying the board.
    expect(matches("due:2026-11-30..2026-11-01")).toBe(true);
  });

  it("reads one canonical term however the range was spelled, so a chip and a toggle agree", () => {
    const [term] = parseTaskQuery("DUE:2026-11-01..").terms;
    expect(term).toMatchObject({ key: "due", value: "2026-11-01..", negated: false });
    expect(queryHas(parseTaskQuery("due:2026-11-01.. crash"), "due:2026-11-01..")).toBe(true);
    expect(toggleQueryTerm("crash due:2026-11-01..", "due:2026-11-01..")).toBe("crash");
  });
});

describe("the Filters bar's controls over the search", () => {
  const value = (search: string, control: "status" | "due-range") => queryControlValue(parseTaskQuery(search), control);

  it("shows none, one, or too many values for a control", () => {
    expect(value("crash", "status")).toBe("");
    expect(value("status:review crash", "status")).toBe("review");
    expect(value("status:review,ready", "status")).toBeNull();
    expect(value("-status:done", "status")).toBeNull();
    expect(value("due:overdue", "due-range")).toBe("");
    expect(value("due:2026-11-01..2026-11-30", "due-range")).toBe("2026-11-01..2026-11-30");
    expect(value("due:2026-11-01.. due:..2026-12-01", "due-range")).toBeNull();
  });

  it("replaces the control's filter and leaves the rest of the search alone", () => {
    expect(setQueryControl("crash is:bug", "status", "status:review")).toBe("crash is:bug status:review");
    expect(setQueryControl("crash status:ready is:bug", "status", "status:review")).toBe("crash is:bug status:review");
    expect(setQueryControl("crash status:ready,review", "status", "")).toBe("crash");
    expect(setQueryControl("crash due:overdue due:2026-11-01..", "due-range", "due:2026-11-01..2026-11-30")).toBe("crash due:overdue due:2026-11-01..2026-11-30");
    expect(setQueryControl("due:2026-11-01..2026-11-30", "due-range", "")).toBe("");
  });

  it("refuses a change that would push the search past the store's limit", () => {
    const long = "x".repeat(MAX_QUERY_TEXT - 4);
    expect(setQueryControl(long, "status", "status:review")).toBe(long);
  });

  it("does not show one range of a list that also holds a named value, and never loses the named value", () => {
    // `overdue OR from the 1st` is not "from the 1st": the control reads it as several.
    expect(value("due:overdue,2026-11-01..", "due-range")).toBeNull();
    expect(value("due:2026-11-01..2026-11-30,overdue", "due-range")).toBeNull();
    expect(value("is:bug,open", "status")).toBeNull();
    // Even if the control is written to, the list's other values stay.
    expect(setQueryControl("due:overdue,2026-11-01..", "due-range", "due:2026-12-01..")).toBe("due:overdue due:2026-12-01..");
    expect(setQueryControl("due:overdue,2026-11-01..", "due-range", "")).toBe("due:overdue");
    expect(setQueryControl("crash is:bug,open", "status", "status:review")).toBe("crash is:bug status:review");
  });

  it("reports several for a control's key spread over more than one term, in any form", () => {
    // Repeated keys widen to OR, so the range beside `overdue` is not the range alone.
    expect(value("due:overdue due:2026-11-01..", "due-range")).toBeNull();
    expect(value("due:2026-11-01.. due:overdue", "due-range")).toBeNull();
    expect(value("due:2026-11-01..2026-11-30 -due:overdue", "due-range")).toBeNull();
    expect(value("due:overdue due:soon", "due-range")).toBeNull();
    expect(value("status:review status:ready", "status")).toBeNull();
    expect(value("status:review -status:done", "status")).toBeNull();
    expect(value("status:review is:open", "status")).toBeNull();
    // One term of the key, or terms of other keys, are still one value.
    expect(value("status:review due:overdue due:soon", "status")).toBe("review");
    expect(value("due:2026-11-01.. status:review status:ready", "due-range")).toBe("2026-11-01..");
  });

  it("writes a range the parser accepts however the two days were typed", () => {
    expect(dueRangeFilter("", "")).toBe("");
    expect(dueRangeFilter("2026-11-01", "")).toBe("due:2026-11-01..");
    expect(dueRangeFilter("", "2026-11-30")).toBe("due:..2026-11-30");
    expect(dueRangeFilter("2026-11-30", "2026-11-01")).toBe("due:2026-11-01..2026-11-30");
    const parsed = parseTaskQuery(dueRangeFilter("2026-11-30", "2026-11-01"));
    expect(parsed.problems).toEqual([]);
    expect(queryControlValue(parsed, "due-range")).toBe("2026-11-01..2026-11-30");
  });

  it("takes a refused range out of the search instead of piling a second one beside it", () => {
    // `due:B..A` is reported and not applied, so the control sees none; the next write must still clear it.
    const stuck = "crash due:2026-11-30..2026-11-01";
    expect(queryControlValue(parseTaskQuery(stuck), "due-range")).toBe("");
    expect(setQueryControl(stuck, "due-range", "due:2026-11-05..")).toBe("crash due:2026-11-05..");
    expect(setQueryControl(stuck, "due-range", "")).toBe("crash");
    // A named value beside the refused one stays.
    expect(setQueryControl("due:overdue,2026-11-30..2026-11-01", "due-range", "")).toBe("due:overdue");
    // A refused value of another key is not the range's to clear.
    expect(setQueryControl("status:nope due:2026-11-30..2026-11-01", "due-range", "")).toBe("status:nope");
  });

  it("is kept by a saved view, because the view keeps the search, and filters the board when applied", () => {
    const search = "crash status:review due:2026-11-01..2026-11-30";
    const saved = sanitizeSnapshot({ search });
    expect(saved.search).toBe(search);
    const parsed = parseTaskQuery(saved.search);
    expect(queryControlValue(parsed, "status")).toBe("review");
    expect(queryControlValue(parsed, "due-range")).toBe("2026-11-01..2026-11-30");
    const at = (y: number, m: number, d: number) => Math.floor(new Date(y, m - 1, d, 12).getTime() / 1000);
    const hit = card({ title: "Crash on save", status: "review", due_at: at(2026, 11, 10) });
    expect(cardMatchesQuery(hit, parsed, ctx)).toBe(true);
    expect(cardMatchesQuery({ ...hit, status: "ready" }, parsed, ctx)).toBe(false);
    expect(cardMatchesQuery({ ...hit, due_at: at(2026, 12, 10) }, parsed, ctx)).toBe(false);
  });
});
