import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import FilterBar from "./FilterBar.svelte";

describe("FilterBar", () => {
  it("focuses on search: one labelled input plus contextual branch chip", () => {
    const { body } = render(FilterBar);

    expect(body).toContain('id="gitpulse-filter"');
    expect(body).toContain("Search commits, authors, paths");
    // Branch spacing and appearance controls live in Settings now; the bar
    // stays lean.
    expect(body).not.toContain('aria-label="Branch spacing"');
    expect(body).not.toContain("Spacious");
  });
});
