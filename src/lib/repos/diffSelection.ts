import { beginGeneration } from "../async/guard";
import { formatError } from "../ui/formatError";
import type { InvokeFn } from "../stores/graphStore";
import type { DiffPayload, RepoSession, SelectionKind } from "./storeTypes";

type SessionPatch = Partial<RepoSession> | ((current: RepoSession) => Partial<RepoSession>);

export interface DiffSelectionDeps {
  invoke: InvokeFn;
  activeSession(): RepoSession | undefined;
  /** Applies a patch to the session if it is still at `generation`; see `applyToSession`. */
  applyToSession(id: string, generation: number, patch: SessionPatch): boolean;
  /** The store's navigation epoch, which moves when the user opens or switches tabs. */
  openEpoch(): number;
}

/** One diff the reader can ask for: which command reads it, and what it selects. */
export interface DiffSelectionSpec {
  /**
   * The IPC read, written out at the call site so each command name stays a
   * literal `scripts/check-ipc-contract.mjs` can match against the Rust
   * registry; a name passed through this module as data would hide it.
   */
  read(invoke: InvokeFn, repoPath: string, ignoreWhitespace: boolean): Promise<DiffPayload>;
  filePath: string | null;
  commitId: string | null;
  isStaged: boolean;
  kind: SelectionKind;
  /**
   * Whether the answer brings the History tab's diff section to the front:
   * always, only if the user has not navigated since the request (a worktree
   * file click can be answered after they moved on), or never (the commit
   * list selects in place).
   */
  history: "always" | "unlessNavigated" | "never";
  /** A worktree read honours the session's whitespace preference and records it with its answer. */
  worktree?: boolean;
}

/**
 * The one path every diff selection takes: file, commit, commit file and
 * range. They used to be four copies of the same fetch, token check, success
 * patch and failure patch that differed only in the command, the selection
 * fields and whether the History tab was raised, so a fix to one (the
 * stale-answer token, the pending flag) had to be made four times.
 *
 * Owns the selection token. Session `generation` only moves on tab
 * activation, so it cannot order two rapid selections of the same tab; the
 * token does, and a superseded answer, success or failure, is dropped.
 */
export function createDiffSelection(deps: DiffSelectionDeps) {
  const selectionGeneration = beginGeneration();

  /**
   * Marks a diff fetch as in flight, but only when the target actually moves.
   *
   * The selections are also how the app refetches the diff already on screen
   * after a mutation or a watcher event. Raising the flag for those would
   * blink a skeleton over content that is about to be replaced by nearly the
   * same content, several times a minute.
   */
  const beginFetch = (
    session: RepoSession,
    target: { filePath: string | null; commitId: string | null; isStaged: boolean; ignoreWhitespace: boolean },
  ): void => {
    const unchanged =
      session.selectedFilePath === target.filePath &&
      session.selectedCommitId === target.commitId &&
      session.selectedIsStaged === target.isStaged &&
      session.selectedIgnoreWhitespace === target.ignoreWhitespace &&
      session.selectedDiff !== null;
    if (unchanged) return;
    deps.applyToSession(session.id, session.generation, { selectedDiffPending: true, selectedDiffError: null });
  };

  /**
   * A diff read that failed: the selection the reader asked for, no diff, and
   * the reason, on the selection itself. The previous file's diff is cleared
   * rather than left under the new file's name, and the repository snapshot's
   * `error` is untouched (it means "status unavailable", which this is not).
   */
  const failFetch = (session: RepoSession, generation: number, err: unknown, selection: SessionPatch): void => {
    deps.applyToSession(session.id, generation, (current) => ({
      ...(typeof selection === "function" ? selection(current) : selection),
      selectedDiff: null,
      selectedDiffTruncated: false,
      selectedDiffTruncationReason: null,
      selectedDiffError: formatError(err),
      selectedDiffPending: false,
    }));
  };

  return {
    /** Supersedes every in-flight selection, for callers that replace the selection themselves. */
    supersede(): number {
      return selectionGeneration.next();
    },

    async select(spec: DiffSelectionSpec): Promise<void> {
      const session = deps.activeSession();
      if (!session) return;
      const generation = session.generation;
      const navigationEpoch = deps.openEpoch();
      // The whitespace preference lives on the session, not on call sites:
      // every refetch of this diff must carry whatever the user last chose.
      const ignoreWhitespace = session.selectedIgnoreWhitespace;
      const token = selectionGeneration.next();
      beginFetch(session, {
        filePath: spec.filePath,
        commitId: spec.commitId,
        isStaged: spec.isStaged,
        ignoreWhitespace,
      });
      const target = {
        selectedFilePath: spec.filePath,
        selectedCommitId: spec.commitId,
        selectedIsStaged: spec.isStaged,
        selectionKind: spec.kind,
      };
      const history = (current: RepoSession): Partial<RepoSession> =>
        spec.history === "always" || (spec.history === "unlessNavigated" && navigationEpoch === deps.openEpoch())
          ? { activeTab: "history" as const, viewSections: { ...current.viewSections, history: "diff" } }
          : {};
      try {
        const diff = await spec.read(deps.invoke, session.path, ignoreWhitespace);
        if (!selectionGeneration.isCurrent(token)) return;
        deps.applyToSession(session.id, generation, (current) => ({
          ...target,
          selectedDiff: diff.text,
          selectedDiffTruncated: diff.truncated,
          selectedDiffTruncationReason: diff.truncation_reason ?? null,
          selectedDiffPending: false,
          selectedDiffError: null,
          ...(spec.worktree ? { selectedIgnoreWhitespace: ignoreWhitespace } : {}),
          ...history(current),
        }));
      } catch (err: unknown) {
        if (!selectionGeneration.isCurrent(token)) return;
        // A fetch that errored is not still running: the flag clears with it.
        failFetch(session, generation, err, (current) => ({ ...target, ...history(current) }));
      }
    },
  };
}
