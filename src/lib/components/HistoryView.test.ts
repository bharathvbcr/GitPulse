import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import ViewSectionTabs from "./ViewSectionTabs.svelte";
import { destinationGuide, tipGuideKey } from "../views/viewGuide";
import { sectionsFor } from "../views/viewRegistry";
import { panelId } from "../dom/tablist";

const here = dirname(fileURLToPath(import.meta.url));
const read = (name: string) => readFileSync(join(here, name), "utf8");
const history = read("HistoryView.svelte");
const sectionBar = read("ViewSectionTabs.svelte");
const viewBar = read("ViewBar.svelte");
const app = readFileSync(join(here, "..", "..", "App.svelte"), "utf8");
const historyList = readFileSync(join(here, "..", "views", "historyList.ts"), "utf8");
const query = readFileSync(join(here, "..", "codeintel", "suspectsQuery.ts"), "utf8");

describe("HistoryView", () => {
  it("hosts all three lenses that used to be top-level tabs", () => {
    expect(history).toContain("CommitTable");
    expect(history).toContain("DiffViewer");
    expect(history).toContain("loadReflog");
  });

  it("draws the reflog inside Graph, as its HEAD-moves list, not as a section", () => {
    // Reflog folded into Graph: no section branch may remain for it, the
    // toggle shows only on Graph, and the details pane sits under either list.
    expect(history).not.toContain('section === "reflog"');
    expect(history).toContain('{#if section === "graph"}');
    expect(history).toContain("onselect={(next) => repoStore.setHistoryList(next)}");
    const graphPane = history.slice(history.indexOf('{#if list === "moves"}'));
    expect(graphPane.indexOf("<LazyView load={loadReflog}")).toBeGreaterThan(-1);
    expect(graphPane.indexOf("<CommitTable />")).toBeGreaterThan(graphPane.indexOf("{:else}"));
    expect(graphPane.indexOf("<CommitDetails />")).toBeGreaterThan(graphPane.indexOf("{/if}"));
    // The word "reflog" stays findable on the control that replaced the tab.
    expect(historyList).toMatch(/title: "Reflog: every movement of HEAD/);
    expect(history).toContain("import { HISTORY_LISTS }");
  });

  it("draws Suspects inside Graph, as its third list, not as a section", () => {
    // Suspects folded into Graph the way the reflog did: its ranking is a
    // list of commits feeding the same details pane, and as a section a click
    // left it for Diff.
    expect(history).not.toContain('section === "suspects"');
    const graphPane = history.slice(history.indexOf('{#if list === "moves"}'));
    const suspectsBranch = graphPane.indexOf('{:else if list === "suspects"}');
    expect(suspectsBranch).toBeGreaterThan(-1);
    expect(graphPane.indexOf("<LazyView load={loadSuspects}")).toBeGreaterThan(suspectsBranch);
    // The details pane sits under all three lists.
    expect(graphPane.indexOf("<CommitDetails />")).toBeGreaterThan(graphPane.indexOf("<CommitTable />"));
    expect(historyList).toMatch(/value: "suspects", *\n? *label: "Suspects"/);
  });

  it("asks the suspects question in the context bar, and only on that list, where the field is not the commit filter", () => {
    // A ranking is one answer; narrowing it by text would reorder nothing and
    // hide rows whose rank still counts. So on Suspects the view bar's field
    // stops being the commit filter (it registers no filter there, and the
    // standing "Commits:" chip is withheld) and the question takes the
    // context bar, rather than both applying at once.
    expect(history).toContain('const asksSuspects = $derived(section === "graph" && list === "suspects")');
    const bar = history.slice(history.indexOf("<ViewContextBar"), history.indexOf("</ViewContextBar>"));
    expect(bar.indexOf("{#if asksSuspects}")).toBeGreaterThan(-1);
    expect(bar.indexOf("<SuspectsQueryBar />")).toBeGreaterThan(bar.indexOf("{#if asksSuspects}"));
    expect(bar).not.toContain("<FilterBar");
    const filter = history.slice(history.indexOf('registerPageSearch("history:graph"') - 200, history.indexOf('registerPageSearch("history:graph"'));
    expect(filter).toContain("if (asksSuspects) return;");
    const standing = history.slice(history.indexOf('registerStandingFilter("history"'));
    expect(standing.slice(0, 200)).toContain("if (asksSuspects) return null;");
  });

  it("drops the suspects answer when the repository changes", () => {
    expect(history).toContain("suspectsQuery.bindRepo($currentRepoPath)");
  });

  it("keeps the reflog lazy so its chunk is not in the entry bundle", () => {
    // The loader is declared at App's module scope and passed in: an inline
    // `() => import(...)` would be a fresh function each render, and LazyView
    // keys its cache on the loader's identity.
    expect(history).toContain("loadReflog: ViewLoader");
    expect(history).not.toContain('import ReflogViewer');
  });

  it("hosts the suspects list on the same lazy contract", () => {
    // It belongs with the code-graph client, which no other History list
    // needs, so eager-importing it would put that chunk in every session that
    // opens a repository. Same stable-loader rule as the reflog. The eager
    // parts (the question, the chips, the verdict in the details) read the
    // client-free `suspectsQuery`, which loads the client on first run.
    expect(history).toContain("loadSuspects: ViewLoader");
    expect(history).not.toContain("import SuspectsList");
    expect(app).toContain("const loadSuspectsList = () =>");
    expect(query).not.toMatch(/^import .*from "\.\/client"/m);
    expect(query).toContain('await import("./client")');
    // History is itself lazy, so App hands its loaders through LazyView's
    // `props={{ … }}` rather than as attributes.
    expect(app).toContain("loadSuspects: loadSuspectsList");
    // Inline arrows are the failure this contract exists for: a new function
    // each render misses LazyView's cache and remounts the pane.
    expect(app).not.toMatch(/loadSuspects(=\{|:\s*)\(\)\s*=>/);
  });

  it("swaps sections with {#if}, never {#key}", () => {
    // Keying would rebuild the pane on every switch, replay the entrance fade
    // and make CommitTable re-hydrate its virtual window from scratch.
    expect(history).toContain('{#if section === "diff"}');
    expect(history).not.toContain("{#key section}");
  });

  it("gives the view bar's field its commit filter, and its diff a find", () => {
    // Graph registers the filter; the Diff section's viewer registers inside
    // the scope History provides. The branch rides in the field as a chip.
    expect(history).toContain('registerPageSearch("history:graph"');
    expect(history).toContain('kind: "filter"');
    expect(history).toContain("filterStore.setSearch(next)");
    expect(history).toContain("filterStore.selectBranch(null)");
    expect(history).toContain('providePageSearchScope("history:diff")');
    // The list segment keeps its own context bar, on Graph only.
    expect(history).toContain('<ViewContextBar label="Graph list">');
  });

  it("keeps the commit filter in sight on every section it narrows", () => {
    expect(history).toContain('registerStandingFilter("history"');
    expect(history).toContain("filterStore.clear()");
  });
});

describe("ViewSectionTabs", () => {
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
    // The attribute comes from the shared `tabProps` owner rather than being
    // hand-written per call site. What must hold is that the active section
    // is announced AND that the tab points at a panel that exists — the bar
    // used to declare role="tab" with no tabpanel anywhere in the app.
    expect(sectionBar).toContain('aria-selected={props["aria-selected"]}');
    expect(sectionBar).toContain('aria-controls={props["aria-controls"]}');
    const body = render(ViewSectionTabs, {
      props: { view: "history", attention: { conflictedCount: 0, verdictStatus: null } },
    }).body;
    expect(body.match(/role="tab"/g)).toHaveLength(sectionsFor("history").length);
    expect(body.match(/aria-selected="true"/g)).toHaveLength(1);
    expect(body).toContain(`aria-controls="${panelId("history", "graph")}"`);
  });

  it("uses liquid tabs so the section strip matches the header", () => {
    // The sections sit inside the header group's own liquid track, so they
    // take its styling from there; a second track of their own drew a box
    // inside the box.
    const viewTabBar = read("ViewTabBar.svelte");
    expect(viewTabBar).toContain("class:gp-liquid-tabs={macos}");
    expect(viewTabBar).toContain("<ViewSectionTabs");
    expect(sectionBar).not.toContain("class:gp-liquid-tabs");
    expect(sectionBar).toContain("gp-liquid-selection");
    // The label is always rendered; below 1100px an inactive tab hides it
    // (the icon and the destination guide stand in) and the active one keeps it.
    expect(sectionBar).toContain("data-section-label>{section.label}</span>");
  });

  it("ships a destination guide instead of a label-only title", () => {
    expect(sectionBar).toContain("data-tip-guide={guideKey}");
    expect(sectionBar).not.toContain('title="{section.label}');
    for (const view of ["work", "code", "history", "insights"] as const) {
      const body = render(ViewSectionTabs, { props: { view, attention: { conflictedCount: 0, verdictStatus: null } } }).body;
      for (const section of sectionsFor(view)) {
        const key = tipGuideKey(view, section.id);
        const guide = destinationGuide(key);
        expect(guide, key).not.toBeNull();
        expect(body, key).toContain(`data-tip-guide="${key}"`);
        expect(body, key).toContain(guide!.summary);
      }
    }
  });
});

describe("the commit filter stopped being a bar of its own", () => {
  it("is the view bar's field, mounted once, not by App or History", () => {
    // It was a full-width strip under the header, then a pill in History's
    // section bar. It is the one search field in row 2 now.
    expect(app).not.toContain("<FilterBar");
    expect(history).not.toContain("<FilterBar");
    expect(history).not.toContain("<PageSearch");
    expect(viewBar.match(/<PageSearch \/>/g)).toHaveLength(1);
  });
});
