<script lang="ts">
  import { onDestroy, untrack } from "svelte";
  import { decisionWrite, decisionQuestions, structuredDecisionAnswer, explainError, listAgentDecisions, saveAgentDecision, WorkbenchError, type AgentDecision, type DecisionQuestion, type DecisionWrite, type TaskRun } from "../workbench/client";
  let { run, refreshToken = 0, active = true }: { run: TaskRun; refreshToken?: number; active?: boolean } = $props();
  let rows = $state<AgentDecision[]>([]), total = $state(0), cursor = $state<string | null>(null);
  let loading = $state(false), busy = $state(false), error = $state("");
  let answers = $state<Record<string, string>>({});
  let questions = $state<Record<string, DecisionQuestion[]>>({});
  let structured = $state<Record<string, Record<string, string>>>({});
  let pending = $state<{ source: AgentDecision; input: DecisionWrite } | null>(null);
  let disposed = false;
  const labels = { pending: "Needs review", decided: "Decision saved · delivery pending", dispatching: "Delivery attempted · awaiting provider confirmation", resolved: "Provider confirmed resolution", cancelled: "Request cancelled" };
  async function refresh(more = false) {
    if (loading || busy || pending || disposed || !active) return;
    loading = true;
    try {
      const page = await listAgentDecisions(run.id, more ? cursor ?? undefined : undefined);
      if (disposed) return;
      if (page.items.some((item) => item.task_id !== run.task_id || item.repository_id !== run.repository_id || item.source_revision !== run.source_revision || item.session_id !== run.session_id)) throw new WorkbenchError("protocol_error", "A request does not belong to this run.");
      const sets = Object.fromEntries(page.items.map((item) => [item.id, decisionQuestions(item)]));
      questions = more ? {...questions, ...sets} : sets;
      rows = more ? [...new Map([...rows, ...page.items].map((item) => [item.id, item])).values()] : page.items;
      total = page.total; cursor = page.next_cursor; error = "";
    } catch (cause) { if (!disposed) error = explainError(cause); }
    finally { if (!disposed) loading = false; }
  }
  $effect(() => { void refreshToken; if (active) untrack(() => { void refresh(); }); });
  onDestroy(() => { disposed = true; });
  async function decide(source: AgentDecision, choice: DecisionWrite["decision"]) {
    if (busy || !active) return;
    busy = true; error = "";
    try {
      if (!pending) {
        const answer = choice === "answer" ? questions[source.id]?.length ? structuredDecisionAnswer(source, structured[source.id] ?? {}) : answers[source.id] : undefined;
        pending = { source, input: await decisionWrite(source, choice, answer) };
      }
      if (disposed) return;
      const result = await saveAgentDecision(pending.source, pending.input);
      if (disposed) return;
      rows = rows.map((row) => row.id === result.id ? result : row); pending = null;
    } catch (cause) {
      if (!disposed) {
        error = explainError(cause);
        if (cause instanceof WorkbenchError && !["transport_error", "worker_error", "store_error", "protocol_error", "timeout"].includes(cause.code)) pending = null;
      }
    } finally { if (!disposed) busy = false; }
  }
</script>

<section class="agent-decisions" aria-label="Agent requests">
  <div class="heading"><h4>Agent requests</h4><button type="button" onclick={() => refresh()} disabled={loading || busy || !!pending}>Refresh requests</button></div>
  <p>Decisions apply to the exact request below. Saving a decision does not prove that the provider received it or accept the task.</p>
  <p>{rows.length} shown / {total} requests</p>
  {#if error}<p role="alert">{error}</p>{/if}
  {#if pending}<p role="status">The save result is uncertain. Retry this exact decision before choosing another.</p><button type="button" onclick={() => { if (pending) void decide(pending.source, pending.input.decision); }} disabled={busy}>Retry saved decision</button>{/if}
  {#if !rows.length}<p>{loading ? "Loading requests…" : "No structured requests recorded. Terminal handoffs continue to ask in the provider’s terminal."}</p>{/if}
  {#each rows as item (item.id)}
    <article>
      <strong>{item.kind === "permission" ? "Permission request" : "Question"} · {labels[item.state]}</strong>
      <p>Repository {item.repository_id} · saved task revision {item.source_revision} · policy revision {item.policy_revision}</p>
      <p>{item.cwd}</p>
      <textarea class="payload" readonly rows="8" aria-label="Complete agent request" value={item.payload}></textarea>
      <details><summary>Request identity</summary><p>Run {item.run_id} · session {item.session_id}</p><p>Provider task {item.provider_thread_id} · turn {item.provider_turn_id} · request {item.protocol_request_id}</p><p>SHA-256 {item.payload_digest}</p></details>
      <p>Expires {new Date(item.expires_at * 1000).toLocaleString()}</p>
      {#if item.reason}<p>{item.reason}</p>{/if}
      {#if item.decision}<p>Saved response: {item.decision === "allow_once" ? "Allow once" : item.decision === "deny" ? "Deny" : item.answer}</p>{/if}
      {#if item.state === "pending" && item.actionable}
        <fieldset disabled={busy || !!pending || !active || Date.now() >= item.expires_at * 1000}>
          {#if item.kind === "question"}
            {#if questions[item.id]?.length}
              {#each questions[item.id] as question (question.id)}
                <label>{question.question}<textarea value={structured[item.id]?.[question.id] ?? ""} oninput={(event) => { structured[item.id] = {...structured[item.id], [question.id]:event.currentTarget.value}; }} maxlength="4096" rows="3"></textarea></label>
                {#each question.options as option}
                  <button type="button" title={option.description} onclick={() => { structured[item.id] = {...structured[item.id], [question.id]:option.label}; }}>{option.label}</button>
                {/each}
              {/each}
              <button type="button" onclick={() => decide(item, "answer")} disabled={!questions[item.id].every((q) => structured[item.id]?.[q.id]?.trim())}>Save answers</button>
            {:else}
              <label>Answer<textarea bind:value={answers[item.id]} maxlength="16384" rows="3"></textarea></label><button type="button" onclick={() => decide(item, "answer")} disabled={!answers[item.id]?.trim()}>Save answer</button>
            {/if}
          {:else}<button type="button" onclick={() => decide(item, "allow_once")}>Allow once</button>{/if}
          <button type="button" onclick={() => decide(item, "deny")}>Deny request</button>
        </fieldset>
      {:else if item.state === "pending"}<p>This request expired or its target changed. Return to the provider for a current request.</p>{/if}
    </article>
  {/each}
  {#if cursor}<button type="button" onclick={() => refresh(true)} disabled={loading || busy || !!pending || rows.length >= 180}>Load more requests</button>{/if}
</section>

<style>
  .agent-decisions{margin:12px 0;padding:12px;border:1px solid rgb(var(--c-border));border-radius:8px}.heading{display:flex;align-items:center;justify-content:space-between;gap:8px}h4{margin:0}p{color:rgb(var(--c-text-muted));line-height:1.5;overflow-wrap:anywhere}article{border-top:1px solid rgb(var(--c-border));padding:12px 0}.payload{font-family:monospace;max-height:280px;overflow:auto;white-space:pre-wrap;overflow-wrap:anywhere;background:rgb(var(--c-bg));padding:10px;border-radius:6px;font-size:11px}button{font:inherit;padding:6px 9px;border:1px solid rgb(var(--c-border));border-radius:6px;margin:4px 6px 4px 0}button:disabled{opacity:.5}fieldset{padding:0;border:0}label{display:grid;gap:6px}textarea{font:inherit;width:100%;color:inherit;background:rgb(var(--c-bg));border:1px solid rgb(var(--c-border));border-radius:6px;padding:8px}summary{cursor:pointer}[role=alert]{color:#dc6565}
</style>
