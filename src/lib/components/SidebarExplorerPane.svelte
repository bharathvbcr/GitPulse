<script lang="ts">
  import { onDestroy } from "svelte";
  import { Columns2, FileCode, FileText, SquareTerminal } from "@lucide/svelte";
  import ExplorerRail from "./files/ExplorerRail.svelte";
  import { requestReveal } from "../files/revealRequests";
  import {
    resolveSidebarFileAction,
    sidebarNavStore,
    type SidebarFileOpenTarget,
  } from "../sidebar/layoutStore";
  import { currentRepoPath, repoStore } from "../stores/repoStore";
  import { toastStore } from "../stores/toastStore";
  import { terminalExplorerSidebar } from "../terminal/explorerSidebar";
  import { TERMINAL_SESSION_CANCELED, terminalLaunchRequests } from "../terminal/launchRequests";
  import { describeError } from "../ui/copy/describeFailure";
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
  let inflightOpen: { key: string; controller: AbortController } | null = null;

  onDestroy(() => {
    inflightOpen?.controller.abort();
    inflightOpen = null;
  });

  function setTarget(target: SidebarFileOpenTarget) {
    sidebarNavStore.setOpenTarget(target);
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
  ) {
    const targetRepo = effectiveRepoPath;
    if (!path || !targetRepo) return;
    const mods = lastClickMods;
    lastClickMods = { altKey: false, metaOrCtrlKey: false };
    const action = resolveSidebarFileAction({
      defaultTarget: openTarget,
      altKey: mods.altKey,
      metaOrCtrlKey: mods.metaOrCtrlKey,
    });

    if (action === "code") {
      requestReveal(path, line, column);
      repoStore.selectFilePath(path);
      repoStore.setActiveTab("code", "explorer");
      return;
    }

    const requestKey = `${targetRepo}\0${action}\0${path}\0${line ?? ""}\0${column ?? ""}`;
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
        { mode: action, focus: true },
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

<!-- Justified: the capture handler only records which modifier keys were held
     at pointer-down so the click handlers inside can read them. It performs no
     action and nothing here needs focus; every action is a real button or row. -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="flex-1 min-h-0 flex flex-col overflow-hidden"
  data-testid="sidebar-explorer-pane"
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
      onSelectFile={(path) => void handleOpenFile(path, null, null)}
      onPinFile={(path) => void handleOpenFile(path, null, null)}
      onSelectMatch={(path, line, col) => void handleOpenFile(path, line, col)}
    />
  </div>
</div>
