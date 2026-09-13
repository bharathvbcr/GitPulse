import { describe, expect, it } from "vitest";
import {
  SETTINGS_CATALOG,
  matchSettings,
  unsupportedSettingIds,
  type SettingRequirement,
} from "./settingsCatalog";
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

describe("host-gated settings", () => {
  /** Every capability named by an entry, derived so a new one is covered. */
  const REQUIREMENTS = [
    ...new Set(
      SETTINGS_CATALOG.flatMap((entry) => (entry.requires ? [entry.requires] : [])),
    ),
  ];

  /**
   * Every requirement the catalog names, all set to `value`.
   *
   * Derived rather than written out, so adding a requirement to the catalog
   * cannot leave these cases silently exercising a stale subset.
   */
  function capabilities(value: boolean): Readonly<Record<SettingRequirement, boolean>> {
    return Object.fromEntries(REQUIREMENTS.map((name) => [name, value])) as Record<
      SettingRequirement,
      boolean
    >;
  }

  it("gates the Dock toggle and on-device drafting on host capabilities", () => {
    expect(REQUIREMENTS).toContain("dockHiding");
    expect(REQUIREMENTS).toContain("appleIntelligence");
  });

  it("reports nothing unsupported when the host has every capability", () => {
    expect(unsupportedSettingIds(capabilities(true)).size).toBe(0);
  });

  it("reports exactly the entries whose requirement is unmet", () => {
    const unsupported = unsupportedSettingIds(capabilities(false));
    const expected = SETTINGS_CATALOG.filter((entry) => entry.requires !== undefined).map(
      (entry) => entry.id,
    );
    expect([...unsupported].sort()).toEqual([...expected].sort());
    expect(unsupported.has("hide-dock")).toBe(true);
    expect(unsupported.has("apple-intelligence")).toBe(true);
  });

  it("never gates a setting that works on every host", () => {
    const unsupported = unsupportedSettingIds(capabilities(false));
    // A portable setting caught by the gate would vanish from a host that can
    // run it perfectly well.
    for (const id of ["theme", "accent", "tab-width", "status-bar", "update-check"]) {
      expect(unsupported.has(id)).toBe(false);
    }
  });
});

describe("search on a host missing a capability", () => {
  const WITHOUT_DOCK = unsupportedSettingIds({ dockHiding: false, appleIntelligence: true });

  it("omits the unsupported control from an unfiltered listing", () => {
    const listed = matchSettings("", WITHOUT_DOCK);
    expect(listed.all).toBe(true);
    expect(listed.settings).not.toContain("hide-dock");
    // Everything else still listed: the gate is narrow, not a blanket.
    expect(listed.settings).toContain("status-icon");
  });

  /**
   * The defect this closes: searching "dock" on Windows previously matched the
   * Dock toggle and opened the Layout section around a control that the page
   * then rendered hidden — a result that shows nothing.
   */
  it("does not return a section whose only match is unsupported", () => {
    const result = matchSettings("dock accessory", WITHOUT_DOCK);
    expect(result.settings).not.toContain("hide-dock");
    expect(result.settings).toHaveLength(0);
    expect(result.sections).toHaveLength(0);
  });

  it("still finds the control on a host that supports it", () => {
    const result = matchSettings(
      "dock accessory",
      unsupportedSettingIds({ dockHiding: true, appleIntelligence: true }),
    );
    expect(result.settings).toContain("hide-dock");
    expect(result.sections).toContain("layout");
  });

  it("keeps a whole-section match free of unsupported controls", () => {
    // "layout" matches the section by name, which keeps all of its controls —
    // that shortcut must still not resurrect a gated one.
    const result = matchSettings("layout", WITHOUT_DOCK);
    expect(result.settings).not.toContain("hide-dock");
  });

  it("defaults to gating nothing when no capability set is supplied", () => {
    expect(matchSettings("dock accessory").settings).toContain("hide-dock");
  });
});
