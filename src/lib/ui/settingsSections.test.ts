import { describe, expect, it } from "vitest";
import { isSettingsSectionId, SETTINGS_SECTION_IDS } from "./settingsSections";

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
