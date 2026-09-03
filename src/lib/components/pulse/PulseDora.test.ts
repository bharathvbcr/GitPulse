import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";

const source = readFileSync(new URL("./PulseDora.svelte", import.meta.url), "utf8");

/**
 * Two of the four DORA numbers are heuristics derived from commit patterns.
 * Presenting them with the same confidence as the tag-derived two would be the
 * "unexamined looks like verified" failure, so the view must label them.
 */
describe("PulseDora component", () => {
  it("shows a loading state instead of an empty scorecard", () => {
    expect(source).toMatch(/loading\?: boolean/);
    expect(source).toContain("{#if loading && !dora}");
  });

  it("marks the approximated metrics as approximations", () => {
    expect(source).toContain("is_mttr_approximation");
    expect(source).toContain("heuristic");
  });

  it("declines to invent a restore time when there were no samples", () => {
    expect(source).toContain("Could not estimate from commit patterns");
    expect(source).toMatch(/is_mttr_approximation && dora\.mttr_hours <= 0/);
  });

  it("names the git commands the measured metrics come from", () => {
    expect(source).toContain("git describe --contains");
  });
});

describe("PulseDora failure state", () => {
  const source = readFileSync(new URL("./PulseDora.svelte", import.meta.url), "utf8");

  it("renders a failed delivery scan distinctly from zero deploys", () => {
    expect(source).toMatch(/error\?: string \| null/);
    expect(source).toContain("{:else if error && !dora}");
    expect(source).toContain("not a delivery frequency of zero");
  });
});
