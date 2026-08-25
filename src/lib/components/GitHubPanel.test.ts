import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import GitHubPanel from "./GitHubPanel.svelte";

describe("GitHubPanel", () => {
  it("renders header and action buttons", () => {
    const { body } = render(GitHubPanel);
    expect(body).toContain("GitHub");
    expect(body).toContain("Run CI locally");
    expect(body).toContain("Refresh");
  });
});
