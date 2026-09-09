<script lang="ts">
  import { onDestroy, untrack } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { repoStore } from "../stores/repoStore";
  import { interfaceStore } from "../stores/interfaceStore";
  import { consumeTaskTerminal, enqueueTaskTerminal } from "../terminal/taskLaunches";
  import AgentDecisions from "./AgentDecisions.svelte";
  import { cancelTaskRun, explainError, getRepository, getTaskRun, launchManagedRun, stopManagedRun, listTaskRuns, newID, PERMISSION_MODES, prepareTaskRun, WorkbenchError, type PermissionMode, type Repository, type RunPreparation, type RunKind, type Task, type TaskRun } from "../workbench/client";

  let { task, repositories, disabled = false, active = true }: { task: Task; repositories: Repository[]; disabled?: boolean; active?: boolean } = $props();
  let provider = $state<"claude" | "codex">("codex");
  let permission = $state<PermissionMode>("ask");
  let kind = $state<RunKind>("external_terminal");
  let detail = $state<TaskRun | null>(null);
  let acknowledged = $state(false);
  let selectedRepository = $state("");
  let checkout = $state("");
  let pending = $state<RunPreparation | null>(null);
  let busy = $state(false);
  let loading = $state(false);
  let error = $state("");
  let historyError = $state("");
  let note = $state("");
  let runs = $state<TaskRun[]>([]);
  let total = $state(0);
  let cursor = $state<string | null>(null);
  let reviewingRunID = $state<string | null>(null), decisionRefresh = $state(0);
  let disposed = false;
  let generation = 0;
  let timer: ReturnType<typeof setTimeout> | null = null;
  const labels: Record<PermissionMode, string> = { inspect: "Inspect and plan", ask: "Ask for permissions", edit: "Allow workspace edits", auto_review: "Provider automatic review", preapproved: "Preapproved actions only", bypass: "Bypass permissions (advanced)" };
  const repositoryID = $derived(task.repository_ids.includes(selectedRepository) ? selectedRepository : task.primary_repository_id);
  const controlDescription = $derived(provider === "codex"
    ? ({ inspect: "Read-only sandbox; permission requests are denied.", ask: "Read-only sandbox; the agent can ask you to approve an escalation.", edit: "Workspace-write sandbox; escalation requests appear in Codex.", auto_review: "Codex reviews escalation requests automatically.", preapproved: "Workspace-write sandbox; further permission requests are denied.", bypass: "Codex bypasses approval checks and its sandbox for this attempt." }[permission])
    : ({ inspect: "Claude Code starts in plan mode.", ask: "Claude Code starts in manual permission mode.", edit: "Claude Code starts in accept-edits mode.", auto_review: "Claude Code starts in automatic permission mode, if available for your account.", preapproved: "Claude Code denies actions that would require a permission prompt.", bypass: "Claude Code bypasses permission checks for this attempt." }[permission]));

  function schedule() {
    if (timer) clearTimeout(timer);
    timer = null;
    if (!disposed && active && document.visibilityState !== "hidden" && runs.some((run) => ["starting", "running"].includes(run.state) || (run.state === "prepared" && run.expires_at * 1000 > Date.now()))) {
      timer = setTimeout(() => { timer = null; void refresh(); }, 1500);
    }
  }
  async function refresh(more = false) {
    if (loading || disposed || !active) return;
    loading = true;
    const ticket = generation;
    try {
      const page = await listTaskRuns(task.id, more ? cursor ?? undefined : undefined);
      if (disposed || ticket !== generation) return;
      runs = more ? [...new Map([...runs, ...page.items].map((run) => [run.id, run])).values()] : page.items;
      total = page.total; cursor = page.next_cursor;
      decisionRefresh += 1;
      historyError = "";
    } catch (cause) { if (!disposed && ticket === generation) historyError = explainError(cause); }
    finally { loading = false; schedule(); }
  }
  $effect(() => {
    if (!active) { if (timer) clearTimeout(timer); timer = null; return; }
    untrack(() => { void refresh(); });
    const wake = () => { if (document.visibilityState !== "hidden") void refresh(); else if (timer) { clearTimeout(timer); timer = null; } };
    document.addEventListener("visibilitychange", wake);
    return () => { document.removeEventListener("visibilitychange", wake); if (timer) clearTimeout(timer); timer = null; };
  });
  onDestroy(() => { disposed = true; generation += 1; if (timer) clearTimeout(timer); });

  async function browse() {
    try { const path = await invoke<string | null>("cmd_pick_folder"); if (path && !disposed) checkout = path; }
    catch (cause) { error = explainError(cause); }
  }
  async function open(run: TaskRun) {
    error = "";
    let queued = false;
    const opened = await repoStore.openRepo(run.cwd, { onReady: (path) => {
      enqueueTaskTerminal({ runId: run.id, repoPath: path, provider: run.provider, title: run.task_title });
      queued = true;
      interfaceStore.setGlobalSurface("repository");
      interfaceStore.setTerminalDockOpen(true);
    } });
    if (!opened || !queued) throw new Error("The checkout did not finish opening. The attempt remains available below; open it again or cancel its preparation.");
  }
  async function launch() {
    if (busy || disabled) return;
    busy = true; error = ""; note = "";
    try {
      if (!pending) {
        const source = { id: task.id, revision: task.revision, repository: repositoryID, checkout, provider, permission, acknowledged, kind };
        const repo = await getRepository(source.repository);
        if (disposed) return;
        if (task.id !== source.id || task.revision !== source.revision) throw new Error("The saved task changed while preparing this launch. Review the latest revision and launch again.");
        pending = { kind: source.kind, id: newID(), request_id: newID(), task_id: source.id, source_revision: source.revision, repository_id: repo.id, repository_revision: repo.revision, repo_path: source.checkout, provider: source.provider, permission_mode: source.permission, acknowledge_bypass: source.acknowledged };
      }
      const run = await prepareTaskRun(pending);
      pending = null; permission = "ask"; acknowledged = false;
      if (disposed) return;
      runs = [run, ...runs.filter((item) => item.id !== run.id)];
      if (run.kind === "managed") {
        await manage(run);
      } else {
        note = `Prepared saved revision ${run.source_revision}. Opening its terminal…`;
        await open(run);
        note = `Terminal requested for saved revision ${run.source_revision}.`;
      }
    } catch (cause) {
      error = explainError(cause);
      if (cause instanceof WorkbenchError && !["transport_error", "worker_error", "store_error"].includes(cause.code)) pending = null;
    } finally { busy = false; schedule(); }
  }
  async function cancel(run: TaskRun) {
    busy = true; error = "";
    try { const saved = await cancelTaskRun(run); consumeTaskTerminal(run.id); runs = runs.map((item) => item.id === saved.id ? saved : item); }
    catch (cause) { error = explainError(cause); }
    finally { busy = false; schedule(); }
  }
  async function manage(run: TaskRun) {
    const current = await launchManagedRun(run.id);
    if (disposed) return;
    runs = runs.map((item) => item.id === current.id ? current : item);
    reviewingRunID = current.id;
    note = current.state === "running" ? "Managed Codex started. Review its requests below." : `Managed attempt: ${current.state}.`;
    schedule();
  }
  async function resumeManaged(run: TaskRun) {
    busy = true; error = "";
    try { await manage(run); } catch (cause) { error = explainError(cause); }
    finally { busy = false; schedule(); }
  }
  async function stopManaged(run: TaskRun) {
    busy = true; error = "";
    try { await stopManagedRun(run.id); note = "Stop requested. Waiting for process confirmation."; }
    catch (cause) { error = explainError(cause); }
    finally { busy = false; schedule(); }
  }
  async function inspect(run: TaskRun) {
    busy = true; error = "";
    try { const current = await getTaskRun(run.id); if (!disposed) detail = current; }
    catch (cause) { error = explainError(cause); }
    finally { busy = false; }
  }
  async function reopen(run: TaskRun) { busy = true; try { await open(run); } catch (cause) { error = explainError(cause); } finally { busy = false; } }
</script>

<section class="task-runs" aria-label="Task agent runs">
  <h3>Agent runs</h3>
  <p>Launch saved revision {task.revision}. Choose a terminal handoff or a managed connection with requests and receipts here. Completing a run leaves task acceptance for review.</p>
  <fieldset disabled={disabled || busy || pending !== null}>
    <label>Repository<select bind:value={selectedRepository}><option value="">Primary repository</option>{#each task.repository_ids as id (id)}<option value={id}>{repositories.find((repo) => repo.id === id)?.name ?? id}</option>{/each}</select></label>
    <label>Checkout path<input bind:value={checkout} placeholder="Choose this repository’s working checkout" /><button type="button" onclick={browse}>Browse…</button></label>
    <label>Coding agent<select bind:value={provider} onchange={() => { if (provider === "claude") kind = "external_terminal"; }}><option value="codex">Codex</option><option value="claude">Claude Code</option></select></label>
    <label>Connection<select bind:value={kind}><option value="external_terminal">Provider terminal</option><option value="managed" disabled={provider !== "codex"}>Managed Codex</option></select></label>
    {#if provider === "claude"}<p>Claude Code currently supports terminal handoffs.</p>{/if}
    <label>Permission mode<select bind:value={permission} onchange={() => { acknowledged = false; }}>{#each PERMISSION_MODES as mode}<option value={mode}>{labels[mode]}</option>{/each}</select></label>
    <p>{controlDescription} These are requested settings. {kind === "managed" ? "Manvi verifies the effective Codex settings before sending the task; supported requests appear here." : "The provider handles requests in its terminal."}</p>
    {#if permission === "bypass"}<label class="bypass"><input type="checkbox" bind:checked={acknowledged} />I authorize bypass for this launch attempt. Worktrees do not isolate host access.</label>{/if}
  </fieldset>
  <button type="button" onclick={launch} disabled={disabled || busy || (!pending && (!checkout || (permission === "bypass" && !acknowledged)))}>{busy ? "Preparing…" : pending ? "Retry preparation" : kind === "managed" ? "Start managed Codex" : `Launch in ${provider === "claude" ? "Claude Code" : "Codex"}`}</button>
  {#if disabled}<p>Save task edits before launching.</p>{/if}
  {#if pending}<p>The preparation result is uncertain. Retry this exact attempt before creating another.</p>{/if}
  {#if note}<p role="status">{note}</p>{/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  <div class="history-heading"><h4>Run history</h4><button type="button" onclick={() => refresh()} disabled={loading}>Refresh</button></div>
  {#if historyError}<p class="error" role="alert">{historyError}</p>{/if}
  {#if !runs.length}<p>{loading ? "Loading runs…" : "No runs yet."}</p>{/if}
  {#each runs as run (run.id)}
    <article>
      <strong>{run.provider === "claude" ? "Claude Code" : "Codex"} · {run.kind === "managed" ? "Managed" : "Terminal"} · {run.state === "exited" ? "Process exited" : run.state}</strong>
      <small>Revision {run.source_revision} · {labels[run.permission_mode]}{run.exit_code !== null ? ` · exit ${run.exit_code}` : ""}</small>
      <small title={run.cwd}>{run.cwd}</small>
      {#if run.reason}<p>{run.reason}</p>{/if}
      {#if run.outcome_uncertain}<p class="error">Execution remains unresolved. Reconcile it before another attempt in this repository.</p>{/if}
      {#if run.kind === "external_terminal" && ["prepared", "starting", "running"].includes(run.state)}<button type="button" onclick={() => reopen(run)} disabled={busy}>Open terminal</button>{/if}
      {#if run.kind === "managed"}
        {#if ["prepared", "starting"].includes(run.state)}<button type="button" onclick={() => resumeManaged(run)} disabled={busy}>Start or recover managed launch</button>{/if}
        {#if ["starting", "running", "unresolved"].includes(run.state)}<button type="button" onclick={() => stopManaged(run)} disabled={busy}>Stop managed run</button>{/if}
        {#if run.provider_state}<p>Provider turn: {run.provider_state}. {run.provider_state === "completed" ? "Review the result before accepting the task." : ""}</p>{/if}
        <button type="button" onclick={() => inspect(run)} disabled={busy}>View output and settings</button>
        {#if detail?.id === run.id}
          <section aria-label="Managed run details">
            <p>Saved output · revision {detail.revision}</p>
            <textarea readonly rows="7" aria-label="Managed agent output" value={detail.output ?? "No output saved yet."}></textarea>
            {#if detail.output_truncated}<p>Output reached the 128 KiB retention limit.</p>{/if}
            <details><summary>Effective provider settings</summary><textarea readonly rows="7" aria-label="Effective provider settings" value={detail.effective_configuration ?? "Settings are not recorded yet."}></textarea></details>
            <button type="button" onclick={() => inspect(run)} disabled={busy}>Refresh output</button><button type="button" onclick={() => { detail = null; }}>Close details</button>
          </section>
        {/if}
      {/if}
      {#if run.state === "prepared"}<button type="button" onclick={() => cancel(run)} disabled={busy}>Cancel preparation</button>{/if}
      {#if run.session_id}<button type="button" onclick={() => { reviewingRunID = reviewingRunID === run.id ? null : run.id; }}>{reviewingRunID === run.id ? "Hide requests" : "Review requests"}</button>{/if}
      {#if reviewingRunID === run.id}<AgentDecisions {run} {active} refreshToken={decisionRefresh} />{/if}
    </article>
  {/each}
  {#if cursor}<button type="button" onclick={() => refresh(true)} disabled={loading || runs.length >= 180}>Load more ({runs.length} of {total})</button>{/if}
</section>

<style>
  .task-runs{border-top:1px solid rgb(var(--c-border));padding-top:16px;margin-top:16px;font-size:12px}h3,h4{margin:0 0 10px;font-weight:650}p{color:rgb(var(--c-text-muted));line-height:1.5;margin:8px 0}fieldset{border:0;padding:0;min-width:0}label{display:flex;flex-direction:column;gap:6px;margin:12px 0}input,select,textarea{min-width:0;width:100%;padding:7px;border:1px solid rgb(var(--c-border));border-radius:6px;background:rgb(var(--c-bg));color:inherit}button{padding:6px 9px;border:1px solid rgb(var(--c-border));border-radius:6px}button:disabled{opacity:.5}.bypass{flex-direction:row;align-items:flex-start}.bypass input{width:auto}.history-heading{display:flex;align-items:center;justify-content:space-between;margin-top:18px}article{padding:10px 0;border-bottom:1px solid rgb(var(--c-border))}small{display:block;color:rgb(var(--c-text-muted));margin:5px 0;overflow-wrap:anywhere}.error{color:#dc6565}
</style>
