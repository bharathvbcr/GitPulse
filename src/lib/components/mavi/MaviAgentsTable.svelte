<script lang="ts">
  /**
   * Mavi threads in the Agents window, under the plane's own rows and in its
   * columns, so the window counts every agent at work. Kept out of the plane's
   * treegrid on purpose: a thread is neither a PTY nor a worktree, and the
   * plane's projection is about those (see `mavi/agentsRows.ts`).
   */
  import { MessagesSquare, Square, ArrowUpRight, Loader2, Plus, AlertCircle, ShieldAlert, CheckCircle2 } from "@lucide/svelte";
  import type { MaviAgentRow } from "../../mavi/agentsRows";
  import { maviStore } from "../../mavi/store.svelte";
  import { composeMaviThread, showMaviThread } from "../../mavi/navigate";
  import { statusTone, type ThreadStatus } from "../../mavi/threads";
  import { toastStore } from "../../stores/toastStore";
  import RepoLogo from "../RepoLogo.svelte";

  let {
    rows,
    compact = false,
    columnVisible,
  }: {
    rows: readonly MaviAgentRow[];
    compact?: boolean;
    columnVisible: (key: "checkout" | "presence" | "attention" | "parallel" | "changes") => boolean;
  } = $props();

  function attentionChipTone(status: ThreadStatus): "warn" | "danger" | "ok" {
    if (status === "failed") return "danger";
    if (status === "needs_you") return "warn";
    return "ok";
  }

  async function end(row: MaviAgentRow) {
    try {
      await maviStore.stop(row.key);
    } catch (error) {
      toastStore.warning(`Couldn't end “${row.title}”: ${error instanceof Error ? error.message : String(error)}`);
    }
  }
</script>

<section class="mt-6" aria-label="Mavi threads">
  <div class="flex items-center gap-2 px-4 pb-2">
    <MessagesSquare size={14} class="text-textMuted shrink-0" aria-hidden="true" />
    <h2 class="m-0 text-xs font-semibold text-textPrimary">Mavi threads</h2>
    <span class="gp-chip" data-kind="count" data-tone="neutral">{rows.length}</span>
    <button
      type="button"
      class="gp-btn ml-auto"
      data-size="sm"
      title="Start a new Mavi thread"
      onclick={() => composeMaviThread()}
    >
      <Plus size={11} aria-hidden="true" />
      <span>New Mavi thread</span>
    </button>
  </div>
  <table
    class="w-full text-left border-collapse {compact ? 'text-meta' : 'text-xs'}"
    data-testid="agents-mavi"
    aria-label="Mavi threads"
  >
    <thead class="text-textMuted">
      <tr>
        <th scope="col" class="px-4 py-2 font-medium">Agent</th>
        {#if columnVisible("checkout")}<th scope="col" class="px-3 py-2 font-medium">Checkout</th>{/if}
        {#if columnVisible("presence")}<th scope="col" class="px-3 py-2 font-medium">Presence</th>{/if}
        {#if columnVisible("attention")}<th scope="col" class="px-3 py-2 font-medium">Attention</th>{/if}
        {#if columnVisible("parallel")}<th scope="col" class="px-3 py-2 font-medium">Parallel</th>{/if}
        {#if columnVisible("changes")}<th scope="col" class="px-3 py-2 font-medium">Changes</th>{/if}
      </tr>
    </thead>
    <tbody>
      {#each rows as row (row.key)}
        {@const tone = statusTone(row.status)}
        {@const cellPad = compact ? "py-2" : "py-2.5"}
        <tr class="hover:bg-surfaceHover/60" data-testid="agents-mavi-row" data-thread={row.key}>
          <th scope="row" class="{cellPad} pl-4 pr-4 font-normal text-left align-top">
            <div class="flex items-center gap-2 min-w-0">
              <span class="mavi-status-dot shrink-0" class:animate-pulse={tone === "accent"} data-tone={tone} aria-hidden="true"></span>
              <span class="font-medium text-textPrimary truncate">{row.title}</span>
            </div>
            <div class="mt-1 flex items-center gap-1.5 text-textMuted min-w-0 pl-4">
              {#if row.root}
                <RepoLogo path={row.root} name={row.where} size={14} />
              {/if}
              <span class="truncate">Mavi thread{row.ssh ? " · SSH host" : ""}{!columnVisible("checkout") && row.where ? ` · ${row.where}` : ""}</span>
            </div>
            <div class="mt-1.5 flex flex-wrap items-center gap-1.5 pl-4">
              <button
                type="button"
                class="gp-btn"
                data-size="sm"
                aria-label="Open Mavi thread {row.title}"
                title="Open this conversation in Mavi"
                onclick={() => void showMaviThread(row.key)}
              >
                <ArrowUpRight size={11} aria-hidden="true" /> <span>Open</span>
              </button>
              {#if row.live}
                <button
                  type="button"
                  class="gp-btn"
                  data-size="sm"
                  aria-label="End Mavi thread {row.title}"
                  title="End this thread's agent; the conversation can be resumed"
                  onclick={() => void end(row)}
                >
                  <Square size={11} aria-hidden="true" /> <span>End</span>
                </button>
              {/if}
            </div>
          </th>
          {#if columnVisible("checkout")}
            <td class="px-3 {cellPad} align-top text-textSecondary">{row.where}</td>
          {/if}
          {#if columnVisible("presence")}
            <td class="px-3 {cellPad} align-top">
              {#if row.presence === "Working" || row.status === "working"}
                <span class="inline-flex items-center gap-1.5 text-accent font-medium" data-testid="mavi-running-indicator">
                  <Loader2 size={11} class="gp-agent-spinner animate-spin shrink-0" aria-hidden="true" />
                  <span>{row.presence}</span>
                </span>
              {:else}
                <span class="text-textSecondary">{row.presence}</span>
              {/if}
            </td>
          {/if}
          {#if columnVisible("attention")}
            <td class="px-3 {cellPad} align-top">
              {#if row.attention}
                {@const attTone = attentionChipTone(row.status)}
                <span class="gp-chip font-medium" data-tone={attTone}>
                  {#if attTone === "danger"}
                    <AlertCircle size={11} aria-hidden="true" />
                  {:else if attTone === "warn"}
                    <ShieldAlert size={11} aria-hidden="true" />
                  {:else}
                    <CheckCircle2 size={11} aria-hidden="true" />
                  {/if}
                  <span>{row.attention}</span>
                </span>
              {:else}
                <span class="text-textMuted">—</span>
              {/if}
            </td>
          {/if}
          {#if columnVisible("parallel")}
            <td class="px-3 {cellPad} align-top text-textMuted" title="Mavi runs one thread per worktree, so a thread never shares its checkout">—</td>
          {/if}
          {#if columnVisible("changes")}
            <td class="px-3 {cellPad} align-top tabular-nums {row.edited === null ? 'text-textMuted' : 'text-textSecondary'}" title="Files the agent edited with its own tools">
              {row.edited === null ? "Not read" : `${row.edited} edited`}
            </td>
          {/if}
        </tr>
      {/each}
    </tbody>
  </table>
</section>

<style>
  .mavi-status-dot {
    width: 7px;
    height: 7px;
    border-radius: var(--gp-r-full);
    background: rgb(var(--c-text-faint));
  }
  .mavi-status-dot[data-tone="warn"] {
    background: rgb(var(--c-warn));
    box-shadow: 0 0 0 3px rgb(var(--c-warn) / 0.2);
  }
  .mavi-status-dot[data-tone="accent"] {
    background: rgb(var(--c-accent));
  }
  .mavi-status-dot[data-tone="danger"] {
    background: rgb(var(--c-danger));
  }
  .mavi-status-dot[data-tone="ok"] {
    background: rgb(var(--c-ok));
  }
  @media (prefers-reduced-motion: reduce) {
    :global(.gp-agent-spinner),
    .mavi-status-dot {
      animation: none;
    }
  }
  :global(html[data-motion="reduced"]) :global(.gp-agent-spinner),
  :global(html[data-motion="reduced"]) .mavi-status-dot {
    animation: none;
  }
</style>
