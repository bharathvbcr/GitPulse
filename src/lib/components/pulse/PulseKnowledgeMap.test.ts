import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";

const source = readFileSync(new URL("./PulseKnowledgeMap.svelte", import.meta.url), "utf8");

describe("PulseKnowledgeMap component", () => {
  it("shows a loading state instead of an empty bus factor", () => {
    expect(source).toMatch(/loading\?: boolean/);
    expect(source).toContain("{#if loading && !knowledge}");
  });

  it("surfaces a truncated blame scan rather than presenting a sample as total", () => {
    expect(source).toContain("knowledge.truncated");
  });

  it("offers a re-scan that is disabled while one is in flight", () => {
    expect(source).toContain("disabled={loading}");
  });
});

describe("PulseKnowledgeMap failure state", () => {
  const source = readFileSync(new URL("./PulseKnowledgeMap.svelte", import.meta.url), "utf8");

  it("renders a failed blame scan distinctly from an empty one", () => {
    expect(source).toMatch(/error\?: string \| null/);
    expect(source).toContain("{:else if error && !knowledge}");
    expect(source).toContain("not a bus factor of zero");
  });

  it("keeps the underlying reason on screen", () => {
    expect(source).toContain("{error}");
  });
});
