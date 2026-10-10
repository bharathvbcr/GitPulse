<script lang="ts">
  import { priorityLabel } from "../workbench/taskOrganize";
  import { STATUS_LABELS, type TaskSuggestions } from "../workbench/client";

  /**
   * Manvi's answer to "what should I work on next": the ranked tasks, why each
   * is there, and what the ranking left out. It only presents; the board owns
   * the request, and nothing here changes a task.
   */
  interface Props {
    result: TaskSuggestions | null;
    loading: boolean;
    error: string;
    /** The most relaxed priority asked for, as a number 0-3, or "all". */
    priority: number | "all";
    onPriority: (priority: number | "all") => void;
    onOpen: (id: string) => void;
    onRefresh: () => void;
    onClose: () => void;
  }
  let { result, loading, error, priority, onPriority, onOpen, onRefresh, onClose }: Props = $props();

  const left = $derived(result ? result.excluded.in_progress + result.excluded.review + result.excluded.duplicate + result.excluded.blocked + result.excluded.filtered : 0);
  const partial = $derived(result !== null && (!result.board.complete || result.board.unhydrated > 0));
  function choose(event: Event) {
    const value = (event.currentTarget as HTMLSelectElement).value;
    onPriority(value === "all" ? "all" : Number(value));
  }
</script>

<section id="task-suggest" class="suggest" data-testid="task-suggest" aria-label="Suggested next tasks" aria-busy={loading}>
  <div class="head">
    <h2>Suggested next</h2>
    <select class="gp-select" aria-label="Suggest tasks at or above this priority" value={String(priority)} onchange={choose} disabled={loading}>
      <option value="all">Any priority</option>
      <option value="0">Urgent only</option>
      <option value="1">High or above</option>
      <option value="2">Normal or above</option>
    </select>
    <button type="button" class="gp-btn" onclick={onRefresh} disabled={loading}>{loading ? "Asking Manvi…" : "Refresh"}</button>
    <button type="button" class="gp-icon-btn" aria-label="Close suggestions" onclick={onClose}>×</button>
  </div>
  {#if error}
    <p class="note" role="alert" data-testid="task-suggest-error">{error}</p>
  {:else if result === null}
    <p class="note" role="status">{loading ? "Ranking the open tasks…" : "Nothing asked yet."}</p>
  {:else if result.suggestions.length === 0}
    <p class="note" role="status" data-testid="task-suggest-empty">No open task is ready to start{left ? ` (${left} held back: in progress, in review, duplicate, blocked or filtered)` : ""}.</p>
  {:else}
    <ol class="list">
      {#each result.suggestions as item (item.id)}
        <li>
          <button type="button" class="item" data-testid="task-suggest-item" data-task-id={item.id} onclick={() => onOpen(item.id)}>
            <span class="rank" aria-hidden="true">{item.rank}</span>
            <span class="body">
              <span class="title">{item.title}</span>
              <span class="reason">{item.reason}</span>
            </span>
            <span class="chips">
              <span class="chip" data-priority={item.priority} title={`${priorityLabel(item.priority)} priority`}>{priorityLabel(item.priority)}</span>
              <span class="chip muted">{STATUS_LABELS[item.status] ?? item.status}</span>
            </span>
            <span class="sr-only">Rank {item.rank}, {item.score} points. Open this task.</span>
          </button>
        </li>
      {/each}
    </ol>
    <p class="foot" data-testid="task-suggest-foot">
      {result.suggestions.length} of {result.candidates} ready to start{left ? `; ${left} held back` : ""}.
      {#if result.blocked.length}<span data-testid="task-suggest-blocked">{result.excluded.blocked} blocked by an open task.</span>{/if}
      {#if result.verification.unchecked > 0}Verification read for {result.verification.checked} of {result.verification.checked + result.verification.unchecked}.{/if}
      {#if partial}<strong data-testid="task-suggest-partial">Ranked over part of the board: {result.board.complete ? `${result.board.unhydrated} tasks' links could not be read` : `only ${result.board.scanned} tasks were read`}.</strong>{/if}
    </p>
  {/if}
</section>

<style>
  .suggest{margin:8px 14px 0;padding:10px 12px;border-radius:12px;border:1px solid rgb(var(--c-border) / 0.65);display:flex;flex-direction:column;gap:8px}
  .head{display:flex;align-items:center;gap:8px;flex-wrap:wrap}
  h2{font-size:12px;font-weight:650;margin:0;flex:1}
  .list{list-style:none;margin:0;padding:0;display:flex;flex-direction:column;gap:4px}
  .item{display:flex;align-items:flex-start;gap:8px;width:100%;text-align:left;border:0;background:transparent;padding:6px 8px;border-radius:8px;cursor:pointer}
  .item:hover,.item:focus-visible{background:var(--mac-fill-surface-hover, rgb(var(--c-surface-hover)))}
  .rank{width:18px;height:18px;border-radius:99px;display:inline-flex;align-items:center;justify-content:center;font-size:10px;font-weight:650;flex-shrink:0;background:color-mix(in srgb,rgb(var(--c-accent)) 14%,transparent)}
  .body{display:flex;flex-direction:column;gap:2px;min-width:0;flex:1}
  .title{font-size:12px;font-weight:550;overflow-wrap:anywhere}
  .reason{font-size:11px;color:rgb(var(--c-text-muted))}
  .chips{display:flex;gap:4px;flex-shrink:0}
  .chip{font-size:9px;padding:1px 5px;border-radius:4px;background:color-mix(in srgb,rgb(var(--c-accent)) 9%,transparent)}
  .chip[data-priority="0"]{background:rgb(209 90 100 / .16);color:#d15a64}
  .chip.muted{color:rgb(var(--c-text-muted))}
  .note,.foot{font-size:11px;margin:0;color:rgb(var(--c-text-muted))}
  .foot strong{color:#b45309;font-weight:600}
</style>
