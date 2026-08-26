import { get } from "svelte/store";
import { askText } from "../stores/modalStore";
import {
  repoStore,
  type FileStatus,
  type MutationOutcome,
} from "../stores/repoStore";

export const QUICK_COMMIT_NO_REPO = "No repository is open.";
export const QUICK_COMMIT_CONFLICTS =
  "Resolve merge conflicts before committing.";
export const QUICK_COMMIT_EMPTY = "Nothing to commit.";
export const QUICK_COMMIT_BLANK_MESSAGE = "Commit message must not be empty.";

export interface QuickCommitState {
  currentPath: string | null;
  currentBranch: string | null;
  statuses: Pick<FileStatus, "is_conflicted">[];
  commitDraft: string;
}

export interface QuickCommitPromptDeps {
  getState: () => QuickCommitState;
  askMessage: (opts: {
    title: string;
    message: string;
    placeholder: string;
    initialValue: string;
    confirmLabel: string;
  }) => Promise<string | null>;
  commitAll: (message: string) => Promise<MutationOutcome>;
  setDraft: (message: string) => void;
  setAmending: (value: boolean) => void;
  setError: (error: string | null) => void;
}

export function defaultQuickCommitDeps(): QuickCommitPromptDeps {
  return {
    getState: () => get(repoStore),
    askMessage: (opts) => askText(opts),
    commitAll: (message) => repoStore.quickCommit(message),
    setDraft: (message) => repoStore.setCommitDraft(message),
    setAmending: (value) => repoStore.setAmending(value),
    setError: (error) => repoStore.setError(error),
  };
}

function fileCountLabel(n: number): string {
  return n === 1 ? "1 file" : `${n} files`;
}

/**
 * Command-palette / native-menu flow: refuse conflicts and a clean tree
 * before prompting, then stage-all+commit as one mutation. Cancel (Escape
 * or backdrop) is silent. A successful commit clears the draft so the
 * composer does not re-offer the message that just landed.
 */
export async function promptQuickCommit(
  deps: QuickCommitPromptDeps = defaultQuickCommitDeps(),
): Promise<MutationOutcome> {
  const state = deps.getState();
  if (!state.currentPath) {
    deps.setError(QUICK_COMMIT_NO_REPO);
    return { ok: false, error: QUICK_COMMIT_NO_REPO };
  }
  if (state.statuses.some((s) => s.is_conflicted)) {
    deps.setError(QUICK_COMMIT_CONFLICTS);
    return { ok: false, error: QUICK_COMMIT_CONFLICTS };
  }
  if (state.statuses.length === 0) {
    deps.setError(QUICK_COMMIT_EMPTY);
    return { ok: false, error: QUICK_COMMIT_EMPTY };
  }
  const branch = state.currentBranch ?? "HEAD";
  const typed = await deps.askMessage({
    title: "Quick Commit",
    message: `Stage all changes and commit ${fileCountLabel(state.statuses.length)} on ${branch}.`,
    placeholder: "feat: …",
    initialValue: state.commitDraft,
    confirmLabel: "Commit all",
  });
  if (typed === null) return { ok: false };
  const message = typed.trim();
  if (!message) {
    deps.setError(QUICK_COMMIT_BLANK_MESSAGE);
    return { ok: false, error: QUICK_COMMIT_BLANK_MESSAGE };
  }
  const outcome = await deps.commitAll(message);
  if (outcome.ok) {
    deps.setDraft("");
    deps.setAmending(false);
  }
  return outcome;
}
