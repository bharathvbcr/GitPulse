import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../ipc/invoke", () => ({ invoke: vi.fn() }));

const { invoke } = await import("../ipc/invoke");
const { deadbranchListBackups, deadbranchScan } = await import("./deadbranch");
const invokeMock = vi.mocked(invoke);

beforeEach(() => invokeMock.mockReset());

// Clean and restore are repoStore mutations (repoStore.deadbranchClean and
// deadbranchRestore, tested in repoStore.test.ts), not IPC wrappers.
describe("the deadbranch IPC wrappers", () => {
  it("scans with the config, or null when none was given", async () => {
    invokeMock.mockResolvedValue({ stale: [] });
    await deadbranchScan("/r");
    expect(invokeMock).toHaveBeenLastCalledWith("cmd_deadbranch_scan", { repoPath: "/r", config: null });
    const config = { staleDays: 30 } as never;
    await deadbranchScan("/r", config);
    expect(invokeMock).toHaveBeenLastCalledWith("cmd_deadbranch_scan", { repoPath: "/r", config });
  });

  it("lists backups for the repository", async () => {
    invokeMock.mockResolvedValue([{ path: "/b/1" }]);
    await expect(deadbranchListBackups("/r")).resolves.toEqual([{ path: "/b/1" }]);
    expect(invokeMock).toHaveBeenCalledWith("cmd_deadbranch_list_backups", { repoPath: "/r" });
  });
});
