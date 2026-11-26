<script lang="ts">
  /**
   * One permission request, answered in place. What the agent wants to run is
   * shown in full before the answer: the command, the URL, or for an edit
   * the change itself (built from the request's own input). The host echoes
   * that same input back when allowed, so what is shown is what runs.
   *
   * Answers are buttons only, with no single-key shortcut: a reader who lands
   * on the card and starts typing must never allow a command by accident.
   * The card can take focus (the composer's "Review" moves it here) so a
   * screen reader starts at the request, but focus alone answers nothing.
   */
  import { onDestroy } from "svelte";
  import { ShieldQuestion, Check, X, Copy, LoaderCircle } from "@lucide/svelte";
  import type { ToolItem } from "../../mavi/types";
  import { hunksFromInput } from "../../mavi/changes";
  import { describeTool, isEditTool } from "../../mavi/rows";
  import { createCopyFlash } from "../../desktop/clipboard";
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
  let pendingDecision = $state<"allow" | "deny" | null>(null);
  let denying = $state(false);
  let note = $state("");
  let error = $state<string | null>(null);
  let copied = $state(false);
  const commandCopy = createCopyFlash((value) => (copied = value), 1600);

  onDestroy(commandCopy.dispose);

  let hunks = $derived(isEditTool(tool.name) ? hunksFromInput(tool) : []);
  let subject = $derived(describeTool(tool, root));
  let command = $derived(tool.name === "Bash" && typeof tool.input.command === "string" ? tool.input.command : null);
  let why = $derived(
    tool.name === "Bash" && typeof tool.input.description === "string" ? tool.input.description : null,
  );

  async function answer(decision: "allow" | "deny") {
    if (busy) return;
    busy = true;
    pendingDecision = decision;
    error = null;
    try {
      await onAnswer(decision, decision === "deny" ? note : undefined);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
      pendingDecision = null;
    }
  }

  async function copyCommand() {
    if (!command) return;
    await commandCopy.copy(command);
  }
</script>

<section
  class="mavi-approval gp-card overflow-hidden"
  aria-label="Permission request: {tool.name}"
  data-request={tool.approval?.requestId}
  tabindex="-1"
>
  <header class="mavi-approval-head flex items-center gap-2 bg-warn/10 text-xs">
    <ShieldQuestion size={14} class="text-warn shrink-0" aria-hidden="true" />
    <span class="font-medium text-textPrimary truncate">
      {tool.name === "Bash" ? "Run a command?" : isEditTool(tool.name) ? `${tool.name === "Write" ? "Write" : "Edit"} ${subject}?` : `Use ${tool.name}?`}
    </span>
    {#if tool.parentId}
      <span class="gp-chip shrink-0" data-tone="warn" title="Asked by a subagent the agent started">Subagent</span>
    {/if}
    {#if tool.approval?.reason && tool.approval.reason !== subject}
      <span class="text-meta text-textMuted truncate ml-auto">{tool.approval.reason}</span>
    {/if}
  </header>

  <div class="mavi-approval-body">
    {#if tool.approval?.hiddenBytes !== undefined}
      <p class="text-meta text-warn">
        This request's input ({tool.approval.hiddenBytes.toLocaleString()} bytes) was too large to show safely. Allowing runs it exactly as the agent sent it; deny it if you can't tell what it does.
      </p>
    {:else if command}
      {#if why}<p class="text-meta text-textMuted mb-2">{why}</p>{/if}
      <div class="relative">
        <pre class="mavi-command mavi-command-bash font-mono text-meta select-text">{command}</pre>
        <span class="mavi-copy-anchor">
          <button
            type="button"
            class="gp-icon-btn bg-surface/90"
            data-size="sm"
            aria-label={copied ? "Copied command" : "Copy command"}
            title={copied ? "Copied" : "Copy command"}
            onclick={copyCommand}
          >
            {#if copied}
              <Check size={12} class="text-ok" aria-hidden="true" />
            {:else}
              <Copy size={12} aria-hidden="true" />
            {/if}
          </button>
        </span>
      </div>
    {:else if hunks.length}
      <div class="mavi-proposed" class:mavi-proposed-compact={compact}>
        <MaviHunks {hunks} numbered={false} maxLines={compact ? 40 : 200} />
      </div>
    {:else if !isEditTool(tool.name)}
      <pre class="mavi-command font-mono text-meta select-text">{JSON.stringify(tool.input, null, 2).slice(0, 2000)}</pre>
    {/if}

    {#if denying}
      <label class="block mt-3">
        <span class="gp-label">Tell the agent why (optional)</span>
        <textarea
          class="gp-field gp-field-multi w-full mt-1.5"
          rows="2"
          bind:value={note}
          placeholder="Not that file; edit the test instead"
        ></textarea>
      </label>
    {/if}

    <footer class="flex items-center gap-2 mt-3.5">
      {#if denying}
        <button class="gp-btn" data-variant="danger" disabled={busy} onclick={() => answer("deny")}>
          {#if busy && pendingDecision === "deny"}
            <LoaderCircle size={12} class="animate-spin" aria-hidden="true" />
          {:else}
            <X size={12} aria-hidden="true" />
          {/if}
          Deny
        </button>
        <button class="gp-btn" data-variant="ghost" disabled={busy} onclick={() => (denying = false)}>Back</button>
      {:else}
        <button class="gp-btn" data-variant="primary" disabled={busy} onclick={() => answer("allow")}>
          {#if busy && pendingDecision === "allow"}
            <LoaderCircle size={12} class="animate-spin" aria-hidden="true" />
          {:else}
            <Check size={12} aria-hidden="true" />
          {/if}
          Allow
        </button>
        <button class="gp-btn" disabled={busy} onclick={() => (denying = true)}>Deny</button>
      {/if}
      {#if error}<span class="text-meta text-danger" role="alert">{error}</span>{/if}
    </footer>
  </div>
</section>

<style>
  .mavi-approval {
    border-color: rgb(var(--c-warn) / 0.65);
  }
  .mavi-approval:focus {
    outline: 2px solid rgb(var(--c-accent));
    outline-offset: 2px;
  }
  .mavi-approval-head {
    padding: 10px 16px;
    border-bottom: 1px solid rgb(var(--c-warn) / 0.35);
  }
  .mavi-approval-body {
    padding: 14px 16px;
  }
  .mavi-command {
    padding: 8px 10px;
    border-radius: var(--gp-r-sm);
    background: rgb(var(--c-bg) / 0.65);
    border: 1px solid var(--gp-edge);
    white-space: pre-wrap;
    word-break: break-all;
    max-height: 14rem;
    overflow: auto;
  }
  .mavi-command-bash {
    padding-right: 36px;
  }
  .mavi-copy-anchor {
    position: absolute;
    top: 4px;
    right: 4px;
  }
  .mavi-proposed {
    max-height: 22rem;
    overflow: auto;
  }
  .mavi-proposed-compact {
    max-height: 9rem;
  }
</style>
