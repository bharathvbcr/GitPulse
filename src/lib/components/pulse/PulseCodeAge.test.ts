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

  it("states a failed scan in the unknown tone and never as a measured distribution", () => {
    expect(source).toContain('describeFailure({ verb: "read", object: "blame data" }, error)');
    expect(source).toContain("No age distribution was measured.");
    expect(source).toContain('class="gp-notice" data-tone="unknown"');
  });

  it("feeds the bar and the legend from one cohort list", () => {
    expect(source).toContain("const cohorts = $derived(");
    expect(source.match(/\{#each cohorts as cohort/g)?.length).toBe(2);
  });
});

describe("PulseCodeAge rescan action", () => {
  it("offers Rescan on a blame scan that measured nothing, only when the parent can refresh", async () => {
    const { render } = await import("svelte/server");
    const { default: PulseCodeAge } = await import("./PulseCodeAge.svelte");
    const failed = { knowledge: null, error: "blame timed out" };
    expect(render(PulseCodeAge, { props: failed }).body).not.toContain("Rescan");
    const withRefresh = render(PulseCodeAge, { props: { ...failed, onRefresh: () => {} } }).body;
    expect(withRefresh).toContain("Rescan");
    expect(withRefresh).toContain('data-tone="unknown"');
  });

  it("calls the parent's refresh from the button, and the parent hands it the knowledge refresh", () => {
    expect(source).toContain("onclick={() => onRefresh?.()}");
    const view = readFileSync(new URL("./PulseView.svelte", import.meta.url), "utf8");
    expect(view).toMatch(/<PulseCodeAge[\s\S]*?onRefresh=\{\(\) => pulseStore\.loadKnowledge\(\)\}[\s\S]*?\/>/);
  });
});
