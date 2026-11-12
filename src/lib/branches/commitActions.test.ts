import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ResetPreview } from "./types";

/**
 * The commit actions the details pane and the graph's row menu share. Driven
 * through mocked prompts and IPC: what is asked, in what order, and what
 * happens on each answer.
 */

const state = { currentPath: "/r/a" as string | null };
const order: string[] = [];
const createBranch = vi.fn(async (..._args: unknown[]) => ({ ok: true, surfaced: false }));
const resetTo = vi.fn(async (..._args: unknown[]) => ({ ok: true, surfaced: false }));
const captureTarget = vi.fn(() => {
  order.push("capture");
  return { tabId: "tab-a", path: "/r/a" };
});
let textAnswer: string | null = "recovered/work";
let confirmAnswer = true;
const askText = vi.fn(async (_opts: unknown) => {
  order.push("askText");
  return textAnswer;
});
const askConfirm = vi.fn(async (_opts: unknown) => {
  order.push("askConfirm");
  return confirmAnswer;
});
let preview: ResetPreview;
let previewError: Error | null = null;
const invoke = vi.fn(async (cmd: string, _args: unknown) => {
  order.push(cmd);
  if (previewError) throw previewError;
  return preview;
});
const toasts: string[] = [];

vi.mock("../stores/repoStore", () => ({
  repoStore: {
    subscribe: (run: (value: typeof state) => void) => {
      run(state);
      return () => {};
    },
    captureTarget: () => captureTarget(),
    createBranch: (...args: unknown[]) => createBranch(...args),
    resetTo: (...args: unknown[]) => resetTo(...args),
  },
}));
vi.mock("../stores/modalStore", () => ({
  askText: (opts: unknown) => askText(opts),
  askConfirm: (opts: unknown) => askConfirm(opts),
}));
vi.mock("../ipc/invoke", () => ({ invoke: (cmd: string, args: unknown) => invoke(cmd, args) }));
vi.mock("../stores/toastStore", () => ({
  toastStore: {
    success: (m: string) => toasts.push(`ok:${m}`),
    error: (m: string) => toasts.push(`err:${m}`),
  },
}));

const { createBranchAt, resetKeepTo } = await import("./commitActions");

const OID = "a".repeat(40);
const TARGET = "b".repeat(40);

beforeEach(() => {
  state.currentPath = "/r/a";
  order.length = 0;
  toasts.length = 0;
  textAnswer = "recovered/work";
  confirmAnswer = true;
  previewError = null;
  preview = {
    head: "c".repeat(40),
    target: TARGET,
    branch: "main",
    leaving: [{ commit_id: "d".repeat(40), summary: "lost work" }],
    leaving_total: 1,
    unreachable_total: 1,
    gaining_total: 0,
  } as unknown as ResetPreview;
  for (const fn of [createBranch, resetTo, askText, askConfirm, invoke, captureTarget]) fn.mockClear();
});

describe("createBranchAt", () => {
  it("names the repository before asking, then creates the branch there", async () => {
    expect(await createBranchAt(OID, "HEAD@{1}")).toBe(true);
    expect(order.slice(0, 2)).toEqual(["capture", "askText"]);
    expect(createBranch).toHaveBeenCalledWith("recovered/work", OID, { tabId: "tab-a", path: "/r/a" });
    expect(String((askText.mock.calls[0]![0] as { message: string }).message)).toContain("HEAD@{1}");
  });

  it("does nothing when the prompt is cancelled or blank", async () => {
    textAnswer = null;
    expect(await createBranchAt(OID)).toBe(false);
    textAnswer = "   ";
    expect(await createBranchAt(OID)).toBe(false);
    expect(createBranch).not.toHaveBeenCalled();
  });

  it("reports a failure the store did not surface", async () => {
    createBranch.mockResolvedValueOnce({ ok: false, surfaced: false, error: "exists" } as never);
    expect(await createBranchAt(OID)).toBe(false);
    expect(toasts.some((t) => t.startsWith("err:Couldn't create the branch"))).toBe(true);
  });
});

describe("resetKeepTo", () => {
  it("previews, then confirms, then resets with --keep to the oid it previewed", async () => {
    expect(await resetKeepTo(OID, "HEAD@{2}")).toBe(true);
    expect(order).toEqual(["capture", "cmd_reset_preview", "askConfirm"]);
    expect(invoke).toHaveBeenCalledWith("cmd_reset_preview", { repoPath: "/r/a", target: OID });
    // The previewed oid, not the one asked about: what was shown is what moves.
    expect(resetTo).toHaveBeenCalledWith("keep", TARGET, { tabId: "tab-a", path: "/r/a" });
    const confirm = askConfirm.mock.calls[0]![0] as { message: string; destructive: boolean };
    expect(confirm.destructive).toBe(true);
    expect(confirm.message).toContain("(HEAD@{2})");
  });

  it("is not destructive when no commit leaves the branch", async () => {
    preview = { ...preview, leaving: [], leaving_total: 0, unreachable_total: 0 };
    await resetKeepTo(OID);
    const confirm = askConfirm.mock.calls[0]![0] as { message: string; destructive: boolean };
    expect(confirm.destructive).toBe(false);
    expect(confirm.message).not.toContain("()");
  });

  it("does not reset when the confirmation is declined", async () => {
    confirmAnswer = false;
    expect(await resetKeepTo(OID)).toBe(false);
    expect(resetTo).not.toHaveBeenCalled();
  });

  it("drops a preview read for a repository that is no longer current", async () => {
    invoke.mockImplementationOnce(async (cmd: string) => {
      order.push(cmd);
      state.currentPath = "/r/other";
      return preview;
    });
    expect(await resetKeepTo(OID)).toBe(false);
    expect(askConfirm).not.toHaveBeenCalled();
    expect(resetTo).not.toHaveBeenCalled();
  });

  it("says the preview failed rather than resetting blind", async () => {
    previewError = new Error("not a commit");
    expect(await resetKeepTo(OID)).toBe(false);
    expect(askConfirm).not.toHaveBeenCalled();
    expect(toasts.some((t) => t.includes("Couldn't preview the reset"))).toBe(true);
  });

  it("refuses without a repository", async () => {
    state.currentPath = null;
    expect(await resetKeepTo(OID)).toBe(false);
    expect(invoke).not.toHaveBeenCalled();
  });

  it("forgets the reflog label once HEAD has moved", async () => {
    // Selectors shift on every HEAD move; a stale HEAD@{2} in the next prompt
    // would name a different entry than the one on screen.
    const { historySelectionLabel } = await import("../views/historyList");
    const { get } = await import("svelte/store");
    historySelectionLabel.set({ commitId: OID, label: "HEAD@{2}" });
    confirmAnswer = false;
    await resetKeepTo(OID, "HEAD@{2}");
    expect(get(historySelectionLabel)).not.toBeNull();
    confirmAnswer = true;
    await resetKeepTo(OID, "HEAD@{2}");
    expect(get(historySelectionLabel)).toBeNull();
  });

  it("never resets hard", async () => {
    await resetKeepTo(OID);
    expect(resetTo.mock.calls.every((call) => call[0] === "keep")).toBe(true);
  });
});
