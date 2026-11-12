import { describe, expect, it } from "vitest";
import { VIEW_TABS } from "../repos/persist";
import {
  COMMIT_FILTER_SECTION,
  FOCUS_PAGE_SEARCH_EVENT,
  FOCUS_SUSPECTS_SYMPTOM_EVENT,
  fieldOwnsFindChord,
  findChordTarget,
  isCommitSearchChord,
  showsCommitFilter,
  tabForCommitSearch,
} from "./commitFilter";

describe("showsCommitFilter", () => {
  it("is on for History, the one view it filters", () => {
    // It used to be true for five views. Three of those are sections of
    // History now, and the bar moved into History's section bar, where it
    // filters the single walk all three sections are drawn from.
    expect(showsCommitFilter("history")).toBe(true);
  });

  it("is off for Work and every other surface", () => {
    expect(showsCommitFilter("work")).toBe(false);
    expect(showsCommitFilter("code")).toBe(false);
    expect(showsCommitFilter("insights")).toBe(false);
  });

  it("decides every registered view, so a new tab cannot inherit the bar silently", () => {
    for (const tab of VIEW_TABS) {
      expect(typeof showsCommitFilter(tab)).toBe("boolean");
    }
  });
});

describe("commit-search chord", () => {
  it("matches unmodified ⌘F / Ctrl+F only", () => {
    expect(isCommitSearchChord({ metaKey: true, key: "f" })).toBe(true);
    expect(isCommitSearchChord({ ctrlKey: true, key: "F" })).toBe(true);
    expect(isCommitSearchChord({ metaKey: true, shiftKey: true, key: "f" })).toBe(false);
    expect(isCommitSearchChord({ metaKey: true, altKey: true, key: "f" })).toBe(false);
    expect(isCommitSearchChord({ key: "f" })).toBe(false);
  });

  it("hands the chord to the view bar's field on every page of an open repository", () => {
    // Code and History's diff used to catch ⌘F in their own toolbars, so the
    // chord meant three things depending on the pane. The field is one control
    // now and the page decides what it does, so ownership no longer asks which
    // view or section is showing.
    expect(fieldOwnsFindChord({ hasRepository: true, onRepositorySurface: true, defaultPrevented: false })).toBe(true);
  });

  it("leaves the chord alone where the field is not on screen", () => {
    // No repository: row 2 has no field. Fleet, Agents or Tasks: row 2 is
    // inert under the surface's name, so focusing it would land on a hidden
    // control.
    expect(fieldOwnsFindChord({ hasRepository: false, onRepositorySurface: true, defaultPrevented: false })).toBe(false);
    expect(fieldOwnsFindChord({ hasRepository: true, onRepositorySurface: false, defaultPrevented: false })).toBe(false);
  });

  it("leaves a chord another pane already took", () => {
    // The terminal's own find marks ⌘F handled; the field must not steal it.
    expect(fieldOwnsFindChord({ hasRepository: true, onRepositorySurface: true, defaultPrevented: true })).toBe(false);
  });

  it("sends native Search Commits to History's graph, where the field filters commits", () => {
    // ⌘F focuses the field wherever it is; the menu item asks for the commit
    // filter by name, so it must land where the field is that filter.
    expect(tabForCommitSearch("work")).toBe("history");
    expect(tabForCommitSearch("insights")).toBe("history");
    expect(tabForCommitSearch("history")).toBe("history");
    expect(tabForCommitSearch("code")).toBe("history");
    expect(COMMIT_FILTER_SECTION).toBe("graph");
    expect(FOCUS_PAGE_SEARCH_EVENT).toBe("gitpulse:focus-page-search");
  });
});

describe("findChordTarget", () => {
  it("sends ⌘F to the Suspects list's symptom on Graph's Suspects list", () => {
    // The commit filter does not apply to a ranking (one answer, not a list
    // text narrows), so the Suspects list asks its question in the context bar
    // and ⌘F focuses that question's symptom, as it did before the field.
    expect(findChordTarget({ tab: "history", section: "graph", historyList: "suspects" })).toBe("suspects-symptom");
    expect(FOCUS_SUSPECTS_SYMPTOM_EVENT).toBe("gitpulse:focus-suspects-symptom");
  });

  it("sends it to the view bar's field everywhere else, Graph's other lists and Diff included", () => {
    expect(findChordTarget({ tab: "history", section: "graph", historyList: "commits" })).toBe("page-search");
    expect(findChordTarget({ tab: "history", section: "graph", historyList: "moves" })).toBe("page-search");
    // The list is remembered while Diff is showing; Diff's field is the diff's find.
    expect(findChordTarget({ tab: "history", section: "diff", historyList: "suspects" })).toBe("page-search");
    expect(findChordTarget({ tab: "code", section: "explorer", historyList: "suspects" })).toBe("page-search");
    expect(findChordTarget({ tab: "work", section: null, historyList: undefined })).toBe("page-search");
  });
});
