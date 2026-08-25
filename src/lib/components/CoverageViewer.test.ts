import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import CoverageViewer from "./CoverageViewer.svelte";

describe("CoverageViewer", () => {
  it("renders empty state when no repo is open", () => {
    const { body } = render(CoverageViewer);
    expect(body).toContain("Test coverage");
    expect(body).toContain("Rescan coverage artifacts");
    expect(body).toContain("No coverage report");
    expect(body).toContain("Pick a file");
  });
});
