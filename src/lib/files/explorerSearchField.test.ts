import { describe, expect, it } from "vitest";
import {
  CONTENTS_TOGGLE_LABEL,
  contentsToggleTitle,
  fieldEscape,
  searchFieldLabel,
  toggledSearchMode,
} from "./explorerSearchField";

describe("explorer search field modes", () => {
  it("has one contents toggle that swaps the names and contents rails", () => {
    expect(toggledSearchMode("files")).toBe("search");
    expect(toggledSearchMode("search")).toBe("files");
  });

  it("names the toggle once and lets its tooltip say what a click does", () => {
    expect(CONTENTS_TOGGLE_LABEL).toBe("Include file contents");
    expect(contentsToggleTitle("files")).toMatch(/inside files/);
    expect(contentsToggleTitle("search")).toMatch(/file name/);
  });

  it("labels the field by what it does in each mode", () => {
    expect(searchFieldLabel("files")).toBe("Filter files");
    expect(searchFieldLabel("search")).toBe("Search file contents");
  });

  it("clears typed text first in either mode", () => {
    expect(fieldEscape("files", "foo")).toBe("clear");
    expect(fieldEscape("search", "foo")).toBe("clear");
  });

  it("leaves Contents mode on Escape only once the field is empty", () => {
    expect(fieldEscape("search", "")).toBe("leave-contents");
    expect(fieldEscape("files", "")).toBe("none");
  });
});
