import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import { get } from "svelte/store";
import PageSearch from "./PageSearch.svelte";
import { repoStore } from "../stores/repoStore";
import { activeSectionFor } from "../views/viewRegistry";
import {
  pageScope,
  registerPageSearch,
  registerStandingFilter,
  type FindProvider,
} from "../search/pageSearch.svelte";

const source = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "PageSearch.svelte"), "utf8");

/** The place the field reads, from the store as the field itself reads it. */
function currentScope() {
  const state = get(repoStore);
  return pageScope(state.activeTab, activeSectionFor(state.activeTab, state.viewSections));
}

function findProvider(query: string, status = { text: "2 of 5", invalid: false, hasMatches: true }): FindProvider {
  return {
    kind: "find",
    label: "Find in this diff",
    placeholder: "Find in diff…",
    query: () => query,
    setQuery: () => {},
    caseSensitive: () => true,
    setCaseSensitive: () => {},
    regex: () => false,
    setRegex: () => {},
    status: () => status,
    step: () => {},
  };
}

describe("PageSearch", () => {
  it("searches the repository through the palette where the page has nothing of its own", () => {
    const { body } = render(PageSearch);
    expect(body).toContain('id="gitpulse-page-search"');
    expect(body).toContain('data-page-search="palette"');
    expect(body).toContain('role="search"');
    expect(body).toContain('role="combobox"');
    expect(body).toContain("Search this repository: commands, / files, @ branches, # commits");
    // Unfocused and empty, it shows the chord that reaches it.
    expect(body).toContain("gp-keycap");
    // No find controls without a find.
    expect(body).not.toContain('aria-label="Next match"');
  });

  it("becomes the page's filter, with its query and second narrowing inside it", () => {
    const dispose = registerPageSearch(currentScope(), {
      kind: "filter",
      label: "Filter commits",
      placeholder: "Filter commits by message, author or path",
      query: () => "author:alice",
      setQuery: () => {},
      chip: () => ({ label: "feature/x", title: "Clear branch filter", clear: () => {} }),
    });
    try {
      const { body } = render(PageSearch);
      expect(body).toContain('data-page-search="filter"');
      expect(body).toContain('aria-label="Filter commits"');
      expect(body).toContain("Filter commits by message, author or path");
      expect(body).toContain('value="author:alice"');
      expect(body).toContain("feature/x");
      expect(body).toContain('title="Clear branch filter"');
      expect(body).toContain('aria-label="Clear search"');
    } finally {
      dispose();
    }
  });

  it("becomes a find in a diff or a file: count, case, pattern, previous and next", () => {
    const dispose = registerPageSearch(currentScope(), findProvider("needle"));
    try {
      const { body } = render(PageSearch);
      expect(body).toContain('data-page-search="find"');
      expect(body).toContain("2 of 5");
      expect(body).toMatch(/aria-pressed="true"[^>]*title="Match case"/);
      expect(body).toMatch(/aria-pressed="false"[^>]*title="Regular expression"/);
      expect(body).toContain('title="Regular expression"');
      expect(body).toContain('aria-label="Previous match"');
      expect(body).toContain('aria-label="Next match"');
    } finally {
      dispose();
    }
  });

  it("reports a bad pattern as such, and disables stepping with nothing to step to", () => {
    const dispose = registerPageSearch(
      currentScope(),
      findProvider("(", { text: "bad pattern", invalid: true, hasMatches: false }),
    );
    try {
      const { body } = render(PageSearch);
      expect(body).toMatch(/class="[^"]*text-danger[^"]*"[^>]*data-find-status[^>]*>bad pattern/);
      expect(body).toMatch(/disabled[^>]*aria-label="Previous match"|aria-label="Previous match"[^>]*disabled/);
    } finally {
      dispose();
    }
  });

  it("keeps an applied filter in sight where the field is not that filter", () => {
    // History's commit filter also narrows the diff's commit list; a find
    // there must not hide that the list is narrowed.
    const view = get(repoStore).activeTab;
    const disposers = [
      registerPageSearch(currentScope(), findProvider("")),
      registerStandingFilter(view, () => ({ label: "Commits: “fix”", title: "The commit filter applied in Graph. Clear it", clear: () => {} })),
    ];
    try {
      const { body } = render(PageSearch);
      expect(body).toContain("Commits: “fix”");
      expect(body).toContain('title="The commit filter applied in Graph. Clear it"');
    } finally {
      for (const dispose of disposers) dispose();
    }
  });

  it("takes focus from the shared event, not from a key listener of its own", () => {
    // App owns ⌘F and decides when the field may take it (fieldOwnsFindChord).
    expect(source).toContain("FOCUS_PAGE_SEARCH_EVENT");
    expect(source).not.toContain('addEventListener("keydown"');
  });

  it("keeps the palette out of the entry chunk", () => {
    // The view bar is in the entry chunk; the palette loads the first time the
    // results card has to open.
    expect(source).not.toMatch(/^\s*import\s+\w+\s+from\s+"\.\/CommandPalette\.svelte"/m);
    expect(source).toContain('import("./CommandPalette.svelte")');
  });
});
