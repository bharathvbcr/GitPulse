import { describe, expect, it } from "vitest";
import { VIEW_REGISTRY, sectionsFor } from "./viewRegistry";
import { SECTION_ICONS, VIEW_ICONS, sectionIcon } from "./viewIcons";
import type { ViewTab } from "../repos/persist";

const views = Object.keys(VIEW_REGISTRY) as ViewTab[];

describe("view icons", () => {
  it("gives every registered view an icon", () => {
    expect(views.length).toBeGreaterThanOrEqual(4);
    for (const view of views) {
      expect(VIEW_ICONS[view], view).toBeTypeOf("function");
    }
  });

  it("gives every registered section its own icon, not the view's fallback", () => {
    let checked = 0;
    for (const view of views) {
      for (const section of sectionsFor(view)) {
        expect(SECTION_ICONS[view][section.id], `${view}/${section.id}`).toBeTypeOf("function");
        expect(sectionIcon(view, section.id), `${view}/${section.id}`).not.toBe(VIEW_ICONS[view]);
        checked += 1;
      }
    }
    // The control: the loop above ran over the real catalog (11 sections since
    // Work's Policy folded into Overview, Remote and Settings and History's
    // Suspects into Graph).
    expect(checked).toBeGreaterThanOrEqual(11);
  });

  it("maps no section the catalog does not have", () => {
    // A stale entry would be a section renamed in the catalog and not here,
    // which the fallback would otherwise hide behind the view's glyph.
    for (const view of views) {
      const known = new Set(sectionsFor(view).map((section) => section.id));
      for (const id of Object.keys(SECTION_ICONS[view])) {
        expect(known.has(id), `${view}/${id}`).toBe(true);
      }
    }
  });

  it("keeps sections within one view visually distinct", () => {
    for (const view of views) {
      const icons = sectionsFor(view).map((section) => sectionIcon(view, section.id));
      expect(new Set(icons).size, view).toBe(icons.length);
    }
  });

  it("falls back to the view's icon for an unknown section", () => {
    expect(sectionIcon("work", "no-such-section")).toBe(VIEW_ICONS.work);
  });
});
