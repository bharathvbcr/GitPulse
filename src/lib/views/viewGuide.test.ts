import { describe, expect, it } from "vitest";
import { REGISTERED_VIEWS, sectionsFor } from "./viewRegistry";
import {
  destinationGuide,
  parseTipGuideKey,
  tipGuideDescId,
  tipGuideKey,
} from "./viewGuide";

describe("destination guides", () => {
  it("resolves every registered view and section", () => {
    for (const view of REGISTERED_VIEWS) {
      const page = destinationGuide(tipGuideKey(view.id));
      expect(page, view.id).not.toBeNull();
      expect(page?.title).toBe(view.label);
      expect(page?.summary.length ?? 0).toBeGreaterThan(view.label.length);
      expect(page?.summary).not.toBe(view.label);
      expect(page?.chips.map((chip) => chip.id)).toEqual(
        sectionsFor(view.id).map((section) => section.id),
      );
      expect(page?.chips.every((chip) => !chip.active)).toBe(true);

      for (const section of sectionsFor(view.id)) {
        const lens = destinationGuide(tipGuideKey(view.id, section.id));
        expect(lens, `${view.id}:${section.id}`).not.toBeNull();
        expect(lens?.title).toBe(section.label);
        expect(lens?.summary.length ?? 0).toBeGreaterThan(section.label.length);
        expect(lens?.summary).not.toBe(section.label);
        expect(lens?.chips.some((chip) => chip.id === section.id && chip.active)).toBe(
          true,
        );
      }
    }
  });

  it("returns null for keys this build does not offer", () => {
    expect(destinationGuide("")).toBeNull();
    expect(destinationGuide("fleet")).toBeNull();
    expect(destinationGuide("work:missing")).toBeNull();
    expect(destinationGuide("work:")).toBeNull();
  });

  it("parses view and section keys the tabs actually write", () => {
    expect(parseTipGuideKey("history")).toEqual({ view: "history", section: null });
    expect(parseTipGuideKey("history:diff")).toEqual({
      view: "history",
      section: "diff",
    });
    expect(tipGuideKey("work", "resolve")).toBe("work:resolve");
    expect(tipGuideDescId("work:resolve")).toBe("gitpulse-view-guide-work-resolve");
  });

  it("keeps Work's F10 and Code's digit chord on the view cards", () => {
    expect(destinationGuide("work")?.shortcut).toBe("F10");
    expect(destinationGuide("code")?.shortcut).toBe("⌘1");
    expect(destinationGuide("work:overview")?.shortcut).toBe("⌥1");
    expect(destinationGuide("work:resolve")?.shortcut).toBe("⌥2");
  });
});
