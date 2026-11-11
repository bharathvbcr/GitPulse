import { describe, expect, it } from "vitest";
import { VIEW_TABS, type ViewTab } from "../repos/persist";
import { REGISTERED_VIEWS, VIEW_REGISTRY, viewTabForMenuId } from "./viewRegistry";
import { MANVI_FOCUS_LIST } from "../ui/manviFocus";
import { SETTINGS_SECTIONS } from "../ui/settingsSections";

describe("viewRegistry", () => {
  it("registers exactly the persisted view tabs, in both directions", () => {
    const registered = Object.keys(VIEW_REGISTRY);
    // Same members both ways; declaration order is a display concern and is
    // pinned separately below.
    expect([...registered].sort()).toEqual([...VIEW_TABS].sort());
    expect(new Set(registered)).toEqual(new Set(VIEW_TABS));
  });

  it("gives every view a label and an id", () => {
    for (const view of REGISTERED_VIEWS) {
      expect(view.label.length).toBeGreaterThan(0);
      expect(view.id.length).toBeGreaterThan(0);
    }
  });

  it("gives every view and section a summary that is not just its label", () => {
    // The header and section bars render these as glanceable tooltips. A
    // missing summary would compile (until the field was required) and then
    // show a card that only repeats the tab name — the defect this field
    // exists to stop.
    for (const view of REGISTERED_VIEWS) {
      expect(view.summary.length, view.id).toBeGreaterThan(view.label.length);
      expect(view.summary).not.toBe(view.label);
      for (const section of view.sections ?? []) {
        expect(section.summary.length, `${view.id}:${section.id}`).toBeGreaterThan(
          section.label.length,
        );
        expect(section.summary).not.toBe(section.label);
      }
    }
  });

  it("keeps MANVI reachable (regression: it once existed but was missing from consumers)", () => {
    // MANVI was its own tab, then Work's Policy section, and the original
    // regression was that it existed without any consumer listing it. Its
    // cards now live on the surfaces that own their subject, so the same
    // regression would read as "a card exists but nothing navigates to it":
    // every focus target must land on a live section or the Settings dialog.
    const work = VIEW_REGISTRY.work.sections?.map((section) => section.id) ?? [];
    expect(work).toEqual(["overview", "remote", "tasks"]);
    for (const target of MANVI_FOCUS_LIST) {
      const destination = target.destination;
      if (destination.kind === "view") {
        expect(VIEW_REGISTRY[destination.tab].sections?.map((s) => s.id), target.id)
          .toContain(destination.section);
      } else {
        expect(SETTINGS_SECTIONS.map((s) => s.id), target.id).toContain(destination.section);
      }
    }
  });

  it("resolves every registered tab from its native menu id and nothing else", () => {
    for (const tab of VIEW_TABS) {
      expect(viewTabForMenuId(`tab-${tab}`)).toBe(tab);
    }
    expect(viewTabForMenuId("tab-nonexistent")).toBeUndefined();
    expect(viewTabForMenuId("tab-")).toBeUndefined();
    expect(viewTabForMenuId("open")).toBeUndefined();
    expect(viewTabForMenuId("")).toBeUndefined();
  });

  it("keeps declaration order stable, because it is the header order", () => {
    expect(REGISTERED_VIEWS.map((view) => view.id)).toEqual([
      "work",
      "code",
      "history",
      "insights",
    ] satisfies ViewTab[]);
  });
});
