import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../ipc/invoke", () => ({ invoke: vi.fn() }));

const { invoke } = await import("../ipc/invoke");
const { deadbranchClean, deadbranchListBackups, deadbranchRestore, deadbranchScan } = await import("./deadbranch");
const invokeMock = vi.mocked(invoke);

beforeEach(() => invokeMock.mockReset());

describe("the deadbranch IPC wrappers", () => {
  it("scans with the config, or null when none was given", async () => {
    invokeMock.mockResolvedValue({ stale: [] });
    await deadbranchScan("/r");
    expect(invokeMock).toHaveBeenLastCalledWith("cmd_deadbranch_scan", { repoPath: "/r", config: null });
    const config = { staleDays: 30 } as never;
    await deadbranchScan("/r", config);
    expect(invokeMock).toHaveBeenLastCalledWith("cmd_deadbranch_scan", { repoPath: "/r", config });
  });

  it("cleans with backups on and force off unless told otherwise, and unwraps the guarded output", async () => {
    invokeMock.mockResolvedValue({ policy: {}, output: { deleted: ["a"] } });
    await expect(deadbranchClean("/r", ["a"])).resolves.toEqual({ deleted: ["a"] });
    expect(invokeMock).toHaveBeenLastCalledWith("cmd_deadbranch_clean", {
      repoPath: "/r",
      branches: ["a"],
      force: false,
      createBackup: true,
    });
    await deadbranchClean("/r", ["a"], true, false);
    expect(invokeMock).toHaveBeenLastCalledWith("cmd_deadbranch_clean", {
      repoPath: "/r",
      branches: ["a"],
      force: true,
      createBackup: false,
    });
  });

  it("lists backups for the repository", async () => {
    invokeMock.mockResolvedValue([{ path: "/b/1" }]);
    await expect(deadbranchListBackups("/r")).resolves.toEqual([{ path: "/b/1" }]);
    expect(invokeMock).toHaveBeenCalledWith("cmd_deadbranch_list_backups", { repoPath: "/r" });
  });

  it("restores everything in a backup unless branches are named, and unwraps the guarded output", async () => {
    invokeMock.mockResolvedValue({ policy: {}, output: { restored: ["a"] } });
    await expect(deadbranchRestore("/r", "/b/1")).resolves.toEqual({ restored: ["a"] });
    expect(invokeMock).toHaveBeenLastCalledWith("cmd_deadbranch_restore", {
      repoPath: "/r",
      backupPath: "/b/1",
      branches: null,
    });
    await deadbranchRestore("/r", "/b/1", ["a"]);
    expect(invokeMock).toHaveBeenLastCalledWith("cmd_deadbranch_restore", {
      repoPath: "/r",
      backupPath: "/b/1",
      branches: ["a"],
    });
  });

  it("does not hide a failed clean behind an empty result", async () => {
    invokeMock.mockRejectedValueOnce(new Error("guard refused"));
    const outcome = await deadbranchClean("/r", ["a"]).then(
      () => "resolved",
      (error: Error) => error.message,
    );
    expect(outcome).toBe("guard refused");
  });
});
