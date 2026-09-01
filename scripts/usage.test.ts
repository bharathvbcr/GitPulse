import { describe, expect, it } from "vitest";
import { formatUsage, wantsHelp } from "./usage.mjs";

describe("shared usage printer", () => {
  it("detects an explicit help request anywhere in argv", () => {
    expect(wantsHelp(["--help"])).toBe(true);
    expect(wantsHelp(["--tag", "v1", "-h"])).toBe(true);
    expect(wantsHelp(["--tag", "v1"])).toBe(false);
    expect(wantsHelp([])).toBe(false);
  });

  it("aligns descriptions into one column regardless of flag length", () => {
    const text = formatUsage({
      name: "demo",
      summary: "Demo checker.",
      flags: [
        { flag: "--a", description: "short" },
        { flag: "--a-much-longer-flag", description: "long" },
      ],
    });
    // the description column, not the start of the padding gap
    const columns = text
      .split("\n")
      .filter((line) => line.startsWith("  --"))
      .map((line) => line.search(/\S+$/));
    expect(columns).toHaveLength(2);
    expect(new Set(columns).size).toBe(1);
  });

  it("includes the exit-code contract when given", () => {
    const text = formatUsage({
      name: "demo",
      summary: "Demo.",
      flags: [{ flag: "--x", description: "x" }],
      exits: "0 ok · 1 violated · 2 internal error",
    });
    expect(text).toContain("Exit codes: 0 ok · 1 violated · 2 internal error");
  });
});
