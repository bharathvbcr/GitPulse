import { get } from "svelte/store";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { MutationOutcome, MutationTarget } from "./storeTypes";

const createBranch = vi.fn<(name: string, startPoint?: string, target?: MutationTarget | null) => Promise<MutationOutcome>>();
vi.mock("../stores/repoStore", () => ({ repoStore: { createBranch: (...args: Parameters<typeof createBranch>) => createBranch(...args) } }));

import { toastStore } from "../stores/toastStore";
import { offerBranchUndo } from "./branchUndo";

const target: MutationTarget = { tabId: "tab-a", path: "/r/a" };

async function clickUndo(): Promise<boolean | void> {
  const toast = get(toastStore).find((t) => t.action);
  expect(toast?.action?.label).toBe("Undo");
  return toast!.action!.onClick();
}

describe("offerBranchUndo", () => {
  beforeEach(() => createBranch.mockReset());
  afterEach(() => toastStore.clear());

  it("with nothing to restore shows the plain message", () => {
    offerBranchUndo('Deleted branch "x"', [], target);
    expect(get(toastStore).map((t) => [t.kind, t.message, t.action])).toEqual([["success", 'Deleted branch "x"', undefined]]);
  });

  it("restores into the originating repository and reports one branch by name", async () => {
    createBranch.mockResolvedValue({ ok: true });
    offerBranchUndo('Deleted branch "x"', [{ name: "x", tip: "abc" }], target);
    expect(await clickUndo()).toBe(true);
    expect(createBranch).toHaveBeenCalledWith("x", "abc", target);
    expect(get(toastStore).map((t) => t.message)).toContain('Restored branch "x"');
  });

  it("reports N branches when every tip is restored", async () => {
    createBranch.mockResolvedValue({ ok: true });
    offerBranchUndo("Deleted 2 branches", [{ name: "x", tip: "a1" }, { name: "y", tip: "b2" }], target);
    expect(await clickUndo()).toBe(true);
    expect(get(toastStore).map((t) => t.message)).toContain("Restored 2 branches");
  });

  it("continues past a failure, names each failed branch with its sha, and keeps the offer", async () => {
    createBranch.mockImplementation(async (name) => (name === "y" ? { ok: false, error: "already exists" } : { ok: true }));
    offerBranchUndo("Deleted 3 branches", [{ name: "x", tip: "a1" }, { name: "y", tip: "b2" }, { name: "z", tip: "c3" }], target);
    expect(await clickUndo()).toBe(false);
    expect(createBranch).toHaveBeenCalledTimes(3);
    const error = get(toastStore).find((t) => t.kind === "error");
    expect(error?.message).toBe("Restored 2 of 3 branches. Failed: y at b2 (already exists)");
    expect(get(toastStore).some((t) => t.message.startsWith("Restored branch") || t.message === "Restored 3 branches")).toBe(false);
  });

  it("a retry restores only what is still missing", async () => {
    createBranch.mockImplementation(async (name) => (name === "y" ? { ok: false, error: "busy" } : { ok: true }));
    offerBranchUndo("Deleted 2 branches", [{ name: "x", tip: "a1" }, { name: "y", tip: "b2" }], target);
    expect(await clickUndo()).toBe(false);
    createBranch.mockClear();
    createBranch.mockResolvedValue({ ok: true });
    expect(await clickUndo()).toBe(true);
    expect(createBranch.mock.calls.map((c) => c[0])).toEqual(["y"]);
    expect(get(toastStore).map((t) => t.message)).toContain("Restored 2 branches");
  });

  it("a refused target restores nothing and says the repository changed", async () => {
    createBranch.mockResolvedValue({ ok: false, error: "The repository changed while this was waiting; nothing was run.", refused: true, surfaced: false });
    offerBranchUndo('Deleted branch "x"', [{ name: "x", tip: "abc" }], target);
    expect(await clickUndo()).toBe(false);
    const error = get(toastStore).find((t) => t.kind === "error");
    expect(error?.message).toContain("Could not restore the branch.");
    expect(error?.message).toContain("x at abc");
    expect(error?.message).toContain("repository changed");
  });
});
