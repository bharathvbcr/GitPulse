<script lang="ts">
  /**
   * One line in, one saved task out.
   *
   * The fastest path onto the board, and the reason the full editor can afford
   * to be a considered form rather than a speed-typing surface. Everything the
   * preview shows is computed by `parseQuickAdd` — the same call that builds
   * the draft — so what is drawn and what is saved cannot disagree.
   *
   * Nothing here is magic-on-save: a token the parser could not honour is
   * shown as a warning *before* Enter, and an unresolved `^repo` stays in the
   * title rather than disappearing into a repository nobody chose.
   */
  import { CornerDownLeft, Plus, TriangleAlert, X } from "@lucide/svelte";
  import {
    QUICK_ADD_HINT,
    parseQuickAdd,
    type QuickAddRepository,
    type QuickAddResult,
    type QuickAddTokenKind,
  } from "../workbench/taskQuickAdd";

  let {
    repositories = [],
    placeholder = "Add a task",
    disabled = false,
    busy = false,
    compact = false,
    onSubmit,
    onExpand,
  }: {
    repositories?: readonly QuickAddRepository[];
    placeholder?: string;
    disabled?: boolean;
    busy?: boolean;
    compact?: boolean;
    /** Resolves true when the task was created, so the field can clear. */
    onSubmit: (parsed: QuickAddResult) => Promise<boolean>;
    /** Hand the typed line to the full editor instead of saving it. */
    onExpand?: (parsed: QuickAddResult) => void;
  } = $props();

  let text = $state("");
  let focused = $state(false);
  let input: HTMLInputElement | undefined = $state();
  const parsed = $derived(parseQuickAdd(text, { repositories }));
  const showPreview = $derived(focused && text.trim().length > 0);

  export function focus() {
    input?.focus();
  }

  async function submit() {
    if (disabled || busy || !parsed.usable) return;
    if (await onSubmit(parsed)) text = "";
  }

  function onKey(event: KeyboardEvent) {
    if (event.key === "Enter") {
      event.preventDefault();
      // The full editor is one keystroke away, carrying whatever is typed —
      // so a line that outgrew one field never has to be retyped.
      if (event.shiftKey) onExpand?.(parsed);
      else void submit();
      return;
    }
    if (event.key === "Escape" && text) {
      // Stopped here so Escape clears the field rather than reaching the
      // board, where it would drop the selection instead.
      event.preventDefault();
      event.stopPropagation();
      text = "";
    }
  }

  function tokenClass(kind: QuickAddTokenKind): string {
    switch (kind) {
      case "priority": return "tok-priority";
      case "label": return "tok-label";
      case "owner": return "tok-owner";
      case "kind": return "tok-kind";
      case "repository": return "tok-repo";
      case "due": return "tok-due";
      case "separator": return "tok-sep";
      case "description": return "tok-desc";
      case "unknown": return "tok-unknown";
      default: return "";
    }
  }
</script>

<div class="quick-add" class:is-compact={compact} data-testid="task-quick-add">
  <div class="field">
    <Plus size={12} class="shrink-0 text-textMuted" />
    <input
      bind:this={input}
      bind:value={text}
      class="gp-field"
      type="text"
      aria-label="Quick add task"
      aria-describedby="quick-add-hint"
      {placeholder}
      maxlength="4096"
      autocomplete="off"
      spellcheck="false"
      disabled={disabled || busy}
      onfocus={() => { focused = true; }}
      onblur={() => { focused = false; }}
      onkeydown={onKey}
    />
    {#if text}
      <button type="button" class="gp-icon-btn" aria-label="Clear quick add" onclick={() => { text = ""; input?.focus(); }}>
        <X size={11} />
      </button>
    {/if}
    <button
      type="button"
      class="gp-btn"
      disabled={disabled || busy || !parsed.usable}
      title={parsed.usable ? "Create this task" : "Type a title first"}
      onclick={() => void submit()}
    >
      {busy ? "Adding…" : "Add"}<CornerDownLeft size={11} />
    </button>
  </div>

  {#if showPreview}
    <div class="preview" data-testid="task-quick-add-preview">
      <p class="tokens" aria-hidden="true">
        {#each parsed.segments as segment, index (index)}<span class={tokenClass(segment.kind)}>{segment.text}</span>{/each}
      </p>
      <p class="summary" role="status">
        {#if parsed.usable}
          <span class="chip chip-title">{parsed.title}</span>
          {#if parsed.priority !== null}<span class="chip tok-priority">{["Urgent", "High", "Normal", "Low"][parsed.priority]}</span>{/if}
          {#if parsed.kind}<span class="chip tok-kind">{parsed.kind}</span>{/if}
          {#if parsed.owner}<span class="chip tok-owner">{parsed.owner}</span>{/if}
          {#if parsed.repositoryId}<span class="chip tok-repo">{repositories.find((repo) => repo.id === parsed.repositoryId)?.name ?? parsed.repositoryId}</span>{/if}
          {#if parsed.dueAt}<span class="chip tok-due">{new Date(parsed.dueAt * 1000).toLocaleDateString()}</span>{/if}
          {#each parsed.labels as label (label)}<span class="chip tok-label">{label}</span>{/each}
          {#if parsed.description}<span class="chip tok-desc">notes</span>{/if}
        {:else}
          <span class="muted">Type a title. Markers alone do not make a task.</span>
        {/if}
      </p>
      {#each parsed.warnings as warning (warning.code + warning.message)}
        <p class="warn"><TriangleAlert size={11} class="shrink-0" />{warning.message}</p>
      {/each}
      <p id="quick-add-hint" class="hint">{QUICK_ADD_HINT}{#if onExpand} · ⇧⏎ opens the editor{/if}</p>
    </div>
  {:else}
    <p id="quick-add-hint" class="sr-only">{QUICK_ADD_HINT}</p>
  {/if}
</div>

<style>
  .quick-add{display:flex;flex-direction:column;gap:6px;min-width:0}
  .field{display:flex;align-items:center;gap:6px;min-width:0}
  .field input{flex:1;min-width:0;font-size:12px}
  .is-compact .field input{font-size:11px;padding:4px 7px}
  .preview{display:flex;flex-direction:column;gap:5px;padding:7px 9px;border-radius:9px;border:1px solid rgb(var(--c-border) / 0.65)}
  .tokens{margin:0;font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:11px;line-height:1.5;white-space:pre-wrap;overflow-wrap:anywhere;color:rgb(var(--c-text))}
  .summary{display:flex;flex-wrap:wrap;gap:4px;align-items:center;margin:0}
  .chip{font-size:10px;padding:1px 6px;border-radius:5px;background:rgb(var(--c-surface-hover) / 0.7);max-width:16rem;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
  .chip-title{font-weight:600;color:rgb(var(--c-text))}
  .muted,.hint{font-size:10px;color:rgb(var(--c-text-muted));margin:0}
  .warn{display:flex;align-items:flex-start;gap:5px;margin:0;font-size:10px;color:rgb(180 83 9);line-height:1.45}
  :global(.dark) .warn{color:rgb(252 211 77)}
  /* One hue per token kind, reused by the highlighted line and its chip so a
     reader maps colour to meaning once. */
  .tok-priority{color:#d15a64}
  .tok-label{color:rgb(var(--c-accent))}
  .tok-owner{color:#7c8cf8}
  .tok-kind{color:#2f9e6e}
  .tok-repo{color:#c08a2e}
  .tok-due{color:#b3722a}
  .tok-sep,.tok-desc{color:rgb(var(--c-text-muted))}
  .tok-unknown{color:rgb(var(--c-text-muted));text-decoration:underline wavy rgb(244 63 94 / 0.7);text-underline-offset:2px}
</style>
