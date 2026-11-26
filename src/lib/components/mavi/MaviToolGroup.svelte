<script lang="ts">
  /**
   * A run of tool calls as one row: a verb summary, the files it edited as
   * chips (edits are never folded out of sight), a failure count that
   * survives collapse, and the call running now. Expanded, each call is a
   * line; a call with output expands again to show it.
   */
  import { onDestroy, untrack } from "svelte";
  import { ChevronRight, LoaderCircle, TriangleAlert, FileCode2, Ban, Copy, Check } from "@lucide/svelte";
  import type { ToolItem } from "../../mavi/types";
  import { describeTool, isEditTool, toolPath, shortPath } from "../../mavi/rows";
  import { copyText } from "../../desktop/clipboard";
  import MaviHunks from "./MaviHunks.svelte";

  let {
    tools,
    summary,
    failed,
    running,
    editedFiles,
    root,
    activity = null,
    defaultExpanded = false,
    onOpenFile,
  }: {
    tools: readonly ToolItem[];
    summary: string;
    failed: number;
    running: ToolItem | null;
    editedFiles: readonly string[];
    root: string | null;
    activity?: string | null;
    defaultExpanded?: boolean;
    onOpenFile: (path: string) => void;
  } = $props();

  let userToggled = false;
  let expanded = $state(untrack(() => defaultExpanded));
  let openCall = $state<string | null>(null);
  let copiedCallId = $state<string | null>(null);
  let copyTimer: ReturnType<typeof setTimeout> | undefined;

  onDestroy(() => {
    if (copyTimer !== undefined) clearTimeout(copyTimer);
  });

  $effect(() => {
    const next = defaultExpanded;
    if (!userToggled) expanded = next;
  });

  function toggleGroup() {
    userToggled = true;
    expanded = !expanded;
  }

  async function copyOutput(callId: string, text: string) {
    if (!(await copyText(text))) return;
    copiedCallId = callId;
    if (copyTimer !== undefined) clearTimeout(copyTimer);
    copyTimer = setTimeout(() => {
      copiedCallId = null;
      copyTimer = undefined;
    }, 1600);
  }

  function statusWord(tool: ToolItem): string {
    switch (tool.status) {
      case "running":
        return tool.background ? "In the background" : "Running";
      case "error":
        return "Failed";
      case "denied":
        return "Denied";
      case "interrupted":
        return "Stopped";
      default:
        return tool.background ? "In the background" : "";
    }
  }
</script>

<div class="mavi-group">
  <button
    class="mavi-group-head flex items-center gap-1.5 w-full text-left text-xs text-textMuted"
    aria-expanded={expanded}
    onclick={toggleGroup}
  >
    <ChevronRight size={12} class="mavi-chevron shrink-0 {expanded ? 'mavi-chevron-open' : ''}" aria-hidden="true" />
    {#if running}
      <LoaderCircle size={12} class="shrink-0 text-accent animate-spin" aria-hidden="true" />
    {/if}
    <span class="truncate">{summary}</span>
    {#if failed > 0}
      <span class="gp-chip shrink-0" data-tone="danger" data-kind="count" title="{failed} failed or denied">
        <TriangleAlert size={10} aria-hidden="true" /> {failed}
      </span>
    {/if}
  </button>
  {#if running && !expanded}
    <div class="mavi-live text-meta text-textMuted truncate">
      <span class="animate-pulse">{activity ?? `${running.name}: ${describeTool(running, root)}`}</span>
    </div>
  {/if}
  {#if editedFiles.length}
    <div class="flex flex-wrap gap-1.5 mt-1.5 ml-4">
      {#each editedFiles.slice(0, 8) as file (file)}
        <button class="gp-chip" data-tone="accent" title="Show the change to {file}" onclick={() => onOpenFile(file)}>
          <FileCode2 size={10} aria-hidden="true" />{file}
        </button>
      {/each}
      {#if editedFiles.length > 8}<span class="gp-chip">+{editedFiles.length - 8}</span>{/if}
    </div>
  {/if}
  {#if expanded}
    <ul class="mavi-calls mt-1.5 ml-4">
      {#each tools as tool (tool.id)}
        {@const path = toolPath(tool)}
        {@const statusText = statusWord(tool)}
        <li>
          <button
            class="mavi-call-row rounded flex items-center gap-2 w-full text-left text-meta"
            aria-expanded={openCall === tool.id}
            onclick={() => (openCall = openCall === tool.id ? null : tool.id)}
          >
            <ChevronRight size={11} class="mavi-chevron shrink-0 text-faint {openCall === tool.id ? 'mavi-chevron-open' : ''}" aria-hidden="true" />
            <span class="mavi-tool-name font-medium text-textPrimary shrink-0">{tool.name}</span>
            <span class="truncate font-mono text-textMuted flex-1 min-w-0">{describeTool(tool, root)}</span>
            {#if statusText}
              <span
                class="shrink-0 text-micro inline-flex items-center gap-1"
                class:text-danger={tool.status === "error"}
                class:text-warn={tool.status === "denied" || tool.status === "interrupted"}
                class:text-accent={tool.status === "running"}
                class:text-textMuted={tool.status === "ok"}
              >
                {#if tool.status === "denied"}<Ban size={10} aria-hidden="true" />{/if}
                {statusText}
              </span>
            {/if}
          </button>
          {#if openCall === tool.id}
            <div class="mavi-call-body">
              {#if isEditTool(tool.name) && tool.patch}
                <MaviHunks hunks={tool.patch} maxLines={200} />
                {#if path}
                  <button class="gp-btn mt-1.5" data-variant="ghost" onclick={() => onOpenFile(shortPath(path, root))}>Show in changes</button>
                {/if}
              {:else}
                {@const outputText = tool.result || JSON.stringify(tool.input, null, 2).slice(0, 3000)}
                <div class="relative">
                  <pre class="mavi-output mavi-output-copyable font-mono text-meta select-text">{outputText}</pre>
                  <span class="mavi-copy-anchor">
                    <button
                      type="button"
                      class="gp-icon-btn bg-surface/90"
                      data-size="sm"
                      aria-label={copiedCallId === tool.id ? "Copied output" : "Copy output"}
                      title={copiedCallId === tool.id ? "Copied" : "Copy output"}
                      onclick={() => copyOutput(tool.id, outputText)}
                    >
                      {#if copiedCallId === tool.id}
                        <Check size={12} class="text-ok" aria-hidden="true" />
                      {:else}
                        <Copy size={12} aria-hidden="true" />
                      {/if}
                    </button>
                  </span>
                </div>
              {/if}
            </div>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .mavi-group-head {
    padding: 4px 8px;
    margin-left: -8px;
    border-radius: var(--gp-r-sm);
    transition: color var(--gp-dur-fast) var(--gp-ease-effect), background-color var(--gp-dur-fast) var(--gp-ease-effect);
  }
  .mavi-group-head:hover {
    color: rgb(var(--c-text));
    background: rgb(var(--c-surface-hover) / 0.65);
  }
  :global(.mavi-chevron) {
    transition: transform var(--gp-dur-fast) var(--gp-ease-effect);
  }
  :global(.mavi-chevron-open) {
    transform: rotate(90deg);
  }
  .mavi-live {
    margin-left: 1.5rem;
    margin-top: 2px;
  }
  .mavi-calls {
    border-left: 1px solid var(--gp-edge);
    padding-left: 10px;
    display: grid;
    gap: 2px;
  }
  .mavi-call-row {
    padding: 3px 6px;
    transition: background-color var(--gp-dur-instant) var(--gp-ease-effect);
  }
  .mavi-call-row:hover {
    background: rgb(var(--c-surface-hover) / 0.65);
  }
  .mavi-tool-name {
    min-width: 4.5rem;
  }
  .mavi-call-body {
    margin: 4px 0 8px 18px;
  }
  .mavi-output {
    max-height: 16rem;
    overflow: auto;
    padding: 8px 10px;
    border-radius: var(--gp-r-sm);
    background: rgb(var(--c-bg) / 0.65);
    border: 1px solid var(--gp-edge);
    white-space: pre-wrap;
    word-break: break-all;
  }
  .mavi-output-copyable {
    padding-right: 36px;
  }
  .mavi-copy-anchor {
    position: absolute;
    top: 4px;
    right: 4px;
  }
</style>
