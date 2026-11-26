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
  import { Send, CheckCircle2, AlertCircle, LoaderCircle } from "@lucide/svelte";
  import { getTaskRun, newID, previewAgentMessage, sendAgentMessage, WorkbenchError, type AgentSendPreview, type AttentionSignal } from "../workbench/client";
  import { describeError, describeFailure } from "../ui/copy/describeFailure";
  import { AGENT_NOTE_MAX_CHARS, composeAgentMessage, confirmLabel, describeResume, describeSend, messageProblem, routeTitle, unknownOutcome } from "../workbench/sendToAgent";
  import { resumeTaskConversation } from "../workbench/taskTerminal";
  import { shortcutTextLabel } from "../ui/platformCopy";
  import { hostPlatform } from "../stores/platformStore";

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
  /**
   * A send whose outcome is not known: Manvi had not learned it (`pending`), cannot tell it from a lost one
   * (`unresolved`), or answered something this build cannot read. The message may have reached the agent, so the
   * note editor warns that a different note is a new message that would be sent in addition. Cleared only when
   * the panel is closed: editing the note does not make the earlier message not have happened.
   */
  let maybeDelivered = $state(false);
  /** Confirm is off and stays off for this message (`unresolved` is never resent), but it is not "Sent". */
  let closed = $state(false);
  /** The outcome is pending: Confirm stays armed and asks again with the same request id. */
  let askAgain = $state(false);
  /** Fresh per message, reused by a retry of the same press: a retry never sends twice. */
  let requestId = $state(newID());
  let generation = 0;
  /** "Couldn't <verb> <object>. <Reason>." for an error line. */
  function failure(verb: string, object: string, cause: unknown): string {
    const copy = describeFailure({ verb, object }, cause);
    return copy.detail ? `${copy.headline} ${copy.detail}` : copy.headline;
  }
  let timer: ReturnType<typeof setTimeout> | null = null;
  let disposed = false;

  const text = $derived(composeAgentMessage(signal, note));
  const problem = $derived(messageProblem(text));
  const confirm = $derived(preview ? confirmLabel(preview.route) : null);
  /** The text the displayed route was computed for; Confirm waits until it is the text on screen. */
  let previewedText = $state<string | null>(null);
  const settled = $derived(preview !== null && previewedText === text && !checking);
  const canConfirm = $derived(settled && !!confirm && !problem && !sending && !done && !closed);

  function onNoteKeydown(event: KeyboardEvent) {
    if (
      event.key === "Enter" &&
      (event.metaKey || event.ctrlKey) &&
      !event.altKey &&
      !event.shiftKey &&
      !event.isComposing &&
      event.keyCode !== 229
    ) {
      if (!canConfirm) return;
      event.preventDefault();
      void send();
    }
  }

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
      preview = null; previewedText = null; previewError = failure("check", "where this would go", cause);
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
    if (!open) { outcome = null; done = false; closed = false; askAgain = false; maybeDelivered = false; requestId = newID(); }
  }
  function edited() {
    // New text is a new message: a request id that carried other text would be refused.
    requestId = newID();
    outcome = null;
    done = false;
    closed = false;
    askAgain = false;
  }

  async function send() {
    if (!preview || preview.route === "unavailable" || !settled || problem || sending || done || closed) return;
    const route = preview.route, sentText = text, id = requestId, retried = askAgain;
    sending = true; outcome = null;
    try {
      const result = await sendAgentMessage(runId, sentText, id, route);
      if (disposed) return;
      if (result.route === "resume" && result.action === "open_resume") {
        const title = taskTitle ?? (await getTaskRun(runId)).task_title;
        const resumed = await resumeTaskConversation(
          { id: runId, task_title: title, ...(result.resumeProvider ? { provider: result.resumeProvider } : {}) },
          "background",
          sentText,
        );
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
        // same request id: a retry cannot send it twice. An outcome that is not
        // known is neither: `pending` keeps Confirm armed on the same request id,
        // the others close it, and all of them warn the note editor.
        const unknown = unknownOutcome(result);
        done = result.sent === true;
        askAgain = unknown === "ask-again";
        closed = unknown === "closed";
        // Asking again with the same request id settles that very message.
        if (unknown) maybeDelivered = true;
        else if (retried) maybeDelivered = false;
      }
    } catch (cause) {
      if (disposed) return;
      if (cause instanceof WorkbenchError && cause.code === "route_changed") {
        outcome = { tone: "error", text: describeError(cause, "send-to-agent") };
        void ask(sentText, id);
      } else {
        outcome = { tone: "error", text: `${failure("confirm", "the send", cause)} Press Confirm again to try the same message. It is not sent twice.` };
        // A managed follow-up gives up after MANAGED_CONTINUE_TIMEOUT (65 s) but
        // Manvi may have written it by then, so "not confirmed" is not "not sent":
        // editing the note must warn that the new text is sent in addition.
        if (route === "managed") maybeDelivered = true;
      }
    } finally {
      if (!disposed) sending = false;
    }
  }
</script>

<div class="send-to-agent" data-testid="send-to-agent">
  <button type="button" class="gp-btn open" data-size="sm" aria-expanded={open} onclick={toggle} data-testid="send-to-agent-open"><Send size={12} /> {label}</button>
  {#if open}
    <section class="panel gp-card" aria-label="Send a message to the agent" data-testid="send-to-agent-panel">
      <label class="note">
        <span>{signal ? "Add a note (optional)" : "Message"}</span>
        <textarea
          class="gp-field gp-field-multi"
          rows="3"
          maxlength={AGENT_NOTE_MAX_CHARS}
          bind:value={note}
          oninput={edited}
          onkeydown={onNoteKeydown}
          data-testid="send-to-agent-note"
          placeholder={signal ? "Anything to add for the agent" : "What should the agent hear?"}
        ></textarea>
      </label>
      {#if maybeDelivered}
        <div class="gp-notice" data-tone="warn" role="note" data-testid="send-to-agent-edit-warning">
          {outcome
            ? "This message may already have reached the agent. Editing the note starts a new message, sent in addition to it, with a new request id."
            : "An earlier message may already have reached the agent. This is a new message, sent in addition to it."}
        </div>
      {/if}
      <div class="preview-frame">
        <div class="preview-head">
          <span class="caption">The agent is sent exactly this:</span>
          {#if note.length > 0}
            <span class="char-count tabular-nums">{note.length} / {AGENT_NOTE_MAX_CHARS}</span>
          {/if}
        </div>
        <pre class="text" data-testid="send-to-agent-text">{text || "(nothing yet)"}</pre>
      </div>
      {#if problem}<div class="gp-notice" data-tone="danger" role="alert" data-testid="send-to-agent-problem">{problem}</div>{/if}
      <div
        class="route"
        class:route-error={Boolean(previewError && !checking && !preview)}
        role="status"
        aria-live="polite"
        data-testid="send-to-agent-route"
        data-route={settled && preview ? preview.route : (checking ? "checking" : "none")}
      >
        {#if checking || (preview && !settled)}
          <LoaderCircle size={14} class="animate-spin text-accent shrink-0 mt-0.5" aria-hidden="true" />
          <div class="route-body">
            <span>Checking where this would go…</span>
          </div>
        {:else if preview}
          {#if preview.route === "unavailable"}
            <AlertCircle size={14} class="text-warn shrink-0 mt-0.5" aria-hidden="true" />
          {:else}
            <CheckCircle2 size={14} class="{preview.route === 'mailbox' ? 'text-ok' : 'text-accent'} shrink-0 mt-0.5" aria-hidden="true" />
          {/if}
          <div class="route-body">
            <strong>{routeTitle(preview.route)}</strong>
            <span data-testid="send-to-agent-reason">{preview.reason}</span>
          </div>
        {:else if previewError}
          <AlertCircle size={14} class="text-danger shrink-0 mt-0.5" aria-hidden="true" />
          <div class="route-body">
            <span>{previewError} Nothing can be sent until the route is known.</span>
          </div>
        {/if}
      </div>
      <div class="actions">
        <button
          type="button"
          class="gp-btn"
          data-variant="primary"
          data-size="sm"
          onclick={() => void send()}
          disabled={!canConfirm}
          data-testid="send-to-agent-confirm"
        >
          {sending ? "Sending…" : done ? "Sent" : closed ? "Not sent again" : askAgain ? "Ask again (same message)" : confirm ?? "Can't be sent"}
        </button>
        <kbd class="gp-keycap">{shortcutTextLabel("⌘↵", $hostPlatform.os)}</kbd>
        <button type="button" class="gp-btn" data-size="sm" onclick={toggle} disabled={sending} data-testid="send-to-agent-cancel">Cancel</button>
      </div>
      {#if outcome}<div class="gp-notice" data-tone={outcome.tone === "error" ? "danger" : "info"} role={outcome.tone === "error" ? "alert" : "status"} data-testid="send-to-agent-outcome" data-outcome-tone={outcome.tone}>{outcome.text}</div>{/if}
    </section>
  {/if}
</div>

<style>
  .send-to-agent{display:flex;flex-direction:column;gap:8px;flex-basis:100%;min-width:0}
  .open{align-self:flex-start}
  .panel{display:flex;flex-direction:column;gap:10px;padding:14px 16px;font-size:var(--text-xs)}
  .note{display:flex;flex-direction:column;gap:4px;color:rgb(var(--c-text-muted))}
  textarea{min-width:0;width:100%;font:inherit}
  .preview-frame{min-width:0;border:1px solid var(--gp-edge);border-radius:var(--gp-r-sm);overflow:hidden;background:var(--mac-fill-bg,rgb(var(--c-bg)))}
  .preview-head{display:flex;align-items:center;justify-content:space-between;gap:8px;padding:5px 8px;background:rgb(var(--c-surface) / 0.65)}
  .caption{margin:0;color:rgb(var(--c-text-muted));font-size:var(--text-meta)}
  .char-count{color:rgb(var(--c-text-muted));font-size:var(--text-micro)}
  .text{margin:0;padding:8px;color:rgb(var(--c-text));font:var(--text-meta) ui-monospace,SFMono-Regular,Menlo,monospace;white-space:pre-wrap;overflow-wrap:anywhere;max-height:180px;overflow:auto}
  .route{display:flex;align-items:flex-start;gap:8px;padding:8px 10px;border-radius:var(--gp-r-md);border:1px solid var(--gp-edge-soft);background:rgb(var(--c-bg) / 0.55);line-height:1.45;color:rgb(var(--c-text))}
  .route:empty{display:none}
  .route[data-route="mailbox"]{border-color:rgb(var(--c-ok) / 0.35);background-image:linear-gradient(rgb(var(--c-ok) / 0.06),rgb(var(--c-ok) / 0.06))}
  .route[data-route="resume"],.route[data-route="managed"]{border-color:rgb(var(--c-accent) / 0.35);background-image:linear-gradient(rgb(var(--c-accent) / 0.06),rgb(var(--c-accent) / 0.06))}
  .route[data-route="unavailable"]{border-color:rgb(var(--c-warn) / 0.35);background-image:linear-gradient(rgb(var(--c-warn) / 0.06),rgb(var(--c-warn) / 0.06))}
  .route.route-error{border-color:rgb(var(--c-danger) / 0.35);background-image:linear-gradient(rgb(var(--c-danger) / 0.06),rgb(var(--c-danger) / 0.06))}
  .route-body{display:flex;flex-direction:column;gap:2px;min-width:0;flex:1}
  .route-body strong{font-weight:600}
  .route-body span{overflow-wrap:anywhere}
  .actions{display:flex;flex-wrap:wrap;align-items:center;gap:6px}
  .panel :global(.gp-notice){overflow-wrap:anywhere}
</style>
