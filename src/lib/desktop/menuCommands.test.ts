import { describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import { createRepoStore } from "../stores/repoStore";
import { createMenuCommands, type MenuCommandDeps } from "./menuCommands";

function harness() {
  let state = { ...get(createRepoStore({ storage: null })), currentPath: "/repo/a", currentBranch: "main", isLoading: false };
  const ok = () => vi.fn(async () => ({ ok: true }));
  const deps: MenuCommandDeps = {
    repo: { fetch: ok(), pull: ok(), push: ok(), stashSave: ok(), stashPop: ok(), stageAll: ok(), unstageAll: ok(),
      createBranch: ok(), renameBranch: ok(), operationAction: ok(),
      listRemotes: vi.fn(async () => ({ remotes: [], truncated: false })), clearRecents: vi.fn(), activateTab: vi.fn() },
    state: () => state, enabled: () => true, ready: () => true, quickCommit: ok(), askText: vi.fn(async () => "new-name"),
    askConfirm: vi.fn(async () => true), copy: vi.fn(async () => true), reveal: vi.fn(), open: vi.fn(),
    commitId: vi.fn(async () => "a".repeat(40)), error: vi.fn(), success: vi.fn(), activity: vi.fn(),
  };
  return { deps, commands: createMenuCommands(deps), change: (patch: Partial<typeof state>) => { state = { ...state, ...patch }; } };
}

describe("native repository commands", () => {
  it.each(["fetch", "pull", "push", "stash", "stashPop", "stageAll", "unstageAll"] as const)(
    "%s reports a refusal, never a success", async (action) => {
      const h = harness();
      const method = action === "stash" ? "stashSave" : action;
      vi.mocked(h.deps.repo[method]).mockResolvedValue({ ok: false, error: "Policy blocked" });
      await h.commands[action]();
      expect(h.deps.success).not.toHaveBeenCalled();
      expect(h.deps.error).toHaveBeenCalledWith("Policy blocked");
      expect(h.deps.activity).toHaveBeenLastCalledWith("/repo/a", null);
    },
  );
  it("disabled commands do not invoke Git", async () => {
    const h = harness(); h.deps.enabled = () => false;
    await h.commands.stageAll();
    expect(h.deps.repo.stageAll).not.toHaveBeenCalled();
  });
  it.each(["createBranch", "renameBranch"] as const)("%s cancels on repository switches and new busy state", async (action) => {
    for (const switched of [true, false]) {
      const h = harness();
      h.deps.askText = async () => {
        if (switched) h.change({ currentPath: "/repo/b" });
        else h.deps.ready = () => false;
        return "new-name";
      };
      await h.commands[action]();
      expect(h.deps.repo[action]).not.toHaveBeenCalled();
      expect(h.deps.error).toHaveBeenCalled();
    }
  });
  it("serializes duplicate invocations through the entire dialog", async () => {
    const h = harness();
    let resolve: (value: string) => void = () => {};
    h.deps.askText = () => new Promise((done) => { resolve = done; });
    const first = h.commands.createBranch();
    await h.commands.createBranch();
    await h.commands.fetch();
    expect(h.deps.repo.fetch).not.toHaveBeenCalled();
    resolve("feature/test"); await first;
    expect(h.deps.repo.createBranch).toHaveBeenCalledExactlyOnceWith("feature/test");
  });
  it("abort and skip require confirmation and refuse a changed operation", async () => {
    const h = harness();
    const operation = { kind: "Rebase" as const, current_step: 1, total_steps: 2, head_ref: "main", incoming_ref: "feature",
      conflicted_paths: [], conflicted_total: 0, available: ["abort", "skip", "continue"] as ("abort" | "skip" | "continue")[] };
    h.change({ operation: { operation, probeFailed: false } });
    h.deps.askConfirm = vi.fn(async () => false);
    await h.commands.operationAbort();
    expect(h.deps.repo.operationAction).not.toHaveBeenCalled();
    h.deps.askConfirm = vi.fn(async () => { h.change({ operation: { operation: null, probeFailed: false } }); return true; });
    await h.commands.operationSkip();
    expect(h.deps.repo.operationAction).not.toHaveBeenCalled();
    h.change({ operation: { operation, probeFailed: false } });
    await h.commands.operationContinue();
    expect(h.deps.repo.operationAction).toHaveBeenCalledExactlyOnceWith("continue");
  });
  it("resolves fresh HEAD or the selected SHA and reports clipboard failures", async () => {
    const h = harness();
    await h.commands.copyCommit();
    expect(h.deps.commitId).toHaveBeenCalledWith("/repo/a", "HEAD");
    expect(h.deps.copy).toHaveBeenCalledWith("a".repeat(40));
    h.change({ selectedCommitId: "b".repeat(40) });
    h.deps.copy = vi.fn(async () => false);
    await h.commands.copyCommit();
    expect(h.deps.commitId).toHaveBeenLastCalledWith("/repo/a", "b".repeat(40));
    expect(h.deps.error).toHaveBeenCalledWith("Could not copy to the clipboard.");
  });
  it("cannot copy a stale result into another repository's context", async () => {
    const h = harness();
    h.deps.commitId = async () => { h.change({ currentPath: "/repo/b" }); return "a".repeat(40); };
    await h.commands.copyCommit(); expect(h.deps.copy).not.toHaveBeenCalled();
  });
  it("opens the default remote website without credentials and surfaces handoff errors", async () => {
    const h = harness();
    h.deps.repo.listRemotes = async () => ({ remotes: [{ name: "origin", fetch_url: "https://user:secret@example.com/team/app.git",
      push_url: null, is_default: true, tracking_branches: 0 }], truncated: false });
    await h.commands.openRemote();
    expect(h.deps.open).toHaveBeenCalledExactlyOnceWith("https://example.com/team/app");
    h.deps.open = async () => { throw new Error("Browser refused"); };
    await h.commands.openRemote();
    expect(h.deps.error).toHaveBeenCalledWith("Browser refused");
  });
  it("does not infer a sole remote from a capped list", async () => {
    const h = harness();
    h.deps.repo.listRemotes = async () => ({ remotes: [{ name: "other", fetch_url: "git@example.com:team/app.git",
      push_url: null, is_default: false, tracking_branches: 0 }], truncated: true });
    await h.commands.openRemote();
    expect(h.deps.open).not.toHaveBeenCalled();
    expect(h.deps.error).toHaveBeenCalled();
  });
  it("reveals the repository root and surfaces failures", async () => {
    const h = harness();
    h.deps.reveal = vi.fn(async () => { throw new Error("Repository missing"); });
    await h.commands.revealRepo();
    expect(h.deps.reveal).toHaveBeenCalledExactlyOnceWith("/repo/a");
    expect(h.deps.error).toHaveBeenCalledWith("Repository missing");
  });
});
