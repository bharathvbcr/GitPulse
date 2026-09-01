import { describe, expect, it } from "vitest";
import { showsWorkspaceControls } from "./WorkspaceActions.svelte";

describe("showsWorkspaceControls", () => {
  it("hides the controls until a second repository is open", () => {
    // With one tab, "fetch all" is just "fetch" and the roll-up repeats what
    // the tab already says; the controls would be pure noise.
    expect(showsWorkspaceControls(0)).toBe(false);
    expect(showsWorkspaceControls(1)).toBe(false);
  });

  it("shows them from the second repository onward", () => {
    expect(showsWorkspaceControls(2)).toBe(true);
    expect(showsWorkspaceControls(24)).toBe(true);
  });
});
