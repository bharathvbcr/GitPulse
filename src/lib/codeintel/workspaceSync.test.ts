import { afterEach, describe, expect, it, vi } from "vitest";
import {
  flushWorkspaceSyncForTests,
  resetWorkspaceSyncForTests,
  scheduleWorkspaceSync,
} from "./workspaceSync";
import { syncWorkspaceTabs } from "./client";

vi.mock("./client", () => ({
  syncWorkspaceTabs: vi.fn().mockResolvedValue({
    version: 1,
    registry_root: "/a",
    registry_path: "/a/.devmap/workspace.json",
    repos: [],
  }),
}));

describe("workspaceSync", () => {
  afterEach(() => {
    resetWorkspaceSyncForTests();
    vi.clearAllMocks();
  });

  it("does nothing without a registry root", async () => {
    scheduleWorkspaceSync(null, ["/a"]);
    await flushWorkspaceSyncForTests();
    expect(syncWorkspaceTabs).not.toHaveBeenCalled();
  });

  it("flushes the latest open-tab set", async () => {
    scheduleWorkspaceSync("/a", ["/a", "/b"]);
    scheduleWorkspaceSync("/a", ["/a"]);
    await flushWorkspaceSyncForTests();
    expect(syncWorkspaceTabs).toHaveBeenCalledTimes(1);
    expect(syncWorkspaceTabs).toHaveBeenCalledWith("/a", ["/a"]);
  });
});
