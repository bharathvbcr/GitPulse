<script lang="ts">
  /**
   * Gap-scan proposals for the open repository, for a person to review.
   *
   * Manvi scans and proposes; it never writes the board. Nothing here is
   * filed until a person clicks Accept (or Edit, then File). Accepting goes
   * through the host's `proposals.file`, which is the board's own intake: it
   * keeps the related-task protection (a proposal that looks like open work
   * waits for the person to confirm they have seen those tasks) and is
   * idempotent, so a double click files one card.
   *
   * A partial scan says so. Coverage is listed with both numbers (shown and
   * hidden), and an empty list is only "no gaps" when the scan was complete.
   * A scanner that is not running is an error here, never an empty list.
   */
  import { onDestroy, untrack } from "svelte";
  import { ScanSearch, X } from "@lucide/svelte";
  import { explainError } from "../workbench/client";
  import { DEFAULT_PRIORITY, PRIORITY_LABELS } from "../workbench/taskPriority";
  import {
    cancelScan, categoryLabel, coverageNotes, dismissedFingerprints, dismissFingerprint, draftOf, fileScanProposal,
    getScan, scanFinished, scanIsPartial, startScan,
    type FileDraft, type FileOutcome, type ScanProposal, type ScanView,
  } from "../workbench/proposals";

  let { repositoryPath, startOpen = false, initial = null }: {
    repositoryPath: string;
    /** Open on mount, for a host that restores the panel. */
    startOpen?: boolean;
    /** A scan already read for this repository, shown instead of an empty panel. */
    initial?: ScanView | null;
  } = $props();

  const POLL_MS = 1500;
  let open = $state(untrack(() => startOpen));
  let view = $state<ScanView | null>(untrack(() => initial));
  let error = $state("");
  let starting = $state(false);
  let dismissed = $state<Set<string>>(untrack(() => (repositoryPath ? dismissedFingerprints(repositoryPath) : new Set())));
  let showDismissed = $state(false);
  let editing = $state<string | null>(null);
  let draft = $state<FileDraft>({ title: "", description: "", kind: "feature", priority: DEFAULT_PRIORITY, labels: [] });
  /** Outcome of each proposal's filing attempt, by fingerprint. */
  let outcomes = $state<Record<string, FileOutcome | { outcome: "error"; message: string }>>({});
  /** Fingerprints being filed right now: a second click while one is in flight does nothing. */
  const filing = new Set<string>();
  let busy = $state<Set<string>>(new Set());
  let timer: ReturnType<typeof setTimeout> | undefined;
  /** Bumped when the repository changes or the panel unmounts, so a late reply is dropped. */
  let epoch = 0;

  const running = $derived(view !== null && !scanFinished(view));
  const notes = $derived(view ? coverageNotes(view) : []);
  const partial = $derived(view ? scanIsPartial(view) : false);
  const visible = $derived(view ? view.proposals.filter((p) => showDismissed || !dismissed.has(p.fingerprint)) : []);
  const hiddenCount = $derived(view ? view.proposals.filter((p) => dismissed.has(p.fingerprint)).length : 0);

  let shownFor = untrack(() => repositoryPath);
  $effect(() => {
    // A scan belongs to one repository: switching drops it rather than showing another repository's proposals.
    const path = repositoryPath;
    if (path === shownFor) return;
    shownFor = path;
    epoch += 1;
    clearTimeout(timer);
    view = null; error = ""; outcomes = {}; editing = null;
    dismissed = path ? dismissedFingerprints(path) : new Set();
  });
  onDestroy(() => { epoch += 1; clearTimeout(timer); });

  async function start() {
    if (starting || running || !repositoryPath) return;
    const mine = ++epoch;
    starting = true; error = ""; outcomes = {};
    try {
      const next = await startScan(repositoryPath);
      if (mine !== epoch) return;
      view = next;
      schedule(mine);
    } catch (cause) {
      if (mine === epoch) { view = null; error = explainError(cause); }
    } finally { if (mine === epoch) starting = false; }
  }
  function schedule(mine: number) {
    clearTimeout(timer);
    if (!view || scanFinished(view)) return;
    const id = view.scan_id;
    timer = setTimeout(async () => {
      try {
        const next = await getScan(id);
        if (mine !== epoch) return;
        view = next;
        error = "";
      } catch (cause) {
        if (mine !== epoch) return;
        // The scan cannot be read: say so and stop, rather than keep showing a "running" list nobody is updating.
        error = `The scan can no longer be read: ${explainError(cause)}`;
        return;
      }
      schedule(mine);
    }, POLL_MS);
  }
  async function stop() {
    if (!view || !running) return;
    const mine = epoch;
    try {
      const next = await cancelScan(view.scan_id);
      if (mine === epoch) view = next;
    } catch (cause) { if (mine === epoch) error = explainError(cause); }
  }

  function dismiss(proposal: ScanProposal) {
    dismissed = dismissFingerprint(repositoryPath, proposal.fingerprint);
    if (editing === proposal.fingerprint) editing = null;
  }
  function edit(proposal: ScanProposal) {
    editing = proposal.fingerprint;
    draft = draftOf(proposal);
  }
  async function file(proposal: ScanProposal, body: FileDraft, reviewed: readonly string[] = []) {
    const fp = proposal.fingerprint;
    if (filing.has(fp)) return;
    filing.add(fp); busy = new Set(filing);
    const mine = epoch;
    try {
      const outcome = await fileScanProposal(repositoryPath, proposal, body, reviewed);
      if (mine === epoch) { outcomes[fp] = outcome; if (outcome.outcome !== "related_tasks_exist") editing = null; }
    } catch (cause) {
      if (mine === epoch) outcomes[fp] = { outcome: "error", message: explainError(cause) };
    } finally { filing.delete(fp); if (mine === epoch) busy = new Set(filing); }
  }
  const outcomeOf = (fp: string) => outcomes[fp];
  const filedOutcome = (fp: string) => { const o = outcomes[fp]; return o && (o.outcome === "filed" || o.outcome === "already_filed") ? o : null; };
</script>

<svelte:window onkeydown={(event) => { if (open && event.key === "Escape") open = false; }} />

{#if repositoryPath}
  <button type="button" class="gp-btn dock-trigger" data-testid="proposals-open" aria-expanded={open} onclick={() => { open = !open; }}>
    <ScanSearch size={14} aria-hidden="true" /> Proposals{#if view && view.new_count > 0} <span class="count">{view.new_count}</span>{/if}
  </button>
{/if}

{#if open}
  <aside class="drawer gp-glass bg-surface" aria-label="Gap scan proposals" data-testid="proposals-drawer">
    <header>
      <h2>Gap scan</h2>
      <span class="actions">
        {#if running}
          <button type="button" class="gp-btn" data-testid="proposals-stop" onclick={stop}>Stop</button>
        {/if}
        <button type="button" class="gp-btn primary" data-testid="proposals-scan" disabled={starting || running} onclick={start}>
          {view ? "Scan again" : "Scan repository"}
        </button>
        <button type="button" class="icon" aria-label="Close proposals" onclick={() => { open = false; }}><X size={14} aria-hidden="true" /></button>
      </span>
    </header>

    <p class="lede">Manvi proposes tasks it found; nothing reaches your board until you accept it.</p>

    {#if error}
      <p class="error" role="alert" data-testid="proposals-error">{error}</p>
    {/if}

    {#if view}
      <p class="state" role="status" data-testid="proposals-state">
        {#if running}Scanning…{:else if view.state === "done"}Scan finished.{:else}Scan {view.state}.{/if}
        {view.proposals.length} listed{#if view.over_cap > 0}, {view.over_cap} more not listed (cap {view.max_proposals}){/if}.
      </p>
      {#if notes.length > 0}
        <section class="coverage" aria-label="Coverage" data-testid="proposals-coverage">
          <strong>{partial ? "Partial result" : "Coverage"}</strong>
          <ul>{#each notes as note}<li>{note}</li>{/each}</ul>
        </section>
      {/if}

      {#if visible.length === 0 && !running}
        <p class="empty" data-testid="proposals-empty">
          {#if partial}Nothing is listed, but the scan was partial. That is not a clean result.
          {:else if hiddenCount > 0}Everything found was dismissed.
          {:else}No gaps found.{/if}
        </p>
      {/if}

      <ul class="list">
        {#each visible as proposal (proposal.fingerprint)}
          {@const outcome = outcomeOf(proposal.fingerprint)}
          {@const done = filedOutcome(proposal.fingerprint)}
          <li class="proposal" class:dismissed={dismissed.has(proposal.fingerprint)} data-testid="proposal-{proposal.fingerprint}">
            <div class="head">
              <span class="chip">{categoryLabel(proposal.category)}</span>
              {#if proposal.status === "attached"}<span class="chip on-board">On the board</span>
              {:else if proposal.status === "known"}<span class="chip">Seen in an earlier scan</span>{/if}
            </div>
            <p class="title">{proposal.title}</p>
            {#if proposal.status === "attached"}
              <p class="attached">Already held by {proposal.attached_item_title ?? proposal.attached_item_id}. Nothing to file.</p>
            {/if}
            {#if proposal.signals.length > 0}
              <ul class="evidence" aria-label="Evidence">
                {#each proposal.signals as signal}<li><span class="source">{signal.source}</span> {signal.result}</li>{/each}
              </ul>
            {/if}
            <details><summary>Description</summary><pre>{proposal.description}</pre></details>

            {#if done}
              <p class="filed" role="status" data-testid="proposal-filed">
                {done.outcome === "filed" ? "Filed in the inbox" : "Already filed"}: {done.title}
              </p>
            {:else if outcome?.outcome === "related_tasks_exist"}
              <div class="related" role="group" aria-label="Possibly the same work" data-testid="proposal-related">
                <p>Not filed. The board already has open work that looks similar:</p>
                <ul>{#each outcome.related as task}<li>{task.title} <span class="status">({task.status})</span></li>{/each}</ul>
                <button type="button" class="gp-btn" data-testid="proposal-file-anyway" disabled={busy.has(proposal.fingerprint)}
                  onclick={() => file(proposal, editing === proposal.fingerprint ? draft : draftOf(proposal), outcome.related.map((t) => t.item_id))}>
                  It is separate: file anyway
                </button>
              </div>
            {:else if outcome?.outcome === "error"}
              <p class="error" role="alert" data-testid="proposal-error">Not filed: {outcome.message}</p>
            {/if}

            {#if editing === proposal.fingerprint}
              <div class="editor">
                <label>Title <input class="gp-field" bind:value={draft.title} maxlength="300" /></label>
                <label>Description <textarea class="gp-field" rows="5" bind:value={draft.description}></textarea></label>
                <label>Priority
                  <select class="gp-field" bind:value={draft.priority}>
                    {#each PRIORITY_LABELS as name, level}<option value={level}>{name}</option>{/each}
                  </select>
                </label>
                <span class="actions">
                  <button type="button" class="gp-btn primary" data-testid="proposal-file-edited" disabled={busy.has(proposal.fingerprint) || !draft.title.trim()} onclick={() => file(proposal, draft)}>File edited task</button>
                  <button type="button" class="gp-btn" onclick={() => { editing = null; }}>Cancel</button>
                </span>
              </div>
            {:else if proposal.status !== "attached" && !done}
              <span class="actions">
                <button type="button" class="gp-btn primary" data-testid="proposal-accept" disabled={busy.has(proposal.fingerprint)} onclick={() => file(proposal, draftOf(proposal))}>Accept</button>
                <button type="button" class="gp-btn" data-testid="proposal-edit" onclick={() => edit(proposal)}>Edit</button>
                {#if !dismissed.has(proposal.fingerprint)}
                  <button type="button" class="gp-btn" data-testid="proposal-dismiss" onclick={() => dismiss(proposal)}>Dismiss</button>
                {/if}
              </span>
            {/if}
          </li>
        {/each}
      </ul>

      {#if hiddenCount > 0}
        <button type="button" class="gp-btn link" data-testid="proposals-show-dismissed" onclick={() => { showDismissed = !showDismissed; }}>
          {showDismissed ? "Hide" : "Show"} {hiddenCount} dismissed
        </button>
      {/if}
    {:else if !error}
      <p class="empty">No scan has run for this repository yet.</p>
    {/if}
  </aside>
{/if}

<style>
  .dock-trigger{position:absolute;right:14px;bottom:14px;z-index:20;display:inline-flex;align-items:center;gap:6px}
  .count{min-width:16px;padding:0 5px;border-radius:8px;background:rgb(var(--c-accent));color:rgb(var(--c-bg));font-size:10px;text-align:center}
  .drawer{position:absolute;top:0;right:0;bottom:0;z-index:30;width:min(30rem,100%);box-sizing:border-box;padding:12px 14px;overflow:auto;border-left:1px solid rgb(var(--c-border) / 0.65);display:flex;flex-direction:column;gap:8px;color:rgb(var(--c-text))}
  header{display:flex;align-items:center;justify-content:space-between;gap:8px}
  h2{margin:0;font-size:14px;font-weight:650}
  .actions{display:flex;gap:6px;align-items:center;flex-wrap:wrap}
  .icon{width:26px;height:26px;padding:0;display:inline-flex;align-items:center;justify-content:center;border:0;background:transparent}
  .lede,.state,.empty{margin:0;font-size:12px;color:rgb(var(--c-text-muted))}
  .error{margin:0;font-size:12px;color:rgb(var(--c-status-red))}
  .coverage{padding:8px 10px;border-radius:10px;font-size:12px;border:1px solid rgb(var(--c-border) / 0.65)}
  .coverage ul{margin:4px 0 0;padding-left:16px}
  .list{list-style:none;margin:0;padding:0;display:flex;flex-direction:column;gap:8px}
  .proposal{padding:10px;border-radius:12px;border:1px solid rgb(var(--c-border) / 0.65);display:flex;flex-direction:column;gap:6px;font-size:12px}
  .proposal.dismissed{opacity:.6}
  .head{display:flex;gap:6px;flex-wrap:wrap}
  .chip{padding:1px 7px;border-radius:8px;font-size:10px;background:rgb(var(--c-surface-hover) / 0.7)}
  .chip.on-board{background:color-mix(in srgb,rgb(var(--c-accent)) 18%,transparent)}
  .title{margin:0;font-weight:600;overflow-wrap:anywhere}
  .evidence{margin:0;padding-left:16px;color:rgb(var(--c-text-muted))}
  .source{font-weight:600}
  pre{margin:4px 0 0;white-space:pre-wrap;overflow-wrap:anywhere;font-size:11px}
  .editor{display:flex;flex-direction:column;gap:6px}
  .editor label{display:flex;flex-direction:column;gap:2px;font-size:11px}
  .filed{margin:0;color:rgb(var(--c-accent))}
  .related{padding:8px;border-radius:8px;border:1px solid rgb(var(--c-border) / 0.65)}
  .related p,.related ul{margin:0 0 4px}
  .attached{margin:0;color:rgb(var(--c-text-muted))}
  .link{align-self:flex-start}
</style>
