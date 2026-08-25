import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import RepoTabBar from "./RepoTabBar.svelte";

import { repoStore } from "../stores/repoStore";

describe("RepoTabBar", () => {
  it("renders nothing when no tabs are open", () => {
    const { body } = render(RepoTabBar);
    expect(body).not.toContain('title="Open repository"');
  });

  it("renders tab bar when tabs are open", async () => {
    await repoStore.openRepo("/repo/my-project", { allowBroken: true, activate: true });

    const { body } = render(RepoTabBar);
    expect(body).toContain("my-project");
    expect(body).toContain('title="Open repository"');
    expect(body).toContain('title="Recent repositories"');

    await repoStore.closeActiveTab();
  });
});
