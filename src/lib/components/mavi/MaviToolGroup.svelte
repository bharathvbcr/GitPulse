<script lang="ts">
  /**
   * A run of tool calls as one row: a verb summary, the files it edited as
   * chips (edits are never folded out of sight), a failure count that
   * survives collapse, and the call running now. Expanded, each call is a
   * line; a call with output expands again to show it.
   */
  import { ChevronRight, LoaderCircle, TriangleAlert, FileCode2, Ban } from "@lucide/svelte";
  import type { ToolItem } from "../../mavi/types";
  import { describeTool, isEditTool, toolPath, shortPath } from "../../mavi/rows";
  import MaviHunks from "./MaviHunks.svelte";

  let {
    tools,
    summary,
    failed,
    running,
    editedFiles,
    root,
    activity = null,
    onOpenFile,
  }: {
    tools: readonly ToolItem[];
    summary: string;
    failed: number;
    running: ToolItem | null;
    editedFiles: readonly string[];
    root: string | null;
    activity?: string | null;
    onOpenFile: (path: string) => void;
  } = $props();

  let expanded = $state(false);
  let openCall = $state<string | null>(null);

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
    onclick={() => (expanded = !expanded)}
  >
    <ChevronRight size={12} class="mavi-chevron shrink-0 {expanded ? 'mavi-chevron-open' : ''}" aria-hidden="true" />
    {#if running}
      <LoaderCircle size={12} class="shrink-0 text-accent animate-spin" aria-hidden="true" />
    {/if}
    <span class="truncate">{summary}</span>
    {#if failed > 0}
      <span class="gp-chip" data-tone="danger" data-kind="count" title="{failed} failed or denied">
        <TriangleAlert size={10} aria-hidden="true" /> {failed}
      </span>
    {/if}
  </button>
  {#if running && !expanded}
    <div class="mavi-live text-meta text-textMuted truncate" aria-live="polite">
      <span class="animate-pulse">{activity ?? `${running.name}: ${describeTool(running, root)}`}</span>
    </div>
  {/if}
  {#if editedFiles.length}
    <div class="flex flex-wrap gap-1 mt-1 ml-4">
      {#each editedFiles.slice(0, 8) as file (file)}
        <button class="gp-chip" data-tone="accent" title="Show the change to {file}" onclick={() => onOpenFile(file)}>
          <FileCode2 size={10} aria-hidden="true" />{file}
        </button>
      {/each}
      {#if editedFiles.length > 8}<span class="gp-chip">+{editedFiles.length - 8}</span>{/if}
    </div>
  {/if}
  {#if expanded}
    <ul class="mavi-calls mt-1 ml-4">
      {#each tools as tool (tool.id)}
        {@const path = toolPath(tool)}
        <li>
          <button
            class="flex items-center gap-2 w-full text-left text-meta py-0.5"
            aria-expanded={openCall === tool.id}
            onclick={() => (openCall = openCall === tool.id ? null : tool.id)}
          >
            <span class="mavi-tool-name font-medium text-textPrimary shrink-0">{tool.name}</span>
            <span class="truncate font-mono text-textMuted">{describeTool(tool, root)}</span>
            {#if statusWord(tool)}
              <span
                class="shrink-0 text-micro"
                class:text-danger={tool.status === "error"}
                class:text-warn={tool.status === "denied" || tool.status === "interrupted"}
                class:text-accent={tool.status === "running"}
              >
                {#if tool.status === "denied"}<Ban size={10} class="inline" aria-hidden="true" />{/if}
                {statusWord(tool)}
              </span>
            {/if}
          </button>
          {#if openCall === tool.id}
            <div class="mavi-call-body">
              {#if isEditTool(tool.name) && tool.patch}
                <MaviHunks hunks={tool.patch} maxLines={200} />
                {#if path}
                  <button class="gp-btn mt-1" data-variant="ghost" onclick={() => onOpenFile(shortPath(path, root))}>Show in changes</button>
                {/if}
              {:else if tool.result}
                <pre class="mavi-output font-mono text-meta select-text">{tool.result}</pre>
              {:else}
                <pre class="mavi-output font-mono text-meta select-text">{JSON.stringify(tool.input, null, 2).slice(0, 3000)}</pre>
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
    padding: 2px 0;
  }
  .mavi-group-head:hover {
    color: rgb(var(--c-text));
  }
  :global(.mavi-chevron) {
    transition: transform var(--gp-dur-fast) var(--gp-ease-effect);
  }
  :global(.mavi-chevron-open) {
    transform: rotate(90deg);
  }
  .mavi-live {
    margin-left: 1.5rem;
  }
  .mavi-calls {
    border-left: 1px solid var(--gp-edge);
    padding-left: 10px;
  }
  .mavi-tool-name {
    min-width: 4.5rem;
  }
  .mavi-call-body {
    margin: 4px 0 8px;
  }
  .mavi-output {
    max-height: 16rem;
    overflow: auto;
    padding: 6px 8px;
    border-radius: var(--gp-r-sm);
    background: rgb(var(--c-bg) / 0.65);
    border: 1px solid var(--gp-edge);
    white-space: pre-wrap;
    word-break: break-all;
  }
</style>
