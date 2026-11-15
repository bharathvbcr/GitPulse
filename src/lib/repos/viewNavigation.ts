import { hasUnstagedChanges } from "../files/fileStatus";
import { graphStore, type InvokeFn } from "../stores/graphStore";
import { interfaceStore } from "../stores/interfaceStore";
import { historySelectionLabel, sanitizeHistoryList, type HistoryList } from "../views/historyList";
import { resolveSection } from "../views/viewRegistry";
import { createDiffSelection } from "./diffSelection";
import { sameRepo, type PathIdentityOptions } from "./paths";
import type { ViewTab } from "./persist";
import type { DiffPayload, RepoSession } from "./storeTypes";

export type ViewSessionPatch =
  | Partial<RepoSession>
  | ((current: RepoSession) => Partial<RepoSession>);

export interface ViewNavigationDeps {
  invoke: InvokeFn;
  options: PathIdentityOptions;
  activeSession(): RepoSession | undefined;
  applyToSession(id: string, generation: number, patchOrFn: ViewSessionPatch): boolean;
  getOpenEpoch(): number;
  bumpOpenEpoch(): number;
  openRepo(
    rawPath: string,
    extras?: { onReady?: (canonicalPath: string) => void },
  ): Promise<boolean>;
  flushPersist(force?: boolean): void;
  selectCommitById?: (
    commitId: string,
    opts?: { summary?: string; pin?: boolean },
  ) => Promise<void>;
  setFleetOpen?: (open: boolean) => void;
}

/**
 * Owns diff/file/commit selection, history lens selection, active view tab/section,
 * terminal dock visibility, and commit amend state.
 */
export function createViewNavigation(deps: ViewNavigationDeps) {
  const { invoke: invokeFn, options } = deps;
  const selectCommitById =
    deps.selectCommitById ??
    ((commitId, opts) => graphStore.selectCommitById(commitId, opts));
  const setFleetOpen =
    deps.setFleetOpen ?? ((open) => interfaceStore.setFleetOpen(open));

  const diffSelection = createDiffSelection({
    invoke: invokeFn,
    activeSession: deps.activeSession,
    applyToSession: deps.applyToSession,
    openEpoch: deps.getOpenEpoch,
  });

  function selectFileDiff(filePath: string, isStaged: boolean = false): Promise<void> {
    return diffSelection.select({
      read: (invoke, repoPath, ignoreWhitespace) =>
        invoke<DiffPayload>("cmd_get_file_diff", {
          repoPath,
          filePath,
          isStaged,
          ignoreWhitespace,
        }),
      filePath,
      commitId: null,
      isStaged,
      kind: "file",
      history: "unlessNavigated",
      worktree: true,
    });
  }

  function selectCommitDiff(commitId: string): Promise<void> {
    return diffSelection.select({
      read: (invoke, repoPath) =>
        invoke<DiffPayload>("cmd_get_commit_diff", { repoPath, commitId }),
      filePath: null,
      commitId,
      isStaged: false,
      kind: "commit",
      history: "never",
    });
  }

  function selectCommitFileDiff(commitId: string, filePath: string): Promise<void> {
    return diffSelection.select({
      read: (invoke, repoPath) =>
        invoke<DiffPayload>("cmd_get_commit_file_diff", {
          repoPath,
          commitId,
          filePath,
        }),
      filePath,
      commitId,
      isStaged: false,
      kind: "commit",
      history: "always",
    });
  }

  function selectRangeDiff(from: string, to: string): Promise<void> {
    return diffSelection.select({
      read: (invoke, repoPath) =>
        invoke<DiffPayload>("cmd_get_range_diff", { repoPath, from, to }),
      filePath: `${from}...${to}`,
      commitId: null,
      isStaged: false,
      kind: "range",
      history: "always",
    });
  }

  function selectComparison(
    base: string,
    target: string,
    filePath: string | null,
  ): Promise<void> {
    return diffSelection.select({
      read: (invoke, repoPath) =>
        invoke<DiffPayload>("cmd_get_compare_diff", {
          repoPath,
          from: base,
          to: target,
          filePath,
        }),
      filePath,
      commitId: target,
      compareBase: base,
      isStaged: false,
      kind: "compare",
      history: "always",
    });
  }

  async function previewUncommitted(
    repoPath?: string,
    isStaged?: boolean,
  ): Promise<void> {
    const session = deps.activeSession();
    const target = repoPath ?? session?.path;
    if (!target) return;
    if (
      !session ||
      !sameRepo(session.path, target, options) ||
      session.isLoading ||
      session.error
    ) {
      let preview: Promise<void> | undefined;
      await deps.openRepo(target, {
        onReady: () => {
          preview = previewUncommitted(undefined, isStaged);
        },
      });
      await preview;
      return;
    }
    const file =
      session.statuses.find(
        (status) =>
          isStaged === undefined ||
          (isStaged ? status.is_staged : hasUnstagedChanges(status)),
      ) ?? session.statuses[0];
    const side =
      isStaged === false && file && hasUnstagedChanges(file)
        ? false
        : (file?.is_staged ?? false);
    deps.bumpOpenEpoch();
    diffSelection.supersede();
    deps.applyToSession(session.id, session.generation, {
      selectedFilePath: file?.path ?? null,
      selectedCommitId: null,
      selectedCompareBase: null,
      selectedDiff: null,
      selectedDiffTruncated: false,
      selectedDiffTruncationReason: null,
      selectedDiffError: null,
      selectedDiffPending: Boolean(file),
      selectedIsStaged: side,
      selectionKind: "file",
      activeTab: "history",
      viewSections: { ...session.viewSections, history: "diff" },
    });
    setFleetOpen(false);
    if (file) await selectFileDiff(file.path, side);
  }

  return {
    previewUncommitted,
    selectFileDiff,
    selectFilePath: (filePath: string | null) => {
      const session = deps.activeSession();
      if (!session) return;
      deps.applyToSession(session.id, session.generation, {
        selectedFilePath: filePath,
        selectedCommitId: null,
        selectedCompareBase: null,
        selectionKind: "file",
      });
    },
    setIgnoreWhitespace: (next: boolean) => {
      const session = deps.activeSession();
      if (!session) return;
      if (session.selectedIgnoreWhitespace === next) return;
      const {
        selectedFilePath: filePath,
        selectedIsStaged: isStaged,
        selectionKind,
      } = session;
      deps.applyToSession(session.id, session.generation, {
        selectedIgnoreWhitespace: next,
      });
      if (selectionKind !== "file" || !filePath) return;
      void selectFileDiff(filePath, isStaged);
    },
    selectHistoryCommit: (
      commitId: string,
      opts: { summary?: string; pin?: boolean; label?: string } = {},
    ) => {
      if (!commitId) return;
      historySelectionLabel.set(opts.label ? { commitId, label: opts.label } : null);
      void selectCommitById(commitId, { summary: opts.summary, pin: opts.pin });
      void selectCommitDiff(commitId);
    },
    selectCommitDiff,
    selectCommitFileDiff,
    selectRangeDiff,
    selectCompareDiff: (base: string, target: string) =>
      selectComparison(base, target, null),
    selectCompareFileDiff: (base: string, target: string, filePath: string) =>
      selectComparison(base, target, filePath),
    setActiveTab: (tab: ViewTab, section?: string) => {
      deps.bumpOpenEpoch();
      const session = deps.activeSession();
      if (!session) return;
      const resolved = section ? resolveSection(tab, section) : null;
      deps.applyToSession(session.id, session.generation, {
        activeTab: tab,
        ...(resolved
          ? { viewSections: { ...session.viewSections, [tab]: resolved } }
          : {}),
        ...(tab === "history" && resolved === "graph"
          ? { historyList: "commits" as const }
          : {}),
      });
      deps.flushPersist();
    },
    setHistoryList: (list: HistoryList) => {
      const session = deps.activeSession();
      if (!session) return;
      const next = sanitizeHistoryList(list);
      if (session.historyList === next) return;
      deps.applyToSession(session.id, session.generation, { historyList: next });
      deps.flushPersist();
    },
    setViewSection: (tab: ViewTab, section: string) => {
      deps.bumpOpenEpoch();
      const session = deps.activeSession();
      if (!session) return;
      const resolved = resolveSection(tab, section);
      if (!resolved || session.viewSections[tab] === resolved) return;
      deps.applyToSession(session.id, session.generation, {
        viewSections: { ...session.viewSections, [tab]: resolved },
      });
      deps.flushPersist();
    },
    setTerminalOpen: (open: boolean): boolean => {
      const session = deps.activeSession();
      if (!session || session.terminalOpen === open) return false;
      if (
        !deps.applyToSession(session.id, session.generation, {
          terminalOpen: open,
        })
      ) {
        return false;
      }
      deps.flushPersist();
      return true;
    },
    toggleTerminal: (): boolean => {
      const session = deps.activeSession();
      if (!session) return false;
      if (
        !deps.applyToSession(session.id, session.generation, {
          terminalOpen: !session.terminalOpen,
        })
      ) {
        return false;
      }
      deps.flushPersist();
      return true;
    },
    inspectCommitInHistory: (commitId: string) => {
      deps.bumpOpenEpoch();
      const session = deps.activeSession();
      if (!session || !commitId) return;
      deps.applyToSession(session.id, session.generation, {
        selectedCommitId: commitId,
        selectedCompareBase: null,
        selectedFilePath: null,
        selectedDiff: null,
        selectedDiffTruncated: false,
        selectedDiffTruncationReason: null,
        selectedDiffError: null,
        selectedIsStaged: false,
        selectionKind: "commit",
        activeTab: "history",
        historyList: "commits",
      });
      deps.flushPersist();
    },
    setAmending: (isAmending: boolean) => {
      const session = deps.activeSession();
      if (!session) return;
      deps.applyToSession(session.id, session.generation, { isAmending });
    },
  };
}
