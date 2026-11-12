<script lang="ts">
  import { onDestroy, untrack } from "svelte";
  import { describeFailure } from "../ui/copy/describeFailure";
  import EmptyState from "./EmptyState.svelte";
  import Skeleton from "./Skeleton.svelte";
  import { MessageSquare } from "@lucide/svelte";
  import { mergeLoadedPage } from "../workbench/pageMerge";
  import { decisionWrite, decisionQuestions, structuredDecisionAnswer, listAgentDecisions, listPendingDecisions, saveAgentDecision, WorkbenchError, type AgentDecision, type DecisionQuestion, type DecisionWrite, type TaskRun } from "../workbench/client";
  let { run, refreshToken = 0, active = true }: { run: TaskRun; refreshToken?: number; active?: boolean } = $props();
  let rows = $state<AgentDecision[]>([]), total = $state(0), cursor = $state<string | null>(null);
  let loading = $state(false), busy = $state(false), error = $state("");
  let answers = $state<Record<string, string>>({});
  let questions = $state<Record<string, DecisionQuestion[]>>({});
  let structured = $state<Record<string, Record<string, string>>>({});
  let pending = $state<{ source: AgentDecision; input: DecisionWrite } | null>(null);
  // Set once a "Load more" page has landed, so a head refresh keeps that cursor.
  let pagedBeyond = false;
  let disposed = false;
  /** "Couldn't <verb> <object>. <Reason>." for an error line. */
  function failure(verb: string, object: string, cause: unknown): string {
    const copy = describeFailure({ verb, object }, cause);
    return copy.detail ? `${copy.headline} ${copy.detail}` : copy.headline;
  }
  const labels = { pending: "Needs review", decided: "Decision saved · delivery pending", dispatching: "Delivery attempted · awaiting provider confirmation", resolved: "Provider confirmed resolution", cancelled: "Request cancelled" };
  async function refresh(more = false) {
    if (loading || busy || pending || disposed || !active) return;
    loading = true;
    try {
      // The store lists a run's requests oldest first, so on a long run the
      // ones waiting now sit pages behind those already answered. The first
      // read leads with what is still pending; paging walks the history.
      const [waiting, page] = await Promise.all([
        more ? null : listPendingDecisions(run.id),
        listAgentDecisions(run.id, more ? cursor ?? undefined : undefined),
      ]);
      if (disposed) return;
      const items = [...(waiting?.items ?? []), ...page.items];
      if (items.some((item) => item.task_id !== run.task_id || item.repository_id !== run.repository_id || item.source_revision !== run.source_revision || item.session_id !== run.session_id)) throw new WorkbenchError("protocol_error", "A request does not belong to this run.");
      const sets = Object.fromEntries(items.map((item) => [item.id, decisionQuestions(item)]));
      // One row per request. A refresh folds the fresh pages into the rows the
      // reader has paged in rather than replacing them. A loaded request that
      // is no longer in the pending list has been answered since and its history
      // page may not be loaded, so it is dropped rather than shown as "Needs
      // review"; with more than a page pending the list cannot prove that.
      rows = mergeLoadedPage(rows, items, { more, dropStale: waiting && !waiting.has_more ? (item) => item.state === "pending" : undefined });
      const kept = new Set(rows.map((item) => item.id));
      questions = Object.fromEntries(Object.entries({ ...questions, ...sets }).filter(([id]) => kept.has(id)));
      if (more) pagedBeyond = true;
      total = page.total; error = "";
      if (more || !pagedBeyond) cursor = page.next_cursor;
    } catch (cause) { if (!disposed) error = failure("load", "agent requests", cause); }
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
        error = failure("save", "the decision", cause);
        if (cause instanceof WorkbenchError && !["transport_error", "worker_error", "store_error", "protocol_error", "timeout"].includes(cause.code)) pending = null;
      }
    } finally { if (!disposed) busy = false; }
  }
</script>

<section class="agent-decisions gp-card" aria-label="Agent requests">
  <div class="heading"><h4>Agent requests</h4><button class="gp-btn" data-size="sm" type="button" onclick={() => refresh()} disabled={loading || busy || !!pending}>Refresh requests</button></div>
  <p>Decisions apply to the exact request below. Saving a decision does not prove that the provider received it or accept the task.</p>
  <p>{rows.length} shown / {total} requests</p>
  {#if error}<div class="gp-notice" data-tone="danger" role="alert">{error}</div>{/if}
  {#if pending}<div class="gp-notice" data-tone="unknown" role="status"><span class="min-w-0 flex-1">Couldn't confirm the save. Try again with this exact decision before choosing another.</span><button class="gp-btn shrink-0" data-size="sm" type="button" onclick={() => { if (pending) void decide(pending.source, pending.input.decision); }} disabled={busy}>Try again</button></div>{/if}
  {#if !rows.length && loading}<Skeleton variant="text" count={3} />
  {:else if !rows.length && !error}<EmptyState compact icon={MessageSquare} title="No agent requests" hint="Terminal handoffs ask in the provider’s terminal." />{/if}
  {#each rows as item (item.id)}
    <article>
      <strong>{item.kind === "permission" ? "Permission request" : "Question"} · {labels[item.state]}</strong>
      <p>Repository {item.repository_id} · saved task revision {item.source_revision} · policy revision {item.policy_revision}</p>
      <p>{item.cwd}</p>
      <textarea class="payload gp-field gp-field-multi" readonly rows="8" aria-label="Complete agent request" value={item.payload}></textarea>
      <details><summary>Request identity</summary><p>Run {item.run_id} · session {item.session_id}</p><p>Provider task {item.provider_thread_id} · turn {item.provider_turn_id} · request {item.protocol_request_id}</p><p>SHA-256 {item.payload_digest}</p></details>
      <p>Expires {new Date(item.expires_at * 1000).toLocaleString()}</p>
      {#if item.reason}<p>{item.reason}</p>{/if}
      {#if item.decision}<p>Saved response: {item.decision === "allow_once" ? "Allow once" : item.decision === "deny" ? "Deny" : item.answer}</p>{/if}
      {#if item.state === "pending" && item.actionable}
        <fieldset disabled={busy || !!pending || !active || Date.now() >= item.expires_at * 1000}>
          {#if item.kind === "question"}
            {#if questions[item.id]?.length}
              {#each questions[item.id] as question (question.id)}
                <label>{question.question}<textarea class="gp-field gp-field-multi" value={structured[item.id]?.[question.id] ?? ""} oninput={(event) => { structured[item.id] = {...structured[item.id], [question.id]:event.currentTarget.value}; }} maxlength="4096" rows="3"></textarea></label>
                {#each question.options as option}
                  <button class="gp-btn" type="button" title={option.description} onclick={() => { structured[item.id] = {...structured[item.id], [question.id]:option.label}; }}>{option.label}</button>
                {/each}
              {/each}
              <button class="gp-btn" type="button" onclick={() => decide(item, "answer")} disabled={!questions[item.id].every((q) => structured[item.id]?.[q.id]?.trim())}>Save answers</button>
            {:else}
              <label>Answer<textarea class="gp-field gp-field-multi" bind:value={answers[item.id]} maxlength="16384" rows="3"></textarea></label><button class="gp-btn" type="button" onclick={() => decide(item, "answer")} disabled={!answers[item.id]?.trim()}>Save answer</button>
            {/if}
          {:else}<button class="gp-btn" type="button" onclick={() => decide(item, "allow_once")}>Allow once</button>{/if}
          <button class="gp-btn" type="button" onclick={() => decide(item, "deny")}>Deny request</button>
        </fieldset>
      {:else if item.state === "pending"}<p>This request expired or its target changed. Return to the provider for a current request.</p>{/if}
    </article>
  {/each}
  {#if cursor && rows.length >= 180}<p>Showing {rows.length} of {total} requests. The rest are not loaded here.</p>
  {:else if cursor}<button class="gp-btn" type="button" onclick={() => refresh(true)} disabled={loading || busy || !!pending}>Load more requests</button>{/if}
</section>

<style>
  .agent-decisions{margin:12px 0;padding:12px;display:flex;flex-direction:column;gap:8px}
  .heading{display:flex;align-items:center;justify-content:space-between;gap:8px}
  h4{margin:0}
  p{margin:0;color:rgb(var(--c-text-muted));line-height:1.5;overflow-wrap:anywhere}
  article{display:flex;flex-direction:column;gap:6px;padding:12px;border-radius:var(--gp-r-md);background:var(--mac-fill-bg,rgb(var(--c-bg)))}
  .payload{font-family:monospace;max-height:280px;overflow:auto;white-space:pre-wrap;overflow-wrap:anywhere;font-size:var(--text-meta)}
  fieldset{padding:0;border:0}
  fieldset button{margin:4px 6px 4px 0}
  label{display:grid;gap:6px}
  textarea{font:inherit;width:100%}
  summary{cursor:pointer}
</style>
