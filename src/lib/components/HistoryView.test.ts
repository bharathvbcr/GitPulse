import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const here = dirname(fileURLToPath(import.meta.url));
const read = (name: string) => readFileSync(join(here, name), "utf8");
const history = read("HistoryView.svelte");
const sectionBar = read("ViewSectionBar.svelte");
const filterBar = read("FilterBar.svelte");
const app = readFileSync(join(here, "..", "..", "App.svelte"), "utf8");

describe("HistoryView", () => {
  it("hosts all three lenses that used to be top-level tabs", () => {
    expect(history).toContain("CommitTable");
    expect(history).toContain("DiffViewer");
    expect(history).toContain("loadReflog");
  });

  it("keeps the reflog lazy so its chunk is not in the entry bundle", () => {
    // The loader is declared at App's module scope and passed in: an inline
    // `() => import(...)` would be a fresh function each render, and LazyView
    // keys its cache on the loader's identity.
    expect(history).toContain("loadReflog: ViewLoader");
    expect(history).not.toContain('import ReflogViewer');
  });

  it("swaps sections with {#if}, never {#key}", () => {
    // Keying would rebuild the pane on every switch, replay the entrance fade
    // and make CommitTable re-hydrate its virtual window from scratch.
    expect(history).toContain('{#if section === "diff"}');
    expect(history).not.toContain("{#key section}");
  });

  it("carries the commit filter in its own header", () => {
    expect(history).toContain("<FilterBar />");
    expect(history).toContain("<ViewSectionBar");
  });
});

describe("ViewSectionBar", () => {
  it("reads the catalog rather than taking a list of its own", () => {
    // One control for every sectioned view, so the lens switcher cannot drift
    // into looking different per view.
    expect(sectionBar).toContain("sectionsFor(view)");
    expect(sectionBar).toContain("activeSectionFor(view");
    expect(sectionBar).toContain("repoStore.setViewSection(view, section.id)");
  });

  it("draws nothing for a view with a single pane", () => {
    expect(sectionBar).toContain("{#if sections.length > 1}");
  });

  it("marks the active section for assistive tech, not just visually", () => {
    expect(sectionBar).toContain('role="tab"');
    // The attribute comes from the shared `tabProps` owner now rather than
    // being hand-written per call site. What must hold is that the active
    // section is announced AND that the tab points at a panel that exists —
    // the bar used to declare role="tab" with no tabpanel anywhere in the app.
    expect(sectionBar).toContain('aria-selected={props["aria-selected"]}');
    expect(sectionBar).toContain('aria-controls={props["aria-controls"]}');
  });
});

describe("the commit filter stopped being a full-width strip", () => {
  it("no longer owns a row of its own", () => {
    expect(filterBar).not.toContain('class="h-10 bg-surface/60 border-b');
    expect(filterBar).toContain('class="flex-1 min-w-0 flex items-center');
  });

  it("is mounted by History, not by App", () => {
    expect(app).not.toContain("<FilterBar");
    expect(history).toContain("<FilterBar />");
  });
});
