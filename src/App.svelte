<script lang="ts">
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { repoStore } from "./lib/stores/repoStore";
  import { graphStore } from "./lib/stores/graphStore";
  import { themeStore } from "./lib/stores/themeStore";
  import { filterStore } from "./lib/stores/filterStore";
  import { applyPlatformClass, isMacOS } from "./lib/platform";
  import { queryNeedsServerFetch } from "./lib/filter/parseQuery";
  import { displayName } from "./lib/repos/paths";
  import { formatError } from "./lib/ui/formatError";
  import { LAYERS } from "./lib/ui/layers";
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
  import DiffViewer from "./lib/components/DiffViewer.svelte";
  import ConflictEditor from "./lib/components/ConflictEditor.svelte";
  import BlameViewer from "./lib/components/BlameViewer.svelte";
  import CoverageViewer from "./lib/components/CoverageViewer.svelte";
  import HealthPanel from "./lib/components/HealthPanel.svelte";
  import StoragePanel from "./lib/components/StoragePanel.svelte";
  import CodeStackViewer from "./lib/components/CodeStackViewer.svelte";
  import LanguageBar from "./lib/components/LanguageBar.svelte";
  import FilterBar from "./lib/components/FilterBar.svelte";
  import CommandPalette from "./lib/components/CommandPalette.svelte";
  import Tooltip from "./lib/components/Tooltip.svelte";
  import RebaseModal from "./lib/components/RebaseModal.svelte";
  import CloneModal from "./lib/components/CloneModal.svelte";
  import SettingsModal from "./lib/components/SettingsModal.svelte";
  import GitHubPanel from "./lib/components/GitHubPanel.svelte";
  import ManviOpsPanel from "./lib/components/ManviOpsPanel.svelte";
  import ReflogViewer from "./lib/components/ReflogViewer.svelte";
  import RepoTabBar from "./lib/components/RepoTabBar.svelte";
  import ViewTabBar from "./lib/components/ViewTabBar.svelte";
  import PromptModal from "./lib/components/PromptModal.svelte";
  import {
    RefreshCw,
    FolderOpen,
    Download,
    Clock,
  } from "lucide-svelte";
  import { interfaceStore } from "./lib/stores/interfaceStore";

  let isRebaseModalOpen = $state(false);
  let isCloneModalOpen = $state(false);
  let isSettingsModalOpen = $state(false);
  let dropActive = $state(false);
  const macos = isMacOS();
  /** Last repo/revision(/path-query) the graph actually fetched for. */
  let lastGraphKey: string | null = null;

  let conflictedCount = $derived($repoStore.statuses.filter((s) => s.is_conflicted).length);

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
    void (async () => {
      // A failed native-shell subscription must not abort startup: it unwinds
      // its own listeners before rethrowing, and skipping the restore below
      // would lose the persisted session. Log and keep booting.
      try {
        track(
          await subscribeNativeShell({
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
            fetch: () => void repoStore.fetch(),
            pull: () => void repoStore.pull(),
            push: () => void repoStore.push(),
            stash: () => void repoStore.stashSave(),
            stashPop: () => void repoStore.stashPop(),
            rebase: () => {
              isRebaseModalOpen = true;
            },
            palette: () => window.dispatchEvent(new CustomEvent("gitpulse:palette")),
            focusFilter: () =>
              window.dispatchEvent(new CustomEvent("gitpulse:focus-filter")),
            openRecent: (path) => void openFromExternal(path),
            openRepo: (path) => void openFromExternal(path),
            closeRepoTab: () => void repoStore.closeActiveTab(),
            nextRepoTab: () => void repoStore.nextTab(),
            prevRepoTab: () => void repoStore.prevTab(),
            reopenRepoTab: () => void repoStore.reopenLastClosed(),
            openError: (message) => repoStore.setError(message),
            setDropActive: (active) => {
              dropActive = active;
            },
          }),
        );
      } catch (err) {
        console.error("native shell unavailable; continuing without menu integration", err);
      }
      const pending = await takePendingOpen();
      await repoStore.restoreWorkspace();
      if (pending) {
        await openFromExternal(pending);
      }
      await syncRecentMenu($repoStore.recentRepos);
      try {
        track(
          await listen<{ path?: string }>("repo-changed", (event) => {
            void repoStore.handleRepoChanged(event.payload?.path);
          }),
        );
      } catch {
        /* vite preview has no Tauri event bus */
      }
    })();
    return () => {
      disposed = true;
      for (const unsub of unsubs) unsub();
    };
  });

  $effect(() => {
    const path = $repoStore.currentPath;
    // Fetch ownership: path/revision/query changes re-walk history here.
    // Activation does NOT: cached rows render instantly via showRepo, and
    // freshness comes from refresh()'s own loadGraph (watcher events,
    // post-mutation refreshes), so switching tabs never re-walks.
    const query = $filterStore.searchQuery;
    const revision = $filterStore.selectedBranch;
    if (!path) {
      lastGraphKey = null;
      return;
    }
    // Only `path:` filters need git to walk history; every other filter runs
    // client-side over the rows already loaded, so keystrokes do not re-walk
    // a large repository per character.
    const needsServer = queryNeedsServerFetch(query);
    const repoRevisionKey = `${path}\u241f${revision ?? ""}`;
    const key = needsServer ? `${repoRevisionKey}\u241f${query}` : repoRevisionKey;
    if (key === lastGraphKey) return;
    lastGraphKey = key;
    const handle = setTimeout(() => {
      void graphStore.loadGraph(path, query, revision);
    }, 200);
    return () => clearTimeout(handle);
  });

  $effect(() => {
    void syncWindowChrome(
      repoWindowTitle($repoStore.currentPath, $repoStore.currentBranch),
      conflictedCount,
    );
  });

  $effect(() => {
    $repoStore.currentPath;
    isRebaseModalOpen = false;
  });
</script>

{#snippet paneFailed(error: unknown, reset: () => void)}
  <!-- Minimal crash isolation: a broken pane never takes down the window. -->
  <div class="flex-1 flex flex-col items-center justify-center gap-2 p-4" title={formatError(error)}>
    <span class="text-xs text-textMuted font-sans">Pane failed to render</span>
    <button type="button" class="gp-btn" onclick={() => reset()}>Reset</button>
  </div>
{/snippet}

<div class="h-screen w-screen flex flex-col bg-background text-textPrimary overflow-hidden font-sans relative">
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

  {#if $repoStore.error}
    <div class="px-3 pt-1.5">
      <div class="gp-pop rounded-xl border border-rose-500/30 bg-rose-500/10 px-3 py-1.5 text-[11px] text-rose-300 flex items-center justify-between">
        <span class="truncate">{$repoStore.error}</span>
        <button type="button" class="shrink-0 px-1.5 hover:text-white" onclick={() => repoStore.setError(null)}>Dismiss</button>
      </div>
    </div>
  {/if}

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
          <main class="flex-1 flex flex-col min-w-0 bg-background gp-pane">
          {#key $repoStore.activeTab}
            <div class="gp-view flex-1 flex flex-col min-h-0">
              {#if $repoStore.activeTab === "history"}
                <div class="flex-1 flex flex-col min-h-0">
                  <CommitTable />
                  <CommitDetails />
                </div>
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
          {/key}
        </main>
        </svelte:boundary>
      </div>
    {/key}
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

  <!-- Modals -->
  <RebaseModal isOpen={isRebaseModalOpen} onClose={() => (isRebaseModalOpen = false)} />
  <CloneModal isOpen={isCloneModalOpen} onClose={() => (isCloneModalOpen = false)} />
  <SettingsModal isOpen={isSettingsModalOpen} onClose={() => (isSettingsModalOpen = false)} />
  <PromptModal />
  <CommandPalette />
  <Tooltip />
</div>
