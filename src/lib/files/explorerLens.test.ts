import { afterEach, describe, expect, it } from "vitest";
import { get } from "svelte/store";
import {
  consumeSearchFocus,
  explorerLens,
  resetExplorerLens,
  showExplorerRail,
  toggleExplorerSearch,
} from "./explorerLens";

afterEach(() => resetExplorerLens());

describe("explorerLens search focus requests", () => {
  it("asks for focus once when Search is shown", () => {
    showExplorerRail("search");
    expect(get(explorerLens).searchFocusPending).toBe(true);
    expect(consumeSearchFocus()).toBe(true);
    expect(consumeSearchFocus()).toBe(false);
  });

  it("withdraws an unconsumed search focus request when the tree comes back", () => {
    showExplorerRail("search");
    showExplorerRail("files");
    expect(get(explorerLens)).toMatchObject({ rail: "files", searchFocusPending: false });
    expect(consumeSearchFocus()).toBe(false);
  });

  it("toggles between the tree and Search with the same focus rule", () => {
    toggleExplorerSearch();
    expect(get(explorerLens)).toMatchObject({ rail: "search", searchFocusPending: true });
    toggleExplorerSearch();
    expect(get(explorerLens)).toMatchObject({ rail: "files", searchFocusPending: false });
  });
});
