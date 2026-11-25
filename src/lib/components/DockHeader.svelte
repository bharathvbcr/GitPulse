<script lang="ts">
  /**
   * The dock's one calm header row: a section switcher, where this dock is
   * (repository, checkout, branch), the single most pressing warning, and a
   * few window controls. Everything else lives behind the ⋯ menu.
   *
   * Presentational: it owns no stores and no session state. The panel passes
   * what to draw and receives the reader's choices through callbacks; the one
   * piece of state it keeps is whether its own menu is open (bindable, so the
   * panel can close it with the other chrome).
   */
  import { tick } from "svelte";
  import {
    Bot,
    ChevronDown,
    ChevronsUpDown,
    FolderTree,
    ListChecks,
    Maximize2,
    Minimize2,
    Pin,
    Check,
    Ellipsis,
    Loader2,
    Search,
    Settings2,
    SquareTerminal,
    Trash2,
    Layers,
  } from "@lucide/svelte";
  import LauncherIcon from "./LauncherIcon.svelte";
  import { portal } from "../dom/portal";
  import { LAYERS } from "../ui/layers";
  import { popover, popoverExit, restoreFocusTo } from "../ui/popover";
  import { platformChord, shortcutTextLabel } from "../ui/platformCopy";
  import { DOCK_SECTIONS, dockSectionChordLabel, type DockSection, type DockStatus } from "../terminal/dockSections";
  import type { TabKind } from "../terminal/tabs";
  import type { HostOS } from "../platform";

  interface ActiveSessionSummary {
    launcher: TabKind;
    primary: string;
    secondary?: string | null;
    fullTitle: string;
    /** The agent's OSC status badge, when it reports one. */
    badge?: { label: string; cls: string } | null;
    launcherLabel: string;
  }

  interface Props {
    section: DockSection;
    onSection: (next: DockSection) => void;
    os: HostOS;
    /** Agents here that want the reader: drawn as a dot on the Agents button. */
    agentAttention?: number;
    /** Live running agents across repositories: shown on Agents when no agent needs attention. */
    agentsRunning?: number;
    repoName: string | null;
    repoPath: string | null;
    /** Tooltip for the context chip; the full repository, checkout and branch. */
    contextTitle?: string | null;
    checkoutLabel?: string | null;
    branch?: string | null;
    status?: { primary: DockStatus | null; others: readonly DockStatus[] };
    /** Detail for the primary status chip's tooltip (files, error text). */
    statusDetail?: string | null;
    activeSession?: ActiveSessionSummary | null;
    /** The active session is already a highlighted row in the Agents list, so the return chip would repeat it there. */
    activeSessionListed?: boolean;
    onReturnToShell?: () => void;
    findDisabled?: boolean;
    onFind?: () => void;
    canClearScrollback?: boolean;
    onClearScrollback?: () => void;
    /** Console only: the Console has output to clear. */
    canClearOutput?: boolean;
    onClearOutput?: () => void;
    sessionCountLabel: string;
    /**
     * Every session slot is taken. The count leaves the ⋯ menu for the header
     * then, because it is the reason a New session button is disabled.
     */
    sessionsAtLimit?: boolean;
    sessionListOpen?: boolean;
    onToggleSessionList: () => void;
    dockMode?: "docked" | "overlay";
    onToggleDockMode?: () => void;
    canTabOptions?: boolean;
    tabOptionsOpen?: boolean;
    onTabOptions?: () => void;
    canFindSession?: boolean;
    onFindSession?: () => void;
    expanded?: boolean;
    onToggleExpanded?: () => void;
    onClose?: () => void;
    /** The ⋯ button: the panel restores focus here when the session list closes. */
    overflowEl?: HTMLButtonElement | null;
    menuOpen?: boolean;
  }

  let {
    section,
    onSection,
    os,
    agentAttention = 0,
    agentsRunning = 0,
    repoName,
    repoPath,
    contextTitle = null,
    checkoutLabel = null,
    branch = null,
    status = { primary: null, others: [] },
    statusDetail = null,
    activeSession = null,
    activeSessionListed = false,
    onReturnToShell,
    findDisabled = true,
    onFind,
    canClearScrollback = false,
    onClearScrollback,
    canClearOutput = false,
    onClearOutput,
    sessionCountLabel,
    sessionsAtLimit = false,
    sessionListOpen = false,
    onToggleSessionList,
    dockMode = "docked",
    onToggleDockMode,
    canTabOptions = false,
    tabOptionsOpen = false,
    onTabOptions,
    canFindSession = false,
    onFindSession,
    expanded = false,
    onToggleExpanded,
    onClose,
    overflowEl = $bindable(null),
    menuOpen = $bindable(false),
  }: Props = $props();

  const FOCUS_RING = "focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-accent";
  const SECTION_ICONS = { shell: SquareTerminal, agents: Bot, files: FolderTree, console: ListChecks } as const;

  function sectionTitle(entry: (typeof DOCK_SECTIONS)[number]): string {
    const chord = dockSectionChordLabel(entry.id, os);
    if (entry.id === "agents") {
      if (agentAttention > 0 && agentsRunning > 0) {
        return `${entry.hint} — ${agentAttention} ${agentAttention === 1 ? "needs" : "need"} you, ${agentsRunning} running (${chord})`;
      }
      if (agentAttention > 0) {
        return `${entry.hint} — ${agentAttention} ${agentAttention === 1 ? "needs" : "need"} you (${chord})`;
      }
      if (agentsRunning > 0) {
        return `${entry.hint} — ${agentsRunning} running (${chord})`;
      }
    }
    return `${entry.hint} (${chord})`;
  }

  const contextParts = $derived([repoName ?? "No repository", checkoutLabel, branch].filter((part): part is string => Boolean(part)));
  const STATUS_TESTID = {
    conflict: "terminal-conflict-pill",
    collision: "terminal-collision-pill",
    unverified: "terminal-collision-unverified",
  } as const;
  const statusTitle = $derived.by(() => {
    if (!status.primary) return "";
    const lines = [statusDetail ? `${status.primary.label} — ${statusDetail}` : status.primary.label];
    for (const other of status.others) lines.push(other.label);
    return lines.join("\n");
  });

  const hasClearItem = $derived(section === "shell" || section === "files" || (section === "console" && canClearOutput));
  const showReturnChip = $derived(
    Boolean(activeSession) && (section === "console" || (section === "agents" && !activeSessionListed)),
  );

  const menuPopover = $derived({
    anchor: { kind: "element" as const, element: overflowEl, gap: 6, align: "end" as const },
    estimate: { width: 248, height: 220 },
    inset: 8,
    revision: `${section}:${canClearScrollback}:${canClearOutput}:${canFindSession}:${canTabOptions}:${Boolean(onToggleDockMode)}`,
    dismiss: {
      // The ⋯ button counts as inside, or the pointerdown that closes the
      // menu reopens it on the click that follows. The menu wears `gp-menu`,
      // which the overlay dock's outside-click rule (`ui/panelDismiss.ts`)
      // already reads as inside the dock.
      inside: "[data-dock-overflow], [data-dock-overflow-menu]",
      resize: true,
      escape: "none" as const,
    },
    onDismiss: () => {
      menuOpen = false;
    },
  });

  function closeMenu(restoreFocus = false) {
    menuOpen = false;
    if (restoreFocus) restoreFocusTo(overflowEl);
  }

  function runItem(action: (() => void) | undefined) {
    closeMenu();
    action?.();
  }

  function focusFirstItem(node: HTMLElement) {
    void tick().then(() => {
      node.querySelector<HTMLButtonElement>('button[role^="menuitem"]:not(:disabled)')?.focus({ preventScroll: true });
    });
  }

  function onMenuKey(event: KeyboardEvent) {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      closeMenu(true);
      return;
    }
    if (event.key === "Tab") {
      closeMenu(true);
      return;
    }
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp" && event.key !== "Home" && event.key !== "End") return;
    const items = Array.from(
      event.currentTarget instanceof HTMLElement
        ? event.currentTarget.querySelectorAll<HTMLButtonElement>('button[role^="menuitem"]:not(:disabled)')
        : [],
    );
    if (items.length === 0) return;
    event.preventDefault();
    event.stopPropagation();
    const at = items.indexOf(document.activeElement as HTMLButtonElement);
    let next = 0;
    if (event.key === "End") next = items.length - 1;
    else if (event.key === "ArrowDown") next = (at + 1) % items.length;
    else if (event.key === "ArrowUp") next = (at - 1 + items.length) % items.length;
    items[next]?.focus();
  }
</script>

<div class="@container px-3 py-1.5 flex flex-wrap gap-2 items-center shrink-0" data-testid="dock-header">
  <div class="flex flex-1 items-center gap-2 min-w-0">
    <div class="gp-segmented shrink-0" role="group" aria-label="Dock section">
      {#each DOCK_SECTIONS as entry, index (entry.id)}
        {@const Icon = SECTION_ICONS[entry.id]}
        <button
          type="button"
          aria-pressed={section === entry.id}
          data-active={section === entry.id ? "true" : "false"}
          data-testid="dock-section-{entry.id}"
          class="gp-seg-btn text-meta! relative inline-flex items-center gap-1 {FOCUS_RING}"
          aria-keyshortcuts={os === "macos" ? `Alt+Shift+Meta+${index + 1}` : `Control+Alt+Shift+${index + 1}`}
          title={sectionTitle(entry)}
          onclick={() => onSection(entry.id)}
        >
          <Icon size={13} class="shrink-0" /><span class="@max-[34rem]:sr-only">{entry.label}</span>
          {#if entry.id === "agents" && agentAttention > 0}
            <span class="inline-block h-1.5 w-1.5 rounded-full bg-warn shrink-0" aria-hidden="true" data-testid="dock-agents-attention"></span>
            <span class="sr-only">, {agentAttention} {agentAttention === 1 ? "agent needs" : "agents need"} you{agentsRunning > 0 ? `, ${agentsRunning} running` : ""}</span>
          {:else if entry.id === "agents" && agentsRunning > 0}
            <Loader2 size={10} class="animate-spin text-accent shrink-0" aria-hidden="true" data-testid="dock-agents-running" />
            <span class="tabular-nums text-micro text-accent shrink-0" aria-hidden="true" data-testid="dock-agents-running-count">{agentsRunning}</span>
            <span class="sr-only">, {agentsRunning} {agentsRunning === 1 ? "agent" : "agents"} running</span>
          {/if}
        </button>
      {/each}
    </div>

    <span
      class="min-w-0 max-w-64 truncate font-mono text-micro text-textMuted @max-[34rem]:hidden"
      data-testid="terminal-checkout-pill"
      title={contextTitle ?? repoPath ?? "No repository"}
    >{contextParts.join(" · ")}</span>

    {#if status.primary}
      <span
        class="gp-chip min-w-0 max-w-48"
        data-tone={status.primary.tone}
        data-testid={STATUS_TESTID[status.primary.kind]}
        title={statusTitle}
      >
        <span class="truncate">{status.primary.label}</span>{#if status.others.length > 0}<span class="ml-1 shrink-0 tabular-nums">+{status.others.length}</span>{/if}
      </span>
    {/if}

    {#if sessionsAtLimit}
      <button
        type="button"
        class="gp-chip shrink-0 cursor-pointer tabular-nums"
        data-tone="warn"
        data-testid="dock-session-limit"
        aria-expanded={sessionListOpen}
        aria-controls="terminal-session-list"
        title="Every session slot is in use — show all sessions to close one, or raise the limit in Settings"
        onclick={onToggleSessionList}
      >{sessionCountLabel}</button>
    {/if}

    {#if showReturnChip && activeSession}
      <button
        type="button"
        class="gp-chip min-w-0 max-w-72 cursor-pointer {FOCUS_RING}"
        data-tone="accent"
        data-testid="terminal-active-session-summary"
        title={`Show active ${activeSession.launcherLabel} session — ${activeSession.fullTitle}`}
        onclick={onReturnToShell}
      >
        <LauncherIcon kind={activeSession.launcher} size={13} class="shrink-0" />
        <span class="font-medium truncate">{activeSession.primary}</span>
        {#if activeSession.secondary}
          <span class="text-textMuted truncate max-w-28">· {activeSession.secondary}</span>
        {/if}
        {#if activeSession.badge}
          <span class="shrink-0 {activeSession.badge.cls}">· {activeSession.badge.label}</span>
        {/if}
        <span class="text-accent shrink-0">→ Shell</span>
      </button>
    {/if}
  </div>

  <div class="flex items-center gap-1 shrink-0">
    {#if section === "shell" || section === "files"}
      <button
        type="button"
        class="gp-icon-btn {FOCUS_RING}"
        disabled={findDisabled}
        aria-label="Find in terminal"
        title="Find in terminal ({platformChord('⌘F', 'Ctrl+Shift+F', os)})"
        onclick={onFind}
      ><Search size={13} /></button>
    {/if}
    <button
      bind:this={overflowEl}
      type="button"
      class="gp-icon-btn {FOCUS_RING}"
      aria-label="Dock options"
      aria-haspopup="menu"
      aria-expanded={menuOpen || sessionListOpen}
      data-dock-overflow
      data-testid="dock-overflow"
      title="Dock options"
      onclick={() => (menuOpen = !menuOpen)}
    ><Ellipsis size={13} /></button>
    {#if onToggleExpanded}
      <button
        type="button"
        class="gp-icon-btn {FOCUS_RING}"
        aria-label={expanded ? "Restore dock size" : "Expand the dock"}
        title={expanded ? "Restore dock size" : "Expand the dock"}
        onclick={onToggleExpanded}
      >
        {#if expanded}<Minimize2 size={13} />{:else}<Maximize2 size={13} />{/if}
      </button>
    {/if}
    {#if onClose}
      <button
        type="button"
        class="gp-icon-btn {FOCUS_RING}"
        aria-label="Hide the dock"
        title="Hide the dock ({shortcutTextLabel('⌃`', os)}) — sessions keep running"
        onclick={onClose}
      ><ChevronDown size={13} /></button>
    {/if}
  </div>
</div>

{#if menuOpen}
  <div
    use:focusFirstItem
    use:portal={"body"}
    use:popover={menuPopover}
    out:popoverExit
    data-dock-overflow-menu
    data-testid="dock-overflow-menu"
    class="fixed w-60 max-w-[calc(100vw-16px)] max-h-[min(23rem,calc(100vh-24px))] overflow-y-auto gp-menu gp-pop p-1 text-meta text-textPrimary"
    style="z-index: {LAYERS.MENU}"
    role="menu"
    aria-label="Dock options"
    tabindex="-1"
    onkeydown={onMenuKey}
  >
    <button
      type="button"
      role="menuitem"
      class="gp-menu-item"
      aria-expanded={sessionListOpen}
      aria-controls="terminal-session-list"
      data-testid="terminal-session-count"
      title="Sessions across repositories"
      onclick={() => runItem(onToggleSessionList)}
    >
      <Layers size={13} class="shrink-0 text-textMuted" />
      <span class="truncate">All sessions</span>
      <span class="ml-auto shrink-0 text-micro tabular-nums text-textMuted">{sessionCountLabel}</span>
    </button>
    {#if canFindSession}
      <button
        type="button"
        role="menuitem"
        class="gp-menu-item"
        data-testid="dock-menu-find-session"
        title="Find a session in this repository"
        onclick={() => runItem(onFindSession)}
      ><ChevronsUpDown size={13} class="shrink-0 text-textMuted" /><span>Find session in this repository</span></button>
    {/if}
    {#if canTabOptions}
      <button
        type="button"
        role="menuitem"
        class="gp-menu-item"
        aria-expanded={tabOptionsOpen}
        data-testid="dock-menu-tab-options"
        onclick={() => runItem(onTabOptions)}
      ><Settings2 size={13} class="shrink-0 text-textMuted" /><span>Tab options</span></button>
    {/if}
    {#if onToggleDockMode}
      <div class="gp-menu-sep" role="separator"></div>
      <button
        type="button"
        role="menuitemcheckbox"
        class="gp-menu-item"
        aria-checked={dockMode === "overlay"}
        data-terminal-dock-mode-toggle
        title={dockMode === "overlay" ? "Give the dock its own row under the view" : "Let the dock float over the view"}
        onclick={() => runItem(onToggleDockMode)}
      >
        <Pin size={13} class="shrink-0 text-textMuted" />
        <span>Float over the view</span>
        {#if dockMode === "overlay"}<Check size={13} class="ml-auto shrink-0 text-accent" />{/if}
      </button>
    {/if}
    {#if hasClearItem}
      <div class="gp-menu-sep" role="separator"></div>
      {#if section === "shell" || section === "files"}
        <button
          type="button"
          role="menuitem"
          class="gp-menu-item"
          data-testid="dock-menu-clear-scrollback"
          disabled={!canClearScrollback}
          title="Clear scrollback; keep the current prompt and session"
          onclick={() => runItem(onClearScrollback)}
        ><Trash2 size={13} class="shrink-0 text-textMuted" /><span>Clear scrollback</span></button>
      {:else if section === "console" && canClearOutput}
        <button
          type="button"
          role="menuitem"
          class="gp-menu-item"
          data-testid="dock-menu-clear-output"
          title="Clear the Console's output"
          onclick={() => runItem(onClearOutput)}
        ><Trash2 size={13} class="shrink-0 text-textMuted" /><span>Clear output</span></button>
      {/if}
    {/if}
  </div>
{/if}
