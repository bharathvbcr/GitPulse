<script lang="ts">
  /**
   * Floating trigger completion menu rendered above a Mavi composer when `/`,
   * `@`, or `&` is active at the caret. Keyboard navigation stays owned by the
   * parent textarea so typing is never interrupted.
   */
  import {
    ArrowUpRight,
    Eraser,
    FileCode2,
    FileDiff,
    GitBranch,
    Minimize2,
    Plus,
    Power,
    RefreshCw,
    SlidersHorizontal,
    Sparkles,
    Square,
    Terminal,
    Wrench,
    X,
  } from "@lucide/svelte";
  import { LAYERS } from "../../ui/layers";
  import LauncherIcon from "../LauncherIcon.svelte";
  import type { ActiveTrigger, AgentModelOption, SlashCommandItem } from "../../mavi/composerTriggers";

  let {
    trigger,
    commands,
    files,
    filesLoading = false,
    agentModels,
    modelsLoading = false,
    selectedIndex,
    placement = "above",
    onSelectCommand,
    onSelectFile,
    onSelectAgentModel,
    onHoverIndex,
    onClose,
  }: {
    trigger: ActiveTrigger | null;
    commands: readonly SlashCommandItem[];
    files: readonly string[];
    filesLoading?: boolean;
    agentModels: readonly AgentModelOption[];
    modelsLoading?: boolean;
    selectedIndex: number;
    placement?: "above" | "below";
    onSelectCommand: (cmd: SlashCommandItem) => void;
    onSelectFile: (path: string) => void;
    onSelectAgentModel: (opt: AgentModelOption) => void;
    onHoverIndex: (index: number) => void;
    onClose: () => void;
  } = $props();

  let listEl: HTMLDivElement | undefined = $state();

  const COMMAND_ICONS: Record<string, typeof Sparkles> = {
    new: Plus,
    changes: FileDiff,
    terminal: Terminal,
    handoff: ArrowUpRight,
    stop: Square,
    customize: SlidersHorizontal,
    clear: Eraser,
    end: Power,
    worktree: GitBranch,
    diff: FileDiff,
    fix: Wrench,
    refactor: RefreshCw,
    compact: Minimize2,
  };

  let categoryTitle = $derived(
    trigger?.kind === "/" ? "Commands" : trigger?.kind === "@" ? "Files" : "Agents & models",
  );

  function splitPath(path: string): { dir: string; name: string } {
    const slash = path.lastIndexOf("/");
    if (slash < 0) return { dir: "", name: path };
    return { dir: path.slice(0, slash + 1), name: path.slice(slash + 1) };
  }

  $effect(() => {
    const idx = selectedIndex;
    if (!trigger || !listEl) return;
    const active = listEl.querySelector<HTMLElement>(`[data-trigger-index="${idx}"]`);
    active?.scrollIntoView({ block: "nearest" });
  });
</script>

{#if trigger}
  <div
    class="mavi-trigger-menu gp-menu shadow-float"
    class:mavi-trigger-below={placement === "below"}
    style:z-index={LAYERS.MENU}
    data-testid="mavi-trigger-menu"
    data-trigger-kind={trigger.kind}
  >
    <div class="flex items-center justify-between gap-2 rounded-lg bg-background/50 px-2.5 py-1.5 mb-1">
      <span class="text-meta font-semibold text-textPrimary shrink-0">{categoryTitle}</span>
      <div class="flex items-center gap-1.5 min-w-0">
        {#if (trigger.kind === "@" && filesLoading && files.length > 0) || (trigger.kind === "&" && modelsLoading && agentModels.length > 0)}
          <span class="text-micro text-textMuted shrink-0">Refreshing…</span>
        {/if}
        <span class="text-micro text-textMuted truncate">↑↓ navigate · ↵ select · Esc close</span>
        <button
          type="button"
          class="gp-icon-btn shrink-0"
          data-size="sm"
          aria-label="Close trigger menu"
          title="Close menu"
          onmousedown={(e) => e.preventDefault()}
          onclick={onClose}
        >
          <X size={11} aria-hidden="true" />
        </button>
      </div>
    </div>

    <div
      bind:this={listEl}
      class="mavi-trigger-list"
      role="listbox"
      aria-label={categoryTitle}
    >
      {#if trigger.kind === "/"}
        {#if commands.length === 0}
          <div class="px-2.5 py-2 text-meta text-textMuted">No matching commands</div>
        {:else}
          {#each commands as cmd, i (cmd.id)}
            {@const Icon = COMMAND_ICONS[cmd.id] ?? Sparkles}
            <button
              type="button"
              role="option"
              aria-selected={i === selectedIndex}
              data-trigger-index={i}
              class="gp-menu-item"
              class:bg-surfaceHover={i === selectedIndex}
              onmousedown={(e) => e.preventDefault()}
              onmouseenter={() => onHoverIndex(i)}
              onclick={() => onSelectCommand(cmd)}
            >
              <Icon size={13} class="text-textMuted shrink-0" aria-hidden="true" />
              <span class="font-mono text-xs font-medium text-accent shrink-0">{cmd.command}</span>
              <span class="text-xs font-medium text-textPrimary shrink-0">{cmd.label}</span>
              <span class="text-meta text-textMuted truncate flex-1 min-w-0">{cmd.description}</span>
              <span
                class="gp-chip shrink-0"
                data-kind="label"
                data-tone={cmd.kind === "action" ? "neutral" : "accent"}
              >
                {cmd.kind === "action" ? "Action" : "Prompt"}
              </span>
            </button>
          {/each}
        {/if}
      {:else if trigger.kind === "@"}
        {#if filesLoading && files.length === 0}
          <div class="px-2.5 py-2 text-meta text-textMuted">Listing files…</div>
        {:else if files.length === 0}
          <div class="px-2.5 py-2 text-meta text-textMuted">No matching files</div>
        {:else}
          {#each files as path, i (path)}
            {@const parts = splitPath(path)}
            <button
              type="button"
              role="option"
              aria-selected={i === selectedIndex}
              data-trigger-index={i}
              class="gp-menu-item"
              class:bg-surfaceHover={i === selectedIndex}
              onmousedown={(e) => e.preventDefault()}
              onmouseenter={() => onHoverIndex(i)}
              onclick={() => onSelectFile(path)}
            >
              <FileCode2 size={13} class="text-textMuted shrink-0" aria-hidden="true" />
              <span class="flex items-baseline min-w-0 flex-1 font-mono">
                {#if parts.dir}
                  <span class="truncate min-w-0 text-meta text-textMuted">{parts.dir}</span>
                {/if}
                <span class="shrink-0 text-xs font-medium text-textPrimary">{parts.name}</span>
              </span>
            </button>
          {/each}
        {/if}
      {:else}
        {#if modelsLoading && agentModels.length === 0}
          <div class="px-2.5 py-2 text-meta text-textMuted">Loading models…</div>
        {:else if agentModels.length === 0}
          <div class="px-2.5 py-2 text-meta text-textMuted">No matching agents or models</div>
        {:else}
          {#each agentModels as opt, i (opt.id)}
            <button
              type="button"
              role="option"
              aria-selected={i === selectedIndex}
              data-trigger-index={i}
              class="gp-menu-item"
              class:bg-surfaceHover={i === selectedIndex}
              onmousedown={(e) => e.preventDefault()}
              onmouseenter={() => onHoverIndex(i)}
              onclick={() => onSelectAgentModel(opt)}
            >
              <LauncherIcon kind={opt.agent} size={13} />
              <span class="text-xs font-medium text-textPrimary shrink-0">{opt.label}</span>
              <span
                class="gp-chip shrink-0"
                data-kind="label"
                data-tone={opt.agent === "agy" ? "info" : "neutral"}
              >
                {opt.agentLabel}
              </span>
              <span class="text-meta text-textMuted truncate flex-1 min-w-0">{opt.detail}</span>
            </button>
          {/each}
        {/if}
      {/if}
    </div>
  </div>
{/if}

<style>
  .mavi-trigger-menu {
    position: absolute;
    bottom: 100%;
    left: 0;
    right: 0;
    margin-bottom: 6px;
  }

  .mavi-trigger-below {
    top: 100%;
    bottom: auto;
    margin-top: 6px;
    margin-bottom: 0;
  }

  .mavi-trigger-list {
    max-height: 15rem;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
</style>
