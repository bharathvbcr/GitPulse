import { describe, expect, it } from "vitest";
import { isSettingsSectionId, SETTINGS_SECTION_IDS, SETTINGS_SECTIONS } from "./settingsSections";

describe("isSettingsSectionId", () => {
  it("accepts every catalog id and refuses others", () => {
    for (const id of SETTINGS_SECTION_IDS) {
      expect(isSettingsSectionId(id)).toBe(true);
    }
    expect(isSettingsSectionId("nope")).toBe(false);
    expect(isSettingsSectionId(1)).toBe(false);
    expect(isSettingsSectionId(null)).toBe(false);
  });
});

describe("Agents section summary", () => {
  it("names the Manvi policy connection and local model servers it now holds", () => {
    const agents = SETTINGS_SECTIONS.find((entry) => entry.id === "agents");
    expect(agents?.summary).toBe(
      "MCP for agents, the Manvi policy connection, local model servers, and installing the DevMap and Manvi CLIs.",
    );
  });
});
