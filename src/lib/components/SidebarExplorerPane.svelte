<script lang="ts">
  import { onDestroy } from "svelte";
  import { Columns2, FileCode, FileText, Maximize2, SquareTerminal, Sparkles } from "@lucide/svelte";
  import ExplorerRail from "./files/ExplorerRail.svelte";
  import { opensExplorerPage } from "../files/explorerPageGesture";
  import { requestPin } from "../files/pinRequests";
  import { requestReveal, treeRevealRequest } from "../files/revealRequests";
  import {
    resolveFileOpen,
    sidebarNavStore,
    type SidebarFileOpenTarget,
    type SidebarLastOpen,
  } from "../sidebar/layoutStore";
  import { currentRepoPath, repoStore } from "../stores/repoStore";
  import { toastStore } from "../stores/toastStore";
  import { terminalExplorerSidebar } from "../terminal/explorerSidebar";
  import { TERMINAL_SESSION_CANCELED, terminalLaunchRequests } from "../terminal/launchRequests";
  import { describeError } from "../ui/copy/describeFailure";
  import { lastSurface } from "../ui/lastSurface";
  import { platformChord } from "../ui/platformCopy";
  import { hostPlatform } from "../stores/platformStore";

  let {
    repoPath = null,
  }: {
    repoPath?: string | null;
  } = $props();

  const effectiveRepoPath = $derived(repoPath ?? $currentRepoPath);
  const codeViewClick = $derived(platformChord("⌘/Ctrl+Click", "Ctrl+Click", $hostPlatform.os));
  const openTarget = $derived($sidebarNavStore.openTarget);
  const dockedInTerminal = $derived($terminalExplorerSidebar.open && $repoStore.terminalOpen);
  let lastClickMods = { altKey: false, metaOrCtrlKey: false };
  /** Where the last click on a file sent it; a double-click follows it (`resolveFileOpen`). */
  let lastOpen: SidebarLastOpen | null = null;
  let inflightOpen: { key: string; controller: AbortController } | null = null;

  onDestroy(() => {
    inflightOpen?.controller.abort();
    inflightOpen = null;
  });

  function setTarget(target: SidebarFileOpenTarget) {
    sidebarNavStore.setOpenTarget(target);
  }

  /** The full Explorer page, through the page owner every Explorer entry uses. */
  function openExplorerPage() {
    repoStore.setActiveTab("code", "explorer");
  }

  function dockExplorerInTerminal() {
    if (!effectiveRepoPath) return;
    if (dockedInTerminal) {
      terminalExplorerSidebar.setOpen(false);
      return;
    }
    terminalExplorerSidebar.setOpen(true);
    repoStore.setTerminalOpen(true);
  }

  async function handleOpenFile(
    path: string,
    line: number | null = null,
    column: number | null = null,
    pin = false,
  ) {
    const targetRepo = effectiveRepoPath;
    if (!path || !targetRepo) return;
    const mods = lastClickMods;
    lastClickMods = { altKey: false, metaOrCtrlKey: false };
    const now = Date.now();
    const action = resolveFileOpen({
      path,
      pin,
      lastOpen,
      now,
      defaultTarget: openTarget,
      // A click here is on the sidebar, which `lastSurface` ignores, so this
      // is where the user was before it; the dock must also be showing.
      terminalActive: $lastSurface === "terminal" && $repoStore.terminalOpen,
      altKey: mods.altKey,
      metaOrCtrlKey: mods.metaOrCtrlKey,
    });
    lastOpen = { path, action, at: now };

    if (action === "code") {
      // Double-click keeps the file's tab: the Explorer page owns the tabs, so
      // ask it to pin rather than leave a preview the next click replaces.
      if (pin) requestPin(targetRepo, path);
      requestReveal(path, line, column);
      repoStore.selectFilePath(path);
      repoStore.setActiveTab("code", "explorer");
      return;
    }

    // A double-click's keep is its own request, never deduped into its click's.
    const requestKey = `${targetRepo}\0${action}\0${path}\0${line ?? ""}\0${column ?? ""}\0${pin ? "keep" : ""}`;
    if (inflightOpen?.key === requestKey) return;
    inflightOpen?.controller.abort();
    const controller = new AbortController();
    const token = { key: requestKey, controller };
    inflightOpen = token;
    try {
      const requested = terminalLaunchRequests.requestFile(
        targetRepo,
        path,
        line,
        column,
        controller.signal,
        { mode: action, focus: true, keep: pin },
      );
      repoStore.setTerminalOpen(true);
      await requested;
    } catch (error) {
      if (error instanceof Error && error.message === TERMINAL_SESSION_CANCELED) return;
      toastStore.error(describeError(error, "terminal"));
    } finally {
      if (inflightOpen === token) inflightOpen = null;
    }
  }
</script>

<!-- The double-click is a mouse shortcut for the "Open the Explorer page"
     button in the strip below, which is the keyboard's way to the same place;
     `opensExplorerPage` leaves rows, fields and controls their own. -->
<!-- Justified: the capture-phase pointerdown only records which modifier keys
     a click on a file row carried (Alt, ⌘/Ctrl); it handles nothing itself,
     and every row and button inside keeps its own keyboard handling. -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="flex-1 min-h-0 flex flex-col overflow-hidden"
  data-testid="sidebar-explorer-pane"
  ondblclick={(event) => {
    if (opensExplorerPage(event)) openExplorerPage();
  }}
  onpointerdowncapture={(event) => {
    lastClickMods = {
      altKey: event.altKey,
      metaOrCtrlKey: event.metaKey || event.ctrlKey,
    };
  }}
>
  <div class="px-3 pt-1.5 pb-1 shrink-0 flex items-center justify-between gap-1.5 select-none">
    <div
      class="inline-flex flex-1 min-w-0 items-center gap-0.5 p-0.5 rounded-lg bg-surface border border-border/65"
      role="group"
      aria-label="Open clicked files in"
    >
      <button
        type="button"
        data-testid="sidebar-explorer-target-auto"
        aria-pressed={openTarget === "auto"}
        class="inline-flex flex-1 justify-center items-center gap-1 px-1.5 py-0.5 rounded text-micro transition-colors {openTarget === 'auto'
          ? 'bg-accent/15 text-accent font-medium'
          : 'text-textMuted hover:text-textPrimary'}"
        title="Open clicked files beside the terminal when it was the last thing you used, otherwise in Code view"
        onclick={() => setTarget("auto")}
      >
        <Sparkles size={11} class="shrink-0" />
        <span>Auto</span>
      </button>
      <button
        type="button"
        data-testid="sidebar-explorer-target-code"
        aria-pressed={openTarget === "code"}
        class="inline-flex flex-1 justify-center items-center gap-1 px-1.5 py-0.5 rounded text-micro transition-colors {openTarget === 'code'
          ? 'bg-accent/15 text-accent font-medium'
          : 'text-textMuted hover:text-textPrimary'}"
        title="Open clicked files in Code view · Alt+Click opens in terminal split"
        onclick={() => setTarget("code")}
      >
        <FileCode size={11} class="shrink-0" />
        <span>Code</span>
      </button>
      <button
        type="button"
        data-testid="sidebar-explorer-target-tab"
        aria-pressed={openTarget === "tab"}
        class="inline-flex flex-1 justify-center items-center gap-1 px-1.5 py-0.5 rounded text-micro transition-colors {openTarget === 'tab'
          ? 'bg-accent/15 text-accent font-medium'
          : 'text-textMuted hover:text-textPrimary'}"
        title={`Open clicked files in a standalone terminal Files tab · ${codeViewClick} opens in Code view`}
        onclick={() => setTarget("tab")}
      >
        <FileText size={11} class="shrink-0" />
        <span>Tab</span>
      </button>
      <button
        type="button"
        data-testid="sidebar-explorer-target-split"
        aria-pressed={openTarget === "split"}
        class="inline-flex flex-1 justify-center items-center gap-1 px-1.5 py-0.5 rounded text-micro transition-colors {openTarget === 'split'
          ? 'bg-accent/15 text-accent font-medium'
          : 'text-textMuted hover:text-textPrimary'}"
        title={`Open clicked files side-by-side in terminal split · ${codeViewClick} opens in Code view`}
        onclick={() => setTarget("split")}
      >
        <Columns2 size={11} class="shrink-0" />
        <span>Split</span>
      </button>
    </div>
    <button
      type="button"
      class="gp-icon-btn shrink-0 hover:text-accent"
      data-size="sm"
      data-testid="sidebar-explorer-open-page"
      aria-label="Open the Explorer page"
      title="Open the Explorer page · or double-click empty space in this pane"
      onclick={openExplorerPage}
    >
      <Maximize2 size={13} />
    </button>
    <button
      type="button"
      class="gp-icon-btn shrink-0 {dockedInTerminal ? 'text-accent bg-accent/15' : 'hover:text-accent'}"
      data-size="sm"
      data-testid="sidebar-explorer-dock-terminal"
      aria-label={dockedInTerminal ? "Hide Explorer in terminal sidebar" : "Show Explorer in terminal sidebar"}
      aria-pressed={dockedInTerminal}
      disabled={!effectiveRepoPath}
      title={dockedInTerminal ? "Hide docked Explorer sidebar inside the terminal" : "Show docked Explorer sidebar inside the terminal"}
      onclick={dockExplorerInTerminal}
    >
      <SquareTerminal size={13} />
    </button>
  </div>

  <div class="flex-1 min-h-0 overflow-hidden">
    <ExplorerRail
      repoPath={effectiveRepoPath}
      selectedFile={$repoStore.selectedFilePath}
      revealRequest={$treeRevealRequest}
      onSelectFile={(path) => void handleOpenFile(path, null, null)}
      onPinFile={(path) => void handleOpenFile(path, null, null, true)}
      onSelectMatch={(path, line, col) => void handleOpenFile(path, line, col)}
    />
  </div>
</div>
