import { describe, expect, it } from "vitest";
import { VIEW_TABS } from "../repos/persist";
import {
  FOCUS_COMMIT_SEARCH_EVENT,
  isCommitSearchChord,
  ownsCommitSearchChord,
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

  it("leaves Code to the in-file search, in both of its sections", () => {
    // Blame is a section of Code and its lines are the Explorer's lines, so
    // ⌘F must not mean one thing on a file and something else on that same
    // file one click later.
    expect(ownsCommitSearchChord("code")).toBe(false);
    expect(ownsCommitSearchChord("code", "explorer")).toBe(false);
    expect(ownsCommitSearchChord("code", "blame")).toBe(false);
  });

  it("leaves History's diff to its own find bar, and keeps the rest", () => {
    // Graph and Reflog are lists of commits, which the filter narrows. Diff
    // is a file — or a commit's worth of files — and the chord belongs to the
    // find bar inside it, for the same reason Code owns it.
    expect(ownsCommitSearchChord("history", "diff")).toBe(false);
    expect(ownsCommitSearchChord("history", "graph")).toBe(true);
    expect(ownsCommitSearchChord("history", "reflog")).toBe(true);
    expect(ownsCommitSearchChord("history")).toBe(true);
  });

  it("takes the chord for every other view, whatever section is open", () => {
    for (const tab of VIEW_TABS) {
      if (tab === "code" || tab === "history") continue;
      expect(ownsCommitSearchChord(tab), tab).toBe(true);
      expect(ownsCommitSearchChord(tab, "diff"), tab).toBe(true);
    }
  });

  it("switches Work (and any other non-filter view) onto History so the bar exists", () => {
    // The failure this prevents: ⌘F on Work dispatched a focus event no
    // listener heard, because FilterBar is unmounted there.
    expect(tabForCommitSearch("work")).toBe("history");
    expect(tabForCommitSearch("insights")).toBe("history");
    expect(tabForCommitSearch("history")).toBe("history");
    expect(tabForCommitSearch("code")).toBe("history");
    expect(FOCUS_COMMIT_SEARCH_EVENT).toBe("gitpulse:focus-filter");
  });
});
