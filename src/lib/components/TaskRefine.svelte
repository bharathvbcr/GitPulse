<script lang="ts">
  /**
   * Manvi's refinement of one task, shown as a diff for a person to accept.
   *
   * Manvi proposes acceptance criteria, parts to split off, and tasks this one
   * duplicates. It applies none of it. Each part is chosen here and applied by
   * the host's `proposals.apply` at the revision that was reviewed: if the task
   * moved since, the write is refused, the task is re-read, and the proposal
   * is dropped for a new one — it is never merged into a task that changed.
   * Split parts become new inbox tasks through the board's own intake, so the
   * related-task protection applies and filing twice is one card.
   */
  import { untrack } from "svelte";
  import { get } from "svelte/store";
  import { Sparkles } from "@lucide/svelte";
  import { explainError, getTask, newID, WorkbenchError, type Task } from "../workbench/client";
  import {
    applyRefine, fileSplitPart, refineDiff, requestRefine,
    type FileOutcome, type RefineResult,
  } from "../workbench/proposals";
  import { effectiveSelection } from "../workbench/taskModel";
  import { harnessStore } from "../stores/harnessStore";

  let { task, repositoryPath, disabled = false, onApplied, initial = null }: {
    task: Task | null;
    repositoryPath: string;
    /** The sheet has unsaved edits or is saving: applying would discard them. */
    disabled?: boolean;
    /** The task as it is now on the board, after an apply or after a stale proposal was refused. */
    onApplied: (task: Task) => void;
    /** A proposal already received for this task, shown for review. */
    initial?: RefineResult | null;
  } = $props();

  let result = $state<RefineResult | null>(untrack(() => initial));
  let asking = $state(false);
  let applying = $state(false);
  let error = $state("");
  let message = $state("");
  let pickedCriteria = $state<Set<string>>(new Set(untrack(() => (initial && task ? initial.criteria.filter((c) => !task.acceptance_criteria.some((have) => have.trim() === c.text.trim())).map((c) => c.text) : []))));
  let pickedDuplicates = $state<Set<string>>(new Set());
  let splitOutcomes = $state<Record<number, FileOutcome | { outcome: "error"; message: string }>>({});
  const splitBusy = new Set<number>();
  /** Kept across a failed ask so a retry replays the same request instead of calling the model again. */
  let attempt = newID();

  const diff = $derived(task && result ? refineDiff(task, result) : null);
  const nothingPicked = $derived(pickedCriteria.size === 0 && pickedDuplicates.size === 0);

  async function ask() {
    if (!task || asking || disabled) return;
    asking = true; error = ""; message = "";
    try {
      const next = await requestRefine(repositoryPath, task, effectiveSelection(get(harnessStore)), attempt);
      attempt = newID();
      result = next; splitOutcomes = {};
      pickedCriteria = new Set(next.criteria.filter((c) => !task!.acceptance_criteria.some((have) => have.trim() === c.text.trim())).map((c) => c.text));
      // Linking a task as the original is a judgement; it starts unchosen.
      pickedDuplicates = new Set();
    } catch (cause) {
      result = null;
      error = explainError(cause);
    } finally { asking = false; }
  }

  function toggle(set: Set<string>, value: string): Set<string> {
    const next = new Set(set);
    if (!next.delete(value)) next.add(value);
    return next;
  }

  async function apply() {
    if (!result || applying || nothingPicked || diff?.stale) return;
    applying = true; error = ""; message = "";
    const reviewed = result;
    try {
      const receipt = await applyRefine(repositoryPath, reviewed, { criteria: [...pickedCriteria], duplicates: [...pickedDuplicates] });
      onApplied(await getTask(reviewed.task_id));
      message = receipt.outcome === "unchanged" ? "The task already had everything you selected."
        : `Applied: ${receipt.criteriaAdded} criteria added, ${receipt.linksAdded} links added.`;
      result = null;
    } catch (cause) {
      if (cause instanceof WorkbenchError && cause.code === "revision_conflict") {
        // Refused, nothing written. Show the task as it is now and ask again against it.
        result = null;
        error = "The task changed after this suggestion was made, so nothing was applied. It has been re-read; ask again to refine the current version.";
        try { onApplied(await getTask(reviewed.task_id)); } catch (reread) { error += ` (It could not be re-read: ${explainError(reread)}.)`; }
      } else {
        error = explainError(cause);
      }
    } finally { applying = false; }
  }

  async function fileSplit(index: number, reviewedRelated: readonly string[] = []) {
    if (!task || !result || splitBusy.has(index)) return;
    const part = result.splits[index];
    splitBusy.add(index);
    try {
      splitOutcomes[index] = await fileSplitPart(repositoryPath, task, result.request_id, index,
        { title: part.title, description: part.description, kind: task.kind, priority: task.priority, labels: [] }, reviewedRelated);
    } catch (cause) {
      splitOutcomes[index] = { outcome: "error", message: explainError(cause) };
    } finally { splitBusy.delete(index); }
  }
</script>

<section class="refine" aria-label="Refine this task" data-testid="task-refine">
  <div class="bar">
    <button type="button" class="gp-btn" data-testid="task-refine-ask" disabled={!task || asking || disabled} onclick={ask}>
      <Sparkles size={14} aria-hidden="true" /> {asking ? "Asking Manvi…" : "Suggest refinements"}
    </button>
    {#if disabled}<span class="hint">Save or discard your edits first.</span>{/if}
  </div>

  {#if error}<p class="error" role="alert" data-testid="task-refine-error">{error}</p>{/if}
  {#if message}<p class="ok" role="status" data-testid="task-refine-message">{message}</p>{/if}

  {#if result && diff}
    <div class="proposal" data-testid="task-refine-result">
      <p class="meta">Proposed against revision {result.source_revision} by {result.model}. Nothing is applied until you apply it.</p>
      {#if diff.stale}
        <p class="error" role="alert" data-testid="task-refine-stale">The task is now at revision {task?.revision}. This suggestion is out of date; ask again.</p>
      {/if}
      <p class="grounding" data-testid="task-refine-grounding">
        {#if result.grounding.status === "grounded"}
          Grounded in the code index for {result.grounding.symbols.filter((s) => s.found).length} named symbol(s){result.grounding.index_fresh ? "" : " (index not fresh)"}.
          {#each result.grounding.symbols.filter((s) => s.truncated || s.walk_incomplete) as symbol}
            <span class="warn">{symbol.symbol}: answer incomplete.</span>
          {/each}
        {:else}
          Not grounded in the code: {result.grounding.reason || result.grounding.status}. Treat these as unverified suggestions.
        {/if}
      </p>
      {#if result.rationale}<p class="rationale">{result.rationale}</p>{/if}

      {#if diff.criteria.length > 0}
        <h4>Acceptance criteria</h4>
        <ul class="rows">
          {#each diff.criteria as row}
            <li class:present={row.state === "present"}>
              {#if row.state === "add"}
                <label><input type="checkbox" checked={pickedCriteria.has(row.text)} onchange={() => { pickedCriteria = toggle(pickedCriteria, row.text); }} />
                  <ins>+ {row.text}</ins></label>
              {:else}
                <span>{row.text} <em>(already on the task)</em></span>
              {/if}
              {#if row.cites.length > 0}<span class="cites">{row.cites.join(", ")}</span>{/if}
            </li>
          {/each}
        </ul>
      {/if}

      {#if diff.duplicates.length > 0}
        <h4>Possible duplicates</h4>
        <ul class="rows">
          {#each diff.duplicates as row}
            <li class:present={row.state === "present"}>
              {#if row.state === "add"}
                <label><input type="checkbox" checked={pickedDuplicates.has(row.id)} onchange={() => { pickedDuplicates = toggle(pickedDuplicates, row.id); }} />
                  <ins>+ duplicate of {row.id}</ins> — {row.reason}</label>
              {:else}
                <span>duplicate of {row.id} <em>(already linked)</em></span>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}

      {#if diff.criteria.length + diff.duplicates.length > 0}
        <button type="button" class="gp-btn primary" data-testid="task-refine-apply" disabled={applying || nothingPicked || diff.stale} onclick={apply}>
          {applying ? "Applying…" : "Apply selected"}
        </button>
      {/if}

      {#if diff.splits.length > 0}
        <h4>Split into</h4>
        <ul class="rows">
          {#each diff.splits as part, index}
            {@const outcome = splitOutcomes[index]}
            <li data-testid="task-refine-split-{index}">
              <strong>{part.title}</strong>
              <pre>{part.description}</pre>
              {#if outcome?.outcome === "filed" || outcome?.outcome === "already_filed"}
                <p class="ok" role="status">{outcome.outcome === "filed" ? "Filed in the inbox" : "Already filed"}: {outcome.title}</p>
              {:else if outcome?.outcome === "related_tasks_exist"}
                <p>Not filed. Similar open work exists: {outcome.related.map((r) => r.title).join("; ")}.</p>
                <button type="button" class="gp-btn" onclick={() => fileSplit(index, outcome.related.map((r) => r.item_id))}>It is separate: file anyway</button>
              {:else}
                {#if outcome?.outcome === "error"}<p class="error" role="alert">Not filed: {outcome.message}</p>{/if}
                <button type="button" class="gp-btn" data-testid="task-refine-file-split-{index}" disabled={diff.stale} onclick={() => fileSplit(index)}>File as a new task</button>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}

      {#if diff.criteria.length + diff.duplicates.length + diff.splits.length === 0}
        <p class="meta" data-testid="task-refine-empty">Manvi has no refinements to suggest.</p>
      {/if}

      {#if result.dropped.length > 0}
        <details class="dropped"><summary>{result.dropped.length} suggestion(s) were dropped</summary>
          <ul>{#each result.dropped as drop}<li>{drop.kind}{drop.ref ? ` ${drop.ref}` : ""}: {drop.reason}</li>{/each}</ul>
        </details>
      {/if}
      <button type="button" class="gp-btn" data-testid="task-refine-dismiss" onclick={() => { result = null; }}>Dismiss</button>
    </div>
  {/if}
</section>

<style>
  .refine{display:flex;flex-direction:column;gap:8px;margin-top:8px;font-size:12px}
  .bar{display:flex;align-items:center;gap:8px}
  .hint,.meta,.rationale,.grounding{margin:0;color:rgb(var(--c-text-muted))}
  .error{margin:0;color:rgb(var(--c-status-red))}
  .ok{margin:0;color:rgb(var(--c-accent))}
  .warn{color:rgb(var(--c-status-red))}
  h4{margin:6px 0 0;font-size:11px;text-transform:uppercase;letter-spacing:.04em;color:rgb(var(--c-text-muted))}
  .proposal{display:flex;flex-direction:column;gap:6px;padding:10px;border-radius:12px;border:1px solid rgb(var(--c-border) / 0.65)}
  .rows{list-style:none;margin:0;padding:0;display:flex;flex-direction:column;gap:6px}
  .present{opacity:.65}
  ins{text-decoration:none;color:rgb(var(--c-accent))}
  .cites{display:block;font-size:10px;color:rgb(var(--c-text-muted))}
  pre{margin:2px 0;white-space:pre-wrap;overflow-wrap:anywhere;font-size:11px}
</style>
