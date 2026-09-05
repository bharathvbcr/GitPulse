<script lang="ts">
  import type { RepoChangedPayload } from "./lib/repos/events";
  import type { LedgerAppended } from "./lib/ledger/types";
  import { onDestroy, onMount, tick } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { repoStore } from "./lib/stores/repoStore";
  import { repoMetrics } from "./lib/metrics/repoMetrics";
  import { graphStore } from "./lib/stores/graphStore";
  import { themeStore } from "./lib/stores/themeStore";
  import { filterStore } from "./lib/stores/filterStore";
  import { interfaceStore } from "./lib/stores/interfaceStore";
  import { toastStore } from "./lib/stores/toastStore";
  import { harnessStore } from "./lib/stores/harnessStore";
  import { applyPlatformClass, isMacOS } from "./lib/platform";
  import { displayName } from "./lib/repos/paths";
  import { formatError } from "./lib/ui/formatError";
  import { LAYERS } from "./lib/ui/layers";
  import { diagnostics } from "./lib/diagnostics/diagnostics";
  import { showsDiagnosticsButton } from "./lib/ui/diagnosticsButton";
  import {
    subscribeNativeShell,
    syncRecentMenu,
    takePendingOpen,
  } from "./lib/desktop/nativeShell";
  import { repoWindowTitle, syncWindowChrome } from "./lib/desktop/windowChrome";
  import Logo from "./lib/components/Logo.svelte";
  import HarnessBadge from "./lib/components/HarnessBadge.svelte";
  import Sidebar from "./lib/components/Sidebar.svelte";
  // Graph, Diff and Reflog live inside History now; App no longer mounts any
  // of them directly, nor the filter bar that used to strip across the top.
  import HistoryView from "./lib/components/HistoryView.svelte";
  import InsightsView from "./lib/components/InsightsView.svelte";
  import {
    FOCUS_COMMIT_SEARCH_EVENT,
    isCommitSearchChord,
    ownsCommitSearchChord,
    tabForCommitSearch,
  } from "./lib/views/commitFilter";
  import { isImeComposition } from "./lib/keyboard/imeGuard";
  import CommandPalette from "./lib/components/CommandPalette.svelte";
  import Tooltip from "./lib/components/Tooltip.svelte";
  import RebaseModal from "./lib/components/RebaseModal.svelte";
  import CloneModal from "./lib/components/CloneModal.svelte";
  import SettingsModal from "./lib/components/SettingsModal.svelte";
  import ShortcutsModal from "./lib/components/ShortcutsModal.svelte";
  import ToastContainer from "./lib/components/ToastContainer.svelte";
  import StatusBar from "./lib/components/StatusBar.svelte";
  import CoachMark from "./lib/components/CoachMark.svelte";
  import WorkspaceView from "./lib/components/WorkspaceView.svelte";
  import CodeView from "./lib/components/CodeView.svelte";
  import LazyView from "./lib/components/LazyView.svelte";
  import TerminalDock from "./lib/components/TerminalDock.svelte";

  // Views the app does not need to start. Each becomes its own chunk, so
  // opening a repository no longer parses the coverage, health, storage,
  // GitHub, terminal and Manvi panes — nor, through TerminalPanel, the
  // 334 KB xterm runtime. WorkView is deliberately NOT here: it is the
  // default tab, so deferring it would only add a round trip to startup.
  //
  // Declared at module scope on purpose. LazyView keys its cache on the
  // loader's identity, so an inline `load={() => import(...)}` would be a
  // fresh function each render and remount the view every update.
  const loadCoverageViewer = () => import("./lib/components/CoverageViewer.svelte");
  const loadHealthPanel = () => import("./lib/components/HealthPanel.svelte");
  const loadStoragePanel = () => import("./lib/components/StoragePanel.svelte");
  const loadTerminalPanel = () => import("./lib/components/TerminalPanel.svelte");
  const loadCodeStackViewer = () => import("./lib/components/CodeStackViewer.svelte");
  const loadGitHubPanel = () => import("./lib/components/GitHubPanel.svelte");
  const loadManviOpsPanel = () => import("./lib/components/ManviOpsPanel.svelte");
  const loadReflogViewer = () => import("./lib/components/ReflogViewer.svelte");
  const loadBlameViewer = () => import("./lib/components/BlameViewer.svelte");
  const loadConflictEditor = () => import("./lib/components/ConflictEditor.svelte");
  const loadPulseView = () => import("./lib/components/pulse/PulseView.svelte");
  const loadFleetView = () => import("./lib/components/FleetView.svelte");
  import DiagnosticsModal from "./lib/components/DiagnosticsModal.svelte";
  import RepoTabBar from "./lib/components/RepoTabBar.svelte";
  import ViewTabBar from "./lib/components/ViewTabBar.svelte";
  import PromptModal from "./lib/components/PromptModal.svelte";
  import { promptQuickCommit } from "./lib/commit/quickCommit";
  import {
    RefreshCw,
    FolderOpen,
    Download,
    Clock,
    Bug,
  } from "lucide-svelte";
  import {
    GRAPH_FETCH_DEBOUNCE_MS,
    createGraphFetchScheduler,
  } from "./lib/graph/graphFetchScheduler";
  import {
    runBootSequence,
    type NativeShellHandlers,
  } from "./lib/boot/bootSequence";
  import {
    checkForAppUpdate,
    describeUpdateCheck,
    maybeNotifyUpdate,
  } from "./lib/updates/updateCheck";
  import { openExternal } from "./lib/desktop/openExternal";
  import { askConfirm } from "./lib/stores/modalStore";
  import {
    hasUnsavedEditorDrafts,
    unsavedEditorDrafts,
  } from "./lib/files/editorDraftRegistry";
  import { editorFileSaveQueue } from "./lib/files/serialSave";

  let isRebaseModalOpen = $state(false);
  let isCloneModalOpen = $state(false);
  let isSettingsModalOpen = $state(false);
  let isShortcutsOpen = $state(false);
  let isDiagnosticsOpen = $state(false);
  let dropActive = $state(false);
  /** Repo path the modal-close effect last saw; poll-tick emissions skip. */
  let lastModalRepoPath: string | null = null;
  const macos = isMacOS();

  // --- terminal dock --------------------------------------------------------
  // The terminal is a dock beneath the active view, not a view of its own: a
  // PTY has to survive a view switch, so this pane was already mounted once
  // and hidden thereafter — a page you could never leave without closing.
  // TerminalDock owns the mount-once-keep-mounted rule; the outer
  // {#key currentPath} still tears the session down on a real repo switch.
  const terminalDockOpen = $derived($interfaceStore.terminalDockOpen);

  // --- fleet keep-alive -----------------------------------------------------
  // Fleet is workspace-scoped, so it sits BESIDE the repository pane rather
  // than inside it, and the two are swapped by hiding — never by unmounting.
  // Rendering Fleet as an {:else} of the repo block would destroy the
  // {#key currentPath} subtree on every toggle, which kills the live terminal
  // PTY it contains and re-hydrates every open tab on the way back.
  const fleetOpen = $derived($interfaceStore.fleetOpen);
  let fleetMounted = $state(false);
  $effect(() => {
    if (fleetOpen) fleetMounted = true;
  });

  // Forward legacy repoStore.error to centralized toast queue
  $effect(() => {
    const err = $repoStore.error;
    if (err) {
      toastStore.error(err);
      repoStore.setError(null);
    }
  });

  // --- drag-drop overlay grace ----------------------------------------------
  // enter/leave pairs fire in bursts near the window edge; hiding after a
  // short grace window stops the full-screen blur from strobing.
  const DROP_HIDE_GRACE_MS = 120;
  let dropHideTimer: ReturnType<typeof setTimeout> | null = null;

  function setDropOverlay(active: boolean) {
    if (active) {
      if (dropHideTimer !== null) {
        clearTimeout(dropHideTimer);
        dropHideTimer = null;
      }
      dropActive = true;
      return;
    }
    if (dropHideTimer !== null) return;
    dropHideTimer = setTimeout(() => {
      dropHideTimer = null;
      dropActive = false;
    }, DROP_HIDE_GRACE_MS);
  }

  let conflictedCount = $derived($repoStore.statuses.filter((s) => s.is_conflicted).length);

  /** Total occurrences across recorded errors, for the header badge. */
  let diagnosticErrorCount = $derived(
    $diagnostics.reduce((total, entry) => (entry.severity === "error" ? total + entry.count : total), 0),
  );

  function reportPaneCrash(error: unknown) {
    // Deferred out of the render pass: boundary failures happen mid-render.
    setTimeout(() => diagnostics.error("pane-crash", error), 0);
  }

  async function openFromExternal(path: string) {
    await repoStore.openRepo(path);
  }

  /**
   * Native "Search Commits" and ⌘F (except Files, which owns in-file search).
   * Switching first is what makes the chord work on Work: FilterBar is not
   * mounted there, so dispatching the focus event alone was a silent no-op.
   */
  async function focusCommitSearch() {
    const tab = $repoStore.activeTab;
    const target = tabForCommitSearch(tab);
    if (target !== tab) {
      repoStore.setActiveTab(target);
      await tick();
    }
    window.dispatchEvent(new CustomEvent(FOCUS_COMMIT_SEARCH_EVENT));
  }

  let exitRequestPending = false;
  let exitApproved = false;

  async function answerNativeExitRequest() {
    if (exitRequestPending) return;
    exitRequestPending = true;

    if (editorFileSaveQueue.pending > 0) {
      const pending = editorFileSaveQueue.pending;
      const waitForSaves = await askConfirm({
        title: "Finish Saving Before Quit?",
        message: `${pending} editor ${pending === 1 ? "save is" : "saves are"} still in progress. GitPulse can wait for accepted writes before quitting.`,
        confirmLabel: "Wait and Quit",
        cancelLabel: "Keep Editing",
      });
      if (!waitForSaves) {
        exitRequestPending = false;
        return;
      }
      toastStore.info("Finishing editor saves before quitting…");
      await editorFileSaveQueue.whenIdle();
    }

    if (hasUnsavedEditorDrafts()) {
      const drafts = unsavedEditorDrafts();
      const fileCount = drafts.reduce((total, entry) => total + entry.paths.length, 0);
      const preview = drafts
        .flatMap((entry) => entry.paths.map((path) => `${displayName(entry.repo)} — ${path}`))
        .slice(0, 5)
        .map((path) => `• ${path}`)
        .join("\n");
      const omitted = Math.max(0, fileCount - 5);
      const confirmed = await askConfirm({
        title: "Discard Unsaved Edits and Quit?",
        message: `${fileCount} unsaved editor ${fileCount === 1 ? "draft" : "drafts"} across ${drafts.length} ${drafts.length === 1 ? "repository" : "repositories"}:\n${preview}${omitted > 0 ? `\n…and ${omitted} more` : ""}`,
        confirmLabel: "Discard and Quit",
        cancelLabel: "Keep Editing",
      });
      if (!confirmed) {
        exitRequestPending = false;
        return;
      }
    }

    exitApproved = true;
    try {
      await invoke("cmd_exit_app");
    } catch (error) {
      exitApproved = false;
      exitRequestPending = false;
      diagnostics.error("desktop:exit", error);
      toastStore.error(`Could not quit GitPulse: ${formatError(error)}`);
    }
  }

  onMount(() => {
    applyPlatformClass();
    const unsubs: Array<() => void> = [];
    // HMR/unmount can tear the component down while the async subscription
    // chain is still awaiting; listeners pushed after cleanup must be
    // unwound immediately instead of leaking past the component.
    let disposed = false;
    const track = (unsub: () => void) => {
      if (disposed) unsub();
      else unsubs.push(unsub);
    };

    const guardBrowserUnload = (event: BeforeUnloadEvent) => {
      if (exitApproved || !hasUnsavedEditorDrafts()) return;
      event.preventDefault();
      // Browsers intentionally ignore custom text but require a value to show
      // their native data-loss confirmation.
      event.returnValue = "";
    };
    window.addEventListener("beforeunload", guardBrowserUnload);
    track(() => window.removeEventListener("beforeunload", guardBrowserUnload));

    // Arm the native side only after its listener exists. This prevents a
    // startup failure from trapping an exit request with nobody to answer it.
    void listen<void>("gitpulse-exit-requested", () => {
      void answerNativeExitRequest();
    }).then(
      (unlisten) => {
        track(unlisten);
        if (disposed) return;
        void invoke("cmd_set_exit_guard_ready").catch((error) =>
          diagnostics.warn("boot:exit-guard", error),
        );
      },
      (error) => diagnostics.warn("boot:exit-listener", error),
    );

    // The command palette opens Diagnostics through this window event.
    const openDiagnostics = () => {
      isDiagnosticsOpen = true;
    };
    window.addEventListener("gitpulse:diagnostics", openDiagnostics);
    track(() => window.removeEventListener("gitpulse:diagnostics", openDiagnostics));

    const openShortcuts = () => {
      isShortcutsOpen = true;
    };
    window.addEventListener("gitpulse:shortcuts", openShortcuts);
    track(() => window.removeEventListener("gitpulse:shortcuts", openShortcuts));

    const openSettings = () => {
      isSettingsModalOpen = true;
    };
    window.addEventListener("gitpulse:settings", openSettings);
    track(() => window.removeEventListener("gitpulse:settings", openSettings));

    const handleGlobalKeydown = (e: KeyboardEvent) => {
      const isInput =
        e.target instanceof HTMLInputElement ||
        e.target instanceof HTMLTextAreaElement ||
        (e.target as HTMLElement | null)?.isContentEditable;

      if ((e.metaKey || e.ctrlKey) && (e.key === "=" || e.key === "+")) {
        e.preventDefault();
        interfaceStore.zoomIn();
        return;
      }
      if ((e.metaKey || e.ctrlKey) && e.key === "-") {
        e.preventDefault();
        interfaceStore.zoomOut();
        return;
      }
      if ((e.metaKey || e.ctrlKey) && e.key === "0") {
        e.preventDefault();
        interfaceStore.resetZoom();
        return;
      }
      if ((e.metaKey || e.ctrlKey) && e.key === "/") {
        e.preventDefault();
        isShortcutsOpen = true;
        return;
      }
      // ⌃` toggles the terminal dock, the chord every terminal-hosting editor
      // uses. Deliberately Control on macOS too: ⌘` is the OS window cycler.
      // Guarded on an open repository because the dock lives inside the
      // repository pane and has no shell to attach to without one.
      if (e.ctrlKey && !e.metaKey && !e.altKey && e.key === "`" && $repoStore.currentPath) {
        e.preventDefault();
        interfaceStore.toggleTerminalDock();
        return;
      }
      if (isCommitSearchChord(e) && !isImeComposition(e) && ownsCommitSearchChord($repoStore.activeTab)) {
        e.preventDefault();
        void focusCommitSearch();
        return;
      }
      if (!isInput && e.key === "?" && !e.metaKey && !e.ctrlKey) {
        e.preventDefault();
        isShortcutsOpen = true;
        return;
      }
    };
    window.addEventListener("keydown", handleGlobalKeydown);
    track(() => window.removeEventListener("keydown", handleGlobalKeydown));

    const shellHandlers: NativeShellHandlers = {
      open: () => void repoStore.pickAndOpenRepo(),
      clone: () => {
        isCloneModalOpen = true;
      },
      settings: () => {
        isSettingsModalOpen = true;
      },
      refresh: () => void repoStore.refresh(),
      toggleTheme: () => themeStore.toggle(),
      themeSystem: () => themeStore.setPreference("system"),
      themeLight: () => themeStore.setTheme("light"),
      themeDark: () => themeStore.setTheme("dark"),
      setTab: (tab) => repoStore.setActiveTab(tab),
      fleet: () => interfaceStore.setFleetOpen(true),
      terminalDock: () => interfaceStore.toggleTerminalDock(),
      fetch: () => {
        repoStore.fetch().then(() => toastStore.info("Fetched remote updates"));
      },
      pull: () => {
        repoStore.pull().then(() => toastStore.success("Pulled changes from remote"));
      },
      push: () => {
        repoStore.push().then(() => toastStore.success("Pushed commits to remote"));
      },
      stash: () => {
        repoStore.stashSave().then(() => {
          toastStore.action("Stashed uncommitted changes", "Pop", () => {
            void repoStore.stashPop().then((outcome) => {
              if (!outcome.ok) toastStore.error(outcome.error ?? "Pop failed");
            });
          });
        });
      },
      stashPop: () => {
        repoStore.stashPop().then((outcome) => {
          if (outcome.ok) toastStore.success("Popped latest stash");
          else toastStore.error(outcome.error ?? "Pop failed");
        });
      },
      rebase: () => {
        isRebaseModalOpen = true;
      },
      quickCommit: () => void promptQuickCommit(),
      palette: () => window.dispatchEvent(new CustomEvent("gitpulse:palette")),
      focusFilter: () => void focusCommitSearch(),
      openRecent: (path) => void openFromExternal(path),
      openRepo: (path) => void openFromExternal(path),
      closeRepoTab: () => void repoStore.closeActiveTab(),
      nextRepoTab: () => void repoStore.nextTab(),
      prevRepoTab: () => void repoStore.prevTab(),
      reopenRepoTab: () => void repoStore.reopenLastClosed(),
      openError: (message) => toastStore.error(message),
      setDropActive: (active) => {
        setDropOverlay(active);
      },
    };
    // Every boot step fails independently: a throw in restoreWorkspace must
    // not take down, say, the repo-changed listener registration with it.
    // Boot errors surface between renders, so unlike reportPaneCrash they
    // need no setTimeout deferral.
    void runBootSequence(
      {
        subscribeNativeShell,
        takePendingOpen,
        restoreWorkspace: () => repoStore.restoreWorkspace(),
        openRepo: openFromExternal,
        syncRecentMenu,
        handleRepoChanged: (path) => {
          void repoStore.handleRepoChanged(path);
          // The same event drives the metric panels. Each metric applies its
          // own debounce and cost floor, so a checkout storm becomes one
          // re-measurement per metric rather than one per file event.
          if (path) repoMetrics.invalidate(path);
        },
        listenRepoChanged: (changed) =>
          listen<RepoChangedPayload>("repo-changed", (event) =>
            changed(event.payload?.path),
          ),
        track,
        onError: (step, err) => diagnostics.warn(`boot:${step}`, err),
      },
      [...$repoStore.recentRepos],
      shellHandlers,
    );

    // The action journal follows the durable ledger rather than accumulating
    // in memory. Every guarded mutation announces its cursor; the store pages
    // from where it left off, so a missed notification costs nothing.
    void listen<LedgerAppended>("ledger-appended", (event) => {
      const path = event.payload?.repo_path;
      if (path) void harnessStore.syncLedger(path);
    }).then(
      // `track` takes the unlisten, not the promise of one. Routing it through
      // the tracker is what makes a listener that resolves *after* teardown
      // unregister itself instead of outliving the webview.
      (unlisten) => track(unlisten),
      (err) => diagnostics.warn("boot:ledger-appended", err),
    );

    // Opt-in release check. `maybeNotifyUpdate` makes no request at all while
    // the preference is off, so the default build never contacts the network
    // about itself. Detached from the boot sequence deliberately: a slow or
    // unreachable remote must not delay the workspace restoring.
    void maybeNotifyUpdate({
      prefs: {
        checkForUpdates: $interfaceStore.checkForUpdates,
        lastUpdateCheckAt: $interfaceStore.lastUpdateCheckAt,
        dismissedUpdateVersion: $interfaceStore.dismissedUpdateVersion,
      },
      now: Date.now(),
      check: () => checkForAppUpdate(),
      markChecked: (at) => interfaceStore.markUpdateChecked(at),
      notify: (result) => {
        toastStore.action(
          describeUpdateCheck(result).message,
          "View release",
          () => {
            // Dismiss this version whether or not the browser hand-off
            // works; the user has seen and acted on the notice.
            interfaceStore.dismissUpdateVersion(result.latestVersion);
            void openExternal(result.releaseUrl).catch((err) =>
              diagnostics.warn("update:open", err),
            );
          },
          12000,
        );
      },
      onError: (message) => diagnostics.warn("update:check", message),
    });

    return () => {
      disposed = true;
      if (dropHideTimer !== null) {
        clearTimeout(dropHideTimer);
        dropHideTimer = null;
      }
      for (const unsub of unsubs) unsub();
    };
  });

  /**
   * Fetch ownership: path/revision/query changes re-walk history here.
   * Activation does NOT: cached rows render instantly via showRepo, and
   * freshness comes from refresh()'s own loadGraph (watcher events,
   * post-mutation refreshes), so switching tabs never re-walks.
   *
   * The scheduler exists because a naive inline effect cannot: repoStore
   * emits on every publish (hydrate, branch-stats batches, the status poll),
   * each emission tears the effect down, and teardown used to clear the armed
   * fetch timer before the memo-guarded body would refuse to reschedule —
   * dropping the one fetch a freshly opened repository needed and leaving the
   * graph loader spinning. Identical requests now leave the armed timer
   * untouched; only a real key change re-arms the window.
   */
  const graphScheduler = createGraphFetchScheduler({
    load: ({ path, query, revision, refScope }) =>
      void graphStore.loadGraph(path, query, revision, { refScope }),
    debounceMs: GRAPH_FETCH_DEBOUNCE_MS,
  });
  onDestroy(() => graphScheduler.reset());

  $effect(() => {
    graphScheduler.sync({
      path: $repoStore.currentPath,
      query: $filterStore.searchQuery,
      revision: $filterStore.selectedBranch,
      // Which refs the graph walks is part of WHICH GRAPH this is, so a
      // change to it re-arms a fetch through the same single owner as a
      // filter edit. Without it the setting was inert: the key never moved,
      // the scheduler saw an already-served request, and nothing reloaded.
      refScope: $interfaceStore.graphRefScope,
    });
  });

  $effect(() => {
    void syncWindowChrome(
      repoWindowTitle($repoStore.currentPath, $repoStore.currentBranch),
      conflictedCount,
    );
  });

  // Catching up on a newly-opened repository.
  //
  // Correctness never depends on GitPulse having been running: git's reflog and
  // the agent transcripts both recorded what happened while it was closed, and
  // this replays them into the ledger. Guarded on a genuine repo switch because
  // the effect re-runs on every store emission — the ~6s status poll included.
  let lastCaughtUpPath: string | null = null;
  $effect(() => {
    const path = $repoStore.currentPath;
    // Switch the public journal projection synchronously. Durable events for
    // other open repositories may still arrive and refresh their own buckets,
    // but they must never become visible in this repository's journal.
    harnessStore.activateRepository(path);
    if (path === lastCaughtUpPath) return;
    lastCaughtUpPath = path;
    if (!path) return;
    void harnessStore.catchUp(path);
  });

  $effect(() => {
    // Only a genuine repo switch closes the Rebase modal: this effect re-runs
    // on every repoStore emission (status poll, stats batches) and must not
    // dismiss a modal the user is working in.
    const path = $repoStore.currentPath;
    if (path === lastModalRepoPath) return;
    lastModalRepoPath = path;
    isRebaseModalOpen = false;
  });
</script>

  {#snippet paneFailed(error: unknown, reset: () => void)}
    <!-- Minimal crash isolation: a broken pane never takes down the window. -->
    {reportPaneCrash(error)}
    <div class="flex-1 flex flex-col items-center justify-center gap-2 p-4" title={formatError(error)}>
      <span class="text-xs text-textMuted font-sans">Pane failed to render</span>
      <button type="button" class="gp-btn" onclick={() => reset()}>Reset</button>
    </div>
  {/snippet}

<div
  class="gp-shell h-screen w-screen flex flex-col bg-background text-textPrimary overflow-hidden font-sans relative"
  style="--ui-font-scale: {$interfaceStore.uiFontScale};"
>
  <!-- Top App Navigation Bar -->
  <svelte:boundary failed={paneFailed}>
    <header
    class="gp-glass gp-titlebar bg-surface border-b border-border flex items-center select-none shrink-0 min-w-0 overflow-hidden {macos
      ? 'h-12 pr-3'
      : 'h-10 px-3'}"
  >
    {#if macos}
      <div class="w-[76px] h-full shrink-0" data-tauri-drag-region></div>
    {/if}
    <div
      class="gp-drag-children flex items-center gap-2 font-bold text-sm tracking-tight text-accent shrink-0"
      data-tauri-drag-region
    >
      <Logo size={19} variant="badge" />
      <span class="text-textPrimary">GitPulse</span>
    </div>
    <div class="h-4 w-px bg-border mx-1 shrink-0"></div>

    <div class="gp-header-scroll min-w-0 flex-1 h-full">
      <div class="flex items-center gap-2 min-w-full w-max px-2 h-full">
        <!-- The icons carry the meaning on their own; the words are the part
             the Layout setting drops. Both keep their accessible name either
             way, so the label is decoration, never the only cue. -->
        <button
          onclick={() => repoStore.pickAndOpenRepo()}
          class="gp-btn !py-1 shrink-0"
          title="Open a repository"
          aria-label="Open a repository"
        >
          <FolderOpen size={13} class="text-accent" />
          {#if $interfaceStore.showHeaderActionLabels}<span>Open...</span>{/if}
        </button>
        <button
          onclick={() => (isCloneModalOpen = true)}
          class="gp-btn !py-1 shrink-0"
          title="Clone a repository"
          aria-label="Clone a repository"
        >
          <Download size={13} class="text-accent" />
          {#if $interfaceStore.showHeaderActionLabels}<span>Clone...</span>{/if}
        </button>

        {#if $repoStore.currentPath}
          <div class="h-4 w-px bg-border mx-1 shrink-0"></div>
          <ViewTabBar {conflictedCount} />
        {/if}
        <div class="flex-1 min-w-4 h-full" data-tauri-drag-region></div>
      </div>
    </div>

    <!-- Right Actions -->
    <div class="gp-titlebar-actions flex items-center gap-2 shrink-0 bg-surface pl-1 h-full">
      <!-- "When recorded" hides this only while the log is genuinely empty,
           errors and warnings alike; the palette opens Diagnostics either way. -->
      {#if showsDiagnosticsButton($interfaceStore.diagnosticsButton, $diagnostics.length)}
      <button
        onclick={() => (isDiagnosticsOpen = true)}
        title="Diagnostics — errors, warnings and crash logs"
        aria-label="Open Diagnostics"
        class="gp-icon-btn relative"
      >
        <Bug size={14} />
        {#if diagnosticErrorCount > 0}
          <span
            class="absolute -top-1 -right-1 min-w-[15px] h-[15px] px-1 rounded-full bg-rose-500 text-white text-[9px] leading-[15px] font-semibold text-center"
            title="{diagnosticErrorCount} error{diagnosticErrorCount === 1 ? '' : 's'} recorded"
          >
            {diagnosticErrorCount > 99 ? "99+" : diagnosticErrorCount}
          </span>
        {/if}
      </button>
      {/if}
      {#if $repoStore.currentPath}
        <button
          onclick={() => repoStore.refresh()}
          title="Refresh Status"
          class="gp-icon-btn"
        >
          <RefreshCw size={14} class={$repoStore.isLoading ? "animate-spin" : ""} />
        </button>
      {/if}
      {#if $interfaceStore.showHarnessBadges}
        <HarnessBadge />
      {/if}
    </div>
  </header>
  </svelte:boundary>

  {#if !$interfaceStore.autoHideRepoTabs || $repoStore.openTabs.length > 1}
    <RepoTabBar onOpen={() => void repoStore.pickAndOpenRepo()} />
  {/if}

  <!-- Global Toast Notification Queue -->
  <ToastContainer />

  <!-- The repository surface. Hidden, not unmounted, while Fleet is open. -->
  <div class="flex-1 flex flex-col min-h-0" class:hidden={fleetOpen}>
  {#if !$repoStore.currentPath}
    <!-- Welcome & Open Repository Screen -->
    <div class="gp-welcome flex-1 flex flex-col items-center justify-center p-8 bg-background select-none relative overflow-hidden">
      <!-- Ambient brand glow behind the hero card. -->
      <div
        aria-hidden="true"
        class="pointer-events-none absolute -top-32 left-1/2 h-96 w-[36rem] -translate-x-1/2 rounded-full opacity-25 blur-3xl"
        style="background: var(--brand-gradient);"
      ></div>
      <div class="gp-welcome-card gp-glass gp-view max-w-md w-full flex flex-col items-center text-center space-y-6 relative">
        <Logo size={68} variant="badge" animated />

        <div>
          <h1 class="text-xl font-bold text-textPrimary">Welcome to GitPulse</h1>
          <p class="text-xs text-textMuted mt-1">High-performance, native Git client built with Rust & Svelte</p>
        </div>

        <div class="w-full flex gap-3">
          <button
            onclick={() => repoStore.pickAndOpenRepo()}
            class="gp-btn-primary flex-1 !py-2.5 !px-4 !text-xs"
          >
            <FolderOpen size={15} />
            <span>Open Repository</span>
          </button>
          <button onclick={() => (isCloneModalOpen = true)} class="gp-btn flex-1 !py-2.5 !px-4">
            <Download size={15} />
            <span>Clone Repo</span>
          </button>
        </div>

        {#if $repoStore.recentRepos.length > 0}
          <div class="w-full text-left pt-4">
            <div class="flex items-center gap-1.5 text-[11px] font-semibold text-textMuted uppercase tracking-wider mb-2">
              <Clock size={12} />
              <span>Recent Repositories</span>
            </div>
            <div class="space-y-1.5">
              {#each $repoStore.recentRepos as repo}
                <button
                  onclick={() => repoStore.openRepo(repo)}
                  class="w-full px-3.5 py-2 rounded-full bg-surface border border-border/70 hover:border-accent/60 shadow-sm hover:shadow-card flex items-center justify-between text-xs text-textPrimary transition-[color,background-color,border-color,box-shadow] duration-150 text-left"
                >
                  <span class="font-medium truncate">{displayName(repo)}</span>
                  <span class="text-[10px] text-textMuted font-mono truncate max-w-xs">{repo}</span>
                </button>
              {/each}
            </div>
          </div>
        {/if}
      </div>
    </div>
  {:else}
    {#key $repoStore.currentPath}
      <div class="flex-1 flex overflow-hidden">
        <svelte:boundary failed={paneFailed}>
          <Sidebar />
        </svelte:boundary>
        <svelte:boundary failed={paneFailed}>
          <main class="gp-workspace flex-1 flex flex-col min-w-0 bg-background gp-pane">
            <!-- No {#key activeTab}: keying here destroyed and rebuilt the
                 entire pane on every view switch and replayed the .gp-view
                 entrance fade — a full-screen flicker per tab. The {#if}
                 chain alone swaps panes; state lives in stores. -->
            <div class="gp-view flex-1 flex flex-col min-h-0">
              {#if $repoStore.activeTab === "work"}
                <WorkspaceView
                  loadConflict={loadConflictEditor}
                  loadGitHub={loadGitHubPanel}
                  loadStack={loadCodeStackViewer}
                  loadManvi={loadManviOpsPanel}
                />
              {:else if $repoStore.activeTab === "code"}
                <CodeView loadBlame={loadBlameViewer} />
              {:else if $repoStore.activeTab === "history"}
                <HistoryView loadReflog={loadReflogViewer} />
              {:else if $repoStore.activeTab === "insights"}
                <InsightsView
                  loadPulse={loadPulseView}
                  loadCoverage={loadCoverageViewer}
                  loadHealth={loadHealthPanel}
                  loadStorage={loadStoragePanel}
                />
              {/if}
            </div>
            <!-- The terminal sits under the view rather than replacing it, so
                 a command's output can be read against the thing that
                 prompted it. A crash in the shell must not take the view with
                 it, hence its own boundary. -->
            <svelte:boundary failed={paneFailed}>
              <TerminalDock
                open={terminalDockOpen}
                onClose={() => interfaceStore.setTerminalDockOpen(false)}
                load={loadTerminalPanel}
              />
            </svelte:boundary>
          </main>
        </svelte:boundary>
      </div>

      <!-- Bottom Ambient Status Bar -->
      <StatusBar onOpenShortcuts={() => (isShortcutsOpen = true)} />
    {/key}

    <!-- First-run coach mark onboarding tip -->
    <CoachMark
      id="coach-palette"
      title="Command Palette"
      description="Press ⌘K anytime to search files, switch branches, run git commands, or jump to commits."
      shortcut="⌘K"
      class="bottom-10 right-6"
    />
  {/if}
  </div>

  {#if fleetMounted}
    <!-- Workspace-scoped, so it survives repository switches; hidden rather
         than unmounted for the same reason the terminal is. -->
    <svelte:boundary failed={paneFailed}>
      <div class="flex-1 flex flex-col min-h-0" class:hidden={!fleetOpen}>
        <LazyView load={loadFleetView} name="the Fleet dashboard" />
      </div>
    </svelte:boundary>
  {/if}

  {#if dropActive}
    <div
      class="gp-overlay absolute inset-0 bg-accent/10 backdrop-blur-sm p-4 flex items-center justify-center pointer-events-none gp-gpu"
      style="z-index: {LAYERS.DROP_OVERLAY}"
    >
      <div class="w-full h-full rounded-3xl border-2 border-dashed border-accent/70 bg-accent/5 flex items-center justify-center">
        <div class="gp-pop px-6 py-4 rounded-2xl bg-surface border border-accent/50 text-sm font-semibold text-textPrimary shadow-float">
          Drop a Git repository to open
        </div>
      </div>
    </div>
  {/if}

  <!-- Overlay widgets: repo views stay in the pane boundaries above. Prompt
       and Diagnostics are isolated so a PromptModal render crash cannot take
       down the log that records it. -->
  <svelte:boundary failed={paneFailed}>
    <RebaseModal isOpen={isRebaseModalOpen} onClose={() => (isRebaseModalOpen = false)} />
    <CloneModal isOpen={isCloneModalOpen} onClose={() => (isCloneModalOpen = false)} />
    <SettingsModal isOpen={isSettingsModalOpen} onClose={() => (isSettingsModalOpen = false)} />
    <ShortcutsModal isOpen={isShortcutsOpen} onClose={() => (isShortcutsOpen = false)} />
    <CommandPalette />
    <Tooltip />
  </svelte:boundary>
  <svelte:boundary failed={paneFailed}>
    <PromptModal />
  </svelte:boundary>
  <svelte:boundary failed={paneFailed}>
    <DiagnosticsModal isOpen={isDiagnosticsOpen} onClose={() => (isDiagnosticsOpen = false)} />
  </svelte:boundary>
</div>
