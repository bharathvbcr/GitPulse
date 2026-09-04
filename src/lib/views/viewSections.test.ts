import { describe, expect, it } from "vitest";
import {
  REGISTERED_VIEWS,
  VIEW_REGISTRY,
  activeSectionFor,
  defaultSectionFor,
  isSectionOnScreen,
  resolveSection,
  sectionsFor,
} from "./viewRegistry";
import { RETIRED_VIEWS, isViewTab } from "../repos/persist";

describe("view sections", () => {
  it("gives History the three lenses that were three tabs", () => {
    expect(sectionsFor("history").map((s) => s.id)).toEqual(["graph", "diff", "reflog"]);
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

  it("keeps a remembered section", () => {
    expect(activeSectionFor("history", { history: "reflog" })).toBe("reflog");
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

  it("keeps the registry's own section list in display order", () => {
    // Declaration order is the order the segmented control renders, so the
    // catalog is the single place that decides it.
    expect(VIEW_REGISTRY.history.sections?.[0]?.label).toBe("Graph");
  });
});
