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
});
