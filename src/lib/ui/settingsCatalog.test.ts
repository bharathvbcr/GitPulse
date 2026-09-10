import { describe, expect, it } from "vitest";
import { SETTINGS_CATALOG, matchSettings } from "./settingsCatalog";
import { SETTINGS_SECTION_IDS, SETTINGS_SECTIONS } from "./settingsSections";

describe("the settings catalog", () => {
  it("gives every entry a unique id, a label and a real section", () => {
    expect(new Set(SETTINGS_CATALOG.map((entry) => entry.id)).size).toBe(
      SETTINGS_CATALOG.length,
    );
    for (const entry of SETTINGS_CATALOG) {
      expect(entry.label.trim(), `${entry.id} has no label`).not.toBe("");
      expect(
        SETTINGS_SECTION_IDS as readonly string[],
        `${entry.id} points at a section that does not exist`,
      ).toContain(entry.section);
    }
  });

  it("leaves no category without a searchable control", () => {
    // A category the filter can never surface is a category search hides.
    const covered = new Set(SETTINGS_CATALOG.map((entry) => entry.section));
    for (const section of SETTINGS_SECTION_IDS) {
      expect(covered, `nothing in the catalog for ${section}`).toContain(section);
    }
  });

  it("finds every entry by its own label", () => {
    for (const entry of SETTINGS_CATALOG) {
      expect(
        matchSettings(entry.label).settings,
        `${entry.id} cannot be found by its label`,
      ).toContain(entry.id);
    }
  });
});

describe("matchSettings", () => {
  it("keeps everything for a blank or whitespace query", () => {
    for (const query of ["", "   ", "\t"]) {
      const result = matchSettings(query);
      expect(result.all).toBe(true);
      expect(result.sections).toEqual(SETTINGS_SECTIONS.map((entry) => entry.id));
      expect(result.settings).toHaveLength(SETTINGS_CATALOG.length);
    }
  });

  it("matches keywords the label does not contain", () => {
    // The point of the keyword column: "side by side" is what other clients
    // call split, and nothing on the page says it.
    expect(matchSettings("side by side").settings).toContain("diff-layout");
    expect(matchSettings("indent").settings).toContain("tab-width");
    expect(matchSettings("animation").settings).toContain("motion");
    expect(matchSettings("dependabot").settings).toContain("auto-github-alerts");
  });

  it("is case-insensitive", () => {
    expect(matchSettings("ACCENT").settings).toEqual(matchSettings("accent").settings);
  });

  it("narrows on a second word rather than widening", () => {
    const one = matchSettings("diff");
    const two = matchSettings("diff whitespace");
    expect(two.settings.length).toBeLessThan(one.settings.length);
    expect(two.settings).toContain("diff-whitespace");
  });

  it("keeps a whole category when the category's own name matches", () => {
    // Searching "graph" should hand back the Graph panel intact, not just the
    // rows that happen to repeat the word.
    const graph = SETTINGS_CATALOG.filter((entry) => entry.section === "graph");
    expect(matchSettings("graph").settings).toEqual(
      expect.arrayContaining(graph.map((entry) => entry.id)),
    );
  });

  it("returns nothing, and no sections, when nothing matches", () => {
    const result = matchSettings("kubernetes");
    expect(result.all).toBe(false);
    expect(result.settings).toEqual([]);
    expect(result.sections).toEqual([]);
  });

  it("orders surviving sections by the rail, never by match order", () => {
    // A filter that reshuffled the categories would move them under the
    // reader's cursor as they typed.
    const result = matchSettings("default");
    const railOrder = SETTINGS_SECTIONS.map((entry) => entry.id).filter((id) =>
      result.sections.includes(id),
    );
    expect(result.sections).toEqual(railOrder);
  });

  it("names only sections that actually kept a control", () => {
    const result = matchSettings("whitespace");
    const sectionsWithHits = new Set(
      SETTINGS_CATALOG.filter((entry) => result.settings.includes(entry.id)).map(
        (entry) => entry.section,
      ),
    );
    expect(new Set(result.sections)).toEqual(sectionsWithHits);
  });
});
