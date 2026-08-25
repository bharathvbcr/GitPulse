import { describe, it, expect, beforeEach } from "vitest";
import { get } from "svelte/store";
import { filterStore } from "../filterStore";

describe("filterStore", () => {
  beforeEach(() => {
    filterStore.clear();
  });

  it("updates search query correctly", () => {
    expect(get(filterStore).searchQuery).toBe("");
    filterStore.setSearch("feat(auth)");
    expect(get(filterStore).searchQuery).toBe("feat(auth)");
  });

  it("selects and clears branch filter", () => {
    filterStore.selectBranch("feature/awesome");
    expect(get(filterStore).selectedBranch).toBe("feature/awesome");
    filterStore.clear();
    expect(get(filterStore).selectedBranch).toBeNull();
    expect(get(filterStore).searchQuery).toBe("");
  });
});
