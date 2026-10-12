<script lang="ts">
  /**
   * Manvi's refinement of one task, shown as a diff for a person to accept.
   *
   * Manvi proposes acceptance criteria, parts to split off, and tasks this one
   * duplicates, stores the proposal, and applies none of it. The parts chosen
   * here are accepted by the host's `proposals.apply` (dc-store's accept of the
   * stored proposal) at the proposal and task revisions that were reviewed: if
   * the task moved since, the write is refused, the task is re-read, and the
   * proposal is dropped for a new one; it is never merged into a task that
   * changed. Accepted splits become new inbox tasks parented to this one, with
   * ids the host makes.
   */
  import { untrack } from "svelte";
  import { get } from "svelte/store";
  import { Sparkles } from "@lucide/svelte";
  import { explainError, getRefinement, getTask, newID, WorkbenchError, type RefinePart, type Task } from "../workbench/client";
  import { applyConflict, applyRefine, confirmReviewed, dismissRefine, refineDiff, requestRefine, selectableParts, type ApplyHeld, type RefineResult } from "../workbench/proposals";
  import { effectiveSelection } from "../workbench/taskModel";
  import { harnessStore } from "../stores/harnessStore";

  let { task, repositoryPath, disabled = false, onApplied, initial = null }: {
    task: Task | null;
    repositoryPath: string;
    /** The sheet has unsaved edits or is busy: asking or applying would discard them. */
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
  /** The parts to accept. Criteria start chosen when one is new; a part the task has locked never does. */
  const defaultParts = (next: RefineResult | null): Set<RefinePart> =>
    new Set(task && next ? selectableParts(task, next).initial : []);
  let picked = $state<Set<RefinePart>>(untrack(() => defaultParts(initial)));
  /** Kept across a failed ask so a retry replays the same request instead of calling the model again. */
  let attempt = newID();

  /** Splits the host held because they look like open work; applying again names these tasks as separate. */
  let held = $state<ApplyHeld | null>(null);
  /** Every related task the person has confirmed as separate for this proposal, across refusals. */
  let reviewed = $state<string[]>([]);

  const diff = $derived(task && result ? refineDiff(task, result) : null);
  const locked = $derived<readonly RefinePart[]>(task && result ? selectableParts(task, result).locked : []);
  /** Only what is chosen and still allowed: a lock that arrived after the choice drops it. */
  const chosen = $derived([...picked].filter((part) => !locked.includes(part)));
  const nothingPicked = $derived(chosen.length === 0);
  const blocked = $derived(applying || disabled || nothingPicked || Boolean(diff?.stale));

  async function ask() {
    if (!task || asking || disabled) return;
    asking = true; error = ""; message = "";
    try {
      const next = await requestRefine(repositoryPath, task, effectiveSelection(get(harnessStore)), attempt);
      attempt = newID();
      result = next; picked = defaultParts(next); held = null; reviewed = [];
    } catch (cause) {
      result = null;
      error = explainError(cause);
    } finally { asking = false; }
  }

  function toggle(part: RefinePart) {
    if (locked.includes(part)) return;
    const next = new Set(picked);
    if (!next.delete(part)) next.add(part);
    picked = next; held = null;
  }

  async function dismiss() {
    const dropped = result;
    result = null;
    // Best effort: a proposal that cannot be rejected now simply stays stored.
    if (dropped) try { await dismissRefine(dropped); } catch { /* nothing was applied either way */ }
  }

  async function apply(confirm: readonly { item_id: string }[] = []) {
    // The sheet's unsaved edits are replaced by the task an apply returns, so a dirty or busy sheet cannot apply.
    if (!result || blocked) return;
    applying = true; error = ""; message = "";
    const reviewedProposal = result;
    const sent = confirmReviewed(reviewed, confirm);
    try {
      const receipt = await applyRefine(repositoryPath, reviewedProposal, chosen, sent);
      if (receipt.outcome === "related_tasks_exist") { held = receipt; reviewed = sent; return; }
      held = null; reviewed = [];
      // Applied, whatever follows: the write is done and a retry would be refused as a reused request.
      message = `Applied: ${receipt.criteriaAdded} criteria added, ${receipt.linksAdded} links added, ${receipt.createdTaskIds.length} tasks created in the inbox.`;
      result = null;
      try { onApplied(await getTask(reviewedProposal.task_id)); }
      catch (reread) { message += ` The task could not be re-read (${explainError(reread)}); reload it to see the result.`; }
    } catch (cause) {
      if (cause instanceof WorkbenchError && cause.code === "revision_conflict") {
        // Refused, nothing written. Say which side moved, show the task as it is now, and ask again against it.
        result = null; held = null; reviewed = [];
        const [freshTask, stored] = await Promise.allSettled([getTask(reviewedProposal.task_id), getRefinement(reviewedProposal.enhancement_id)]);
        const current = freshTask.status === "fulfilled" ? freshTask.value : null;
        error = applyConflict(reviewedProposal, current, stored.status === "fulfilled" ? stored.value : null).message;
        if (current) onApplied(current);
        else error += ` (The task could not be re-read: ${freshTask.status === "rejected" ? explainError(freshTask.reason) : "no reply"}.)`;
      } else {
        error = explainError(cause);
      }
    } finally { applying = false; }
  }

</script>

<!-- The part checkboxes choose what to apply; they are not the task's fields. TaskEditor marks
     its sheet dirty on any input or change reaching its form, and dirty disables Apply. -->
<section class="refine" aria-label="Refine this task" data-testid="task-refine" oninput={(event) => event.stopPropagation()} onchange={(event) => event.stopPropagation()}>
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
        <label class="part"><input type="checkbox" data-testid="task-refine-part-acceptance_criteria" checked={chosen.includes("acceptance_criteria")} disabled={locked.includes("acceptance_criteria")} onchange={() => toggle("acceptance_criteria")} /> Add these to the task{#if locked.includes("acceptance_criteria")} <em>(locked on this task)</em>{/if}</label>
        <ul class="rows">
          {#each diff.criteria as row}
            <li class:present={row.state === "present"}>
              {#if row.state === "add"}<ins>+ {row.text}</ins>{:else}<span>{row.text} <em>(already on the task)</em></span>{/if}
              {#if row.cites.length > 0}<span class="cites">{row.cites.join(", ")}</span>{/if}
            </li>
          {/each}
        </ul>
      {/if}

      {#if diff.duplicates.length > 0}
        <h4>Possible duplicates</h4>
        <label class="part"><input type="checkbox" data-testid="task-refine-part-duplicate_of" checked={chosen.includes("duplicate_of")} disabled={locked.includes("duplicate_of")} onchange={() => toggle("duplicate_of")} /> Link these as the original</label>
        <ul class="rows">
          {#each diff.duplicates as row}
            <li class:present={row.state === "present"}>
              {#if row.state === "add"}<ins>+ duplicate of {row.id}</ins> — {row.reason}{:else}<span>duplicate of {row.id} <em>(already linked)</em></span>{/if}
            </li>
          {/each}
        </ul>
      {/if}

      {#if diff.splits.length > 0}
        <h4>Split into</h4>
        <label class="part"><input type="checkbox" data-testid="task-refine-part-split_into" checked={chosen.includes("split_into")} disabled={locked.includes("split_into")} onchange={() => toggle("split_into")} /> Create these as new inbox tasks</label>
        <ul class="rows">
          {#each diff.splits as part, index}
            <li data-testid="task-refine-split-{index}">
              <strong>{part.title}</strong>
              <pre>{part.description}</pre>
            </li>
          {/each}
        </ul>
      {/if}

      {#if diff.criteria.length + diff.duplicates.length + diff.splits.length > 0}
        <button type="button" class="gp-btn primary" data-testid="task-refine-apply" disabled={blocked} onclick={() => apply()}>
          {applying ? "Applying…" : "Apply selected"}
        </button>
        {#if held}
          <div class="held" role="alert" data-testid="task-refine-related">
            <p>Nothing was applied. Similar open work exists for {held.splits.length === 1 ? "a proposed split" : "some proposed splits"}:</p>
            <ul>{#each held.splits as split}<li><strong>{split.title}</strong>: {split.related.map((r) => r.title).join("; ")}</li>{/each}</ul>
            <button type="button" class="gp-btn" data-testid="task-refine-apply-anyway" disabled={blocked} onclick={() => apply(held!.splits.flatMap((split) => split.related))}>They are separate: apply anyway</button>
          </div>
        {/if}
      {/if}

      {#if diff.criteria.length + diff.duplicates.length + diff.splits.length === 0}
        <p class="meta" data-testid="task-refine-empty">Manvi has no refinements to suggest.</p>
      {/if}

      {#if result.dropped.length > 0}
        <details class="dropped"><summary>{result.dropped.length} suggestion(s) were dropped</summary>
          <ul>{#each result.dropped as drop}<li>{drop.kind}{drop.ref ? ` ${drop.ref}` : ""}: {drop.reason}</li>{/each}</ul>
        </details>
      {/if}
      <button type="button" class="gp-btn" data-testid="task-refine-dismiss" onclick={dismiss}>Dismiss</button>
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
  .held{display:flex;flex-direction:column;gap:6px;color:rgb(var(--c-status-red))}
  .held p,.held ul{margin:0}
  .part{display:flex;align-items:center;gap:6px}
  ins{text-decoration:none;color:rgb(var(--c-accent))}
  .cites{display:block;font-size:10px;color:rgb(var(--c-text-muted))}
  pre{margin:2px 0;white-space:pre-wrap;overflow-wrap:anywhere;font-size:11px}
</style>
