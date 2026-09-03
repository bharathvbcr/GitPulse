import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";

const source = readFileSync(new URL("./PulseCodeAge.svelte", import.meta.url), "utf8");

describe("PulseCodeAge component", () => {
  it("shows a loading state rather than an all-zero distribution", () => {
    expect(source).toMatch(/loading\?: boolean/);
    expect(source).toContain("{#if loading && !knowledge}");
  });

  it("reads the age cohorts from the backend report, not from commit dates", () => {
    expect(source).toContain("age_distribution");
    expect(source).toContain("half_life_days");
  });
});

describe("PulseCodeAge failure state", () => {
  const source = readFileSync(new URL("./PulseCodeAge.svelte", import.meta.url), "utf8");

  it("renders a failed blame scan distinctly from no age data", () => {
    expect(source).toMatch(/error\?: string \| null/);
    expect(source).toContain("{:else if error && !knowledge}");
  });
});
