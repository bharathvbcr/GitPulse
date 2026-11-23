<script lang="ts">
  /**
   * One permission request, answered in place. What the agent wants to run is
   * shown in full before the answer: the command, the URL, or for an edit
   * the change itself (built from the request's own input). The host echoes
   * that same input back when allowed, so what is shown is what runs.
   *
   * Keys work only while this card has focus (A allows, D opens deny), never
   * from the composer, where typing a word must not answer a request.
   */
  import { ShieldQuestion, Check, X } from "@lucide/svelte";
  import type { ToolItem } from "../../mavi/types";
  import { hunksFromInput } from "../../mavi/changes";
  import { describeTool, isEditTool } from "../../mavi/rows";
  import MaviHunks from "./MaviHunks.svelte";

  let {
    tool,
    root,
    onAnswer,
    compact = false,
  }: {
    tool: ToolItem;
    root: string | null;
    onAnswer: (decision: "allow" | "deny", message?: string) => Promise<void>;
    compact?: boolean;
  } = $props();

  let busy = $state(false);
  let denying = $state(false);
  let note = $state("");
  let error = $state<string | null>(null);

  let hunks = $derived(isEditTool(tool.name) ? hunksFromInput(tool) : []);
  let subject = $derived(describeTool(tool, root));
  let command = $derived(tool.name === "Bash" && typeof tool.input.command === "string" ? tool.input.command : null);
  let why = $derived(
    tool.name === "Bash" && typeof tool.input.description === "string" ? tool.input.description : null,
  );

  async function answer(decision: "allow" | "deny") {
    if (busy) return;
    busy = true;
    error = null;
    try {
      await onAnswer(decision, decision === "deny" ? note : undefined);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  function onKeydown(event: KeyboardEvent) {
    if (event.target instanceof HTMLTextAreaElement || event.metaKey || event.ctrlKey || event.altKey) return;
    if (event.key === "a" || event.key === "A") {
      event.preventDefault();
      void answer("allow");
    } else if (event.key === "d" || event.key === "D") {
      event.preventDefault();
      denying = true;
    }
  }
</script>

<!-- Justified: the card takes focus so its A/D keys apply only while it is
     focused; both answers are also real buttons inside it. -->
<!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
<section
  class="mavi-approval gp-card"
  aria-label="Permission request: {tool.name}"
  tabindex="0"
  onkeydown={onKeydown}
>
  <header class="flex items-center gap-2 text-xs">
    <ShieldQuestion size={14} class="text-warn shrink-0" aria-hidden="true" />
    <span class="font-medium text-textPrimary">{tool.name === "Bash" ? "Run a command?" : isEditTool(tool.name) ? `${tool.name === "Write" ? "Write" : "Edit"} ${subject}?` : `Use ${tool.name}?`}</span>
    {#if tool.approval?.reason && tool.approval.reason !== subject}
      <span class="text-meta text-textMuted truncate">{tool.approval.reason}</span>
    {/if}
  </header>

  {#if command}
    {#if why}<p class="text-meta text-textMuted mt-1">{why}</p>{/if}
    <pre class="mavi-command font-mono text-meta select-text">{command}</pre>
  {:else if hunks.length}
    <div class="mt-2 mavi-proposed" class:mavi-proposed-compact={compact}>
      <MaviHunks {hunks} numbered={false} maxLines={compact ? 40 : 200} />
    </div>
  {:else if !isEditTool(tool.name)}
    <pre class="mavi-command font-mono text-meta select-text">{JSON.stringify(tool.input, null, 2).slice(0, 2000)}</pre>
  {/if}

  {#if denying}
    <label class="block mt-2">
      <span class="gp-label">Tell the agent why (optional)</span>
      <textarea
        class="gp-field gp-field-multi w-full mt-1 py-1.5"
        rows="2"
        bind:value={note}
        placeholder="Not that file; edit the test instead"
      ></textarea>
    </label>
  {/if}

  <footer class="flex items-center gap-2 mt-2">
    {#if denying}
      <button class="gp-btn" data-variant="danger" disabled={busy} onclick={() => answer("deny")}>
        <X size={12} aria-hidden="true" /> Deny
      </button>
      <button class="gp-btn" data-variant="ghost" disabled={busy} onclick={() => (denying = false)}>Back</button>
    {:else}
      <button class="gp-btn" data-variant="primary" disabled={busy} onclick={() => answer("allow")}>
        <Check size={12} aria-hidden="true" /> Allow <span class="gp-keycap">A</span>
      </button>
      <button class="gp-btn" disabled={busy} onclick={() => (denying = true)}>
        Deny <span class="gp-keycap">D</span>
      </button>
    {/if}
    {#if error}<span class="text-meta text-danger" role="alert">{error}</span>{/if}
  </footer>
</section>

<style>
  .mavi-approval {
    padding: 10px 12px;
    border-color: rgb(var(--c-warn) / 0.65);
  }
  .mavi-approval:focus-visible {
    outline: 2px solid rgb(var(--c-accent));
    outline-offset: 2px;
  }
  .mavi-command {
    margin-top: 6px;
    padding: 6px 8px;
    border-radius: var(--gp-r-sm);
    background: rgb(var(--c-bg) / 0.65);
    border: 1px solid var(--gp-edge);
    white-space: pre-wrap;
    word-break: break-all;
    max-height: 14rem;
    overflow: auto;
  }
  .mavi-proposed {
    max-height: 22rem;
    overflow: auto;
  }
  .mavi-proposed-compact {
    max-height: 9rem;
  }
</style>
