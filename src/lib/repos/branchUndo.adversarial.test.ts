import { get } from "svelte/store";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { MutationOutcome, MutationTarget } from "./storeTypes";

const createBranch = vi.fn<(name: string, startPoint?: string, target?: MutationTarget | null) => Promise<MutationOutcome>>();
vi.mock("../stores/repoStore", () => ({ repoStore: { createBranch: (...args: Parameters<typeof createBranch>) => createBranch(...args) } }));

import { toastStore } from "../stores/toastStore";
import { offerBranchUndo } from "./branchUndo";

const target: MutationTarget = { tabId: "tab-a", path: "/r/a" };
const undoToast = () => get(toastStore).find((t) => t.action);

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((res) => {
    resolve = res;
  });
  return { promise, resolve };
}

describe("attack 5: Undo", () => {
  beforeEach(() => createBranch.mockReset());
  afterEach(() => {
    toastStore.clear();
    vi.useRealTimers();
  });

  it("HOLDS: a double click while a restore is running creates each branch once and the second click keeps the offer", async () => {
    const gate = deferred<MutationOutcome>();
    createBranch.mockReturnValue(gate.promise);
    offerBranchUndo("Deleted", [{ name: "x", tip: "a" }, { name: "y", tip: "b" }], target);
    const first = undoToast()!.action!.onClick();
    const second = undoToast()!.action!.onClick();
    expect(await second).toBe(false);
    gate.resolve({ ok: true });
    expect(await first).toBe(true);
    expect(createBranch.mock.calls.map((c) => c[0])).toEqual(["x", "y"]);
    expect(get(toastStore).filter((t) => t.message.startsWith("Restored")).map((t) => t.message)).toEqual(["Restored 2 branches"]);
  });

  it("HOLDS: an Undo whose tab closed restores nothing and names every branch with its sha", async () => {
    createBranch.mockResolvedValue({ ok: false, error: "The repository changed while this was waiting; nothing was run.", refused: true, surfaced: false });
    offerBranchUndo("Deleted 2", [{ name: "x", tip: "a1" }, { name: "y", tip: "b2" }], target);
    expect(await undoToast()!.action!.onClick()).toBe(false);
    const message = get(toastStore).find((t) => t.kind === "error")!.message;
    expect(message).toContain("x at a1");
    expect(message).toContain("y at b2");
  });

  it("CHARACTERIZATION: two tips with the same branch name can never both be restored, so the retained Undo can never finish", async () => {
    const made = new Set<string>();
    createBranch.mockImplementation(async (name) => {
      if (made.has(name)) return { ok: false, error: "already exists" };
      made.add(name);
      return { ok: true };
    });
    offerBranchUndo("Deleted", [{ name: "x", tip: "a" }, { name: "x", tip: "b" }], target);
    expect(await undoToast()!.action!.onClick()).toBe(false);
    expect(await undoToast()!.action!.onClick()).toBe(false);
    expect(get(toastStore).find((t) => t.kind === "error")!.message).toContain("Restored 1 of 2");
  });

  it("BREAK: a restore that fails just before the 12 s clock ends leaves no offer to retry, though the contract says Undo stays available", async () => {
    vi.useFakeTimers();
    createBranch.mockResolvedValue({ ok: false, error: "busy" });
    offerBranchUndo("Deleted", [{ name: "x", tip: "a" }], target);
    await vi.advanceTimersByTimeAsync(11_900);
    expect(await undoToast()!.action!.onClick()).toBe(false);
    await vi.advanceTimersByTimeAsync(500);
    expect(undoToast()).toBeDefined();
  });
});
