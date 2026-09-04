<script lang="ts" module>
  import { createRepoPanelCache } from "../panels/repoPanelCache";
  import { hasDirtyEditorTabs, type EditorTabState } from "../files/editorTabs";
  import type { FileSidePane } from "../files/filePaneLayout";
  import type { FileBlob } from "../files/types";
  import { recordEditorDrafts } from "../files/editorDraftRegistry";
  import {
    createLatestOwnerRegistry,
    editorFileSaveQueue,
    fileSaveKey,
    type LatestOwnerLease,
  } from "../files/serialSave";

  const tabCache = createRepoPanelCache<{
    tabs: EditorTabState;
    explorerOpen: boolean;
    dashboardOpen: boolean;
    preferredSidePane: FileSidePane;
  }>({
    // A cache bound may discard clean navigation history, never unsaved work.
    canEvict: ({ tabs }) => !hasDirtyEditorTabs(tabs),
  });
  // Both registries must outlive a rendered FileViewer: App remounts this
  // component when switching views while an accepted write may still settle.
  const fileSaveQueue = editorFileSaveQueue;
  const viewerOwners = createLatestOwnerRegistry<{
    completeSave(path: string, savedContent: string, tabs: EditorTabState): void;
  }>();
</script>

<script lang="ts">
  import { onMount, untrack } from "svelte";
  import { repoStore } from "../stores/repoStore";
  import { invoke } from "@tauri-apps/api/core";
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
  } from "lucide-svelte";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import { formatError } from "../ui/formatError";
  import { copyText } from "../desktop/clipboard";
  import { askConfirm } from "../stores/modalStore";
  import { openPath } from "@tauri-apps/plugin-opener";
  import FileTreePanel from "./files/FileTreePanel.svelte";
  import MediaViewer from "./files/MediaViewer.svelte";
  import LivePulseDashboard from "./files/LivePulseDashboard.svelte";
  import EmptyState from "./EmptyState.svelte";
  import LanguageLogo from "./LanguageLogo.svelte";
  import { joinWorktreePath } from "../files/fileTree";
  import { classifyFileChange, statusBadgeClass, statusBadgeLabel } from "../files/fileStatus";
  import { nextRovingIndex, type RovingKey } from "../dom/rovingFocus";
  import { resolveFilePaneLayout } from "../files/filePaneLayout";
  import {
    activateEditorTab,
    completeEditorSave,
    closeEditorTab,
    closeEditorTabs,
    dirtyEditorTabPaths,
    discardEditorDraft,
    editorDraft,
    emptyEditorTabs,
    isEditorTabDirty,
    openPinned,
    openPreview,
    pinEditorTab,
    updateEditorDraft,
    type EditorTabState as Tabs,
  } from "../files/editorTabs";


  let explorerOpen = $state(true);
  let dashboardOpen = $state(true);
  let tabState = $state<Tabs>(emptyEditorTabs());
  let fileViewRoot: HTMLDivElement | undefined = $state();
  let fileViewWidth = $state(0);
  let preferredSidePane = $state<FileSidePane>("explorer");
  let compactPane = $state<FileSidePane | null>(null);
  let revealRequest = $state<{ path: string; nonce: number }>({ path: "", nonce: 0 });

  let activeBlob = $state<FileBlob | null>(null);
  let isLoadingFile = $state(false);
  let fileError = $state<string | null>(null);
  let inflightGuard: AsyncGuard | null = null;
  let prevLoadKey = "";
  let hydratedRepo: string | null = null;
  let lastAppliedStoreFile = "";
  let viewerLease: LatestOwnerLease | null = null;

  let openTabs = $derived(tabState.tabs);
  let activeTabPath = $derived(tabState.active);
  let activeDraft = $derived(
    activeTabPath ? editorDraft(tabState, activeTabPath) : undefined,
  );
  let activeTabDirty = $derived(
    activeTabPath ? isEditorTabDirty(tabState, activeTabPath) : false,
  );
  let paneLayout = $derived(resolveFilePaneLayout({
    containerWidth: fileViewWidth,
    explorerRequested: explorerOpen,
    dashboardRequested: dashboardOpen,
    preferredPane: preferredSidePane,
    compactPane,
  }));

  let pathSegments = $derived.by(() => (activeTabPath ? activeTabPath.split("/") : []));

  let activeStatus = $derived.by(() => {
    if (!activeTabPath) return null;
    return $repoStore.statuses.find((s) => s.path === activeTabPath) ?? null;
  });

  let activeKind = $derived(classifyFileChange(activeStatus));

  function persistTabs(repo: string | null) {
    if (!repo) return;
    tabCache.set(repo, {
      tabs: {
        tabs: tabState.tabs,
        active: tabState.active,
        drafts: tabState.drafts,
      },
      explorerOpen,
      dashboardOpen,
      preferredSidePane,
    });
    recordEditorDrafts(repo, Object.keys(tabState.drafts));
  }

  async function loadFileContent(path: string) {
    const repo = $repoStore.currentPath;
    if (!repo || !path) {
      activeBlob = null;
      return;
    }
    inflightGuard?.cancel();
    const guard = createAsyncGuard();
    inflightGuard = guard;
    isLoadingFile = true;
    fileError = null;
    try {
      const blob = await invoke<FileBlob>("cmd_get_file_blob", {
        repoPath: repo,
        filePath: path,
        commitId: null,
      });
      if (!guard.isLive()) return;
      activeBlob = blob;
    } catch (err: unknown) {
      if (!guard.isLive()) return;
      fileError = formatError(err);
      activeBlob = null;
    } finally {
      if (guard.isLive()) isLoadingFile = false;
    }
  }

  function previewFile(path: string) {
    if (!path) return;
    compactPane = null;
    tabState = openPreview(tabState, path);
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

  async function confirmDiscardDrafts(candidatePaths: readonly string[]): Promise<boolean> {
    const repo = hydratedRepo;
    if (!repo) return false;
    // An accepted save owns the disk outcome. Let it settle before deciding
    // what remains dirty, so “Discard” can never race a queued write that
    // later persists the content the user explicitly chose to discard.
    const saveKeys = [...new Set(candidatePaths)].map((path) => fileSaveKey(repo, path));
    await fileSaveQueue.whenIdle(saveKeys);
    if (!isCurrentViewer(repo)) return false;
    const dirtyPaths = dirtyEditorTabPaths(tabState, candidatePaths);
    if (dirtyPaths.length === 0) return true;
    const names = dirtyPaths.length <= 3
      ? dirtyPaths.map((path) => `• ${path}`).join("\n")
      : `${dirtyPaths.length} files`;
    return askConfirm({
      title: "Discard Unsaved Edits?",
      message: `These editor drafts have not been saved:\n${names}`,
      confirmLabel: "Discard Unsaved Edits",
      cancelLabel: "Keep Editing",
    });
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
    if (!(await confirmDiscardDrafts([path]))) return;
    if (!isCurrentViewer(repo)) return;
    tabState = closeEditorTab(tabState, path);
    syncSelectedFilePath();
    persistTabs(repo);
    if (!tabState.active) {
      activeBlob = null;
      prevLoadKey = "";
    }
  }

  async function closeAllTabs() {
    const repo = hydratedRepo;
    if (!repo) return;
    const pathsAtRequest = tabState.tabs.map((tab) => tab.path);
    if (!(await confirmDiscardDrafts(pathsAtRequest))) return;
    if (!isCurrentViewer(repo)) return;
    tabState = closeEditorTabs(tabState, pathsAtRequest);
    syncSelectedFilePath();
    if (!tabState.active) {
      activeBlob = null;
      prevLoadKey = "";
    }
    persistTabs(repo);
  }

  async function closeOtherTabs() {
    const keepPath = tabState.active;
    if (!keepPath) return;
    const repo = hydratedRepo;
    if (!repo) return;
    const pathsToClose = tabState.tabs
      .filter((tab) => tab.path !== keepPath)
      .map((tab) => tab.path);
    if (!(await confirmDiscardDrafts(pathsToClose))) return;
    if (!isCurrentViewer(repo)) return;
    tabState = pinEditorTab(closeEditorTabs(tabState, pathsToClose), keepPath);
    syncSelectedFilePath();
    persistTabs(repo);
  }

  function handleDraftChange(path: string, newContent: string, sourceContent: string) {
    tabState = updateEditorDraft(tabState, path, newContent, sourceContent);
    persistTabs(hydratedRepo);
  }

  async function requestDiscardDraft(path: string): Promise<boolean> {
    const repo = hydratedRepo;
    if (!repo) return false;
    if (!(await confirmDiscardDrafts([path]))) return false;
    if (!isCurrentViewer(repo)) return false;
    tabState = discardEditorDraft(tabState, path);
    persistTabs(repo);
    return true;
  }

  function applyCompletedSaveToCurrentViewer(
    repo: string,
    path: string,
    savedContent: string,
    tabs: Tabs,
  ) {
    if (!isCurrentViewer(repo)) return;
    tabState = tabs;
    if (activeTabPath === path && activeBlob?.path === path) {
      activeBlob = { ...activeBlob, text: savedContent };
    }
  }

  function completeCachedEditorSave(repo: string, path: string, savedContent: string) {
    const cached = tabCache.get(repo);
    if (!cached) return;
    // The cache is synchronously updated on every keystroke. Completing
    // against this latest state, rather than the initiating instance's state,
    // preserves a draft typed while the backend write was in flight.
    const tabs = completeEditorSave(cached.tabs, path, savedContent);
    tabCache.set(repo, { ...cached, tabs });
    recordEditorDrafts(repo, Object.keys(tabs.drafts));
    // Dispatch through the registry rather than this initiating component:
    // after a view remount the newest owner receives the latest cached state.
    viewerOwners.current(repo)?.completeSave(path, savedContent, tabs);
  }

  async function handleFileSave(path: string, newContent: string) {
    const repo = hydratedRepo;
    if (!repo || !path) throw new Error("No active file to save");
    if ($repoStore.currentPath !== repo) {
      throw new Error("Repository changed before save; the editor draft was kept");
    }
    // Save requests can survive a view remount. The module-level queue binds
    // every accepted operation to its canonical repository and relative path,
    // and keeps completion/refresh order the same as disk-write order.
    return fileSaveQueue.run(fileSaveKey(repo, path), async () => {
      if ($repoStore.currentPath !== repo) {
        throw new Error("Repository changed before save; the editor draft was kept");
      }
      await invoke("cmd_write_file_content", {
        repoPath: repo,
        filePath: path,
        content: newContent,
      });
      completeCachedEditorSave(repo, path, newContent);
      if ($repoStore.currentPath === repo) await repoStore.refresh();
    });
  }

  function openInDefaultApp() {
    const repo = $repoStore.currentPath;
    if (!repo || !activeTabPath) return;
    const fullPath = joinWorktreePath(repo, activeTabPath);
    if (!fullPath) {
      repoStore.setError("Cannot open a path outside the repository");
      return;
    }
    void openPath(fullPath);
  }

  async function copyActivePath() {
    if (!activeTabPath) return;
    if (!(await copyText(activeTabPath))) {
      repoStore.setError("Could not copy path to clipboard");
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
  function inspectIn(lens: "diff" | "blame") {
    if (!activeTabPath) return;
    repoStore.selectFilePath(activeTabPath);
    if (lens === "diff") repoStore.setActiveTab("history", "diff");
    else repoStore.setViewSection("code", "blame");
  }

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
    const requested = pane === "explorer" ? explorerOpen : dashboardOpen;
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
        explorerOpen = false;
      } else {
        dashboardOpen = false;
      }
      persistTabs(hydratedRepo);
      return;
    }

    if (!requested) {
      if (pane === "explorer") explorerOpen = true;
      else dashboardOpen = true;
    }
    // Ignored when the pane fits beside the editor; used as a full-workspace
    // fallback when it does not.
    compactPane = pane;
    persistTabs(hydratedRepo);
  }

  /**
   * Arrow keys walk the tab strip, as `role="tab"` implies. Focus follows the
   * key rather than activating on arrival: moving across tabs should not load
   * six files on the way to the seventh.
   */
  function handleTabStripKeydown(e: KeyboardEvent) {
    const strip = e.currentTarget as HTMLElement;
    const tabs = [...strip.querySelectorAll<HTMLElement>('[role="tab"]')];
    const current = tabs.findIndex((tab) => tab.contains(e.target as Node));
    const next = nextRovingIndex(current, tabs.length, e.key as RovingKey);
    if (next === null) return;
    e.preventDefault();
    tabs[next]?.focus();
  }

  function handleWindowKeydown(e: KeyboardEvent) {
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "b" && !e.shiftKey) {
      e.preventDefault();
      toggleSidePane("explorer");
    } else if ((e.metaKey || e.ctrlKey) && e.shiftKey && e.key.toLowerCase() === "d") {
      e.preventDefault();
      toggleSidePane("dashboard");
    }
  }

  $effect(() => {
    const repo = $repoStore.currentPath;
    if (repo === hydratedRepo) return;
    if (hydratedRepo) untrack(() => persistTabs(hydratedRepo));
    viewerLease?.release();
    hydratedRepo = repo;
    viewerLease = repo
      ? viewerOwners.claim(repo, {
          completeSave: (path, savedContent, tabs) =>
            applyCompletedSaveToCurrentViewer(repo, path, savedContent, tabs),
        })
      : null;
    compactPane = null;
    lastAppliedStoreFile = "";
    if (!repo) {
      tabState = emptyEditorTabs();
      activeBlob = null;
      prevLoadKey = "";
      return;
    }
    const cached = tabCache.get(repo);
    if (cached) {
      tabState = cached.tabs;
      explorerOpen = cached.explorerOpen;
      dashboardOpen = cached.dashboardOpen;
      preferredSidePane = cached.preferredSidePane ?? "explorer";
    } else {
      tabState = emptyEditorTabs();
      explorerOpen = true;
      dashboardOpen = true;
      preferredSidePane = "explorer";
    }
    prevLoadKey = "";
  });

  $effect(() => {
    const storeSelected = $repoStore.selectedFilePath;
    if (!storeSelected) {
      lastAppliedStoreFile = "";
      return;
    }
    if (storeSelected === lastAppliedStoreFile) return;
    lastAppliedStoreFile = storeSelected;
    if (!tabState.tabs.some((t) => t.path === storeSelected)) {
      tabState = openPreview(tabState, storeSelected);
      persistTabs(hydratedRepo);
    } else {
      tabState = activateEditorTab(tabState, storeSelected);
    }
  });

  $effect(() => {
    const repo = $repoStore.currentPath;
    const path = tabState.active;
    const code = path
      ? ($repoStore.statuses.find((s) => s.path === path)?.status_code ?? "")
      : "";
    const key = `${repo ?? ""}\u0000${path ?? ""}\u0000${code}`;
    if (key === prevLoadKey) return;
    prevLoadKey = key;
    if (!repo || !path) {
      if (!path) activeBlob = null;
      return;
    }
    void loadFileContent(path);
  });

  $effect(() => {
    return () => {
      viewerLease?.release();
      inflightGuard?.cancel();
    };
  });

  $effect(() => {
    // A compact selection is a temporary replacement surface. Once the view
    // grows enough to restore an editor split, do not resurrect it on a later
    // resize unless the user requests it again.
    if (paneLayout.mode !== "compact" && compactPane !== null) compactPane = null;
  });

  onMount(() => {
    window.addEventListener("keydown", handleWindowKeydown);
    const observer = new ResizeObserver((entries) => {
      const measured = entries.find((entry) => entry.target === fileViewRoot)?.contentRect.width;
      fileViewWidth = typeof measured === "number" && Number.isFinite(measured) && measured > 0
        ? measured
        : 0;
    });
    if (fileViewRoot) {
      fileViewWidth = fileViewRoot.clientWidth || 0;
      observer.observe(fileViewRoot);
    }
    return () => {
      window.removeEventListener("keydown", handleWindowKeydown);
      observer.disconnect();
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
  <div class="flex items-center px-1.5 bg-surface/90 border-b border-border/70 shrink-0 h-9 gap-1.5">
    <button
      type="button"
      onclick={() => toggleSidePane("explorer")}
      aria-label="{paneLayout.explorerVisible ? 'Hide' : 'Show'} Explorer"
      aria-pressed={paneLayout.explorerVisible}
      title="{paneLayout.explorerVisible ? 'Hide' : 'Show'} Explorer (⌘B)"
      class="gp-icon-btn !p-1.5 shrink-0 {paneLayout.explorerVisible ? 'text-accent bg-accent/15' : 'text-textMuted hover:text-textPrimary'}"
    >
      {#if paneLayout.explorerVisible}
        <PanelLeftClose size={14} />
      {:else}
        <PanelLeftOpen size={14} />
      {/if}
    </button>

    <div class="h-4 w-px bg-border/70 shrink-0" aria-hidden="true"></div>

    {#if openTabs.length === 0}
      <span class="text-xs text-textMuted/60 italic pl-1 flex-1">No open files</span>
    {:else}
      <!-- The strip listens for the arrow keys but is not itself a tab stop:
           the roving tabindex on the tabs is what the user lands on. -->
      <div
        class="flex items-stretch gap-1 min-w-0 flex-1 h-full overflow-x-auto gp-header-scroll py-1"
        role="tablist"
        aria-label="Open files"
        tabindex="-1"
        onkeydown={handleTabStripKeydown}
      >
        {#each openTabs as tab (tab.path)}
          {@const isActive = tab.path === activeTabPath}
          {@const tabDirty = isEditorTabDirty(tabState, tab.path)}
          {@const tabKind = classifyFileChange($repoStore.statuses.find((s) => s.path === tab.path))}
          <div
            role="tab"
            data-editor-tab={tab.path}
            tabindex={isActive ? 0 : -1}
            aria-selected={isActive}
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
                class="w-1.5 h-1.5 rounded-full bg-amber-400 not-italic shrink-0"
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
              class="opacity-0 group-hover:opacity-100 p-0.5 rounded hover:bg-surfaceHover text-textMuted hover:text-rose-400 transition-opacity shrink-0"
              aria-label={`Close ${tab.name}`}
              title="Close tab"
            >
              <X size={11} />
            </button>
          </div>
        {/each}
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
          title="Close Other Tabs"
          class="gp-icon-btn !p-1 text-textMuted hover:text-textPrimary"
        >
          <Columns2 size={13} />
        </button>
        <button
          type="button"
          onclick={closeAllTabs}
          aria-label="Close all tabs"
          title="Close All Tabs"
          class="gp-icon-btn !p-1 text-textMuted hover:text-rose-400"
        >
          <XCircle size={13} />
        </button>
        <div class="h-4 w-px bg-border/70 shrink-0" aria-hidden="true"></div>
      {/if}

      <button
        type="button"
        onclick={() => toggleSidePane("dashboard")}
        aria-pressed={paneLayout.dashboardVisible}
        aria-label="{paneLayout.dashboardVisible ? 'Hide' : 'Show'} Live Pulse dashboard"
        title="{paneLayout.dashboardVisible ? 'Hide' : 'Show'} Live Pulse Dashboard (⌘⇧D)"
        class="gp-btn !py-0.5 !px-2 flex items-center gap-1 text-[11px] {paneLayout.dashboardVisible
          ? 'border-accent/60 bg-accent/15 text-accent font-semibold'
          : ''}"
      >
        <Activity size={12} />
        <span>Live Pulse</span>
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
    <div class="flex items-center justify-between gap-2 px-2.5 py-1 bg-surface/40 border-b border-border/50 shrink-0 text-xs">
      <nav
        class="flex items-center gap-0.5 text-[11px] min-w-0 flex-1 text-textMuted font-mono"
        aria-label="Path to the open file"
      >
        {#each pathSegments as seg, idx}
          {#if idx > 0}
            <ChevronRight size={10} class="shrink-0 text-textMuted/40" aria-hidden="true" />
          {/if}
          {#if idx === pathSegments.length - 1}
            <span class="text-textPrimary font-semibold truncate select-text">{seg}</span>
          {:else}
            {@const dir = pathSegments.slice(0, idx + 1).join("/")}
            <button
              type="button"
              onclick={() => revealDirectory(dir)}
              title="Reveal {dir}/ in the Explorer"
              class="shrink-0 max-w-[14ch] truncate rounded px-1 py-0.5 hover:bg-surfaceHover hover:text-textPrimary transition-colors"
            >{seg}</button>
          {/if}
        {/each}

        {#if activeKind !== "clean"}
          <span class="ml-1.5 shrink-0 px-1.5 py-0.2 text-[9px] font-bold rounded {statusBadgeClass(activeKind)}">
            {statusBadgeLabel(activeKind, true)}
          </span>
        {/if}
        {#if activeTabDirty}
          <span class="ml-1.5 shrink-0 text-[10px] font-semibold text-amber-400">Unsaved</span>
        {/if}
      </nav>

      <div class="flex items-center gap-1 shrink-0">
        <button
          type="button"
          onclick={copyActivePath}
          class="gp-icon-btn !p-1 text-textMuted hover:text-textPrimary"
          aria-label="Copy relative path"
          title="Copy Relative Path"
        >
          <Copy size={12} />
        </button>

        <button
          type="button"
          onclick={openInDefaultApp}
          class="gp-icon-btn !p-1 text-textMuted hover:text-textPrimary"
          aria-label="Open in default application"
          title="Open in Default Application"
        >
          <ExternalLink size={12} />
        </button>

        <div class="h-4 w-px bg-border/70 shrink-0" aria-hidden="true"></div>

        <button
          type="button"
          onclick={() => inspectIn("diff")}
          class="gp-btn !py-0.5 !px-1.5 text-[10px] flex items-center gap-1 text-cyan-400"
          title="View in Diff Tab"
        >
          <Layers size={11} />
          <span>Diff</span>
        </button>

        <button
          type="button"
          onclick={() => inspectIn("blame")}
          class="gp-btn !py-0.5 !px-1.5 text-[10px] flex items-center gap-1 text-purple-400"
          title="View Git Blame"
        >
          <GitCommit size={11} />
          <span>Blame</span>
        </button>
      </div>
    </div>
  {/if}

  <div class="flex-1 flex min-h-0 overflow-hidden relative">
    {#if paneLayout.explorerVisible}
      <div class="h-full overflow-hidden {paneLayout.editorVisible ? 'w-72 shrink-0' : 'flex-1 min-w-0'}">
        <FileTreePanel
          selectedFile={activeTabPath}
          {revealRequest}
          onSelectFile={(path) => previewFile(path)}
          onPinFile={(path) => pinFile(path)}
        />
      </div>
    {/if}

    {#if paneLayout.editorVisible}
      <div class="flex-1 flex flex-col min-w-0 h-full bg-background overflow-hidden relative">
        {#if isLoadingFile}
          <div class="flex-1 flex flex-col items-center justify-center text-textMuted text-xs gap-2">
            <span>Loading file contents...</span>
          </div>
        {:else if fileError}
          <div class="flex-1 flex flex-col items-center justify-center text-rose-400 text-xs p-6 text-center">
            <ShieldAlert size={28} class="mb-2 text-rose-400" />
            <span class="font-bold text-sm text-rose-300 mb-1">Failed to read file</span>
            <span class="max-w-md font-mono text-[11px]">{fileError}</span>
          </div>
        {:else if activeBlob && activeTabPath}
          <MediaViewer
            filePath={activeTabPath}
            blob={activeBlob}
            draftContent={activeDraft?.content ?? null}
            dirty={activeTabDirty}
            onSave={(newContent) => handleFileSave(activeTabPath, newContent)}
            onDraftChange={(newContent, sourceContent) =>
              handleDraftChange(activeTabPath, newContent, sourceContent)}
            onRequestDiscard={() => requestDiscardDraft(activeTabPath)}
          />
        {:else}
          <EmptyState
            icon={FileText}
            title="No file opened"
            hint="Single-click previews a file; double-click pins it. Filter the tree with globs, /regex/, or ~fuzzy."
          />
        {/if}
      </div>
    {/if}

    {#if paneLayout.dashboardVisible}
      <div class="h-full overflow-hidden {paneLayout.editorVisible ? 'w-80 shrink-0' : 'flex-1 min-w-0'}">
        <LivePulseDashboard selectedFile={activeTabPath} onSelectFile={(path) => previewFile(path)} />
      </div>
    {/if}
  </div>
</div>
