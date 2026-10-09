import { describe, expect, it } from "vitest";
import type { TaskCard } from "./client";
import {
  cardMatchesQuery,
  MAX_QUERY_TERMS,
  MAX_QUERY_TEXT,
  parseTaskQuery,
  queryHas,
  removeQueryTerm,
  toggleQueryTerm,
  type QueryContext,
} from "./taskQuery";

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
