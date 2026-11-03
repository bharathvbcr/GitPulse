<script lang="ts">
  /**
   * "Send to agent": a CI result, or a person's note, back into the agent that
   * pushed the branch.
   *
   * Nothing here sends on its own. Opening the panel composes the text and asks
   * the host which way it would go (`previewAgentMessage`, which sends nothing);
   * the panel shows that exact text and that route and why; and only the
   * Confirm button sends, along the route that was shown. If the attempt
   * changes in between, the host refuses (`route_changed`) and the panel asks
   * again rather than send somewhere the person did not see.
   */
  import { onDestroy, untrack } from "svelte";
  import { Send } from "@lucide/svelte";
  import { explainError, getTaskRun, newID, previewAgentMessage, sendAgentMessage, WorkbenchError, type AgentSendPreview, type AttentionSignal } from "../workbench/client";
  import { AGENT_NOTE_MAX_CHARS, composeAgentMessage, confirmLabel, describeResume, describeSend, messageProblem, routeTitle } from "../workbench/sendToAgent";
  import { resumeTaskConversation } from "../workbench/taskTerminal";

  let { runId, taskTitle, signal, label = "Send to agent" }: {
    /** The attempt whose agent hears it. */
    runId: string;
    /** The task's title, for a resumed conversation's tab. Read from the attempt when absent. */
    taskTitle?: string;
    /** The CI result to compose the message from. Without one the message is the note alone. */
    signal?: AttentionSignal;
    label?: string;
  } = $props();

  /** How long typing settles before the route is asked again. */
  const PREVIEW_DELAY_MS = 250;

  let open = $state(false);
  let note = $state("");
  let preview = $state<AgentSendPreview | null>(null);
  let previewError = $state("");
  let checking = $state(false);
  let sending = $state(false);
  let outcome = $state<{ tone: "note" | "error"; text: string } | null>(null);
  /**
   * This message has gone: sent, queued, or handed to a resumed conversation.
   * Confirm stays off until the text changes, so a second press cannot send the
   * same message twice; writing something different is a new message.
   */
  let done = $state(false);
  /** Fresh per message, reused by a retry of the same press: a retry never sends twice. */
  let requestId = $state(newID());
  let generation = 0;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let disposed = false;

  const text = $derived(composeAgentMessage(signal, note));
  const problem = $derived(messageProblem(text));
  const confirm = $derived(preview ? confirmLabel(preview.route) : null);
  /** The text the displayed route was computed for; Confirm waits until it is the text on screen. */
  let previewedText = $state<string | null>(null);
  const settled = $derived(preview !== null && previewedText === text && !checking);

  async function ask(forText: string, id: string) {
    const ticket = ++generation;
    checking = true;
    try {
      const next = await previewAgentMessage(runId, forText, id);
      if (disposed || ticket !== generation) return;
      preview = next; previewedText = forText; previewError = "";
    } catch (cause) {
      if (disposed || ticket !== generation) return;
      // A route that could not be asked for is not a route. Nothing is offered.
      preview = null; previewedText = null; previewError = explainError(cause);
    } finally {
      if (!disposed && ticket === generation) checking = false;
    }
  }

  $effect(() => {
    if (!open) return;
    const forText = text, id = requestId;
    if (timer) clearTimeout(timer);
    timer = null;
    if (messageProblem(forText)) {
      // Unsendable text is not asked about; the problem is on screen.
      untrack(() => { generation += 1; checking = false; preview = null; previewedText = null; previewError = ""; });
      return;
    }
    timer = setTimeout(() => { timer = null; void ask(forText, id); }, PREVIEW_DELAY_MS);
    return () => { if (timer) clearTimeout(timer); timer = null; };
  });
  onDestroy(() => { disposed = true; generation += 1; if (timer) clearTimeout(timer); });

  function toggle() {
    open = !open;
    if (!open) { outcome = null; done = false; requestId = newID(); }
  }
  function edited() {
    // New text is a new message: a request id that carried other text would be refused.
    requestId = newID();
    outcome = null;
    done = false;
  }

  async function send() {
    if (!preview || preview.route === "unavailable" || !settled || problem || sending || done) return;
    const route = preview.route, sentText = text, id = requestId;
    sending = true; outcome = null;
    try {
      const result = await sendAgentMessage(runId, sentText, id, route);
      if (disposed) return;
      if (result.route === "resume" && result.action === "open_resume") {
        const title = taskTitle ?? (await getTaskRun(runId)).task_title;
        const resumed = await resumeTaskConversation({ id: runId, task_title: title }, "background", sentText);
        if (disposed) return;
        if (resumed.outcome === "unavailable") {
          outcome = { tone: "error", text: `Nothing was sent. ${resumed.reason}` };
        } else {
          outcome = { tone: "note", text: describeResume(resumed.outcome === "queued" ? "queued" : "started") };
          done = true;
        }
      } else {
        outcome = describeSend(result);
        // A refusal that says to try again leaves the message unsent, with the
        // same request id: a retry cannot send it twice.
        done = result.sent;
      }
    } catch (cause) {
      if (disposed) return;
      if (cause instanceof WorkbenchError && cause.code === "route_changed") {
        outcome = { tone: "error", text: `${cause.message}` };
        void ask(sentText, id);
      } else {
        outcome = { tone: "error", text: `Not confirmed sent: ${explainError(cause)} Pressing Confirm again retries this same message without sending it twice.` };
      }
    } finally {
      if (!disposed) sending = false;
    }
  }
</script>

<div class="send-to-agent" data-testid="send-to-agent">
  <button type="button" class="gp-btn open" aria-expanded={open} onclick={toggle} data-testid="send-to-agent-open"><Send size={12} /> {label}</button>
  {#if open}
    <section class="panel" aria-label="Send a message to the agent" data-testid="send-to-agent-panel">
      <label class="note">
        <span>{signal ? "Add a note (optional)" : "Message"}</span>
        <textarea class="gp-field gp-field-multi" rows="3" maxlength={AGENT_NOTE_MAX_CHARS} bind:value={note} oninput={edited} data-testid="send-to-agent-note"
          placeholder={signal ? "Anything to add for the agent" : "What should the agent hear?"}></textarea>
      </label>
      <p class="caption">The agent is sent exactly this:</p>
      <pre class="text" data-testid="send-to-agent-text">{text || "(nothing yet)"}</pre>
      {#if problem}<p class="problem" role="alert" data-testid="send-to-agent-problem">{problem}</p>{/if}
      <div class="route" role="status" aria-live="polite" data-testid="send-to-agent-route" data-route={settled && preview ? preview.route : (checking ? "checking" : "none")}>
        {#if checking || (preview && !settled)}
          <span>Checking where this would go…</span>
        {:else if preview}
          <strong>{routeTitle(preview.route)}</strong>
          <span data-testid="send-to-agent-reason">{preview.reason}</span>
        {:else if previewError}
          <span class="problem">Could not check where this would go, so nothing can be sent: {previewError}</span>
        {/if}
      </div>
      <div class="actions">
        <button type="button" class="gp-btn-primary" onclick={() => void send()} disabled={!settled || !confirm || !!problem || sending || done} data-testid="send-to-agent-confirm">
          {sending ? "Sending…" : done ? "Sent" : confirm ?? "Cannot be sent"}
        </button>
        <button type="button" class="gp-btn" onclick={toggle} disabled={sending} data-testid="send-to-agent-cancel">Cancel</button>
      </div>
      {#if outcome}<p class="outcome" data-tone={outcome.tone} role={outcome.tone === "error" ? "alert" : "status"} data-testid="send-to-agent-outcome">{outcome.text}</p>{/if}
    </section>
  {/if}
</div>

<style>
  .send-to-agent{display:flex;flex-direction:column;gap:8px;flex-basis:100%;min-width:0}
  .open{align-self:flex-start}
  .panel{display:flex;flex-direction:column;gap:7px;padding:9px 10px;border:1px solid rgb(var(--c-border));border-radius:8px;background:rgb(var(--c-bg)/.35);font-size:12px}
  .note{display:flex;flex-direction:column;gap:4px;color:rgb(var(--c-text-muted))}
  textarea{min-width:0;width:100%;padding:7px;border:1px solid rgb(var(--c-border));border-radius:6px;background:var(--mac-fill-bg,rgb(var(--c-bg)));color:rgb(var(--c-text));font:inherit}
  .caption{margin:0;color:rgb(var(--c-text-muted));font-size:11px}
  .text{margin:0;padding:7px 8px;border:1px solid rgb(var(--c-border));border-radius:6px;background:rgb(var(--c-bg)/.5);color:rgb(var(--c-text));font:11.5px ui-monospace,SFMono-Regular,Menlo,monospace;white-space:pre-wrap;overflow-wrap:anywhere;max-height:180px;overflow:auto}
  .route{display:flex;flex-direction:column;gap:2px;line-height:1.45;color:rgb(var(--c-text))}
  .route strong{font-weight:600}
  .route span{overflow-wrap:anywhere}
  .actions{display:flex;flex-wrap:wrap;gap:6px}
  .problem,.outcome[data-tone="error"]{color:#dc6565}
  .problem{margin:0}
  .outcome{margin:0;padding:6px 8px;border-radius:7px;background:rgb(var(--c-accent)/.08);color:rgb(var(--c-text));overflow-wrap:anywhere}
  .outcome[data-tone="error"]{background:rgb(220 101 101/.1)}
  button:disabled{opacity:.5}
</style>
