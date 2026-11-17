import { afterEach, describe, expect, it } from "vitest";
import { render } from "svelte/server";
import WatchScopeSetting from "./WatchScopeSetting.svelte";
import { hostPlatformNow, resetHostPlatformForTests } from "../stores/platformStore";

afterEach(() => resetHostPlatformForTests());

describe("WatchScopeSetting", () => {
  it("renders both options with Narrow selected by default, and the help text", () => {
    resetHostPlatformForTests({ ...hostPlatformNow(), os: "macos" });
    const { body } = render(WatchScopeSetting);
    expect(body).toContain("Narrow (skip ignored build folders)");
    expect(body).toContain("Whole worktree");
    expect(body).toMatch(/aria-pressed="true"[^>]*data-active="true"[^>]*>\s*Narrow/);
    expect(body).toMatch(/aria-pressed="false"[^>]*data-active="false"[^>]*>\s*Whole worktree/);
    expect(body).toContain("Narrow stops watching folders git ignores");
  });

  it("tells a host other than macOS that the choice changes nothing there", () => {
    resetHostPlatformForTests({ ...hostPlatformNow(), os: "linux" });
    const { body } = render(WatchScopeSetting);
    expect(body).toContain("macOS only");
    expect(body).toContain("Linux");
    expect(body).not.toContain("Narrow stops watching");
  });
});
