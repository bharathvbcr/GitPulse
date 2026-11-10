import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "PulseHeatmap.svelte"),
  "utf8",
);

describe("PulseHeatmap", () => {
  it("keyboard-activates a day the same way a click does", () => {
    expect(source).toContain("handleDayKey");
    expect(source).toContain('event.key === "Enter"');
  });

  it("switches between commits and lines changed through the shared segmented control", () => {
    expect(source).toContain('import SettingSegment from "../SettingSegment.svelte"');
    expect(source).toContain('{ value: "count", label: "Commits" }');
    expect(source).toContain('{ value: "churn", label: "Lines changed" }');
  });

  it("keeps empty days visible on the opaque card by stepping up a tone", () => {
    expect(source).toContain('"bg-surfaceHover hover:bg-border/60"');
  });
});
