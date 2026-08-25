import { describe, it, expect, beforeEach } from "vitest";
import { get } from "svelte/store";
import { densityStore } from "../densityStore";

describe("densityStore", () => {
  beforeEach(() => {
    densityStore.setDensity("spacious");
  });

  it("defaults to spacious density mode", () => {
    expect(get(densityStore)).toBe("spacious");
  });

  it("allows switching to compact density mode", () => {
    densityStore.setDensity("compact");
    expect(get(densityStore)).toBe("compact");
  });

  it("toggles between spacious and compact", () => {
    expect(get(densityStore)).toBe("spacious");
    densityStore.toggle();
    expect(get(densityStore)).toBe("compact");
    densityStore.toggle();
    expect(get(densityStore)).toBe("spacious");
  });
});
