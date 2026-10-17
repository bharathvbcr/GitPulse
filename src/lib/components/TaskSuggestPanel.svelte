<script lang="ts">
  import { PRIORITY_LABELS, priorityLabel } from "../workbench/taskOrganize";
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

<section id="task-suggest" class="suggest" data-testid="task-suggest" aria-label="Suggested next tasks" aria-busy={loading}>
  <div class="head">
    <h2>Suggested next</h2>
    <select class="gp-select" aria-label="Suggest tasks at or above this priority" value={String(priority)} onchange={choose} disabled={loading}>
      <option value="all">Any priority</option>
      {#each PRIORITY_LABELS as label, level (level)}
        <option value={String(level)}>{level === 0 ? `${label} only` : `${label} or above`}</option>
      {/each}
    </select>
    <button type="button" class="gp-btn" onclick={onRefresh} disabled={loading}>{loading ? "Asking Manvi…" : "Refresh"}</button>
    <button type="button" class="gp-icon-btn" aria-label="Close suggestions" onclick={onClose}>×</button>
  </div>
  {#if error}
    <p class="note" role="alert" data-testid="task-suggest-error">{error}</p>
  {:else if result === null}
    <p class="note" role="status">{loading ? "Ranking the open tasks…" : "Nothing asked yet."}</p>
  {:else if result.suggestions.length === 0}
    <p class="note" role="status" data-testid="task-suggest-empty">
      {partial ? "No task could be confirmed ready to start" : "No open task is ready to start"}{left ? ` (${left} held back: in progress, in review, duplicate, blocked, not checked or filtered)` : ""}.
      {#if partial}<strong data-testid="task-suggest-partial">Ranked over part of the board: {partialNote}.</strong>{/if}
    </p>
    {@render blockedList()}
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
      {#if result.excluded.blocked > 0}<span data-testid="task-suggest-blocked">{result.excluded.blocked} blocked by an open task.</span>{/if}
      {#if result.excluded.unread > 0}<span data-testid="task-suggest-unread">{result.excluded.unread} not checked: their links were not read.</span>{/if}
      {#if result.verification.unchecked > 0}Verification read for {result.verification.checked} of {result.verification.checked + result.verification.unchecked}.{/if}
      {#if partial}<strong data-testid="task-suggest-partial">Ranked over part of the board: {partialNote}.</strong>{/if}
    </p>
    {@render blockedList()}
  {/if}
</section>

{#snippet blockedList()}
  {#if result && result.blocked.length > 0}
    <ul class="blocked" data-testid="task-suggest-blocked-list" aria-label="Tasks blocked by an open task">
      {#each result.blocked as item (item.id)}
        <li><button type="button" class="link" data-task-id={item.id} onclick={() => onOpen(item.id)}>{item.title}</button> <span class="muted">blocked by {item.blocked_by.join(", ")}</span></li>
      {/each}
      {#if blockedHidden > 0}<li class="muted">and {blockedHidden} more</li>{/if}
    </ul>
  {/if}
{/snippet}

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
  .blocked{list-style:none;margin:0;padding:0 8px;display:flex;flex-direction:column;gap:2px;font-size:11px}
  .blocked .muted{color:rgb(var(--c-text-muted))}
  .link{border:0;background:transparent;padding:0;font:inherit;color:inherit;text-decoration:underline;cursor:pointer;text-align:left}
  .foot strong,.note strong{color:#b45309;font-weight:600}
</style>
