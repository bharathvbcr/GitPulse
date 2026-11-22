<script lang="ts" module>
  import { createRepoPanelCache } from "../panels/repoPanelCache";
  import { hostPlatform } from "../stores/platformStore";
  import { shortcutTextLabel } from "../ui/platformCopy";
  import type { EditorTabState } from "../files/editorTabs";
  import type { FileSidePane } from "../files/filePaneLayout";
  import { livePulsePref } from "../files/livePulsePref";
  import { consumePin, pinRequest } from "../files/pinRequests";
  import { overwriteNote } from "../files/overwriteNote";
  import { draftPaths } from "../files/openDocuments";
  import { loadPersistedEditorTabs, savePersistedEditorTabs } from "../files/editorTabPersist";
  import {
    createLatestOwnerRegistry,
    editorFileSaveQueue,
    type LatestOwnerLease,
  } from "../files/serialSave";

  const tabCache = createRepoPanelCache<{
    tabs: EditorTabState;
    explorerOpen: boolean;
    preferredSidePane: FileSidePane;
  }>({
    // A cache bound may discard clean navigation history, never unsaved work.
    // The drafts are the open-file store's, not the tabs': a repository that
    // still owes one keeps its tabs too.
    canEvict: (_cached, repo) => draftPaths(repo).length === 0,
  });
  // Both registries must outlive a rendered FileViewer: App remounts this
  // component when switching views while an accepted write may still settle.
  const fileSaveQueue = editorFileSaveQueue;
  // Module scope so LazyView sees one stable loader (see LazyView). Blame is
  // the editor's other lens and stays out of the Code chunk until it is used.
  const loadBlame = () => import("./BlameViewer.svelte");
  // The newest live viewer of a repository is the only one whose pending
  // decisions (a discard confirmation) may still be applied.
  const viewerOwners = createLatestOwnerRegistry<true>();
</script>

<script lang="ts">
  import { onMount, tick, untrack } from "svelte";
  import { currentRepoPath, repoStore } from "../stores/repoStore";
  import {
    PanelLeftClose,
    PanelLeftOpen,
    FileText,
    Copy,
    ExternalLink,
    ChevronRight,
    X,
    Layers,
    GitCommit,
    ShieldAlert,
    Activity,
    Columns2,
    XCircle,
  } from "@lucide/svelte";
  import { describeError } from "../ui/copy/describeFailure";
  import { copyText } from "../desktop/clipboard";
  import { askConfirm } from "../stores/modalStore";
  import { openInDefaultApp } from "../desktop/openInShell";
  import {
    claimDocuments,
    discardDraft,
    documentConflict,
    documentIn,
    documentKey,
    editDocument,
    hasDraft,
    holdDocument,
    isHeldElsewhere,
    loadDocument,
    openDocuments,
    saveDocument,
  } from "../files/openDocuments";
  import EditorConflictBanner from "./files/EditorConflictBanner.svelte";
  import FileTreeColumn from "./files/FileTreeColumn.svelte";
  import ExplorerRail from "./files/ExplorerRail.svelte";
  import LazyView from "./LazyView.svelte";
  import { explorerLens, toggleBlameLens } from "../files/explorerLens";
  import { sidebarExplorerDocked } from "../sidebar/layoutStore";
  import MediaViewer from "./files/MediaViewer.svelte";
  import { sendToAgentFromPage } from "../terminal/sendFromPage";
  import LivePulseDashboard from "./files/LivePulseDashboard.svelte";
  import FileHistoryMenu from "./files/FileHistoryMenu.svelte";
  import EmptyState from "./EmptyState.svelte";
  import LanguageLogo from "./LanguageLogo.svelte";
  import ScrollCue from "./ScrollCue.svelte";
  import { classifyFileChange, statusBadgeClass, statusBadgeLabel } from "../files/fileStatus";
  import { focusTabAt, handleTablistKeydown } from "../dom/tablist";
  import { observeResize } from "../dom/observeResize";
  import {
    FILE_DASHBOARD_WIDTH,
    FILE_EDITOR_MIN_WIDTH,
    FILE_SIDE_CARD_GUTTERS,
    resolveFilePaneLayout,
  } from "../files/filePaneLayout";
  import {
    activateEditorTab,
    closeEditorTabs,
    dirtyEditorTabPaths,
    emptyEditorTabs,
    openPinned,
    openPreview,
    pinEditorTab,
    type EditorTabState as Tabs,
  } from "../files/editorTabs";

  /** Id of the editor region the open-file tabs control. */
  const EDITOR_PANE_ID = "gitpulse-editor-pane";
  let tabStrip: HTMLDivElement | undefined = $state();
  let livePulseToggle: HTMLButtonElement | undefined = $state();
  let pathStrip: HTMLElement | undefined = $state();


  let explorerOpen = $state(true);
  // App-wide and remembered across launches (`livePulsePref`), so hiding Live
  // pulse sticks across repository switches and restarts.
  const dashboardOpen = $derived($livePulsePref.visible);
  let tabState = $state<Tabs>(emptyEditorTabs());
  let fileViewRoot: HTMLDivElement | undefined = $state();
  let fileViewWidth = $state(0);
  let preferredSidePane = $state<FileSidePane>("explorer");
  let compactPane = $state<FileSidePane | null>(null);
  let revealRequest = $state<{ path: string; nonce: number }>({ path: "", nonce: 0 });

  /** How this page names itself to the open-file store, which owns every file's text, draft and save. */
  const HOLDER = "code-page";
  let prevLoadKey = "";
  let hydratedRepo: string | null = null;
  let lastAppliedStoreFile = "";
  let viewerLease: LatestOwnerLease | null = null;

  let openTabs = $derived(tabState.tabs);
  let activeTabPath = $derived(tabState.active);
  /** The open file's document: its text, its one draft, and a refused save's conflict. */
  const activeDoc = $derived(documentIn($openDocuments, $currentRepoPath, activeTabPath));
  /**
   * A reload of the open file (a status change, an agent edit, a refused save)
   * swaps the blob in place, so the viewer keeps its scroll, selection, find
   * bar and view mode; only a different file, or a different repository's file
   * of the same name (its own document), shows the placeholder.
   */
  const activeBlob = $derived(activeDoc?.blob ?? null);
  const isLoadingFile = $derived(Boolean(activeDoc?.loading));
  const fileError = $derived(activeDoc?.error ?? null);
  let activeDraft = $derived(activeDoc?.draft ?? undefined);
  let activeTabDirty = $derived(activeDraft !== undefined);
  /** True when `path` carries a draft in the repository on screen; tracked when read in a template. */
  function isTabDirty(path: string): boolean {
    return documentIn($openDocuments, $currentRepoPath, path)?.draft != null;
  }
  /** The same question, answered from the store as it is now (for state transitions, which must not wait for a render). */
  function isDirtyHere(path: string): boolean {
    return hydratedRepo !== null && hasDraft(hydratedRepo, path);
  }
  /**
   * Set while the open file's draft was made from a version of it that is no
   * longer on disk: a save was refused, or the blob that reloaded carries a
   * different base than the draft. `actual` is the version Overwrite saves over.
   */
  let activeConflict = $derived(documentConflict(activeDoc));
  const outerExplorerDocked = $derived($sidebarExplorerDocked);
  let explorerForcedWithOuterSidebar = $state(false);
  $effect(() => {
    if (!outerExplorerDocked) {
      explorerForcedWithOuterSidebar = false;
    }
  });
  const effectiveExplorerOpen = $derived(
    explorerOpen && (!outerExplorerDocked || explorerForcedWithOuterSidebar),
  );
  let paneLayout = $derived(resolveFilePaneLayout({
    containerWidth: fileViewWidth,
    explorerRequested: effectiveExplorerOpen,
    dashboardRequested: dashboardOpen,
    preferredPane: preferredSidePane,
    compactPane,
  }));
  // The tree may grow, but not into the editor's minimum — or the dashboard,
  // which stays a fixed rail beside it. Each side column is a floating card
  // whose margins the editor has to leave room for too.
  let explorerReserve = $derived(
    FILE_EDITOR_MIN_WIDTH + FILE_SIDE_CARD_GUTTERS + (paneLayout.dashboardVisible ? FILE_DASHBOARD_WIDTH + FILE_SIDE_CARD_GUTTERS : 0),
  );

  let pathSegments = $derived.by(() => (activeTabPath ? activeTabPath.split("/") : []));

  let activeStatus = $derived.by(() => {
    if (!activeTabPath) return null;
    return $repoStore.statuses.find((s) => s.path === activeTabPath) ?? null;
  });

  let activeKind = $derived(classifyFileChange(activeStatus));

  function persistTabs(repo: string | null, commit = false) {
    if (!repo) return;
    tabCache.set(repo, {
      tabs: {
        tabs: tabState.tabs,
        active: tabState.active,
      },
      explorerOpen,
      preferredSidePane,
    });
    // Open tabs outlive this component (the cache, then storage), so they are
    // claimed in the store whether or not they are on screen.
    claimDocuments(repo, HOLDER, tabState.tabs.map((tab) => tab.path));
    savePersistedEditorTabs(repo, {
      tabs: tabState.tabs.map((tab) => ({ path: tab.path, preview: tab.preview })),
      active: tabState.active,
    }, commit);
  }

  function previewFile(path: string) {
    if (!path) return;
    compactPane = null;
    tabState = openPreview(tabState, path, isDirtyHere);
    repoStore.selectFilePath(path);
    persistTabs(hydratedRepo);
  }

  function pinFile(path: string) {
    if (!path) return;
    compactPane = null;
    tabState = openPinned(tabState, path);
    repoStore.selectFilePath(path);
    persistTabs(hydratedRepo);
  }

  function activateTab(path: string) {
    if (!path) return;
    tabState = activateEditorTab(tabState, path);
    repoStore.selectFilePath(path);
    persistTabs(hydratedRepo);
  }

  /**
   * Asks before the drafts of `candidatePaths` are thrown away. Resolves the
   * paths whose drafts the caller must now discard, or null when the user kept
   * them. `closing` is a tab going away rather than a draft being thrown away:
   * a draft another host (a terminal split) still shows is that host's to
   * keep, so it neither asks nor goes.
   */
  async function confirmDiscardDrafts(
    candidatePaths: readonly string[],
    closing: boolean,
  ): Promise<string[] | null> {
    const repo = hydratedRepo;
    if (!repo) return null;
    // An accepted save owns the disk outcome. Let it settle before deciding
    // what remains dirty, so “Discard” can never race a queued write that
    // later persists the content the user explicitly chose to discard.
    const saveKeys = [...new Set(candidatePaths)].map((path) => documentKey(repo, path));
    await fileSaveQueue.whenIdle(saveKeys);
    if (!isCurrentViewer(repo)) return null;
    const dirtyPaths = dirtyEditorTabPaths(
      tabState,
      candidatePaths,
      (path) => hasDraft(repo, path) && !(closing && isHeldElsewhere(repo, path, HOLDER)),
    );
    if (dirtyPaths.length === 0) return [];
    const names = dirtyPaths.length <= 3
      ? dirtyPaths.map((path) => `• ${path}`).join("\n")
      : `${dirtyPaths.length} files`;
    const confirmed = await askConfirm({
      title: "Discard Unsaved Edits?",
      message: `These editor drafts have not been saved:\n${names}`,
      confirmLabel: "Discard Unsaved Edits",
      cancelLabel: "Keep Editing",
    });
    return confirmed ? dirtyPaths : null;
  }

  /** Closes exactly these tabs after asking about their drafts; false when the user kept them. */
  async function closeTabsAfterConfirm(paths: readonly string[]): Promise<boolean> {
    const repo = hydratedRepo;
    if (!repo) return false;
    const discarded = await confirmDiscardDrafts(paths, true);
    if (discarded === null || !isCurrentViewer(repo)) return false;
    for (const path of discarded) discardDraft(repo, path);
    tabState = closeEditorTabs(tabState, paths);
    return true;
  }

  function isCurrentViewer(repo: string): boolean {
    return hydratedRepo === repo && viewerLease?.isCurrent() === true;
  }

  function syncSelectedFilePath() {
    lastAppliedStoreFile = tabState.active ?? "";
    repoStore.selectFilePath(tabState.active);
  }

  async function closeTab(path: string, event?: MouseEvent) {
    event?.stopPropagation();
    const repo = hydratedRepo;
    if (!repo) return;
    if (!(await closeTabsAfterConfirm([path]))) return;
    syncSelectedFilePath();
    persistTabs(repo, true);
    if (!tabState.active) prevLoadKey = "";
  }

  async function closeAllTabs() {
    const repo = hydratedRepo;
    if (!repo) return;
    const pathsAtRequest = tabState.tabs.map((tab) => tab.path);
    if (!(await closeTabsAfterConfirm(pathsAtRequest))) return;
    syncSelectedFilePath();
    persistTabs(repo, true);
    if (!tabState.active) prevLoadKey = "";
  }

  async function closeOtherTabs() {
    const keepPath = tabState.active;
    if (!keepPath) return;
    const repo = hydratedRepo;
    if (!repo) return;
    const pathsToClose = tabState.tabs
      .filter((tab) => tab.path !== keepPath)
      .map((tab) => tab.path);
    if (!(await closeTabsAfterConfirm(pathsToClose))) return;
    tabState = pinEditorTab(tabState, keepPath);
    syncSelectedFilePath();
    persistTabs(repo, true);
  }

  function handleDraftChange(path: string, newContent: string, sourceContent: string) {
    const repo = hydratedRepo;
    if (!repo) return;
    // The first keystroke records the disk version this draft is made from;
    // the store ignores the base on every later one.
    editDocument(repo, path, newContent, sourceContent);
  }

  /**
   * A save of `path` in the repository this page is on. The store carries the
   * draft's base, completes the draft and, on a refusal, keeps it and records
   * the disk version it saw (see `saveDocument`); the error is still thrown so
   * the editor stays in edit mode. `overrideBase` undefined saves against the
   * draft's own base; a string saves against that version, which is how
   * Overwrite names the one it already saw; null is an unconditional write.
   */
  async function saveDraft(
    path: string,
    newContent: string,
    overrideBase?: string | null,
    overwrite = false,
  ) {
    // The repository the save was started in, not whichever repository is open
    // when it settles.
    const repo = hydratedRepo;
    if (!repo || !path) throw new Error("No active file to save");
    const repositoryChanged = () => {
      if ($repoStore.currentPath !== repo) {
        throw new Error("Repository changed before save; the editor draft was kept");
      }
    };
    repositoryChanged();
    await saveDocument(repo, path, newContent, {
      overrideBase,
      overwrite,
      beforeWrite: repositoryChanged,
      afterSave: async () => {
        if ($repoStore.currentPath === repo) await repoStore.refresh();
      },
    });
  }

  /** Overwrite: save the draft over exactly the disk version the refusal saw. */
  async function overwriteDraft() {
    const path = activeTabPath;
    const draft = activeDraft;
    const conflict = activeConflict;
    if (!path || !draft || !conflict) return;
    // With no version on record the write is unconditional, so it is asked for.
    if (
      conflict.actual === null &&
      !(await askConfirm({
        title: "Overwrite Without a Known Version?",
        message: `${path} has no recorded disk version. Saving will replace whatever is on disk, including changes made since you started editing.`,
        confirmLabel: "Overwrite",
        cancelLabel: "Keep Editing",
        destructive: true,
      }))
    ) {
      return;
    }
    try {
      // `overwrite` is the user's choice to keep the buffer: the write is
      // byte-exact even over a disk file the editor could not round-trip.
      await saveDraft(path, draft.content, conflict.actual, true);
    } catch (err: unknown) {
      repoStore.setError(describeError(err, "repo"));
    }
  }

  /** Reload: discard the draft (after confirming) and show what is on disk. */
  async function reloadFromDisk() {
    const path = activeTabPath;
    if (!path) return;
    if (!(await requestDiscardDraft(path))) return;
    const repo = hydratedRepo;
    if (!repo) return;
    // The draft is gone and the editor must close on the disk version, which a
    // viewer kept mounted across an in-place reload would not do; dropping the
    // blob (`remount`) brings the placeholder back and remounts it.
    await loadDocument(repo, path, { force: true, remount: true });
  }

  async function requestDiscardDraft(path: string): Promise<boolean> {
    const repo = hydratedRepo;
    if (!repo) return false;
    const discarded = await confirmDiscardDrafts([path], false);
    if (discarded === null || !isCurrentViewer(repo)) return false;
    for (const draftPath of discarded) discardDraft(repo, draftPath);
    return true;
  }

  function openActiveInDefaultApp() {
    const repo = $repoStore.currentPath;
    if (!repo || !activeTabPath) return;
    // Rust validates containment and rebuilds the absolute path; a failure
    // surfaces rather than leaving a menu item that quietly does nothing.
    void openInDefaultApp(repo, activeTabPath).catch((err) =>
      repoStore.setError(describeError(err, "repo")),
    );
  }

  async function copyActivePath() {
    if (!activeTabPath) return;
    if (!(await copyText(activeTabPath))) {
      repoStore.setError("Couldn't copy path to clipboard");
    }
  }

  /**
   * Open the current file under another lens.
   *
   * Both are sections now, so neither button leaves the subject behind:
   * Blame is a lens on this same file inside Code, and Diff is a lens on the
   * repository's history. Selecting the path first is what makes the switch
   * land on the file the user is looking at rather than the last one.
   */
  function inspectInDiff() {
    if (!activeTabPath) return;
    repoStore.selectFilePath(activeTabPath);
    repoStore.setActiveTab("history", "diff");
  }

  // A request to search made from elsewhere (the command palette) must be
  // able to see its own result: bring the column up for it. Visibility is
  // tracked, not peeked: the request usually lands before this component has
  // hydrated the repository's pane preferences, and hydration can hide the
  // column again — this re-runs and shows it. The flag clears once the search
  // box has taken focus, which needs the column on screen.
  $effect(() => {
    if (!$explorerLens.searchFocusPending || paneLayout.explorerVisible || outerExplorerDocked) return;
    untrack(() => toggleSidePane("explorer"));
  });

  /**
   * Ask the Explorer to show a directory. A nonce rather than the bare path,
   * because clicking the same crumb twice is a repeat request, not a no-op —
   * the tree may have been scrolled away in between.
   */
  function revealDirectory(dir: string) {
    if (!dir) return;
    if (!paneLayout.explorerVisible) toggleSidePane("explorer");
    revealRequest = { path: dir, nonce: revealRequest.nonce + 1 };
  }

  function toggleSidePane(pane: FileSidePane) {
    const requested = pane === "explorer" ? effectiveExplorerOpen : dashboardOpen;
    const visible = pane === "explorer"
      ? paneLayout.explorerVisible
      : paneLayout.dashboardVisible;
    preferredSidePane = pane;

    if (visible) {
      if (paneLayout.mode === "compact") {
        // Compact panes are transient workspaces. Closing one returns to the
        // editor without erasing the user's wide-window pane preference.
        compactPane = null;
      } else if (pane === "explorer") {
        if (outerExplorerDocked && explorerForcedWithOuterSidebar) {
          explorerForcedWithOuterSidebar = false;
        } else {
          explorerOpen = false;
        }
      } else {
        livePulsePref.setVisible(false);
      }
      persistTabs(hydratedRepo);
      return;
    }

    if (!requested) {
      if (pane === "explorer") {
        explorerOpen = true;
        if (outerExplorerDocked) explorerForcedWithOuterSidebar = true;
      } else {
        livePulsePref.setVisible(true);
      }
    } else if (pane === "explorer" && outerExplorerDocked) {
      explorerForcedWithOuterSidebar = true;
    }
    // Ignored when the pane fits beside the editor; used as a full-workspace
    // fallback when it does not.
    compactPane = pane;
    persistTabs(hydratedRepo);
  }

  /**
   * The Live pulse card's own Hide button. Unlike the toolbar toggle, which
   * treats a compact-layout pane as a transient peek, this is an explicit
   * choice, so it is remembered in every layout. Focus goes to the toolbar
   * toggle, the control that brings the column back, instead of falling to the
   * page when the card leaves.
   */
  function hideLivePulse() {
    livePulsePref.setVisible(false);
    if (compactPane === "dashboard") compactPane = null;
    persistTabs(hydratedRepo);
    void tick().then(() => livePulseToggle?.focus());
  }

  /**
   * Arrow keys walk the tab strip, as `role="tab"` implies. Focus follows the
   * key rather than activating on arrival: moving across tabs should not load
   * six files on the way to the seventh.
   */
  function handleTabStripKeydown(e: KeyboardEvent) {
    const current = openTabs.findIndex((tab) => tab.path === activeTabPath);
    const move = handleTablistKeydown(e.key, current, openTabs.length);
    if (!move) return;
    e.preventDefault();
    const target = openTabs[move.index];
    if (!target) return;
    // Selection follows focus, as it does in the view and section switchers:
    // the tab a keyboard user has arrowed onto is the one that is open.
    activateTab(target.path);
    focusTabAt(tabStrip, move.index);
  }

  function handleWindowKeydown(e: KeyboardEvent) {
    // A key another surface already handled is not ours, and neither is one
    // pressed while this page is covered: a maximized terminal leaves Code
    // mounted but `inert`, and a chord it does not consume (⌘⇧D outside shell
    // mode) would otherwise flip a column nobody can see — and Live pulse's
    // visibility is remembered, so the flip would outlive the session.
    if (e.defaultPrevented || !fileViewRoot || fileViewRoot.closest("[inert]")) return;
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "b" && !e.shiftKey) {
      e.preventDefault();
      toggleSidePane("explorer");
    } else if ((e.metaKey || e.ctrlKey) && e.shiftKey && e.key.toLowerCase() === "d") {
      e.preventDefault();
      toggleSidePane("dashboard");
    }
  }

  $effect(() => {
    const repo = $currentRepoPath;
    if (repo === hydratedRepo) return;
    if (hydratedRepo) untrack(() => persistTabs(hydratedRepo));
    viewerLease?.release();
    hydratedRepo = repo;
    viewerLease = repo ? viewerOwners.claim(repo, true) : null;
    lastAppliedStoreFile = "";
    if (!repo) {
      untrack(() => {
        compactPane = null;
        tabState = emptyEditorTabs();
      });
      prevLoadKey = "";
      return;
    }
    let cached = tabCache.get(repo);
    if (!cached) {
      const disk = loadPersistedEditorTabs(repo);
      if (disk && disk.tabs.length > 0) {
        cached = {
          tabs: disk,
          explorerOpen: true,
          preferredSidePane: "explorer",
        };
        tabCache.set(repo, cached);
      }
    }
    untrack(() => {
      compactPane = null;
      if (cached) {
        tabState = cached.tabs;
        explorerOpen = cached.explorerOpen;
        preferredSidePane = cached.preferredSidePane ?? "explorer";
      } else {
        tabState = emptyEditorTabs();
        explorerOpen = true;
        preferredSidePane = "explorer";
      }
      // Tabs restored from storage are open before any mutation announces them.
      claimDocuments(repo, HOLDER, tabState.tabs.map((tab) => tab.path));
    });
    prevLoadKey = "";
  });

  const storeSelection = $derived($repoStore.selectedFilePath);
  $effect(() => {
    const storeSelected = storeSelection;
    if (!storeSelected) {
      lastAppliedStoreFile = "";
      return;
    }
    if (storeSelected === lastAppliedStoreFile) return;
    lastAppliedStoreFile = storeSelected;
    untrack(() => {
      if (!tabState.tabs.some((t) => t.path === storeSelected)) {
        tabState = openPreview(tabState, storeSelected, isDirtyHere);
        persistTabs(hydratedRepo);
      } else {
        tabState = activateEditorTab(tabState, storeSelected);
      }
    });
  });

  // A pin asked for by an explorer that does not own these tabs (the main
  // sidebar's double-click). Declared after the hydration and selection
  // effects so it runs after them in the same flush: a request made before
  // this page mounted lands on the hydrated tabs instead of being overwritten
  // by them. Tracking the store also catches a pin of the file already
  // selected, which no selection change would announce.
  const pinAsk = $derived($pinRequest);
  $effect(() => {
    const ask = pinAsk;
    const repo = $currentRepoPath;
    if (!ask || !repo) return;
    untrack(() => {
      if (hydratedRepo !== repo || !consumePin(repo, ask.path)) return;
      pinFile(ask.path);
    });
  });

  // The open file is held while it is shown, so the store keeps its text
  // loaded and drops it once no host shows it. Declared before the read below,
  // which needs the hold in place.
  $effect(() => {
    const repo = $currentRepoPath;
    const path = tabState.active;
    if (!repo || !path) return;
    return holdDocument(repo, path, HOLDER);
  });

  // A tab that owns unsaved work is kept: the draft may have been typed in a
  // terminal split, which this page's own handlers never see.
  $effect(() => {
    const docs = $openDocuments;
    const repo = $currentRepoPath;
    const tabs = tabState.tabs;
    if (!repo) return;
    untrack(() => {
      let next = tabState;
      for (const tab of tabs) {
        if (tab.preview && documentIn(docs, repo, tab.path)?.draft) next = pinEditorTab(next, tab.path);
      }
      if (next === tabState) return;
      tabState = next;
      persistTabs(hydratedRepo);
    });
  });

  const activeStatusCode = $derived(
    tabState.active ? ($repoStore.statuses.find((s) => s.path === tabState.active)?.status_code ?? "") : "",
  );
  $effect(() => {
    const repo = $currentRepoPath;
    const path = tabState.active;
    const code = activeStatusCode;
    const key = `${repo ?? ""}\u0000${path ?? ""}\u0000${code}`;
    if (key === prevLoadKey) return;
    prevLoadKey = key;
    if (!repo || !path) return;
    // Forced: the open file is read again on every activation and status
    // change, kept on screen meanwhile. Held above, so the store keeps it.
    void loadDocument(repo, path, { force: true });
  });

  $effect(() => {
    return () => {
      viewerLease?.release();
    };
  });

  $effect(() => {
    // A compact selection is a temporary replacement surface. Once the view
    // grows enough to restore an editor split, do not resurrect it on a later
    // resize unless the user requests it again.
    if (paneLayout.mode !== "compact" && compactPane !== null) {
      untrack(() => { compactPane = null; });
    }
  });

  onMount(() => {
    window.addEventListener("keydown", handleWindowKeydown);
    const stopResize = fileViewRoot
      ? observeResize(fileViewRoot, (entries) => {
          const measured = entries.find((entry) => entry.target === fileViewRoot)?.contentRect.width;
          fileViewWidth = typeof measured === "number" && Number.isFinite(measured) && measured > 0
            ? measured
            : 0;
        })
      : () => {};
    if (fileViewRoot) {
      fileViewWidth = fileViewRoot.clientWidth || 0;
    }
    return () => {
      window.removeEventListener("keydown", handleWindowKeydown);
      stopResize();
    };
  });
</script>

<div
  bind:this={fileViewRoot}
  class="flex-1 flex flex-col min-h-0 min-w-0 bg-background overflow-hidden relative select-none"
  data-pane-layout={paneLayout.mode}
>
  <!--
    Tab strip.

    `role="tablist"` used to wrap the Explorer toggle, a decorative divider and
    the tabs alike, so assistive tech counted three tabs where there was one
    file open. The toggle now sits outside the list, which is also why the tab
    strip finally gets the full width between the two control clusters.

    Tabs are one tab stop, not one per file: `tabindex` roves to the active tab
    and Left/Right/Home/End move between them, which is what `role="tab"`
    already promised a keyboard user.
  -->
  <!-- The Code page's own bar sits on the page's ground, like the window's
       header rows: no band fill and no edge line under it. -->
  <div class="flex items-center px-1.5 shrink-0 h-9 gap-1.5" data-file-view-bar>
    <button
      type="button"
      onclick={() => toggleSidePane("explorer")}
      aria-label="{paneLayout.explorerVisible ? 'Hide' : 'Show'} Explorer"
      aria-pressed={paneLayout.explorerVisible}
      title="{paneLayout.explorerVisible ? 'Hide' : 'Show'} Explorer ({shortcutTextLabel('⌘B', $hostPlatform.os)})"
      class="gp-icon-btn shrink-0 {paneLayout.explorerVisible ? 'text-accent bg-accent/15' : 'text-textMuted hover:text-textPrimary'}"
    >
      {#if paneLayout.explorerVisible}
        <PanelLeftClose size={14} />
      {:else}
        <PanelLeftOpen size={14} />
      {/if}
    </button>

    <div class="h-3.5 w-1 rounded-full bg-border/50 shrink-0" aria-hidden="true"></div>

    {#if openTabs.length === 0}
      <span class="text-xs text-faint italic pl-1 flex-1">No open files</span>
    {:else}
      <!-- The strip listens for the arrow keys but is not itself a tab stop:
           the roving tabindex on the tabs is what the user lands on. -->
      <div class="relative min-w-0 flex-1 h-full">
      <div
        bind:this={tabStrip}
        class="flex items-stretch gap-1 min-w-0 h-full overflow-x-auto gp-header-scroll py-1"
        role="tablist"
        aria-label="Open files"
        tabindex="-1"
        onkeydown={handleTabStripKeydown}
      >
        {#each openTabs as tab (tab.path)}
          {@const isActive = tab.path === activeTabPath}
          {@const tabDirty = isTabDirty(tab.path)}
          {@const tabKind = classifyFileChange($repoStore.statuses.find((s) => s.path === tab.path))}
          <div
            role="tab"
            data-editor-tab={tab.path}
            tabindex={isActive ? 0 : -1}
            aria-selected={isActive}
            aria-controls={EDITOR_PANE_ID}
            onclick={() => activateTab(tab.path)}
            ondblclick={() => pinFile(tab.path)}
            onkeydown={(e) => {
              if (e.key === "Enter" || e.key === " ") {
                e.preventDefault();
                activateTab(tab.path);
              }
            }}
            class="px-2 flex items-center gap-1.5 rounded-lg text-xs transition-colors shrink-0 max-w-[190px] group cursor-pointer {isActive
              ? 'bg-background text-textPrimary font-semibold shadow-card'
              : 'text-textMuted hover:bg-surfaceHover hover:text-textPrimary'} {tab.preview ? 'italic' : ''}"
            title={tabDirty
              ? `${tab.path} — Unsaved changes`
              : tab.preview ? `${tab.path} (preview — double-click to pin)` : tab.path}
          >
            <LanguageLogo filePath={tab.path} size={13} class="shrink-0" />
            <span class="truncate">{tab.name}</span>
            {#if tabDirty}
              <span
                class="w-1.5 h-1.5 rounded-full bg-warn not-italic shrink-0"
                title="Unsaved changes"
                aria-label="Unsaved changes"
              ></span>
            {:else if tabKind !== "clean"}
              <span
                class="w-1.5 h-1.5 rounded-full bg-accent not-italic shrink-0"
                title={statusBadgeLabel(tabKind, true)}
                aria-label={statusBadgeLabel(tabKind, true)}
              ></span>
            {/if}
            <button
              type="button"
              tabindex="-1"
              onclick={(e) => closeTab(tab.path, e)}
              class="opacity-0 group-hover:opacity-100 p-0.5 rounded hover:bg-surfaceHover text-textMuted hover:text-danger transition-opacity shrink-0"
              aria-label={`Close ${tab.name}`}
              title="Close tab"
            >
              <X size={11} />
            </button>
          </div>
        {/each}
      </div>
      <ScrollCue target={tabStrip} axis="x" />
      </div>
    {/if}

    <div class="flex items-center gap-1 shrink-0">
      {#if openTabs.length > 1}
        <!-- Two word-buttons used to sit here eating roughly 130px of the tab
             strip on every repository with more than one file open. -->
        <button
          type="button"
          onclick={closeOtherTabs}
          aria-label="Close other tabs"
          title="Close other tabs"
          class="gp-icon-btn text-textMuted hover:text-textPrimary" data-size="sm"
        >
          <Columns2 size={13} />
        </button>
        <button
          type="button"
          onclick={closeAllTabs}
          aria-label="Close all tabs"
          title="Close all tabs"
          class="gp-icon-btn text-textMuted hover:text-danger" data-size="sm"
        >
          <XCircle size={13} />
        </button>
        <div class="h-3.5 w-1 rounded-full bg-border/50 shrink-0" aria-hidden="true"></div>
      {/if}

      <button
        type="button"
        bind:this={livePulseToggle}
        onclick={() => toggleSidePane("dashboard")}
        aria-pressed={paneLayout.dashboardVisible}
        aria-label="{paneLayout.dashboardVisible ? 'Hide' : 'Show'} Live pulse dashboard"
        title="{paneLayout.dashboardVisible ? 'Hide' : 'Show'} Live pulse dashboard ({shortcutTextLabel('⌘⇧D', $hostPlatform.os)})"
        class="gp-btn px-2! flex items-center gap-1 text-meta {paneLayout.dashboardVisible ? 'border-accent bg-accent/15 text-accent font-semibold' : ''}" data-size="sm"
      >
        <Activity size={12} />
        <span>Live pulse</span>
      </button>
    </div>
  </div>

  {#if activeTabPath}
    <!--
      Subject line. The breadcrumb was decoration: it printed the same segments
      the tab already named and did nothing when clicked. Each folder segment
      is now a control that reveals that folder in the Explorer, which is the
      question a breadcrumb exists to answer — "where is this?" — and the
      answer used to require scrolling the tree by hand.
    -->
    <div class="flex items-center justify-between gap-2 px-2.5 py-1 gp-section-edge shrink-0 text-xs" data-file-path-strip>
      <div class="relative min-w-0 flex-1">
      <nav
        bind:this={pathStrip}
        class="gp-header-scroll text-meta text-textMuted font-mono"
        aria-label="Path to the open file"
      >
        <div class="flex items-center gap-0.5 min-w-max py-0.5">
        {#each pathSegments as seg, idx}
          {#if idx > 0}
            <ChevronRight size={10} class="shrink-0 text-faint" aria-hidden="true" />
          {/if}
          {#if idx === pathSegments.length - 1}
            <span class="text-textPrimary font-semibold select-text">{seg}</span>
          {:else}
            {@const dir = pathSegments.slice(0, idx + 1).join("/")}
            <button
              type="button"
              onclick={() => revealDirectory(dir)}
              title="Reveal {dir}/ in the Explorer"
              class="shrink-0 rounded px-1 py-0.5 hover:bg-surfaceHover hover:text-textPrimary transition-colors"
            >{seg}</button>
          {/if}
        {/each}

        {#if activeKind !== "clean"}
          <span class="ml-1.5 shrink-0 px-1.5 py-0.2 text-micro font-semibold rounded {statusBadgeClass(activeKind)}">
            {statusBadgeLabel(activeKind, true)}
          </span>
        {/if}
        {#if activeTabDirty}
          <span class="ml-1.5 shrink-0 text-micro font-semibold text-warn">Unsaved</span>
        {/if}
        </div>
      </nav>
      <ScrollCue target={pathStrip} axis="x" />
      </div>

      <div class="flex items-center gap-1 shrink-0">
        <button
          type="button"
          onclick={copyActivePath}
          class="gp-icon-btn text-textMuted hover:text-textPrimary" data-size="sm"
          aria-label="Copy relative path"
          title="Copy relative path"
        >
          <Copy size={12} />
        </button>

        <button
          type="button"
          onclick={openActiveInDefaultApp}
          class="gp-icon-btn text-textMuted hover:text-textPrimary" data-size="sm"
          aria-label="Open in default application"
          title="Open in Default Application"
        >
          <ExternalLink size={12} />
        </button>

        <div class="h-3.5 w-1 rounded-full bg-border/50 shrink-0" aria-hidden="true"></div>

        <button
          type="button"
          onclick={inspectInDiff}
          class="gp-btn px-1.5! text-micro flex items-center gap-1 text-info" data-size="sm"
          title="View in Diff Tab"
        >
          <Layers size={11} />
          <span>Diff</span>
        </button>

        <!-- Diff is this file's uncommitted change; History is the way back
             through the commits before it, each opening on the Diff page. -->
        <FileHistoryMenu repoPath={$currentRepoPath} path={activeTabPath} />

        <!-- A toggle, not a destination: the open file reads as its blame
             in place, and the tabs, tree and selection stay where they are. -->
        <button
          type="button"
          onclick={toggleBlameLens}
          aria-pressed={$explorerLens.lens === "blame"}
          data-blame-lens
          class="gp-btn px-1.5! text-micro flex items-center gap-1 {$explorerLens.lens === 'blame' ? 'text-accent bg-accent/15 border-accent!' : 'text-accent'}" data-size="sm"
          title={$explorerLens.lens === "blame" ? "Back to the editor" : "Show Git blame for this file"}
        >
          <GitCommit size={11} />
          <span>Blame</span>
        </button>
      </div>
    </div>
  {/if}

  {#snippet explorerTree()}
    <ExplorerRail
      selectedFile={activeTabPath}
      {revealRequest}
      onSelectFile={(path) => previewFile(path)}
      onPinFile={(path) => pinFile(path)}
    />
  {/snippet}

  <div class="flex-1 flex min-h-0 overflow-hidden relative">
    {#if paneLayout.explorerVisible}
      {#if paneLayout.editorVisible}
        <FileTreeColumn class="gp-float-card" reserve={explorerReserve}>
          {@render explorerTree()}
        </FileTreeColumn>
      {:else}
        <div class="gp-float-card flex-1 min-w-0" data-explorer-card>
          {@render explorerTree()}
        </div>
      {/if}
    {/if}

    {#if paneLayout.editorVisible}
      <!-- The panel the open-file tabs control; the strip announced the
           relationship with no referent before this id existed. -->
      <div
        id={EDITOR_PANE_ID}
        role="tabpanel"
        class="flex-1 flex flex-col min-w-0 h-full bg-background overflow-hidden relative"
      >
        {#if $explorerLens.lens === "blame" && activeTabPath}
          <!-- Blame reads the same selection the tabs drive, so it follows
               the active tab. It does not wait for the editor's read. -->
          <LazyView load={loadBlame} name="Blame" />
        {:else if isLoadingFile && !activeBlob}
          <div class="flex-1 flex flex-col items-center justify-center text-textMuted text-xs gap-2">
            <span>Loading file contents…</span>
          </div>
        {:else if fileError}
          <div class="flex-1 flex flex-col items-center justify-center text-danger text-xs p-6 text-center">
            <ShieldAlert size={28} class="mb-2 text-danger" />
            <span class="font-semibold text-sm text-danger mb-1">Couldn't read this file.</span>
            <span class="max-w-md font-mono text-meta">{fileError}</span>
          </div>
        {:else if activeBlob && activeTabPath}
          {#if activeConflict && activeDraft}
            <EditorConflictBanner
              path={activeTabPath}
              unbased={activeConflict.actual === null}
              overwriteNote={overwriteNote(activeBlob)}
              disk={activeBlob.text ?? ""}
              draft={activeDraft.content}
              onReload={() => void reloadFromDisk()}
              onOverwrite={() => void overwriteDraft()}
            />
          {/if}
          <MediaViewer
            filePath={activeTabPath}
            blob={activeBlob}
            draftContent={activeDraft?.content ?? null}
            dirty={activeTabDirty}
            onSave={(newContent) => saveDraft(activeTabPath, newContent)}
            onDraftChange={(newContent, sourceContent) =>
              handleDraftChange(activeTabPath, newContent, sourceContent)}
            onRequestDiscard={() => requestDiscardDraft(activeTabPath)}
            onSendSelectionToAgent={(text, checkout) => void sendToAgentFromPage(text, checkout)}
          />
        {:else}
          <EmptyState
            icon={FileText}
            title="No file opened"
            hint={$explorerLens.lens === "blame"
              ? "Blame is on: the file you open shows its line authorship and code age."
              : "Single-click previews a file; double-click pins it. Filter the tree with globs, /regex/, or ~fuzzy."}
          />
        {/if}
      </div>
    {/if}

    {#if paneLayout.dashboardVisible}
      <!-- A floating card beside the editor, like the sidebar beside the workspace. -->
      <div class="gp-float-card {paneLayout.editorVisible ? 'w-80 shrink-0' : 'flex-1 min-w-0'}" data-live-pulse-card>
        <LivePulseDashboard
          selectedFile={activeTabPath}
          onSelectFile={(path) => previewFile(path)}
          onHide={hideLivePulse}
        />
      </div>
    {/if}
  </div>
</div>
