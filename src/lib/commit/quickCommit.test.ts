import { describe, expect, it, vi } from "vitest";
import {
  promptQuickCommit,
  QUICK_COMMIT_BLANK_MESSAGE,
  QUICK_COMMIT_CONFLICTS,
  QUICK_COMMIT_EMPTY,
  QUICK_COMMIT_NO_REPO,
  type QuickCommitPromptDeps,
  type QuickCommitState,
} from "./quickCommit";

function state(overrides: Partial<QuickCommitState> = {}): QuickCommitState {
  return {
    currentPath: "/r/app",
    currentBranch: "feat/quick",
    statuses: [{ is_conflicted: false }],
    commitDraft: "feat: draft",
    ...overrides,
  };
}

function deps(
  overrides: Partial<QuickCommitPromptDeps> = {},
): QuickCommitPromptDeps & {
  errors: Array<string | null>;
  drafts: string[];
  amending: boolean[];
  commits: string[];
} {
  const errors: Array<string | null> = [];
  const drafts: string[] = [];
  const amending: boolean[] = [];
  const commits: string[] = [];
  return {
    errors,
    drafts,
    amending,
    commits,
    getState: () => state(),
    askMessage: async () => "feat: from prompt",
    commitAll: async (message) => {
      commits.push(message);
      return { ok: true };
    },
    setDraft: (message) => {
      drafts.push(message);
    },
    setAmending: (value) => {
      amending.push(value);
    },
    setError: (error) => {
      errors.push(error);
    },
    ...overrides,
  };
}

describe("promptQuickCommit", () => {
  it("refuses when no repository is open, without prompting", async () => {
    const d = deps({
      getState: () => state({ currentPath: null, statuses: [] }),
    });
    const asked = vi.fn();
    d.askMessage = asked;
    const outcome = await promptQuickCommit(d);
    expect(outcome).toEqual({ ok: false, error: QUICK_COMMIT_NO_REPO });
    expect(d.errors).toEqual([QUICK_COMMIT_NO_REPO]);
    expect(asked).not.toHaveBeenCalled();
    expect(d.commits).toEqual([]);
  });

  it("refuses a conflicted tree before prompting", async () => {
    const d = deps({
      getState: () =>
        state({
          statuses: [{ is_conflicted: true }, { is_conflicted: false }],
        }),
    });
    const asked = vi.fn();
    d.askMessage = asked;
    const outcome = await promptQuickCommit(d);
    expect(outcome.error).toBe(QUICK_COMMIT_CONFLICTS);
    expect(asked).not.toHaveBeenCalled();
  });

  it("refuses a clean tree before prompting", async () => {
    const d = deps({ getState: () => state({ statuses: [] }) });
    const asked = vi.fn();
    d.askMessage = asked;
    const outcome = await promptQuickCommit(d);
    expect(outcome.error).toBe(QUICK_COMMIT_EMPTY);
    expect(asked).not.toHaveBeenCalled();
  });

  it("treats cancel as a silent no-op", async () => {
    const d = deps({ askMessage: async () => null });
    const outcome = await promptQuickCommit(d);
    expect(outcome).toEqual({ ok: false });
    expect(d.errors).toEqual([]);
    expect(d.commits).toEqual([]);
  });

  it("refuses a whitespace-only message after confirm", async () => {
    const d = deps({ askMessage: async () => "   \n" });
    const outcome = await promptQuickCommit(d);
    expect(outcome.error).toBe(QUICK_COMMIT_BLANK_MESSAGE);
    expect(d.commits).toEqual([]);
  });

  it("commits the trimmed message, names the file count, and clears the draft", async () => {
    const asked: Array<{
      message: string;
      initialValue: string;
      confirmLabel: string;
    }> = [];
    const d = deps({
      getState: () =>
        state({
          statuses: [{ is_conflicted: false }, { is_conflicted: false }],
          commitDraft: "wip",
        }),
      askMessage: async (opts) => {
        asked.push({
          message: opts.message,
          initialValue: opts.initialValue,
          confirmLabel: opts.confirmLabel,
        });
        return "  feat: land it  ";
      },
    });
    const outcome = await promptQuickCommit(d);
    expect(outcome.ok).toBe(true);
    expect(d.commits).toEqual(["feat: land it"]);
    expect(d.drafts).toEqual([""]);
    expect(d.amending).toEqual([false]);
    expect(asked[0]?.message).toBe(
      "Stage all changes and commit 2 files on feat/quick.",
    );
    expect(asked[0]?.initialValue).toBe("wip");
    expect(asked[0]?.confirmLabel).toBe("Commit all");
  });

  it("singularizes the file count and falls back to HEAD when detached", async () => {
    const asked: string[] = [];
    const d = deps({
      getState: () =>
        state({ currentBranch: null, statuses: [{ is_conflicted: false }] }),
      askMessage: async (opts) => {
        asked.push(opts.message);
        return "msg";
      },
    });
    await promptQuickCommit(d);
    expect(asked).toEqual(["Stage all changes and commit 1 file on HEAD."]);
  });

  it("keeps the draft when the mutation is refused", async () => {
    const d = deps({
      commitAll: async () => ({ ok: false, error: "Blocked by policy" }),
    });
    const outcome = await promptQuickCommit(d);
    expect(outcome.ok).toBe(false);
    expect(d.drafts).toEqual([]);
    expect(d.amending).toEqual([]);
  });
});
