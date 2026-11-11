<script lang="ts">
  import { PRIORITY_LABELS, priorityLabel } from "../workbench/taskOrganize";
  import { STATUS_LABELS, type TaskSuggestions } from "../workbench/client";
  import { X } from "@lucide/svelte";
  import Skeleton from "./Skeleton.svelte";

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

  const left = $derived(result ? result.excluded.in_progress + result.excluded.review + result.excluded.duplicate + result.excluded.blocked + result.excluded.unread + result.excluded.filtered : 0);
  const partial = $derived(result !== null && (!result.board.complete || result.board.unhydrated > 0));
  const blockedHidden = $derived(result ? Math.max(0, result.excluded.blocked - result.blocked.length) : 0);
  /** One line per open task whose links were not read, and one for a read a bound cut: either can hide a blocker. */
  const partialNote = $derived.by(() => {
    if (!result) return "";
    const notes: string[] = [];
    if (result.board.unhydrated > 0) notes.push(`${result.board.unhydrated} open tasks' links could not be read, so a task here may be blocked by one of them`);
    if (!result.board.complete && result.board.unhydrated === 0) notes.push(`only ${result.board.scanned} tasks were read`);
    return notes.join("; ");
  });
  function choose(event: Event) {
    const value = (event.currentTarget as HTMLSelectElement).value;
    onPriority(value === "all" ? "all" : Number(value));
  }
</script>

<section id="task-suggest" class="suggest gp-card" data-testid="task-suggest" aria-label="Suggested next tasks" aria-busy={loading}>
  <div class="head">
    <h2 class="text-xs font-semibold">Suggested next</h2>
    <select class="gp-select" aria-label="Suggest tasks at or above this priority" value={String(priority)} onchange={choose} disabled={loading}>
      <option value="all">Any priority</option>
      {#each PRIORITY_LABELS as label, level (level)}
        <option value={String(level)}>{level === 0 ? `${label} only` : `${label} or above`}</option>
      {/each}
    </select>
    <button type="button" class="gp-btn" data-size="sm" onclick={onRefresh} disabled={loading}>{loading ? "Asking Manvi…" : "Refresh"}</button>
    <button type="button" class="gp-icon-btn" data-size="sm" aria-label="Close suggestions" onclick={onClose}><X size={14} /></button>
  </div>
  {#if error}
    <div class="gp-notice" data-tone="danger" role="alert" data-testid="task-suggest-error">{error}</div>
  {:else if result === null}
    {#if loading}<Skeleton variant="text" count={3} hold={false} />{/if}
    <p class={loading ? "sr-only" : "note"} role="status">{loading ? "Ranking the open tasks…" : "Nothing asked yet."}</p>
  {:else if result.suggestions.length === 0}
    <p class="note" role="status" data-testid="task-suggest-empty">
      {partial ? "No task could be confirmed ready to start" : "No open task is ready to start"}{left ? ` (${left} held back: in progress, in review, duplicate, blocked, not checked or filtered)` : ""}.
      {#if partial}<strong class="text-warn font-semibold" data-testid="task-suggest-partial">Ranked over part of the board: {partialNote}.</strong>{/if}
    </p>
    {@render blockedList()}
  {:else}
    <ol class="list">
      {#each result.suggestions as item (item.id)}
        <li>
          <button type="button" class="item gp-row" data-testid="task-suggest-item" data-task-id={item.id} onclick={() => onOpen(item.id)}>
            <span class="rank" aria-hidden="true">{item.rank}</span>
            <span class="body">
              <span class="title">{item.title}</span>
              <span class="reason">{item.reason}</span>
            </span>
            <span class="chips">
              <span class="gp-chip" data-tone={item.priority === 0 ? "danger" : "neutral"} title={`${priorityLabel(item.priority)} priority`}>{priorityLabel(item.priority)}</span>
              <span class="gp-chip" data-tone="neutral">{STATUS_LABELS[item.status] ?? item.status}</span>
            </span>
            <span class="sr-only">Rank {item.rank}, {item.score} points. Open this task.</span>
          </button>
        </li>
      {/each}
    </ol>
    <p class="foot" data-testid="task-suggest-foot">
      {result.suggestions.length} of {result.candidates} ready to start{left ? `; ${left} held back` : ""}.
      {#if result.excluded.blocked > 0}<span data-testid="task-suggest-blocked">{result.excluded.blocked} blocked by an open task.</span>{/if}
      {#if result.excluded.unread > 0}<span data-testid="task-suggest-unread">{result.excluded.unread} not checked: their links were not read.</span>{/if}
      {#if result.verification.unchecked > 0}Verification read for {result.verification.checked} of {result.verification.checked + result.verification.unchecked}.{/if}
      {#if partial}<strong class="text-warn font-semibold" data-testid="task-suggest-partial">Ranked over part of the board: {partialNote}.</strong>{/if}
    </p>
    {@render blockedList()}
  {/if}
</section>

{#snippet blockedList()}
  {#if result && result.blocked.length > 0}
    <ul class="blocked" data-testid="task-suggest-blocked-list" aria-label="Tasks blocked by an open task">
      {#each result.blocked as item (item.id)}
        <li><button type="button" class="gp-btn blocked-open" data-variant="ghost" data-size="sm" data-task-id={item.id} onclick={() => onOpen(item.id)}>{item.title}</button> <span class="muted">blocked by {item.blocked_by.join(", ")}</span></li>
      {/each}
      {#if blockedHidden > 0}<li class="muted">and {blockedHidden} more</li>{/if}
    </ul>
  {/if}
{/snippet}

<style>
  .suggest{margin:8px 14px 0;padding:10px 12px;display:flex;flex-direction:column;gap:8px}
  .head{display:flex;align-items:center;gap:8px;flex-wrap:wrap}
  h2{margin:0;flex:1}
  .list{list-style:none;margin:0;padding:0;display:flex;flex-direction:column;gap:4px}
  .item{align-items:flex-start;width:100%;text-align:left;padding-block:6px;cursor:pointer}
  .rank{width:18px;height:18px;border-radius:var(--gp-r-full, 999px);display:inline-flex;align-items:center;justify-content:center;font-size:0.625rem;font-weight:600;flex-shrink:0;background:color-mix(in srgb,rgb(var(--c-accent)) 14%,transparent)}
  .body{display:flex;flex-direction:column;gap:2px;min-width:0;flex:1}
  .title{font-size:0.75rem;font-weight:500;overflow-wrap:anywhere}
  .reason{font-size:0.6875rem;color:rgb(var(--c-text-muted))}
  .chips{display:flex;gap:4px;flex-shrink:0}
  .note,.foot{font-size:0.6875rem;margin:0;color:rgb(var(--c-text-muted))}
  .blocked{list-style:none;margin:0;padding:0 8px;display:flex;flex-direction:column;gap:2px;font-size:0.6875rem}
  .blocked .muted{color:rgb(var(--c-text-muted))}
  .blocked-open{white-space:normal;text-align:left;padding-inline:0}
</style>
