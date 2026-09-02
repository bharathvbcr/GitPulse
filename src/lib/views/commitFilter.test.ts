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
  it("is on for views the commit search actually filters", () => {
    expect(showsCommitFilter("history")).toBe(true);
    expect(showsCommitFilter("diff")).toBe(true);
    expect(showsCommitFilter("blame")).toBe(true);
    expect(showsCommitFilter("stack")).toBe(true);
    expect(showsCommitFilter("reflog")).toBe(true);
  });

  it("is off for Work and every other non-history surface", () => {
    expect(showsCommitFilter("work")).toBe(false);
    expect(showsCommitFilter("files")).toBe(false);
    expect(showsCommitFilter("conflict")).toBe(false);
    expect(showsCommitFilter("coverage")).toBe(false);
    expect(showsCommitFilter("health")).toBe(false);
    expect(showsCommitFilter("storage")).toBe(false);
    expect(showsCommitFilter("terminal")).toBe(false);
    expect(showsCommitFilter("github")).toBe(false);
    expect(showsCommitFilter("manvi")).toBe(false);
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

  it("leaves Files to the in-file search, and takes every other view", () => {
    expect(ownsCommitSearchChord("files")).toBe(false);
    for (const tab of VIEW_TABS) {
      if (tab === "files") continue;
      expect(ownsCommitSearchChord(tab), tab).toBe(true);
    }
  });

  it("switches Work (and any other non-filter view) onto Graph so the bar exists", () => {
    // The failure this prevents: ⌘F on Work dispatched a focus event no
    // listener heard, because FilterBar is unmounted there.
    expect(tabForCommitSearch("work")).toBe("history");
    expect(tabForCommitSearch("github")).toBe("history");
    expect(tabForCommitSearch("history")).toBe("history");
    expect(tabForCommitSearch("diff")).toBe("diff");
    expect(FOCUS_COMMIT_SEARCH_EVENT).toBe("gitpulse:focus-filter");
  });
});
