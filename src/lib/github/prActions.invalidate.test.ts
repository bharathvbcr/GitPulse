import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.hoisted(() => vi.fn());
const confirmed = vi.hoisted(() => ({ value: true }));
vi.mock("../ipc/invoke", () => ({ invoke: (...args: unknown[]) => invoke(...args) }));
vi.mock("../stores/modalStore", () => ({ askConfirm: async () => confirmed.value }));
vi.mock("../stores/harnessStore", () => ({
  harnessStore: { recordVerdict: () => {}, recordAction: () => {} },
  verdictLabel: () => "",
}));

import { invalidateGitHubContext, invalidateOnReturn, readGitHubContext } from "../ipc/githubContext";
import { confirmAndRunPrAction } from "./prActions";

const context = { available: true, owner: "o", repo: "r", workflow_runs: [], releases: [] };
const contextReads = () => invoke.mock.calls.filter(([command]) => command === "cmd_github_context").length;
const merge = { kind: "merge", number: 7, method: "squash", delete_branch: false, head_oid: "c".repeat(40) } as const;

beforeEach(() => {
  invoke.mockReset();
  invalidateGitHubContext();
  confirmed.value = true;
  invoke.mockImplementation(async (command: string) => {
    if (command === "cmd_github_context") return context;
    if (command === "cmd_github_pr_action") return { output: "merged", policy: null };
    throw new Error(`unexpected ${command}`);
  });
});

describe("a pull-request action invalidates the shared GitHub context", () => {
  it("a read taken before a merge is not served after it, for any consumer", async () => {
    await readGitHubContext("/repo");
    await readGitHubContext("/repo");
    expect(contextReads()).toBe(1);
    await confirmAndRunPrAction("/repo", merge, "o/r", {});
    await readGitHubContext("/repo");
    expect(contextReads()).toBe(2);
  });

  it("a refused or failed action invalidates too, since it may have been applied in part", async () => {
    await readGitHubContext("/repo");
    invoke.mockImplementation(async (command: string) => {
      if (command === "cmd_github_pr_action") throw new Error("merge conflict");
      return context;
    });
    await expect(confirmAndRunPrAction("/repo", merge, "o/r", {})).rejects.toThrow("merge conflict");
    await readGitHubContext("/repo");
    expect(contextReads()).toBe(2);
  });

  it("an action the person cancelled changes nothing", async () => {
    await readGitHubContext("/repo");
    confirmed.value = false;
    await expect(confirmAndRunPrAction("/repo", merge, "o/r", {})).resolves.toBeNull();
    await readGitHubContext("/repo");
    expect(contextReads()).toBe(1);
  });

  it("only that repository's read is dropped", async () => {
    await readGitHubContext("/repo");
    await readGitHubContext("/other");
    await confirmAndRunPrAction("/repo", merge, "o/r", {});
    await readGitHubContext("/other");
    expect(contextReads()).toBe(2);
  });
});

describe("a sign-in, sign-out or token change", () => {
  it("is picked up when the person returns to the window, because that is the only signal", async () => {
    const target = new EventTarget();
    const stop = invalidateOnReturn(target);
    await readGitHubContext("/repo");
    await readGitHubContext("/repo");
    expect(contextReads()).toBe(1);
    target.dispatchEvent(new Event("focus")); // `gh auth login` ran in a terminal; the window is back
    await readGitHubContext("/repo");
    expect(contextReads()).toBe(2);
    stop();
    await readGitHubContext("/repo");
    target.dispatchEvent(new Event("focus"));
    await readGitHubContext("/repo");
    expect(contextReads()).toBe(2);
  });
});
