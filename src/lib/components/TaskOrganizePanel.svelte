<script lang="ts">
  import { PRIORITY_LABELS, priorityLabel } from "../workbench/taskOrganize";
  import { STATUS_LABELS, type TaskOrganization } from "../workbench/client";

  /**
   * Manvi's reviewable priority proposal. It only presents. The board owns the
   * request and the writes; a row changes only when a person accepts it.
   */
  interface Props {
    result: TaskOrganization | null;
    loading: boolean;
    applying: boolean;
    error: string;
    outcomes: Readonly<Record<string, string>>;
    onOpen: (id: string) => void;
    onAccept: (id: string) => void;
    onAcceptAll: (ids: string[]) => void;
    onRefresh: () => void;
    onClose: () => void;
  }
  let { result, loading, applying, error, outcomes, onOpen, onAccept, onAcceptAll, onRefresh, onClose }: Props = $props();

  let rejected = $state<string[]>([]);
  const pending = $derived(
    result ? result.changes.filter((change) => !rejected.includes(change.id) && outcomes[change.id] === undefined) : [],
  );
  const partial = $derived(
    result !== null && (!result.coverage.complete || result.coverage.unread > 0 || result.coverage.unhydrated > 0 || result.coverage.unrevisioned > 0),
  );
  const partialNote = $derived.by(() => {
    if (!result) return "";
    const notes: string[] = [];
    if (result.coverage.unread > 0) notes.push(`${result.coverage.unread} tasks were not read`);
    if (result.coverage.unhydrated > 0) notes.push(`${result.coverage.unhydrated} open tasks' links were not read, so they were not treated as blocking and were not lowered`);
    if (result.coverage.unrevisioned > 0) notes.push(`${result.coverage.unrevisioned} tasks had no revision, so they were left out of the proposal`);
    if (result.coverage.note) notes.push(result.coverage.note);
    return notes.join(" ");
  });

  function toggleReject(id: string) {
    rejected = rejected.includes(id) ? rejected.filter((row) => row !== id) : [...rejected, id];
  }
</script>

<section id="task-organize" class="organize" data-testid="task-organize" aria-label="Priority proposal" aria-busy={loading || applying}>
  <div class="head">
    <h2>Organise by priority</h2>
    <button type="button" class="gp-btn" data-testid="task-organize-accept-all" onclick={() => onAcceptAll(pending.map((change) => change.id))} disabled={loading || applying || pending.length === 0}>Accept all</button>
    <button type="button" class="gp-btn" onclick={onRefresh} disabled={loading || applying}>{loading ? "Asking Manvi…" : "Refresh"}</button>
    <button type="button" class="gp-icon-btn" aria-label="Close the priority proposal" onclick={onClose} disabled={applying}>×</button>
  </div>
  {#if error}
    <p class="note" role="alert" data-testid="task-organize-error">{error}</p>
  {:else if result === null}
    <p class="note" role="status">{loading ? "Reading the open board…" : "Nothing asked yet."}</p>
  {:else}
    <p class="note" data-testid="task-organize-order">{result.order_note}</p>
    <p class="note">Nothing on the board changes until you accept a row. Accepted rows use the revision this proposal was built from.</p>
    {#if partial}
      <p class="note" role="status" data-testid="task-organize-partial">
        <strong>Proposal over part of the board.</strong>
        Read {result.coverage.read}. {partialNote}
      </p>
    {:else}
      <p class="note" role="status">Read all {result.coverage.read} open tasks in scope.</p>
    {/if}
    {#if result.changes.length === 0}
      <p class="note" role="status" data-testid="task-organize-empty">No priority would change.</p>
    {:else}
      <ul class="list">
        {#each result.changes as change (change.id)}
          {@const outcome = outcomes[change.id]}
          {@const skipped = rejected.includes(change.id)}
          <li class="row" data-testid="task-organize-change" data-task-id={change.id} data-rule={change.rule}>
            <button type="button" class="title" onclick={() => onOpen(change.id)}>{change.title}</button>
            <span class="chips">
              <span class="chip" data-priority={change.from}>{PRIORITY_LABELS[change.from] ?? priorityLabel(change.from)}</span>
              <span class="arrow" aria-hidden="true">→</span>
              <span class="chip" data-priority={change.to}>{PRIORITY_LABELS[change.to] ?? priorityLabel(change.to)}</span>
              <span class="chip muted">{STATUS_LABELS[change.status] ?? change.status}</span>
            </span>
            <span class="reason">{change.reason}</span>
            {#if outcome}
              <span class="outcome" data-testid="task-organize-outcome">{outcome}</span>
            {:else}
              <span class="actions">
                <button type="button" class="gp-btn" data-testid="task-organize-accept" disabled={applying || skipped} onclick={() => onAccept(change.id)}>Accept</button>
                <button type="button" class="gp-btn" data-testid="task-organize-reject" aria-pressed={skipped} disabled={applying} onclick={() => toggleReject(change.id)}>{skipped ? "Rejected" : "Reject"}</button>
              </span>
            {/if}
          </li>
        {/each}
      </ul>
    {/if}
    {#if result.groups.length > 0}
      <ul class="groups" data-testid="task-organize-groups" aria-label="Duplicate groups">
        {#each result.groups as group (`${group.keep}:${group.members.join(",")}`)}
          <li>Duplicates, not merged. Keep {group.keep}. Members {group.members.join(", ")}.</li>
        {/each}
      </ul>
    {/if}
  {/if}
</section>

<style>
  .organize{margin:8px 14px 0;padding:10px 12px;border-radius:12px;border:1px solid rgb(var(--c-border) / 0.65);display:flex;flex-direction:column;gap:8px}
  .head{display:flex;align-items:center;gap:8px;flex-wrap:wrap}
  h2{font-size:12px;font-weight:650;margin:0;flex:1}
  .list,.groups{list-style:none;margin:0;padding:0;display:flex;flex-direction:column;gap:6px}
  .row{display:flex;flex-wrap:wrap;align-items:center;gap:6px 8px;padding:6px 8px;border-radius:8px}
  .row:hover{background:var(--mac-fill-surface-hover, rgb(var(--c-surface-hover)))}
  .title{border:0;background:transparent;padding:0;text-align:left;font-size:12px;font-weight:550;cursor:pointer;overflow-wrap:anywhere}
  .chips{display:flex;gap:4px;align-items:center;flex-shrink:0}
  .chip{font-size:9px;padding:1px 5px;border-radius:4px;background:color-mix(in srgb,rgb(var(--c-accent)) 9%,transparent)}
  .chip[data-priority="0"]{background:rgb(209 90 100 / .16);color:#d15a64}
  .chip.muted,.reason,.note,.groups,.outcome{font-size:11px;color:rgb(var(--c-text-muted))}
  .note,.groups{margin:0}
  .reason{flex:1;min-width:12em}
  .arrow{font-size:11px;color:rgb(var(--c-text-muted))}
  .actions{display:flex;gap:4px}
  .outcome{flex-basis:100%}
</style>
