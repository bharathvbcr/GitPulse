import { describe, expect, it } from "vitest";
import { EXPLORER_SEARCH_MODES, fieldEscape, searchFieldLabel } from "./explorerSearchField";

describe("explorer search field modes", () => {
  it("offers Names then Contents, mapped onto the files and search rails", () => {
    expect(EXPLORER_SEARCH_MODES.map((m) => [m.value, m.label])).toEqual([
      ["files", "Names"],
      ["search", "Contents"],
    ]);
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
