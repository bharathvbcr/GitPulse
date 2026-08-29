<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { repoStore } from "./lib/stores/repoStore";
  import { graphStore } from "./lib/stores/graphStore";
  import { themeStore } from "./lib/stores/themeStore";
  import { filterStore } from "./lib/stores/filterStore";
  import { interfaceStore } from "./lib/stores/interfaceStore";
  import { toastStore } from "./lib/stores/toastStore";
  import { applyPlatformClass, isMacOS } from "./lib/platform";
  import { displayName } from "./lib/repos/paths";
  import { formatError } from "./lib/ui/formatError";
  import { LAYERS } from "./lib/ui/layers";
  import { diagnostics } from "./lib/diagnostics/diagnostics";
  import {
    subscribeNativeShell,
    syncRecentMenu,
    takePendingOpen,
  } from "./lib/desktop/nativeShell";
  import { repoWindowTitle, syncWindowChrome } from "./lib/desktop/windowChrome";
  import Logo from "./lib/components/Logo.svelte";
  import HarnessBadge from "./lib/components/HarnessBadge.svelte";
  import Sidebar from "./lib/components/Sidebar.svelte";
  import CommitTable from "./lib/components/CommitTable.svelte";
  import CommitDetails from "./lib/components/CommitDetails.svelte";
  import FileViewer from "./lib/components/FileViewer.svelte";
  import DiffViewer from "./lib/components/DiffViewer.svelte";
  import ConflictEditor from "./lib/components/ConflictEditor.svelte";
  import BlameViewer from "./lib/components/BlameViewer.svelte";
  import CoverageViewer from "./lib/components/CoverageViewer.svelte";
  import HealthPanel from "./lib/components/HealthPanel.svelte";
  import StoragePanel from "./lib/components/StoragePanel.svelte";
  import TerminalPanel from "./lib/components/TerminalPanel.svelte";
  import CodeStackViewer from "./lib/components/CodeStackViewer.svelte";
  import LanguageBar from "./lib/components/LanguageBar.svelte";
  import FilterBar from "./lib/components/FilterBar.svelte";
  import CommandPalette from "./lib/components/CommandPalette.svelte";
  import Tooltip from "./lib/components/Tooltip.svelte";
  import RebaseModal from "./lib/components/RebaseModal.svelte";
  import CloneModal from "./lib/components/CloneModal.svelte";
  import SettingsModal from "./lib/components/SettingsModal.svelte";
  import ShortcutsModal from "./lib/components/ShortcutsModal.svelte";
  import ToastContainer from "./lib/components/ToastContainer.svelte";
  import StatusBar from "./lib/components/StatusBar.svelte";
  import CoachMark from "./lib/components/CoachMark.svelte";
  import GitHubPanel from "./lib/components/GitHubPanel.svelte";
  import ManviOpsPanel from "./lib/components/ManviOpsPanel.svelte";
  import ReflogViewer from "./lib/components/ReflogViewer.svelte";
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

  let isRebaseModalOpen = $state(false);
  let isCloneModalOpen = $state(false);
  let isSettingsModalOpen = $state(false);
  let isShortcutsOpen = $state(false);
  let isDiagnosticsOpen = $state(false);
  let dropActive = $state(false);
  /** Repo path the modal-close effect last saw; poll-tick emissions skip. */
  let lastModalRepoPath: string | null = null;
  const macos = isMacOS();

  // --- terminal keep-alive --------------------------------------------------
  // The PTY and xterm instance must survive view-tab switches (a shell dies
  // the moment its pane unmounts). Mounted once on first visit per repo,
  // hidden afterwards; the outer {#key currentPath} tears it down on a real
  // repo switch.
  let terminalMounted = $state(false);
  const terminalActive = $derived($repoStore.activeTab === "terminal");

  $effect(() => {
    if (!$repoStore.currentPath) {
      terminalMounted = false;
      return;
    }
    if (terminalActive) terminalMounted = true;
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
            void repoStore.stashPop();
          });
        });
      },
      stashPop: () => {
        repoStore.stashPop().then(() => toastStore.success("Popped latest stash"));
      },
      rebase: () => {
        isRebaseModalOpen = true;
      },
      quickCommit: () => void promptQuickCommit(),
      palette: () => window.dispatchEvent(new CustomEvent("gitpulse:palette")),
      focusFilter: () =>
        window.dispatchEvent(new CustomEvent("gitpulse:focus-filter")),
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
        handleRepoChanged: (path) => void repoStore.handleRepoChanged(path),
        listenRepoChanged: (changed) =>
          listen<{ path?: string }>("repo-changed", (event) =>
            changed(event.payload?.path),
          ),
        track,
        onError: (step, err) => diagnostics.warn(`boot:${step}`, err),
      },
      [...$repoStore.recentRepos],
      shellHandlers,
    );
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
    load: ({ path, query, revision }) => void graphStore.loadGraph(path, query, revision),
    debounceMs: GRAPH_FETCH_DEBOUNCE_MS,
  });
  onDestroy(() => graphScheduler.reset());

  $effect(() => {
    graphScheduler.sync({
      path: $repoStore.currentPath,
      query: $filterStore.searchQuery,
      revision: $filterStore.selectedBranch,
    });
  });

  $effect(() => {
    void syncWindowChrome(
      repoWindowTitle($repoStore.currentPath, $repoStore.currentBranch),
      conflictedCount,
    );
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
  class="h-screen w-screen flex flex-col bg-background text-textPrimary overflow-hidden font-sans relative"
  style="--ui-font-scale: {$interfaceStore.uiFontScale};"
>
  <!-- Top App Navigation Bar -->
  <svelte:boundary failed={paneFailed}>
    <header
    class="bg-surface border-b border-border flex items-center select-none shrink-0 min-w-0 overflow-hidden {macos
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
        <button onclick={() => repoStore.pickAndOpenRepo()} class="gp-btn !py-1 shrink-0">
          <FolderOpen size={13} class="text-accent" />
          <span>Open...</span>
        </button>
        <button onclick={() => (isCloneModalOpen = true)} class="gp-btn !py-1 shrink-0">
          <Download size={13} class="text-accent" />
          <span>Clone...</span>
        </button>

        {#if $repoStore.currentPath}
          <div class="h-4 w-px bg-border mx-1 shrink-0"></div>
          <ViewTabBar {conflictedCount} />
        {/if}
        <div class="flex-1 min-w-4 h-full" data-tauri-drag-region></div>
      </div>
    </div>

    <!-- Right Actions -->
    <div class="flex items-center gap-2 shrink-0 bg-surface pl-1 h-full">
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

  <RepoTabBar onOpen={() => void repoStore.pickAndOpenRepo()} />

  <!-- Global Toast Notification Queue -->
  <ToastContainer />

  {#if !$repoStore.currentPath}
    <!-- Welcome & Open Repository Screen -->
    <div class="flex-1 flex flex-col items-center justify-center p-8 bg-background select-none relative overflow-hidden">
      <!-- Ambient brand glow behind the hero card. -->
      <div
        aria-hidden="true"
        class="pointer-events-none absolute -top-32 left-1/2 h-96 w-[36rem] -translate-x-1/2 rounded-full opacity-25 blur-3xl"
        style="background: var(--brand-gradient);"
      ></div>
      <div class="gp-view max-w-md w-full flex flex-col items-center text-center space-y-6 relative">
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
      {#if $interfaceStore.showLanguageBar}
        <LanguageBar />
      {/if}
      <FilterBar />
      <div class="flex-1 flex overflow-hidden">
        <svelte:boundary failed={paneFailed}>
          <Sidebar />
        </svelte:boundary>
        <svelte:boundary failed={paneFailed}>
          <main class="flex-1 flex flex-col min-w-0 bg-background gp-pane" class:hidden={terminalActive}>
            <!-- No {#key activeTab}: keying here destroyed and rebuilt the
                 entire pane on every view switch and replayed the .gp-view
                 entrance fade — a full-screen flicker per tab. The {#if}
                 chain alone swaps panes; state lives in stores. -->
            <div class="gp-view flex-1 flex flex-col min-h-0">
              {#if $repoStore.activeTab === "history"}
                <div class="flex-1 flex flex-col min-h-0">
                  <CommitTable />
                  <CommitDetails />
                </div>
              {:else if $repoStore.activeTab === "files"}
                <FileViewer />
              {:else if $repoStore.activeTab === "diff"}
                <DiffViewer />
              {:else if $repoStore.activeTab === "conflict"}
                <ConflictEditor />
              {:else if $repoStore.activeTab === "blame"}
                <BlameViewer />
              {:else if $repoStore.activeTab === "coverage"}
                <CoverageViewer />
              {:else if $repoStore.activeTab === "health"}
                <HealthPanel />
              {:else if $repoStore.activeTab === "storage"}
                <StoragePanel />
              {:else if $repoStore.activeTab === "stack"}
                <CodeStackViewer />
              {:else if $repoStore.activeTab === "github"}
                <GitHubPanel />
              {:else if $repoStore.activeTab === "manvi"}
                <ManviOpsPanel />
              {:else if $repoStore.activeTab === "reflog"}
                <ReflogViewer />
              {/if}
            </div>
          </main>
        </svelte:boundary>
        {#if terminalMounted}
          <!-- Kept mounted across tab switches so the PTY/xterm session
               survives; display:none pauses rendering, not the process. -->
          <svelte:boundary failed={paneFailed}>
            <div class="flex-1 flex flex-col min-w-0 bg-background gp-pane" class:hidden={!terminalActive}>
              <TerminalPanel />
            </div>
          </svelte:boundary>
        {/if}
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
