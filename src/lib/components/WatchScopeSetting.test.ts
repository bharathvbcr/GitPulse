import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import WatchScopeSetting from "./WatchScopeSetting.svelte";

describe("WatchScopeSetting", () => {
  it("renders both options with Narrow selected by default, and the help text", () => {
    const { body } = render(WatchScopeSetting);
    expect(body).toContain("Narrow (skip ignored build folders)");
    expect(body).toContain("Whole worktree");
    expect(body).toMatch(/aria-pressed="true"[^>]*data-active="true"[^>]*>\s*Narrow/);
    expect(body).toMatch(/aria-pressed="false"[^>]*data-active="false"[^>]*>\s*Whole worktree/);
    expect(body).toContain("only affects macOS");
  });
});
