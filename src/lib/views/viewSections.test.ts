import { describe, expect, it } from "vitest";
import {
  REGISTERED_VIEWS,
  VIEW_REGISTRY,
  activeSectionFor,
  defaultSectionFor,
  describeDestination,
  isSectionOnScreen,
  resolveSection,
  retiredSectionFor,
  RETIRED_SECTIONS,
  sectionsFor,
} from "./viewRegistry";
import { RETIRED_VIEWS, isViewTab, sanitizeViewSections, type ViewTab } from "../repos/persist";
import { WIP_DESTINATIONS, WIP_REASON_KINDS } from "../repos/wipSummary";

describe("view sections", () => {
  it("keeps Graph and Diff in order, with no Reflog or Suspects section", () => {
    // Graph, Diff and Reflog were top-level tabs, then sections. Reflog then
    // folded into Graph as its "HEAD moves" list (views/historyList.ts), and
    // Suspects — the history read backwards, from a symptom to its causes —
    // followed as Graph's third list, so neither is a section any more.
    expect(sectionsFor("history").map((s) => s.id)).toEqual(["graph", "diff"]);
  });

  it("lands a remembered Suspects section on Graph", () => {
    // The list it implies is restored by `restoredHistoryList`.
    expect(activeSectionFor("history", { history: "suspects" })).toBe("graph");
  });

  it("lands a remembered Reflog section on Graph, the view's default", () => {
    // The list mode a remembered Reflog implies is restored separately, by
    // `restoredHistoryList` in persist; the section itself must still open.
    expect(activeSectionFor("history", { history: "reflog" })).toBe("graph");
    expect(resolveSection("history", "reflog")).toBe("graph");
  });

  it("opens a sectioned view on its first section", () => {
    expect(defaultSectionFor("history")).toBe("graph");
    expect(activeSectionFor("history", {})).toBe("graph");
  });

  it("reports no section for a view that renders one pane", () => {
    // Not "the first of none": callers branch on null to decide whether to
    // draw a segmented control at all.
    //
    // Every registered view offers sections today, so this loop has nothing
    // to run on — and says so rather than passing as if it had checked. The
    // branch stays because a single-pane registration is legitimate; the day
    // one is added, this starts covering it without being touched.
    const singlePane = REGISTERED_VIEWS.filter((view) => (view.sections ?? []).length === 0);
    for (const view of singlePane) {
      expect(defaultSectionFor(view.id)).toBeNull();
      expect(sectionsFor(view.id)).toEqual([]);
      expect(activeSectionFor(view.id, { [view.id]: "anything" })).toBeNull();
    }
    expect(singlePane.length, "no section-less view exists to exercise the branch").toBe(0);
  });

  it("falls back to the default for a section this build no longer offers", () => {
    // Persisted section ids outlive the build that wrote them. A renamed or
    // dropped section must not leave the view with no pane selected.
    expect(resolveSection("history", "a-section-that-was-removed")).toBe("graph");
    expect(resolveSection("history", 42)).toBe("graph");
    expect(resolveSection("history", null)).toBe("graph");
    expect(activeSectionFor("history", { history: "nonsense" })).toBe("graph");
  });

  it("lands a remembered Code Search or Blame on Explorer, which now holds both", () => {
    // Search and Blame were sections of Code; they are toggles inside
    // Explorer now. A session saved on either must reopen on Explorer (the
    // file it was blaming is still the selection), not on an empty pane.
    expect(sectionsFor("code").map((s) => s.id)).toEqual(["explorer", "map"]);
    expect(activeSectionFor("code", { code: "search" })).toBe("explorer");
    expect(activeSectionFor("code", { code: "blame" })).toBe("explorer");
  });

  it("lands a section folded into another where its content went, not on the default", () => {
    // Resolve folded into Policy, then both into Overview, which leads with
    // the conflict editor. A saved selection, a native menu build or a caller
    // that still names either must open Overview, never fail to resolve.
    expect(sectionsFor("work").map((s) => s.id)).not.toContain("resolve");
    expect(sectionsFor("work").map((s) => s.id)).not.toContain("policy");
    expect(resolveSection("work", "resolve")).toBe("overview");
    expect(activeSectionFor("work", { work: "resolve" })).toBe("overview");
    // Overview is also Work's default, so assert the alias itself, not only
    // where the fallback happens to land: a retired id must be on the map.
    expect(retiredSectionFor("work", "resolve")).toBe("overview");
    expect(retiredSectionFor("work", "policy")).toBe("overview");
    expect(activeSectionFor("work", { work: "policy" })).toBe("overview");
    // Scoped to its view: the same id elsewhere is just unknown.
    expect(resolveSection("history", "resolve")).toBe("graph");
    // Every alias points at a live section of its own view, and none shadows one.
    for (const [view, aliases] of Object.entries(RETIRED_SECTIONS)) {
      const ids = sectionsFor(view as ViewTab).map((s) => s.id);
      for (const [retired, target] of Object.entries(aliases ?? {})) {
        expect(ids, `${view}:${retired} is still a live section`).not.toContain(retired);
        expect(ids, `${view}:${retired} retires to a missing section`).toContain(target);
        expect(retiredSectionFor(view as ViewTab, retired)).toBe(target);
      }
    }
    expect(retiredSectionFor("work", "__proto__")).toBeNull();
  });

  it("keeps every fold in the one retirement map, for restores and callers alike", () => {
    // Secrets, Search, Blame and Reflog folded away before Resolve did. A
    // second map beside this one answered only restored sessions, so a caller
    // naming Secrets opened Pulse rather than Health.
    expect(resolveSection("insights", "secrets")).toBe("health");
    expect(resolveSection("code", "search")).toBe("explorer");
    expect(resolveSection("code", "blame")).toBe("explorer");
    expect(resolveSection("history", "reflog")).toBe("graph");
    expect(sanitizeViewSections({ insights: "secrets", work: "resolve" })).toEqual({
      insights: "health",
      work: "overview",
    });
  });

  it("keeps a remembered section", () => {
    expect(activeSectionFor("history", { history: "diff" })).toBe("diff");
  });

  it("answers what is actually on screen, not merely which view is open", () => {
    // The distinction the diff refetch depends on: being on History is not
    // the same as having the diff pane in front of you, and refetching a
    // worktree diff for a user reading the graph yanks them somewhere new.
    expect(isSectionOnScreen("history", { history: "diff" }, "history", "diff")).toBe(true);
    expect(isSectionOnScreen("history", { history: "graph" }, "history", "diff")).toBe(false);
    expect(isSectionOnScreen("code", { history: "diff" }, "history", "diff")).toBe(false);
  });

  it("gives every section a unique id within its view", () => {
    for (const view of REGISTERED_VIEWS) {
      const ids = (view.sections ?? []).map((s) => s.id);
      expect(new Set(ids).size, `${view.id} has duplicate section ids`).toBe(ids.length);
    }
  });

  it("leaves every retired view a door in the command palette", () => {
    // This is the promise consolidation makes: a view that stops being a tab
    // stays reachable by typing. Two shapes satisfy it, and exactly one of
    // them has to — a section carries its own label, unless it is the
    // section its view already opens on, in which case the view's own
    // command lands there.
    let checked = 0;
    for (const [id, retired] of Object.entries(RETIRED_VIEWS)) {
      if (!retired.section) continue;
      checked += 1;
      const section = sectionsFor(retired.tab).find((s) => s.id === retired.section);
      expect(section, `${id} retires to a section that does not exist`).toBeDefined();
      const door =
        section?.paletteCommand ??
        (retired.section === defaultSectionFor(retired.tab)
          ? VIEW_REGISTRY[retired.tab].paletteCommand
          : undefined);
      expect(
        door,
        `${id} became a section with no palette command and is not its view's default, so it lost its only door`,
      ).toBeTruthy();
    }
    // Guarded, so an empty retirement map could not pass this as a sweep.
    expect(checked).toBeGreaterThanOrEqual(10);
  });

  it("points every retirement at a view and section this build has", () => {
    for (const [id, retired] of Object.entries(RETIRED_VIEWS)) {
      expect(isViewTab(retired.tab), `${id} retires to an unregistered view`).toBe(true);
      if (retired.section) {
        expect(
          resolveSection(retired.tab, retired.section),
          `${id} retires to a section ${retired.tab} does not offer`,
        ).toBe(retired.section);
      }
    }
  });

  it("points every work-in-progress destination at a view and section this build has", () => {
    // The workspace roll-up sends a reader from a named repository to the
    // pane that answers its worst reason. `setActiveTab` resolves an unknown
    // section to the view's default rather than failing, so a renamed section
    // would not throw — it would quietly land everyone on Overview while the
    // row's tooltip still promised Resolve. Derived from the kind list, so a
    // new reason cannot skip this check.
    expect(WIP_REASON_KINDS.length).toBeGreaterThanOrEqual(6);
    for (const kind of WIP_REASON_KINDS) {
      const { tab, section } = WIP_DESTINATIONS[kind];
      expect(isViewTab(tab), `${kind} lands on an unregistered view`).toBe(true);
      expect(
        resolveSection(tab, section),
        `${kind} lands on a section ${tab} does not offer`,
      ).toBe(section);
    }
  });

  it("names a destination the way the header and section bar spell it", () => {
    // The name travels into tooltips that promise where a click will land, so
    // it is read out of the registry rather than written at the call site.
    expect(describeDestination("work", "remote")).toBe("Work → Remote");
    expect(describeDestination("history", "diff")).toBe("History → Diff");
    // No section, or one this build dropped: the view's own name, never an
    // arrow pointing at a pane that is not there.
    expect(describeDestination("work")).toBe("Work");
    expect(describeDestination("work", "retired-section")).toBe("Work");
  });

  it("keeps the registry's own section list in display order", () => {
    // Declaration order is the order the segmented control renders, so the
    // catalog is the single place that decides it.
    expect(VIEW_REGISTRY.history.sections?.[0]?.label).toBe("Graph");
  });
});
