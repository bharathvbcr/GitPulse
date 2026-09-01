<script lang="ts" module>
  import { createRepoPanelCache } from "../panels/repoPanelCache";
  import type { EditorTabState } from "../files/editorTabs";
  import type { FileBlob } from "../files/types";

  const tabCache = createRepoPanelCache<{
    tabs: EditorTabState;
    explorerOpen: boolean;
    dashboardOpen: boolean;
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
    Folder,
    Layers,
    GitCommit,
    ShieldAlert,
    Activity,
  } from "lucide-svelte";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import { formatError } from "../ui/formatError";
  import { copyText } from "../desktop/clipboard";
  import { openPath } from "@tauri-apps/plugin-opener";
  import FileTreePanel from "./files/FileTreePanel.svelte";
  import MediaViewer from "./files/MediaViewer.svelte";
  import LivePulseDashboard from "./files/LivePulseDashboard.svelte";
  import EmptyState from "./EmptyState.svelte";
  import { getFileIconMeta } from "../files/fileIcons";
  import { joinWorktreePath } from "../files/fileTree";
  import { classifyFileChange } from "../files/fileStatus";
  import {
    activateEditorTab,
    closeAllEditorTabs,
    closeEditorTab,
    closeOtherEditorTabs,
    emptyEditorTabs,
    openPinned,
    openPreview,
    type EditorTabState as Tabs,
  } from "../files/editorTabs";


  let explorerOpen = $state(true);
  let dashboardOpen = $state(true);
  let tabState = $state<Tabs>(emptyEditorTabs());

  let activeBlob = $state<FileBlob | null>(null);
  let isLoadingFile = $state(false);
  let fileError = $state<string | null>(null);
  let inflightGuard: AsyncGuard | null = null;
  let prevLoadKey = "";
  let hydratedRepo: string | null = null;
  let lastAppliedStoreFile = "";

  let openTabs = $derived(tabState.tabs);
  let activeTabPath = $derived(tabState.active);

  let pathSegments = $derived.by(() => (activeTabPath ? activeTabPath.split("/") : []));

  let activeStatus = $derived.by(() => {
    if (!activeTabPath) return null;
    return $repoStore.statuses.find((s) => s.path === activeTabPath) ?? null;
  });

  let activeKind = $derived(classifyFileChange(activeStatus));

  function persistTabs(repo: string | null) {
    if (!repo) return;
    tabCache.set(repo, {
      tabs: { tabs: tabState.tabs, active: tabState.active },
      explorerOpen,
      dashboardOpen,
    });
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
    tabState = openPreview(tabState, path);
    repoStore.selectFilePath(path);
    persistTabs($repoStore.currentPath);
  }

  function pinFile(path: string) {
    if (!path) return;
    tabState = openPinned(tabState, path);
    repoStore.selectFilePath(path);
    persistTabs($repoStore.currentPath);
  }

  function activateTab(path: string) {
    if (!path) return;
    tabState = activateEditorTab(tabState, path);
    repoStore.selectFilePath(path);
    persistTabs($repoStore.currentPath);
  }

  function closeTab(path: string, event?: MouseEvent) {
    event?.stopPropagation();
    tabState = closeEditorTab(tabState, path);
    persistTabs($repoStore.currentPath);
    if (!tabState.active) {
      activeBlob = null;
      prevLoadKey = "";
    }
  }

  function closeAllTabs() {
    tabState = closeAllEditorTabs();
    activeBlob = null;
    prevLoadKey = "";
    persistTabs($repoStore.currentPath);
  }

  function closeOtherTabs() {
    if (!tabState.active) return;
    tabState = closeOtherEditorTabs(tabState, tabState.active);
    persistTabs($repoStore.currentPath);
  }

  async function handleFileSave(newContent: string) {
    const repo = $repoStore.currentPath;
    const current = activeTabPath;
    if (!repo || !current) return;
    await invoke("cmd_write_file_content", {
      repoPath: repo,
      filePath: current,
      content: newContent,
    });
    if (activeBlob) {
      activeBlob = { ...activeBlob, text: newContent };
    }
    await repoStore.refresh();
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

  function copyActivePath() {
    if (activeTabPath) void copyText(activeTabPath);
  }

  function inspectIn(tab: "diff" | "blame") {
    if (!activeTabPath) return;
    repoStore.selectFilePath(activeTabPath);
    repoStore.setActiveTab(tab);
  }

  function handleWindowKeydown(e: KeyboardEvent) {
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "b" && !e.shiftKey) {
      e.preventDefault();
      explorerOpen = !explorerOpen;
      persistTabs($repoStore.currentPath);
    } else if ((e.metaKey || e.ctrlKey) && e.shiftKey && e.key.toLowerCase() === "d") {
      e.preventDefault();
      dashboardOpen = !dashboardOpen;
      persistTabs($repoStore.currentPath);
    }
  }

  $effect(() => {
    const repo = $repoStore.currentPath;
    if (repo === hydratedRepo) return;
    if (hydratedRepo) untrack(() => persistTabs(hydratedRepo));
    hydratedRepo = repo;
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
    } else {
      tabState = emptyEditorTabs();
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
      persistTabs($repoStore.currentPath);
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
    return () => inflightGuard?.cancel();
  });

  onMount(() => {
    window.addEventListener("keydown", handleWindowKeydown);
    return () => window.removeEventListener("keydown", handleWindowKeydown);
  });
</script>

<div class="flex-1 flex flex-col min-h-0 bg-background overflow-hidden relative select-none">
  <div class="flex items-center justify-between px-2 bg-surface/90 border-b border-border/70 shrink-0 h-9 gap-2">
    <div class="flex items-center gap-1 min-w-0 flex-1 h-full overflow-x-auto gp-header-scroll" role="tablist" aria-label="Open files">
      <button
        type="button"
        onclick={() => {
          explorerOpen = !explorerOpen;
          persistTabs($repoStore.currentPath);
        }}
        title="{explorerOpen ? 'Hide' : 'Show'} Explorer (⌘B)"
        class="gp-icon-btn !p-1.5 shrink-0 text-textMuted hover:text-textPrimary"
      >
        {#if explorerOpen}
          <PanelLeftClose size={14} />
        {:else}
          <PanelLeftOpen size={14} />
        {/if}
      </button>

      <div class="h-4 w-px bg-border/70 mx-1 shrink-0"></div>

      {#if openTabs.length === 0}
        <span class="text-xs text-textMuted/60 italic pl-1">No open files</span>
      {:else}
        {#each openTabs as tab (tab.path)}
          {@const isActive = tab.path === activeTabPath}
          {@const iconMeta = getFileIconMeta(tab.path)}
          {@const tabKind = classifyFileChange($repoStore.statuses.find((s) => s.path === tab.path))}
          <div
            role="tab"
            tabindex="0"
            aria-selected={isActive}
            onclick={() => activateTab(tab.path)}
            ondblclick={() => pinFile(tab.path)}
            onkeydown={(e) => {
              if (e.key === "Enter") activateTab(tab.path);
            }}
            class="h-full px-2.5 flex items-center gap-1.5 border-r border-border/60 text-xs transition-colors shrink-0 max-w-[200px] group cursor-pointer {isActive
              ? 'bg-background text-textPrimary font-semibold border-b-2 border-b-accent'
              : 'text-textMuted hover:bg-surfaceHover hover:text-textPrimary'} {tab.preview ? 'italic' : ''}"
            title={tab.preview ? `${tab.path} (preview — double-click to pin)` : tab.path}
          >
            <span class="text-[9px] font-mono font-bold not-italic {iconMeta.colorClass}">
              {iconMeta.badgeLabel}
            </span>
            <span class="truncate">{tab.name}</span>
            {#if tabKind !== "clean"}
              <span class="w-1.5 h-1.5 rounded-full bg-accent not-italic"></span>
            {/if}
            <button
              type="button"
              onclick={(e) => closeTab(tab.path, e)}
              class="opacity-0 group-hover:opacity-100 p-0.5 rounded hover:bg-surface text-textMuted hover:text-rose-400 transition-opacity"
              title="Close tab"
            >
              <X size={11} />
            </button>
          </div>
        {/each}
      {/if}
    </div>

    <div class="flex items-center gap-1 shrink-0">
      {#if openTabs.length > 1}
        <button
          type="button"
          onclick={closeOtherTabs}
          title="Close Other Tabs"
          class="px-2 py-0.5 text-[10px] text-textMuted hover:text-textPrimary font-mono rounded hover:bg-surface transition-colors"
        >
          Close Others
        </button>
        <button
          type="button"
          onclick={closeAllTabs}
          title="Close All Tabs"
          class="px-2 py-0.5 text-[10px] text-textMuted hover:text-rose-400 font-mono rounded hover:bg-surface transition-colors"
        >
          Close All
        </button>
      {/if}

      <button
        type="button"
        onclick={() => {
          dashboardOpen = !dashboardOpen;
          persistTabs($repoStore.currentPath);
        }}
        title="{dashboardOpen ? 'Hide' : 'Show'} Live Pulse Dashboard (⌘⇧D)"
        class="gp-btn !py-0.5 !px-2 flex items-center gap-1 text-[11px] {dashboardOpen
          ? 'border-accent/60 bg-accent/15 text-accent font-semibold'
          : ''}"
      >
        <Activity size={12} />
        <span>Live Pulse</span>
      </button>
    </div>
  </div>

  {#if activeTabPath}
    <div class="flex items-center justify-between px-3 py-1 bg-surface/40 border-b border-border/50 shrink-0 text-xs select-none">
      <div class="flex items-center gap-1 text-[11px] min-w-0 flex-1 truncate text-textMuted font-mono">
        <Folder size={12} class="shrink-0 text-amber-400" />
        {#each pathSegments as seg, idx}
          {#if idx > 0}
            <ChevronRight size={10} class="shrink-0 text-textMuted/40" />
          {/if}
          <span class="{idx === pathSegments.length - 1 ? 'text-textPrimary font-semibold' : 'text-textMuted'} truncate">
            {seg}
          </span>
        {/each}

        {#if activeKind !== "clean"}
          <span
            class="ml-2 px-1.5 py-0.2 text-[9px] font-bold rounded {activeKind === 'staged'
              ? 'bg-emerald-500/20 text-emerald-300'
              : activeKind === 'conflict'
                ? 'bg-rose-500/20 text-rose-300'
                : activeKind === 'untracked'
                  ? 'bg-cyan-500/20 text-cyan-300'
                  : 'bg-amber-500/20 text-amber-300'}"
          >
            {activeKind === "staged"
              ? "STAGED"
              : activeKind === "conflict"
                ? "CONFLICT"
                : activeKind === "untracked"
                  ? "UNTRACKED"
                  : "MODIFIED"}
          </span>
        {/if}
      </div>

      <div class="flex items-center gap-1.5 shrink-0">
        <button
          type="button"
          onclick={copyActivePath}
          class="gp-icon-btn !p-1 text-textMuted hover:text-textPrimary"
          title="Copy Relative Path"
        >
          <Copy size={12} />
        </button>

        <button
          type="button"
          onclick={openInDefaultApp}
          class="gp-icon-btn !p-1 text-textMuted hover:text-textPrimary"
          title="Open in Default Application"
        >
          <ExternalLink size={12} />
        </button>

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
    {#if explorerOpen}
      <div class="w-72 shrink-0 h-full overflow-hidden">
        <FileTreePanel
          selectedFile={activeTabPath}
          onSelectFile={(path) => previewFile(path)}
          onPinFile={(path) => pinFile(path)}
        />
      </div>
    {/if}

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
        <MediaViewer filePath={activeTabPath} blob={activeBlob} onSave={handleFileSave} />
      {:else}
        <EmptyState
          icon={FileText}
          title="No file opened"
          hint="Single-click previews a file; double-click pins it. Filter the tree with globs, /regex/, or ~fuzzy."
        />
      {/if}
    </div>

    {#if dashboardOpen}
      <div class="w-80 shrink-0 h-full overflow-hidden">
        <LivePulseDashboard selectedFile={activeTabPath} onSelectFile={(path) => previewFile(path)} />
      </div>
    {/if}
  </div>
</div>
