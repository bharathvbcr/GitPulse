<script lang="ts">
  /**
   * The board's Merge: pick the card that stays, say why, and fold the rest
   * into it.
   *
   * It owns no merge of its own. `mergeTasks` sends the selection to the
   * host's `items.merge`, which is the code `gitpulse_merge_tasks` runs, so
   * the order of writes, every refusal and the partial report are the same
   * whether a person or an agent merged. Each card carries the revision the
   * board drew it at, and a card changed since is refused, not merged as it
   * no longer looks.
   *
   * A partial merge is shown as one: which sources are still on the board and
   * why, and that running the same merge again finishes it.
   */
  import { onMount, tick, untrack } from "svelte";
  import { Merge, X } from "@lucide/svelte";
  import { portal } from "../dom/portal";
  import { trapFocus } from "../ui/focusTrap";
  import { LAYERS } from "../ui/layers";
  import { explainError, mergeTasks, type MergeResult, type TaskCard } from "../workbench/client";

  let { cards, repositoryId, onClose, onMerged }: {
    cards: TaskCard[];
    repositoryId: string;
    onClose: () => void;
    /** The merge wrote something; the board reloads and drops deleted cards. */
    onMerged: (result: MergeResult) => void;
  } = $props();

  // The selection is fixed while the dialog is open; the first card is only the starting choice.
  let keep = $state(untrack(() => cards[0]?.id ?? ""));
  let reason = $state("");
  let busy = $state(false), error = $state(""), result = $state<MergeResult | null>(null);
  let cancelButton: HTMLButtonElement;
  const target = $derived(cards.find((card) => card.id === keep));
  const sources = $derived(cards.filter((card) => card.id !== keep));
  const ready = $derived(!busy && result === null && target !== undefined && sources.length > 0 && reason.trim().length > 0);

  onMount(() => { void tick().then(() => cancelButton?.focus()); });

  async function run() {
    if (!ready || !target) return;
    busy = true; error = "";
    try {
      result = await mergeTasks(repositoryId, target, sources, reason.trim());
      if (result.outcome !== "unchanged") onMerged(result);
    } catch (cause) {
      // A refusal writes nothing; the board is as it was.
      error = explainError(cause);
    } finally { busy = false; }
  }
  function close() { if (!busy) onClose(); }
  function key(event: KeyboardEvent) {
    event.stopPropagation();
    if (event.key === "Escape") { event.preventDefault(); close(); }
  }
</script>

<div use:portal class="merge-backdrop gp-scrim" role="presentation" style="z-index:{LAYERS.PROMPT}">
  <div class="merge-dialog gp-card gp-glass shadow-float" role="dialog" aria-modal="true" aria-label="Merge tasks" tabindex="-1" onkeydown={key} use:trapFocus={{initial: () => cancelButton}} data-testid="task-merge-dialog">
    <header><h2>Merge {cards.length} tasks</h2><button class="gp-icon-btn" aria-label="Close merge" disabled={busy} onclick={close}><X size={16} /></button></header>
    <div class="body">
      {#if result === null}
        <p>The task you keep gets a section with each other task's description, their acceptance criteria, labels, checklist and links, and the most urgent priority, severity and due date. The others are deleted; they stay in history and can be restored from the archive's Deleted view.</p>
        <fieldset>
          <legend>Keep</legend>
          {#each cards as card (card.id)}
            <label><input type="radio" name="merge-keep" value={card.id} bind:group={keep} disabled={busy} /> <span>{card.title}</span></label>
          {/each}
        </fieldset>
        <label class="reason">Why these are one task
          <textarea class="gp-field" rows="2" maxlength="2000" bind:value={reason} disabled={busy} placeholder="Recorded in each task's history" data-testid="task-merge-reason"></textarea>
        </label>
        {#if error}<p class="error" role="alert">{error}</p>{/if}
      {:else}
        <p role="status" data-testid="task-merge-outcome">{result.outcome === "merged" ? `Merged into “${target?.title ?? result.item_id}”.` : result.outcome === "unchanged" ? "Already merged; nothing changed." : "Merged part of the selection."}</p>
        <ul>
          {#each result.sources as row (row.item_id)}
            <li><span>{row.title}</span><small>{row.outcome === "merged" ? "Merged" : row.outcome === "already_merged" ? "Already merged" : "Still on the board"}</small>{#if row.detail}<p class="error">{row.detail}</p>{/if}</li>
          {/each}
        </ul>
        {#if result.next_step}<p role="alert">{result.next_step}</p>{/if}
      {/if}
    </div>
    <footer>
      <button bind:this={cancelButton} class="gp-btn" disabled={busy} onclick={close}>{result ? "Done" : "Cancel"}</button>
      {#if result === null}
        <button class="gp-btn-primary" disabled={!ready} onclick={run} data-testid="task-merge-run"><Merge size={13} /> {busy ? "Merging…" : `Merge ${sources.length} into this one`}</button>
      {:else if !result.ok}
        <button class="gp-btn-primary" disabled={busy} onclick={() => { result = null; }}>Review and run again</button>
      {/if}
    </footer>
  </div>
</div>
<style>
  .merge-backdrop{position:fixed;inset:0;display:grid;place-items:center;padding:20px;background:#0005;color:rgb(var(--c-text))}.merge-dialog{width:min(520px,100%);max-height:calc(100dvh - 40px);display:flex;flex-direction:column;border-radius:16px;overflow:hidden}header,footer{display:flex;align-items:center;justify-content:space-between;gap:10px;padding:16px 20px;flex-shrink:0}header{border-bottom:1px solid rgb(var(--c-border)/.5)}h2{font-size:15px;font-weight:650;margin:0}.body{padding:0 20px;overflow:auto;font-size:12px}p{line-height:1.6;margin:14px 0;color:rgb(var(--c-text-muted))}fieldset{border:0;padding:0;margin:0 0 12px;display:flex;flex-direction:column;gap:6px;max-height:220px;overflow:auto}legend{font-weight:600;margin-bottom:6px}fieldset label{display:flex;gap:8px;align-items:flex-start}fieldset span{overflow-wrap:anywhere}.reason{display:flex;flex-direction:column;gap:6px;font-weight:600}textarea{font:inherit;font-weight:400;resize:vertical}ul{list-style:none;padding:0;margin:0;max-height:260px;overflow:auto}li{padding:10px 0;border-bottom:1px solid rgb(var(--c-border)/.4);display:flex;flex-wrap:wrap;gap:4px 12px;justify-content:space-between}li span{overflow-wrap:anywhere;flex:1;min-width:160px}li p{flex-basis:100%;margin:4px 0}.error{color:#dc6565}small{color:rgb(var(--c-text-muted))}footer{justify-content:flex-end}
</style>
