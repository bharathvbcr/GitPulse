import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";

const source = readFileSync(new URL("./PulsePeriodCompare.svelte", import.meta.url), "utf8");

describe("PulsePeriodCompare component", () => {
  it("delegates the period arithmetic to the tested metric", () => {
    expect(source).toContain("computePeriodCompare");
    expect(source).not.toContain("86400");
  });
});
