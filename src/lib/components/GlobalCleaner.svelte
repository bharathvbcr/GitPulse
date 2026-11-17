<script lang="ts">
  import Checkbox from "./Checkbox.svelte";
  import { invoke } from "../ipc/invoke";
  import { hostPlatform } from "../stores/platformStore";
  import { closedAppSchedulingReason } from "../ui/platformCopy";
  import { FolderPlus, RefreshCw, Clock, ShieldCheck, X } from "@lucide/svelte";
  import type { CleanerConfig, CleanerInventory, CleanerState, CloneBudget, ReclaimScopes } from "../storage/hygiene/globalTypes";
  import { anyScopeOn, cloneBudgetOf, scopeLabel, scopesOf } from "../storage/hygiene/reclaimConfig";
  import { cleanupAllowed, coverageNotice, itemStatusLabel, runRowsSummary, scheduledWaitNotice } from "../storage/hygiene/globalEligibility";
  import { humanBytes } from "../storage/format";
  import { formatError } from "../ui/formatError";
  import { createAdaptiveTimer } from "../runtime/adaptiveTimer";
  let { active = true }: { active?: boolean } = $props();
  let cleaner = $state<CleanerState | null>(null);
  let draft = $state<CleanerConfig | null>(null);
  let inventory = $state<CleanerInventory | null>(null);
  let rootsText = $state("");
  let exclusionsText = $state("");
  let longLivedText = $state("");
  let dirty = $state(false);
  let busy = $state(false);
  let inspecting = $state(false);
  /** A save/run/inspect/stop failure: stays until the next action, whatever a poll finds. */
  let error = $state<string | null>(null);
  /** The last status read failed. Separate so a good poll clears only this, never an action error unread. */
  let refreshError = $state<string | null>(null);
  let notice = $state<string | null>(null);
  let epoch = 0;
  let live = false;
  let loading = false;
  /** Coverage warning for an inventory that could not read everything; never a refusal. */
  const coverage = $derived(coverageNotice(inventory));
  /** The backend's prefix for a kept folder whose check did not run: not an answer, so not "left alone". */
  const COULD_NOT_CHECK = "could not be checked:";
  // A running Claude session no scratch folder can be matched to: the scan says
  // so once, as its own sentence, so it does not hide in the folders left alone.
  const SESSION_UNMATCHED = "cannot be matched to a session folder";
  const uncheckedCount = $derived(inventory?.kept?.filter((item) => item.reason.startsWith(COULD_NOT_CHECK)).length ?? 0);
  const GIB = 1073741824;
  const IDLE_DAY_CHOICES = [7, 14, 30, 90];
  const CLONE_CHOICES = [2, 4, 8, 16];
  const PRIVATE_BYTE_CHOICES = [0, 50 * GIB, 100 * GIB, 200 * GIB];
  /** The draft's scopes and budget with the backend defaults filled in; the wire omits them while at default. */
  const scopes = $derived(draft ? scopesOf(draft) : null);
  const budget = $derived(draft ? cloneBudgetOf(draft) : null);
  /** The saved policy turns a scope on, but nobody has inspected it yet: no run uses it. */
  const scheduleWait = $derived(cleaner ? scheduledWaitNotice(cleaner) : null);
  const scopesAwaitPreview = $derived(cleaner ? anyScopeOn(scopesOf(cleaner.config)) && !cleaner.scopes_previewed : false);
  /** A saved value outside the presets stays selectable instead of silently reading as the first option. */
  const withCurrent = (choices: number[], current: number) => (choices.includes(current) ? choices : [...choices, current].sort((a, b) => a - b));
  function setScope<K extends keyof ReclaimScopes>(key: K, value: ReclaimScopes[K]) {
    if (!draft) return;
    draft.scopes = { ...scopesOf(draft), [key]: value }; edited();
  }
  function setBudget<K extends keyof CloneBudget>(key: K, value: CloneBudget[K]) {
    if (!draft) return;
    draft.clone_budget = { ...cloneBudgetOf(draft), [key]: value }; edited();
  }
  const paths = (text: string) => text.split("\n").map(p => p.trim()).filter(Boolean);

  function accept(next: CleanerState, reset = false) {
    cleaner = next;
    if (!dirty || reset) {
      draft = { ...next.config, roots: [...next.config.roots], exclusions: [...next.config.exclusions] };
      rootsText = draft.roots.join("\n"); exclusionsText = draft.exclusions.join("\n"); longLivedText = (draft.long_lived_paths ?? []).join("\n"); dirty = false;
    }
  }
  async function refresh() {
    if (loading) return;
    const mine = epoch; loading = true;
    try {
      const next = await invoke<CleanerState>("cmd_cleaner_state");
      if (live && mine === epoch) { accept(next); refreshError = null; }
    } catch (e) { if (live && mine === epoch) refreshError = formatError(e); }
    finally { if (mine === epoch) loading = false; }
  }
  function edited() { dirty = true; inventory = null; }
  async function reloadSaved() {
    if (busy) return;
    const mine = epoch; busy = true; error = null;
    try { const next = await invoke<CleanerState>("cmd_cleaner_state"); if (live && mine === epoch) { accept(next, true); notice = "Reloaded the saved policy. Unsaved edits were discarded."; } }
    catch (e) { if (live && mine === epoch) error = formatError(e); }
    finally { if (live && mine === epoch) busy = false; }
  }
  async function addRoot() {
    try { const path = await invoke<string | null>("cmd_pick_folder"); if (path && live) { rootsText = [...new Set([...paths(rootsText), path])].join("\n"); edited(); } }
    catch (e) { if (live) error = formatError(e); }
  }
  async function save() {
    if (!draft || busy) return;
    const mine = epoch; busy = true; error = null; notice = null;
    const config = { ...draft, roots: paths(rootsText), exclusions: paths(exclusionsText), long_lived_paths: paths(longLivedText) };
    if (config.enabled && config.next_run_at === 0) config.next_run_at = Math.floor(Date.now()/1000) + config.interval_hours * 3600;
    try {
      const next = await invoke<CleanerState>("cmd_cleaner_save", { config });
      if (live && mine === epoch) { accept(next, true); notice = "Policy saved. Changes stop any cleanup already in progress."; }
    } catch (e) { if (live && mine === epoch) error = formatError(e); }
    finally { if (live && mine === epoch) busy = false; }
  }
  async function inspect() {
    if (busy || inspecting || dirty) return;
    const mine = epoch; inspecting = true; error = null;
    try { const next = await invoke<CleanerInventory>("cmd_cleaner_scan"); if (live && mine === epoch) inventory = next; }
    catch (e) { if (live && mine === epoch) error = formatError(e); }
    finally { if (live && mine === epoch) inspecting = false; }
  }
  async function run() {
    if (!cleaner || dirty || busy || cleaner.running || !cleanupAllowed(inventory)) return;
    const mine = epoch; busy = true; error = null;
    try { const next = await invoke<CleanerState>("cmd_cleaner_run", { revision: cleaner.config.revision }); if (live && mine === epoch) { accept(next); inventory = null; notice = "Cleanup started with the saved policy. Every target is inspected again before removal."; } }
    catch (e) { if (live && mine === epoch) error = formatError(e); }
    finally { if (live && mine === epoch) busy = false; }
  }
  async function stop() {
    const mine = epoch; error = null;
    try { const next = await invoke<CleanerState>("cmd_cleaner_cancel"); if (live && mine === epoch) { accept(next); notice = "Cancellation requested. Removed entries cannot be restored."; } }
    catch (e) { if (live && mine === epoch) error = formatError(e); }
  }
  $effect(() => {
    if (!active) return;
    live = true; epoch++; busy = false; inspecting = false; loading = false; void refresh();
    const stopTimer = createAdaptiveTimer(() => { void refresh(); }, 3000);
    return () => { live = false; epoch++; stopTimer(); };
  });
</script>

<section aria-label="Global build cleaner" class="@container space-y-4 text-xs">
  <div class="flex flex-wrap justify-between gap-3">
    <div><h3 class="flex items-center gap-2 font-semibold text-sm text-textPrimary"><ShieldCheck size={16} /> Global build cleaner</h3><p class="mt-1 text-textMuted">Maintain repositories beneath your chosen roots, including repositories that are closed in GitPulse. This policy is the scheduler's own: it keeps its own retention because a sweep runs without this window, and a repository's override in Storage does not change it.</p></div>
    <button class="gp-btn" data-size="sm" onclick={refresh}><RefreshCw size={12} />Refresh status</button>
  </div>
  {#if error}<p role="alert" class="gp-notice" data-tone="danger">{error}</p>{/if}
  {#if refreshError}<p role="alert" class="gp-notice" data-tone="danger">{refreshError}</p>{/if}
  {#if notice}<p role="status" class="text-textSecondary">{notice}</p>{/if}
  {#if draft && cleaner}
    {#if cleaner.background_error}<p role="alert" class="gp-notice" data-tone="warn">Couldn't set up background runs, so the schedule is off. {cleaner.background_error}</p>{/if}
    <fieldset disabled={busy} class="space-y-4 border-0 p-0 m-0 min-w-0">
    {#if !cleaner.supported}<p class="gp-notice" data-tone="warn">Cleanup is unavailable on this platform: removal relies on Unix descriptor-relative deletion that has no Windows implementation, so nothing is deleted here. Inventory and policy guidance remain available.</p>{/if}
    <div class="grid gap-3 @2xl:grid-cols-2">
      <label class="space-y-1">Repository roots · one absolute directory per line<textarea aria-label="Repository roots" class="gp-field-multi block w-full font-mono" rows="3" bind:value={rootsText} oninput={edited} disabled={busy}></textarea></label>
      <label class="space-y-1">Excluded directories · one absolute directory per line<textarea aria-label="Excluded directories" class="gp-field-multi block w-full font-mono" rows="3" bind:value={exclusionsText} oninput={edited} disabled={busy}></textarea></label>
    </div>
    <label class="space-y-1 block">Long-lived data to list · one absolute directory per line (listed with its size, never cleaned)<textarea aria-label="Long-lived data directories" class="gp-field-multi block w-full font-mono" rows="2" bind:value={longLivedText} oninput={edited} disabled={busy}></textarea></label>
    <button class="gp-btn" data-size="sm" onclick={addRoot} disabled={busy}><FolderPlus size={12} />Add repository root</button>
    <div class="flex flex-wrap gap-4">
      <label>Sweeps keep output modified within <select aria-label="Global retention" class="gp-select ml-1" bind:value={draft.retention_days} onchange={edited}><option value={7}>7 days</option><option value={14}>14 days</option><option value={30}>30 days</option><option value={90}>90 days</option></select></label>
      <label>Per-run byte budget <select aria-label="Cleanup byte budget" class="gp-select ml-1" bind:value={draft.max_run_bytes} onchange={edited}><option value={1073741824}>1 GiB</option><option value={10737418240}>10 GiB</option><option value={53687091200}>50 GiB</option><option value={107374182400}>100 GiB</option></select></label>
      <label>Maximum directories <input aria-label="Maximum cleanup directories" class="gp-field ml-1 w-16" type="number" min="1" max="100" bind:value={draft.max_targets} oninput={edited} /></label>
    </div>
    <div class="gp-card p-3 space-y-2">
      <label class="flex items-center gap-2"><Checkbox ariaLabel="Enable scheduled cleanup" checked={draft.enabled} disabled={!cleaner.supported} onchange={(next) => { if (draft) { draft.enabled = next; edited(); } }} /><Clock size={13} />Automatically remove eligible output on schedule</label>
      <label>Repeat every <select aria-label="Cleanup interval" class="gp-select mx-1" bind:value={draft.interval_hours} onchange={() => { if (draft) draft.next_run_at = 0; edited(); }}><option value={24}>day</option><option value={168}>week</option><option value={720}>30 days</option></select></label>
      <label class="flex items-center gap-2"><Checkbox ariaLabel="Run cleanup when GitPulse is closed" checked={draft.run_when_closed} disabled={!cleaner.background_supported} onchange={(next) => { if (draft) { draft.run_when_closed = next; edited(); } }} />Also run when GitPulse is closed</label>
      <p class="text-textMuted">{closedAppSchedulingReason($hostPlatform.os, cleaner.background_supported)} Otherwise, schedules run while GitPulse is open, including in the background. A missed run is attempted once after restart or wake. Scheduling is off until you save it here.</p>
      <p class="text-textMuted">Next scheduled run: {cleaner.config.enabled ? new Date(cleaner.config.next_run_at * 1000).toLocaleString() : "Off"}</p>
    </div>
    {#if scopes && budget}
    <div class="gp-card p-3 space-y-2" aria-label="Reclaim more">
      <strong>Reclaim more (off by default)</strong>
      {#if scheduleWait}<p role="status" class="gp-notice" data-tone="warn" data-testid="scheduled-wait">{scheduleWait}</p>{/if}
      {#if scopesAwaitPreview}<p role="status" class="gp-notice" data-tone="warn">No run, scheduled or manual, uses these scopes until you inspect saved roots with this exact set. Saving a change to them clears that.</p>{/if}
      <label class="flex items-center gap-2"><Checkbox ariaLabel="Reclaim build caches in idle linked worktrees" checked={scopes.worktree_caches} onchange={(next) => setScope("worktree_caches", next)} />Reclaim build caches in idle linked worktrees</label>
      <p class="text-textMuted">Deletes regenerable build caches (<span class="font-mono">target</span>, <span class="font-mono">.turbo</span>) inside linked worktrees that no process is using, whose tree is clean and whose cache has been untouched for the idle period below. The worktree and its source stay.</p>
      <label class="flex items-center gap-2"><Checkbox ariaLabel="Judge uncommitted work per directory, not per repository" checked={scopes.per_tree_vetoes} onchange={(next) => setScope("per_tree_vetoes", next)} />Judge uncommitted work per directory, not per repository</label>
      <p class="text-textMuted">Makes uncommitted changes protect only the directory that produced the output, so an unrelated dirty file elsewhere in a monorepo no longer blocks cleaning it. This can make more output eligible for permanent removal.</p>
      <label class="flex items-center gap-2"><Checkbox ariaLabel="Remove stale linked worktrees" checked={scopes.prune_worktrees} onchange={(next) => setScope("prune_worktrees", next)} />Remove stale linked worktrees</label>
      <p class="text-textMuted">Removes the whole worktree directory, source files included, when no process is inside it, it is clean and unlocked, and its commits are merged or idle past the period below. Branches are not deleted. Unpushed work in a clean worktree on its own branch is only protected if that branch is merged or idle.</p>
      <label>Idle period for these scopes <select aria-label="Worktree idle days" class="gp-select ml-1" value={scopes.worktree_idle_days} onchange={(e) => setScope("worktree_idle_days", Number(e.currentTarget.value))}>{#each withCurrent(IDLE_DAY_CHOICES, scopes.worktree_idle_days) as days}<option value={days}>{days} days</option>{/each}</select></label>
      <label class="flex items-center gap-2"><Checkbox ariaLabel="Remove agent scratch builds" checked={scopes.agent_scratch_builds} onchange={(next) => setScope("agent_scratch_builds", next)} />Remove agent scratch builds</label>
      <p class="text-textMuted">Deletes Cargo build folders that agents leave in the scratchpad of a finished session, in scratch directories inside a repository that git ignores, and Cargo targets directly under a checkout's .claude folder (including the copies a new session worktree is seeded with). A folder qualifies only when Cargo made it, no running Claude session has its id, no process uses it, and nothing in it changed within the period below. The scratchpad and every other file in it stay. Temp directories nobody can account for are listed and never removed. A run applies these rules to what it finds then: folders that newly match since your inspection are included, and folders inside an excluded folder are not. Finished session scratchpads live in the system temp directory, so this scope is not limited to the roots above. Its approval lapses 30 days after your last inspection; a run after that skips it and says so.</p>
      <label>Period for agent scratch builds <select aria-label="Agent scratch retention days" class="gp-select ml-1" value={scopes.agent_scratch_days} onchange={(e) => setScope("agent_scratch_days", Number(e.currentTarget.value))}>{#each withCurrent(IDLE_DAY_CHOICES, scopes.agent_scratch_days) as days}<option value={days}>{days} days</option>{/each}</select></label>
    </div>
    <div class="gp-card p-3 space-y-2" aria-label="Build-cache clones">
      <strong>Build-cache clones</strong>
      <label class="block">Worktrees that may hold a cloned Cargo target <select aria-label="Maximum cloned targets" class="gp-select ml-1" value={budget.max_target_clones} onchange={(e) => setBudget("max_target_clones", Number(e.currentTarget.value))}>{#each [...withCurrent(CLONE_CHOICES, budget.max_target_clones).filter(n => n !== 0), 0] as n}<option value={n}>{n === 0 ? "No limit" : n}</option>{/each}</select></label>
      <p class="text-textMuted">Past this many in one repository, a new worktree is created without a cloned target and says why. A clone shares its blocks with the worktree it was cloned from and costs little disk until it is rebuilt; a worktree without one builds cold, which costs far more, so the default is no limit.</p>
      <label class="block">Advisory total for linked worktrees' private bytes <select aria-label="Advisory private bytes" class="gp-select ml-1" value={budget.max_private_bytes} onchange={(e) => setBudget("max_private_bytes", Number(e.currentTarget.value))}>{#each withCurrent(PRIVATE_BYTE_CHOICES, budget.max_private_bytes) as bytes}<option value={bytes}>{bytes === 0 ? "No limit" : humanBytes(bytes)}</option>{/each}</select></label>
      <p class="text-textMuted">Storage shows a notice when linked worktrees would free more than this. It never deletes anything or blocks creation.</p>
    </div>
    {/if}
    <p class="text-textMuted">Only ignored output with recognized producer evidence is eligible. Dirty repositories, active tasks, recent files, symlinks, environments, dependencies and persistent state are preserved. Shared package caches and folders that are not Git repositories are excluded from scheduled sweeps.</p>
    </fieldset>
    <div class="flex flex-wrap gap-2">
      <button class="gp-btn" data-variant="primary" onclick={save} disabled={!dirty || busy}>{busy ? "Working…" : "Save cleanup policy"}</button>
      <button class="gp-btn" onclick={inspect} disabled={dirty || busy || inspecting || cleaner.running || !cleaner.config.roots.length}><RefreshCw size={12} />{inspecting ? "Inspecting roots…" : "Inspect saved roots"}</button>
      {#if cleaner.running || inspecting}<button class="gp-btn" onclick={stop}><X size={12} />Stop {inspecting ? "inspection" : "cleanup"}</button>{/if}
      {#if dirty}<button class="gp-btn" onclick={reloadSaved} disabled={busy}>Discard changes</button><span class="self-center text-warn">Unsaved policy · inspect and run after saving</span>{/if}
    </div>
    {#if inventory}
      <div class="gp-card space-y-2 p-3">
        <strong>{inventory.repositories} repositories · {inventory.candidates.length} output candidates · {inventory.visited_entries.toLocaleString()} entries inspected</strong>
        {#if coverage}<p class="gp-notice" data-tone="warn">{coverage}</p>{/if}
        {#each inventory.issues as issue}<p class="gp-notice break-all" data-tone="warn" data-testid={issue.includes(SESSION_UNMATCHED) ? "session-unmatched" : undefined}>{issue}</p>{/each}
        <div class="max-h-52 overflow-y-auto">{#each inventory.candidates as row (`${row.repo_path}/${row.path}`)}<div class="flex justify-between gap-3 py-2"><div class="min-w-0"><p class="font-mono break-all">{row.path.startsWith("/") ? row.path : `${row.repo_path}/${row.path}`}</p><p class="text-textMuted"><span class="gp-chip" data-kind="label" data-testid="candidate-scope">{scopeLabel(row.scope)}</span> {row.provider}</p>{#if row.evidence?.length}<p class="text-textMuted break-all" data-testid="candidate-evidence">{row.evidence.join(" · ")}</p>{/if}</div><span class="shrink-0 text-right">{humanBytes(row.bytes)}{#if row.private_bytes !== undefined}<span class="block text-textMuted">frees {humanBytes(row.private_bytes)}</span>{/if}</span></div>{/each}</div>
        {#if inventory.kept?.length}
          <div class="space-y-1" data-testid="kept-items">
            <strong>{inventory.kept.length - uncheckedCount} agent scratch folder{inventory.kept.length - uncheckedCount === 1 ? "" : "s"} left alone</strong>
            {#if uncheckedCount > 0}<p class="gp-notice" data-tone="warn" data-testid="unchecked-count">Couldn't check {uncheckedCount} folder{uncheckedCount === 1 ? "" : "s"}. They were kept, but nothing was learned about them; inspect again.</p>{/if}
            <div class="max-h-40 overflow-y-auto">{#each inventory.kept as item (`${item.scope}/${item.path}`)}<p class="py-1"><span class="font-mono break-all">{item.path}</span><span class="block text-textMuted" data-unchecked={item.reason.startsWith(COULD_NOT_CHECK) ? "true" : undefined}>{item.reason}</span></p>{/each}</div>
          </div>
        {/if}
        {#if inventory.leaked_temp?.length}
          <div class="space-y-1" data-testid="leaked-temp">
            <strong>Temp directories nobody can account for (listed only, never removed)</strong>
            {#each inventory.leaked_temp as leak (leak.path)}<p class="py-1"><span class="font-mono break-all">{leak.path}</span><span class="block text-textMuted">{leak.bytes === null ? "size not fully measured" : humanBytes(leak.bytes)} · {leak.age_days} days old · {leak.reason}</span></p>{/each}
          </div>
        {/if}
        {#if inventory.long_lived?.length}
          <div class="space-y-1" data-testid="long-lived">
            <strong>Long-lived data · not cleaned by GitPulse</strong>
            <p class="text-textMuted">Where space is held by data this cleaner never touches. Listed so a full disk has an explanation; manage it with the tool that owns it.</p>
            {#each inventory.long_lived as item (item.path)}<p class="py-1"><span class="text-textMuted">{item.label}</span> <span class="font-mono break-all">{item.path}</span><span class="block text-textMuted" data-testid="long-lived-size">{item.error ? item.error :item.bytes === undefined || item.bytes === null ? "size unknown" : `${item.at_least ? "at least " : ""}${humanBytes(item.bytes)}`}</span></p>{/each}
          </div>
        {/if}
        <p class="text-textMuted">These are current logical sizes. Each file is counted once, and only files wholly inside the folder count. Fresh retention, index, activity and policy checks decide which candidates can be removed, up to the saved limits. Removal is permanent.</p>
        <button class="gp-btn" data-variant="danger" onclick={run} disabled={dirty || busy || cleaner.running || !cleaner.supported || !cleanupAllowed(inventory)}>Clean eligible output now</button>
      </div>
    {/if}
    {#if cleaner.history.length}
      <details open><summary class="font-semibold cursor-pointer">Recent cleanup runs ({cleaner.history.length})</summary>
        <div class="max-h-72 overflow-y-auto space-y-3 mt-2">{#each cleaner.history as run (run.id)}<div class="gp-card p-3 space-y-1"><strong>{run.status.replaceAll("_", " ")} · {run.trigger}</strong><p class="text-textMuted">{new Date(run.started_at*1000).toLocaleString()} · {run.repositories} repositories · {runRowsSummary(run)}</p>{#each run.issues as issue}<p class="gp-notice break-all" data-tone="warn">{issue}</p>{/each}{#each run.items as item}<div class="pt-2"><p class="font-mono break-all">{item.repo_path}/{item.path} · {itemStatusLabel(item.status)}</p><p>{item.message}</p><p class="text-textMuted" data-testid="item-sizes">{#if item.status === "partially_removed"}Was {humanBytes(item.bytes_before)} · the rest was left in place; its size was not measured{:else}Before {humanBytes(item.bytes_before)} · After {item.bytes_after === null ? "unavailable" : humanBytes(item.bytes_after)}{/if}</p></div>{/each}</div>{/each}</div>
      </details>
    {/if}
    <details><summary class="cursor-pointer">DevCouncil agent hygiene rules</summary><p class="mt-2 whitespace-pre-wrap text-textMuted">{cleaner.agent_rules}</p></details>
  {:else if refreshError}
    <div class="flex items-center gap-3"><p class="text-textMuted">Couldn't read the cleanup policy.</p><button class="gp-btn" data-size="sm" onclick={refresh}><RefreshCw size={12} />Try again</button></div>
  {:else}<p class="text-textMuted">Loading saved cleanup policy…</p>{/if}
</section>
