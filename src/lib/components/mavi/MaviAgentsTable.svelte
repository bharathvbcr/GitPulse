<script lang="ts">
  /**
   * Mavi threads in the Agents window, under the plane's own rows and in its
   * columns, so the window counts every agent at work. Kept out of the plane's
   * treegrid on purpose: a thread is neither a PTY nor a worktree, and the
   * plane's projection is about those (see `mavi/agentsRows.ts`).
   */
  import { MessagesSquare, Square, ArrowUpRight } from "@lucide/svelte";
  import type { MaviAgentRow } from "../../mavi/agentsRows";
  import { maviStore } from "../../mavi/store.svelte";
  import { showMaviThread } from "../../mavi/navigate";
  import { toastStore } from "../../stores/toastStore";

  let {
    rows,
    compact = false,
    columnVisible,
  }: {
    rows: readonly MaviAgentRow[];
    compact?: boolean;
    columnVisible: (key: "checkout" | "presence" | "attention" | "parallel" | "changes") => boolean;
  } = $props();

  async function end(row: MaviAgentRow) {
    try {
      await maviStore.stop(row.key);
    } catch (error) {
      toastStore.warning(`Couldn't end “${row.title}”: ${error instanceof Error ? error.message : String(error)}`);
    }
  }
</script>

<table
  class="w-full text-left border-collapse mt-4 {compact ? 'text-meta' : 'text-xs'}"
  data-testid="agents-mavi"
  aria-label="Mavi threads"
>
  <caption class="text-left px-4 pb-1 gp-label">
    <span class="inline-flex items-center gap-1.5"><MessagesSquare size={12} aria-hidden="true" /> Mavi threads</span>
  </caption>
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
      <tr class="hover:bg-surfaceHover/60" data-testid="agents-mavi-row" data-thread={row.key}>
        <th scope="row" class="py-2 pl-4 pr-4 font-normal text-left align-top">
          <span class="block font-medium text-textPrimary">{row.title}</span>
          <span class="block text-textMuted">Mavi thread{row.ssh ? " · SSH host" : ""}</span>
          <span class="mt-1 flex flex-wrap gap-1">
            <button
              type="button"
              class="gp-btn"
              data-size="sm"
              aria-label="Open Mavi thread {row.title}"
              title="Open this conversation in Mavi"
              onclick={() => void showMaviThread(row.key)}
            >
              <ArrowUpRight size={11} /> <span>Open</span>
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
                <Square size={11} /> <span>End</span>
              </button>
            {/if}
          </span>
        </th>
        {#if columnVisible("checkout")}
          <td class="px-3 py-2 align-top text-textSecondary">{row.where}</td>
        {/if}
        {#if columnVisible("presence")}
          <td class="px-3 py-2 align-top">{row.presence}</td>
        {/if}
        {#if columnVisible("attention")}
          <td class="px-3 py-2 align-top" class:text-warn={row.status === "needs_you"} class:text-danger={row.status === "failed"}>
            {row.attention || "—"}
          </td>
        {/if}
        {#if columnVisible("parallel")}
          <td class="px-3 py-2 align-top text-textMuted" title="Mavi runs one thread per worktree, so a thread never shares its checkout">—</td>
        {/if}
        {#if columnVisible("changes")}
          <td class="px-3 py-2 align-top tabular-nums" title="Files the agent edited with its own tools">
            {row.edited === null ? "Not read" : `${row.edited} edited`}
          </td>
        {/if}
      </tr>
    {/each}
  </tbody>
</table>
