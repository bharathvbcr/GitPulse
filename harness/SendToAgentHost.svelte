<script lang="ts">
  import SendToAgent from "../src/lib/components/SendToAgent.svelte";
  import type { AttentionSignal } from "../src/lib/workbench/client";

  // A CI notice's row, and an attempt's run card, as the two places the action
  // is offered. A narrow column on purpose: the panel must fit where it is put.
  let { signal, runId, cardRunId }: { signal: AttentionSignal; runId: string; cardRunId: string } = $props();
</script>

<div style="display:flex;gap:24px;padding:16px;background:rgb(11 14 23);color:#e6edf5;min-height:100vh">
  <article data-testid="ci-row" style="width:360px;display:flex;flex-wrap:wrap;gap:12px;padding:10px 12px;border:1px solid #2a3350;border-radius:10px">
    <div><strong>Coding agent branch failed CI</strong><br /><span>{signal.workflow} · {signal.branch} · {signal.conclusion}</span></div>
    <SendToAgent {runId} {signal} />
  </article>
  <article data-testid="run-card" style="width:360px;display:flex;flex-direction:column;gap:8px;padding:10px 12px;border:1px solid #2a3350;border-radius:10px">
    <strong>Claude Code · Terminal</strong>
    <SendToAgent runId={cardRunId} taskTitle="Fix the build" />
  </article>
</div>
